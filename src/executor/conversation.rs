use super::{arguments, launch};
use crate::{config::Config, router::Decision};
use anyhow::{bail, Context, Result};
use serde_json::Value;
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

pub fn conversation_with_cancellation(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    output: &mut dyn Write,
    cancel: &super::CancellationToken,
) -> Result<String> {
    if cancel.is_cancelled() {
        bail!("Turn cancelled; partial output was not saved or replayed.");
    }
    let command = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude.command,
        "codex" => &cfg.inner.codex.command,
        "gemini" => &cfg.inner.gemini.command,
        _ => bail!("unsupported runtime"),
    };
    let mut args = arguments(d, cfg)?;
    match d.runtime.as_str() {
        "gemini" => {
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
            args.extend([
                "--verbose".into(),
                "--include-partial-messages".into(),
                "--no-session-persistence".into(),
            ]);
        }
        "codex" => {
            let index = args.len() - 2;
            args.splice(
                index..index,
                [OsString::from("--json"), OsString::from("--ephemeral")],
            );
        }
        _ => unreachable!(),
    }
    let mut command = launch(command)?;
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owned = super::process::OwnedChild::spawn(&mut command)?;
    crate::activity::set_phase(crate::activity::Phase::Waiting);
    let mut stderr = owned
        .child
        .stderr
        .take()
        .context("opening agent diagnostics")?;
    let activity = crate::activity::current();
    let diagnostics = thread::spawn(move || -> std::io::Result<()> {
        let mut buffer = [0; 8192];
        loop {
            let count = stderr.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            crate::activity::diagnostic(&activity, &buffer[..count])?;
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
            if completion_time.is_some_and(|t: Instant| t.elapsed() >= Duration::from_secs(2)) {
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
            if let Some(text) = state.event(&d.runtime, &event)? {
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
    // Kill the owned job/group before joining: descendants may still hold pipes open.
    owned.stop();
    drop(rx);
    let deadline = Instant::now() + Duration::from_secs(2);
    let read = super::process::join_until(reader, deadline);
    let diagnosed = super::process::join_until(diagnostics, deadline);
    let written = super::process::join_until(writer, deadline);
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
}
impl StreamState {
    fn event(&mut self, runtime: &str, event: &Value) -> Result<Option<String>> {
        let kind = event["type"].as_str().unwrap_or("");
        let failure = kind == "error"
            || (runtime == "gemini" && kind == "result" && event["status"] != "success")
            || kind == "turn.failed"
            || (kind == "assistant" && !event["error"].is_null())
            || (kind == "result"
                && (event["is_error"] == true
                    || event["subtype"].as_str().is_some_and(|s| s != "success")));
        if failure {
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
