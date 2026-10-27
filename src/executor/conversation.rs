use super::{arguments, launch};
use crate::{config::Config, router::Decision};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    process::Stdio,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct AuthenticationRequired {
    pub safe_to_retry: bool,
}
impl std::fmt::Display for AuthenticationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "provider authentication is required")
    }
}
impl std::error::Error for AuthenticationRequired {}

/// The turn ran past the deadline it was given and was stopped.
#[derive(Debug)]
pub struct DeadlineExceeded;
impl std::fmt::Display for DeadlineExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "provider exceeded the turn deadline")
    }
}
impl std::error::Error for DeadlineExceeded {}

/// Sees every provider event, with the text it displays; an error stops the turn.
pub type Observer<'a> = &'a mut dyn FnMut(&Value, Option<&str>) -> Result<()>;

pub fn conversation(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    output: &mut dyn Write,
) -> Result<String> {
    let cancel = super::CancellationToken::default();
    let _interrupt = super::process::InterruptGuard::install(&cancel)?;
    conversation_with_cancellation(d, prompt, cfg, output, &cancel)
}

/// `conversation` that keeps, or continues, a provider session.
pub fn conversation_resumable(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    output: &mut dyn Write,
    session: Option<&mut super::ProviderSession>,
) -> Result<String> {
    let cancel = super::CancellationToken::default();
    let _interrupt = super::process::InterruptGuard::install(&cancel)?;
    conversation_with_session(d, prompt, cfg, output, &cancel, session)
}

pub fn conversation_with_cancellation(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    output: &mut dyn Write,
    cancel: &super::CancellationToken,
) -> Result<String> {
    conversation_with_session(d, prompt, cfg, output, cancel, None)
}

/// Like `conversation_with_cancellation`, but keeps (or continues) a provider
/// session so the next turn can resume it. `None` keeps the turn ephemeral.
pub fn conversation_with_session(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    output: &mut dyn Write,
    cancel: &super::CancellationToken,
    session: Option<&mut super::ProviderSession>,
) -> Result<String> {
    conversation_observed(
        d,
        prompt,
        cfg,
        output,
        cancel,
        session,
        None,
        &mut |_, _| Ok(()),
    )
}

/// `conversation_with_session` with a deadline and an observer of the raw events.
#[allow(clippy::too_many_arguments)]
pub fn conversation_observed(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    output: &mut dyn Write,
    cancel: &super::CancellationToken,
    session: Option<&mut super::ProviderSession>,
    timeout: Option<Duration>,
    observe: Observer,
) -> Result<String> {
    if cancel.is_cancelled() {
        bail!("Turn cancelled; partial output was not saved or replayed.");
    }
    let persist = session.is_some();
    let resume = session.as_deref().is_some_and(|s| s.resume);
    let prompt = &*super::with_knowledge(&d.runtime, prompt, cfg, resume);
    let command = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude.command,
        "codex" => &cfg.inner.codex.command,
        "gemini" => &cfg.inner.gemini.command,
        "agy" => &cfg.inner.agy.command,
        _ => bail!("unsupported runtime"),
    };
    let mut args = arguments(d, cfg)?;
    match d.runtime.as_str() {
        "gemini" | "agy" => {
            let index = args
                .iter()
                .position(|a| a == "--output-format")
                .context("missing output format")?;
            args[index + 1] = "stream-json".into();
        }
        "claude" => {
            let index = args
                .iter()
                .position(|a| a == "--output-format")
                .context("missing output format")?;
            args[index + 1] = "stream-json".into();
            args.extend(["--verbose".into(), "--include-partial-messages".into()]);
            if !persist {
                args.push("--no-session-persistence".into());
            }
        }
        "codex" => {
            let index = args.len() - 2;
            let mut flags = vec![OsString::from("--json")];
            if !persist {
                flags.push("--ephemeral".into());
            }
            args.splice(index..index, flags);
        }
        _ => unreachable!(),
    }
    if let Some(session) = session.as_deref() {
        args = super::session_arguments(args, &d.runtime, session)?;
    }
    let mut command = launch(command)?;
    command
        .args(args)
        .envs(crate::mcp::for_launch(&d.runtime, cfg).1);
    // A tool-less launch starts in an empty directory, so no project file
    // (`.claude/`, CLAUDE.md) in Nectra's own directory applies.
    let neutral = cfg.no_tools.then(tempfile::tempdir).transpose()?;
    if let Some(dir) = &neutral {
        command.current_dir(dir.path());
    }
    // The thread id is kept even when the turn fails: it spent tokens, and
    // `nectra usage` finds them through it.
    let mut thread = None;
    // agy reads one NDJSON message per line; a newline inside the text is escaped by JSON.
    let message = (d.runtime == "agy")
        .then(|| format!("{}\n", json!({"event":"user","message":{"content":prompt}})));
    let reply = run_command(
        d,
        message.as_deref().unwrap_or(prompt),
        &mut command,
        output,
        cancel,
        timeout,
        &mut thread,
        observe,
    );
    if let (Some(session), Some(thread)) = (session, thread) {
        session.id.get_or_insert(thread);
    }
    reply
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_command(
    d: &Decision,
    prompt: &str,
    command: &mut std::process::Command,
    output: &mut dyn Write,
    cancel: &super::CancellationToken,
    timeout: Option<Duration>,
    thread: &mut Option<String>,
    observe: Observer,
) -> Result<String> {
    if cancel.is_cancelled() {
        bail!("Turn cancelled; partial output was not saved or replayed.");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owned = super::process::OwnedChild::spawn(command)?;
    let started = Instant::now();
    crate::activity::set_phase(crate::activity::Phase::Waiting);
    let mut stderr = owned
        .child
        .stderr
        .take()
        .context("opening agent diagnostics")?;
    let activity = crate::activity::current();
    let diagnostics = thread::spawn(move || -> std::io::Result<()> {
        // Never cut inside a credential, so one split across two reads is still masked.
        let mut buffer = [0; 8192];
        let mut pending = vec![];
        loop {
            let count = stderr.read(&mut buffer)?;
            pending.extend_from_slice(&buffer[..count]);
            let end = crate::redact::ready(&pending, count == 0, crate::redact::LINE);
            if end == 0 && count != 0 {
                continue;
            }
            let lines: Vec<u8> = pending.drain(..end).collect();
            let text = crate::redact::redact(&String::from_utf8_lossy(&lines));
            crate::activity::diagnostic(&activity, text.as_bytes())?;
            if count == 0 {
                return Ok(());
            }
        }
    });
    let mut stdin = owned.child.stdin.take().context("opening agent stdin")?;
    let prompt = prompt.as_bytes().to_vec();
    let writer = thread::spawn(move || stdin.write_all(&prompt));
    let stdout = owned.child.stdout.take().context("opening agent stdout")?;
    let (tx, rx) = mpsc::sync_channel(8);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = vec![];
            let result = (&mut reader)
                .take((MAX_EVENT_BYTES + 1) as u64)
                .read_until(b'\n', &mut line);
            let result = match result {
                Ok(0) => break,
                Ok(length) if length > MAX_EVENT_BYTES => {
                    Err(std::io::Error::other("agent event exceeded 8 MiB"))
                }
                Ok(_) => Ok(line),
                Err(error) => Err(error),
            };
            let failed = result.is_err();
            if tx.send(result).is_err() || failed {
                break;
            }
        }
    });
    let mut state = StreamState::default();
    let mut exited = None;
    let mut exit_time = None;
    let mut completion_time = None;
    let mut shutdown_time = None;
    let streamed = (|| -> Result<()> {
        loop {
            if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                return Err(DeadlineExceeded.into());
            }
            if cancel.is_cancelled() {
                bail!("Turn cancelled; partial output was not saved or replayed.");
            }
            if exited.is_none() {
                exited = owned.child.try_wait()?;
                if exited.is_some() {
                    exit_time = Some(Instant::now());
                }
            }
            if exit_time.is_some_and(|t| t.elapsed() >= Duration::from_millis(500)) {
                owned.stop();
                exit_time = None;
                shutdown_time = Some(Instant::now());
            }
            if shutdown_time.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
                bail!("provider descendants retained output after shutdown; turn was not saved");
            }
            // Claude Code 2.1 takes 1.7-2.0 s to exit after its result event, so 2 s flaked.
            if completion_time.is_some_and(|t: Instant| t.elapsed() >= Duration::from_secs(10)) {
                bail!("provider did not exit after completing its turn; stopped without saving a completed answer");
            }
            let line = match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => line?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let event: Value = serde_json::from_slice(&line)
                .context("agent emitted an invalid JSON event; check CLI version and extra_args")?;
            if let Some(phase) = crate::activity::event_phase(&d.runtime, &event) {
                crate::activity::set_phase(phase);
            }
            let text = state.event(&d.runtime, &event);
            observe(&event, text.as_ref().ok().and_then(Option::as_deref))?;
            if let Some(text) = text? {
                write!(output, "{text}")?;
                output.flush()?;
            }
            if state.completed && completion_time.is_none() {
                completion_time = Some(Instant::now());
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while exited.is_none() {
            if cancel.is_cancelled() {
                bail!("Turn cancelled; partial output was not saved or replayed.");
            }
            exited = owned.child.try_wait()?;
            if Instant::now() >= deadline {
                bail!("provider closed output but did not exit; turn was not saved");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    *thread = state.thread.take();
    // Kill the owned job/group before joining: descendants may still hold pipes open.
    owned.stop();
    drop(rx);
    let deadline = Instant::now() + Duration::from_secs(2);
    let read = super::process::join_until(reader, deadline);
    let diagnosed = super::process::join_until(diagnostics, deadline);
    let written = super::process::join_until(writer, deadline);
    if cancel.is_cancelled() {
        bail!("Turn cancelled; partial output was not saved or replayed.");
    }
    streamed?;
    read?;
    diagnosed??;
    if !exited.context("missing provider exit status")?.success() {
        bail!("{} exited unsuccessfully", d.runtime);
    }
    written??;
    let reply = state.finish()?;
    writeln!(output)?;
    Ok(reply)
}

#[derive(Default)]
struct StreamState {
    activity: bool,
    reply: Option<String>,
    completed: bool,
    claude_displayed: String,
    last_error: Option<String>,
    /// Codex thread id, or agy conversation id, for resuming the session next turn.
    thread: Option<String>,
    agy_shown: usize,
}
impl StreamState {
    fn event(&mut self, runtime: &str, event: &Value) -> Result<Option<String>> {
        if runtime == "agy" {
            return self.agy_event(event);
        }
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "thread.started" {
            self.thread = event["thread_id"].as_str().map(String::from);
        }
        let failure = kind == "error"
            || (runtime == "gemini" && kind == "result" && event["status"] != "success")
            || kind == "turn.failed"
            || (kind == "assistant" && !event["error"].is_null())
            || (kind == "result"
                && (event["is_error"] == true
                    || event["subtype"].as_str().is_some_and(|s| s != "success")));
        if failure {
            self.classify(event)?;
        }
        if (runtime == "codex" && (kind == "item.started" || kind == "item.completed"))
            || (runtime == "claude"
                && (kind == "assistant" || kind == "stream_event" || kind == "user"))
        {
            self.activity = true;
        }
        match runtime {
            "gemini" => match kind {
                "tool_use" | "tool_result" => {
                    self.activity = true;
                    self.reply = None;
                }
                "message" if event["role"] == "assistant" => {
                    self.activity = true;
                    let text = event["content"]
                        .as_str()
                        .context("Gemini assistant message has no content")?;
                    let reply = self.reply.get_or_insert_with(String::new);
                    if reply.len() + text.len() > MAX_EVENT_BYTES {
                        bail!("Gemini reply exceeds 8 MiB");
                    }
                    reply.push_str(text);
                    return Ok(Some(text.into()));
                }
                "result" => {
                    if event["status"] != "success" {
                        bail!("Gemini turn failed: {}", event["error"]);
                    }
                    self.completed = true;
                }
                "error" => {
                    self.last_error = Some(event.to_string());
                }
                _ => {}
            },
            "claude" => match kind {
                "stream_event"
                    if event["parent_tool_use_id"].is_null()
                        && event["event"]["delta"]["type"] == "text_delta" =>
                {
                    if let Some(text) = event["event"]["delta"]["text"].as_str() {
                        self.claude_displayed.push_str(text);
                        if self.claude_displayed.len() > MAX_EVENT_BYTES {
                            let mut start = self.claude_displayed.len() - MAX_EVENT_BYTES;
                            while !self.claude_displayed.is_char_boundary(start) {
                                start += 1;
                            }
                            self.claude_displayed.drain(..start);
                        }
                        return Ok(Some(text.into()));
                    }
                }
                "result" => {
                    if event["is_error"].as_bool() == Some(true)
                        || event["subtype"].as_str().is_some_and(|s| s != "success")
                    {
                        bail!(
                            "Claude turn failed: {}",
                            event["result"]
                                .as_str()
                                .unwrap_or("no successful result; see agent diagnostics")
                        );
                    }
                    let text = event["result"]
                        .as_str()
                        .context("Claude result has no text")?
                        .to_string();
                    self.completed = true;
                    self.reply = Some(text.clone());
                    if !self.claude_displayed.ends_with(&text) {
                        return Ok(Some(if self.claude_displayed.is_empty() {
                            text
                        } else {
                            format!("\n{text}")
                        }));
                    }
                }
                _ => {}
            },
            "codex" => match kind {
                "item.completed" if event["item"]["type"] == "agent_message" => {
                    let text = event["item"]["text"]
                        .as_str()
                        .context("Codex message has no text")?
                        .to_string();
                    if event["item"]["phase"] != "commentary" {
                        self.reply = Some(text.clone());
                    }
                    return Ok(Some(format!("{text}\n")));
                }
                "turn.completed" => self.completed = true,
                "error" => {
                    let message = event["message"]
                        .as_str()
                        .unwrap_or("agent reported an error")
                        .to_string();
                    self.last_error = Some(message.clone());
                    return Ok(Some(format!("[Codex: {message}]\n")));
                }
                "turn.failed" => bail!(
                    "Codex turn failed: {}",
                    event["error"]["message"]
                        .as_str()
                        .or_else(|| event["message"].as_str())
                        .unwrap_or("see agent diagnostics")
                ),
                _ => {}
            },
            _ => bail!("unsupported runtime"),
        }
        Ok(None)
    }
    /// Turns a failure event into the typed error that fallback and the
    /// session-skip logic key on; Ok if it is none of them.
    fn classify(&self, event: &Value) -> Result<()> {
        if let Some(error) =
            crate::access::ModelUnavailable::detect(&event.to_string(), !self.activity)
        {
            return Err(error.into());
        }
        if let Some(limit) = crate::limits::RateLimited::from_error(event, !self.activity) {
            return Err(limit.into());
        }
        let diagnostic = event.to_string().to_lowercase();
        if [
            "authentication_error",
            "authentication required",
            "unauthenticated",
            "not logged in",
            "login required",
            "please log in",
            "invalid api key",
            "expired token",
            "token expired",
            "invalid_grant",
        ]
        .iter()
        .any(|s| diagnostic.contains(s))
        {
            return Err(AuthenticationRequired {
                safe_to_retry: !self.activity,
            }
            .into());
        }
        Ok(())
    }
    /// agy's stream-json: `init`, `step_update`s (the reply as `text_delta`s,
    /// tools as `tool` steps) and a final `result` with the whole `response`.
    fn agy_event(&mut self, event: &Value) -> Result<Option<String>> {
        match event["event"].as_str().unwrap_or("") {
            "init" => self.thread = event["conversation_id"].as_str().map(String::from),
            "step_update" => {
                let step = &event["step_update"];
                if self.thread.is_none() {
                    self.thread = step["conversation_id"].as_str().map(String::from);
                }
                match step["step_type"].as_str() {
                    Some("tool") => self.activity = true,
                    Some("agent_response") => {
                        if let Some(text) = step["text_delta"].as_str() {
                            self.activity = true;
                            self.agy_shown += text.len();
                            if self.agy_shown > MAX_EVENT_BYTES {
                                bail!("agy reply exceeds 8 MiB");
                            }
                            return Ok(Some(text.into()));
                        }
                    }
                    _ => {}
                }
            }
            "result" => {
                let result = &event["result"];
                if self.thread.is_none() {
                    self.thread = result["conversation_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(String::from);
                }
                if result["status"] != "SUCCESS" {
                    // The error is a bare string, not the object the shared classifiers read.
                    let message = result["error"].as_str().unwrap_or("see agent diagnostics");
                    self.classify(&json!({"type":"error","message":message}))?;
                    bail!("agy turn failed: {message}");
                }
                let reply = result["response"].as_str().unwrap_or_default();
                // Headless agy auto-denies a tool that needs approval and still
                // reports SUCCESS, with or without text: the work was not done.
                if let Some(denied) = result["denied_actions"]
                    .as_array()
                    .filter(|a| !a.is_empty())
                {
                    let names: Vec<&str> = denied
                        .iter()
                        .filter_map(|a| a["display_name"].as_str().or(a["action"].as_str()))
                        .collect();
                    bail!(
                        "agy was denied permission to act ({}): rerun with --dangerously-skip-permissions",
                        names.join(", ")
                    );
                }
                self.completed = true;
                self.reply = Some(reply.to_string());
            }
            _ => {}
        }
        Ok(None)
    }
    fn finish(self) -> Result<String> {
        if !self.completed {
            bail!("agent exited without a completed turn; partial output was not added to conversation history. {}", self.last_error.as_deref().unwrap_or("No completion event received."));
        }
        let reply = self
            .reply
            .context("agent completed without an assistant answer")?;
        if reply.trim().is_empty() {
            bail!("agent returned an empty answer");
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn gemini_stream_keeps_final_answer_and_detects_limits() {
        let mut state = StreamState::default();
        state
            .event(
                "gemini",
                &json!({"type":"message","role":"user","content":"input"}),
            )
            .unwrap();
        let error = state.event("gemini", &json!({"type":"result","status":"error","error":{"message":"rate limit exceeded"}})).unwrap_err();
        assert!(
            error
                .downcast_ref::<crate::limits::RateLimited>()
                .unwrap()
                .safe_to_retry
        );
        state
            .event(
                "gemini",
                &json!({"type":"message","role":"assistant","content":"Looking...","delta":true}),
            )
            .unwrap();
        state
            .event(
                "gemini",
                &json!({"type":"tool_use","tool_name":"read_file"}),
            )
            .unwrap();
        state
            .event("gemini", &json!({"type":"tool_result","output":"ignored"}))
            .unwrap();
        state
            .event(
                "gemini",
                &json!({"type":"message","role":"assistant","content":"Done","delta":true}),
            )
            .unwrap();
        state
            .event("gemini", &json!({"type":"result","status":"success"}))
            .unwrap();
        assert_eq!(state.finish().unwrap(), "Done");
    }
    /// Feeds a fixture's events to a fresh state; the first error, or the answer.
    fn agy_run(fixture: &str) -> Result<String> {
        let mut state = StreamState::default();
        for line in fixture.lines() {
            let event: Value = serde_json::from_str(line).unwrap();
            state.event("agy", &event)?;
        }
        state.finish()
    }
    #[test]
    fn agy_stream_replies_with_its_final_response_and_names_the_conversation() {
        let mut state = StreamState::default();
        let mut shown = String::new();
        for line in include_str!("../../tests/fixtures/agy-stream-tools.jsonl").lines() {
            let event: Value = serde_json::from_str(line).unwrap();
            shown += &state.event("agy", &event).unwrap().unwrap_or_default();
        }
        assert_eq!(shown, "done.\n");
        assert_eq!(
            state.thread.as_deref(),
            Some("d9945309-f13f-4e6c-963f-94f5a6fe5332")
        );
        assert_eq!(state.finish().unwrap(), "done.\n");
    }
    #[test]
    fn agy_errors_fall_back_by_class() {
        let bad = agy_run(include_str!("../../tests/fixtures/agy-bad-model.jsonl")).unwrap_err();
        assert!(bad.is::<crate::access::ModelUnavailable>());
        let denied = agy_run(include_str!("../../tests/fixtures/agy-denied.jsonl")).unwrap_err();
        assert!(denied
            .to_string()
            .contains("--dangerously-skip-permissions"));
        let partial = r#"{"event":"result","result":{"status":"SUCCESS","response":"I wrote it.","denied_actions":[{"action":"command","display_name":"RunCommand"}]}}"#;
        assert!(agy_run(partial)
            .unwrap_err()
            .to_string()
            .contains("RunCommand"));
        // Not captured live: see the fixture's first line.
        let lines: Vec<&str> = include_str!("../../tests/fixtures/agy-limit-and-auth.jsonl")
            .lines()
            .skip(1)
            .collect();
        let auth = agy_run(lines[0]).unwrap_err();
        assert!(
            auth.downcast_ref::<AuthenticationRequired>()
                .unwrap()
                .safe_to_retry
        );
        for limited in &lines[1..] {
            let limit = agy_run(limited).unwrap_err();
            let limit = limit.downcast_ref::<crate::limits::RateLimited>().unwrap();
            assert!(limit.safe_to_retry, "{limited}");
        }
        // The live quota error is the whole account's, until its stated reset.
        let quota = agy_run(lines[3]).unwrap_err();
        let quota = quota.downcast_ref::<crate::limits::RateLimited>().unwrap();
        assert!(quota.provider_wide);
        assert_eq!(quota.retry_after.as_secs(), 79 * 3600 + 35 * 60 + 25);
    }
    #[test]
    fn a_session_id_cannot_become_a_flag() {
        for runtime in ["agy", "codex"] {
            let args = vec!["exec".into(), "--".into(), "-".into()];
            for bad in ["--add-dir", "-x", "a b", ""] {
                assert!(crate::executor::session_arguments(
                    args.clone(),
                    runtime,
                    &crate::executor::ProviderSession::resume(bad)
                )
                .is_err());
            }
            assert!(crate::executor::session_arguments(
                args.clone(),
                runtime,
                &crate::executor::ProviderSession::resume("d9945309-f13f-4e6c-963f-94f5a6fe5332")
            )
            .is_ok());
        }
    }
    #[test]
    fn agy_gets_the_prompt_on_stdin_and_resumes_by_conversation() {
        let d = crate::router::Decision {
            task: "general".into(),
            provider: "google".into(),
            runtime: "agy".into(),
            model: "gemini-3.8-flash-low".into(),
            effort: String::new(),
            mode: crate::cli::Mode::Fast,
            score: 1.0,
            score_margin: None,
            ambiguous: false,
            overridden: false,
            reason: String::new(),
            candidates: vec![],
        };
        let mut cfg = crate::config::Config::from_defaults().unwrap();
        cfg.skip_permissions = true;
        let args = crate::executor::arguments(&d, &cfg).unwrap();
        let args = crate::executor::session_arguments(
            args,
            "agy",
            &crate::executor::ProviderSession::resume("c1"),
        )
        .unwrap();
        let args: Vec<_> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--print=",
                "--input-format",
                "stream-json",
                "--model",
                "gemini-3.8-flash-low",
                "--output-format",
                "text",
                "--dangerously-skip-permissions",
                "--conversation",
                "c1"
            ]
        );
    }
    #[test]
    fn limits_only_retry_before_activity() {
        let event = json!({"type":"turn.failed","error":{"message":"usage limit reached"}});
        let mut state = StreamState::default();
        let error = state.event("codex", &event).unwrap_err();
        assert!(
            error
                .downcast_ref::<crate::limits::RateLimited>()
                .unwrap()
                .safe_to_retry
        );
        state
            .event(
                "codex",
                &json!({"type":"item.started","item":{"type":"command_execution"}}),
            )
            .unwrap();
        let error = state.event("codex", &event).unwrap_err();
        assert!(
            !error
                .downcast_ref::<crate::limits::RateLimited>()
                .unwrap()
                .safe_to_retry
        );
        let mut state = StreamState::default();
        assert!(state.event("claude", &json!({"type":"result","is_error":false,"subtype":"success","result":"Explain rate limits"})).is_ok());
        assert!(StreamState::default()
            .event(
                "claude",
                &json!({"type":"result","is_error":true,"errors":["rate_limit_error"]})
            )
            .unwrap_err()
            .is::<crate::limits::RateLimited>());
    }
    #[test]
    fn nonterminal_error_can_recover_before_completed_turn() {
        let mut state = StreamState::default();
        state
            .event("codex", &json!({"type":"error","message":"Reconnecting"}))
            .unwrap();
        state.event("codex", &json!({"type":"item.completed","item":{"type":"agent_message","text":"Recovered"}})).unwrap();
        state
            .event("codex", &json!({"type":"turn.completed"}))
            .unwrap();
        assert_eq!(state.finish().unwrap(), "Recovered");
    }
    #[test]
    fn claude_stream_uses_final_result_for_history_without_duplicate_display() {
        let mut state = StreamState::default();
        assert_eq!(state.event("claude", &json!({"type":"stream_event", "event":{"delta":{"type":"text_delta","text":"Hello"}}})).unwrap(), Some("Hello".into()));
        assert_eq!(
            state
                .event(
                    "claude",
                    &json!({"type":"result","subtype":"success","is_error":false,"result":"Hello"})
                )
                .unwrap(),
            None
        );
        assert_eq!(
            state
                .event(
                    "claude",
                    &json!({"type":"result","subtype":"success","is_error":false,"result":"Hello!"})
                )
                .unwrap(),
            Some("\nHello!".into())
        );
        assert_eq!(state.finish().unwrap(), "Hello!");
    }
    #[test]
    fn codex_excludes_tools_and_commentary_from_final_answer() {
        let mut state = StreamState::default();
        state.event("codex", &json!({"type":"item.completed","item":{"type":"command_execution","aggregated_output":"secret tool output"}})).unwrap();
        state.event("codex", &json!({"type":"item.completed","item":{"type":"agent_message","phase":"commentary","text":"Looking"}})).unwrap();
        state
            .event(
                "codex",
                &json!({"type":"item.completed","item":{"type":"agent_message","text":"Done"}}),
            )
            .unwrap();
        state
            .event("codex", &json!({"type":"turn.completed"}))
            .unwrap();
        assert_eq!(state.finish().unwrap(), "Done");
    }
    #[test]
    fn errors_and_truncated_streams_do_not_become_answers() {
        assert!(StreamState::default()
            .event(
                "claude",
                &json!({"type":"result","is_error":true,"result":"Login required"})
            )
            .is_err());
        assert!(StreamState::default()
            .event(
                "codex",
                &json!({"type":"turn.failed","error":{"message":"Unavailable"}})
            )
            .is_err());
        let mut state = StreamState::default();
        state
            .event(
                "codex",
                &json!({"type":"item.completed","item":{"type":"agent_message","text":"Partial"}}),
            )
            .unwrap();
        assert!(state.finish().is_err());
    }
}
