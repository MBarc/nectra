//! Interactive provider launches and a separate, structured answer handoff.
use super::{arguments, launch, pane};
use crate::{config::Config, router::Decision};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: u64 = 8 * 1024 * 1024;
pub fn take_queued_messages() -> Vec<String> {
    pane::take_queued()
}

pub(super) fn one_shot_context(prompt: &str) -> String {
    format!(
        "Answer current_user_message directly.\n{}",
        json!({"conversation": [], "current_user_message": prompt})
    )
}
fn visible_prompt(root: &Path) -> Result<String> {
    let context = fs::read_to_string(root.join("context.txt"))?;
    let (_, payload) = context
        .split_once('\n')
        .context("missing conversation payload")?;
    let payload: Value = serde_json::from_str(payload)?;
    let message = payload["current_user_message"]
        .as_str()
        .context("missing user message")?;
    // Keep Windows launch arguments bounded; the hook supplies the full message.
    let mut visible: String = message.chars().take(2000).collect();
    if message.chars().count() > 2000 {
        visible.push('…');
    }
    Ok(visible)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ProviderView {
    #[default]
    Auto,
    Terminal,
    Stream,
}

/// Whether this runtime's turn runs in the provider's own terminal.
/// Ollama has none. Gemini CLI 0.64+ loads per-launch settings, and so Nectra's
/// hooks, only from admin-owned files, so its turns use stream view.
pub fn enabled(cfg: &Config, runtime: &str) -> Result<bool> {
    if matches!(runtime, "ollama" | "gemini") {
        return Ok(false);
    }
    let interactive = io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").ok().as_deref() != Some("dumb");
    match cfg.provider_view {
        ProviderView::Terminal if !interactive => {
            bail!("the provider terminal needs an interactive terminal; use --provider-view stream for pipes")
        }
        ProviderView::Stream => Ok(false),
        _ => Ok(interactive),
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    kind: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    error: String,
    /// Codex thread id from its turn notification, for resuming next turn.
    #[serde(default)]
    session: String,
}

/// Invoked only by the per-launch provider hooks, before ordinary CLI/config loading.
pub fn hook_main() -> Result<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(2).collect();
    if args.len() < 2 {
        bail!("invalid terminal hook invocation");
    }
    let root = Path::new(&args[0]);
    let runtime = args[1].to_str().context("invalid hook runtime")?;
    let raw = if runtime == "codex" {
        args.get(3)
            .context("missing notification")?
            .to_string_lossy()
            .into_owned()
    } else {
        let mut bytes = String::new();
        io::stdin().take(LIMIT + 1).read_to_string(&mut bytes)?;
        bytes
    };
    if raw.len() as u64 > LIMIT {
        bail!("terminal hook payload is too large");
    }
    let event: Value = serde_json::from_str(&raw)?;
    if let Some(record) = record(runtime, &event) {
        write_record(root, &record)?;
    }
    let kind = event["hook_event_name"].as_str().unwrap_or("");
    if matches!(kind, "UserPromptSubmit" | "BeforeAgent")
        && event["prompt"] == visible_prompt(root)?
    {
        let context = fs::read_to_string(root.join("context.txt"))?;
        println!(
            "{}",
            json!({"hookSpecificOutput":{"hookEventName":kind,"additionalContext":context}})
        );
    } else if runtime != "codex" {
        // No permission decisions: the provider's normal approval workflow stays in control.
        println!("{{}}");
    }
    if runtime == "codex" {
        {
            // Carry executable configuration in the launch arguments, not a mutable hook file.
            let notify: Vec<String> = serde_json::from_str(
                args.get(2)
                    .context("missing original notification")?
                    .to_str()
                    .context("invalid original notification")?,
            )?;
            if let Some(program) = notify.first() {
                // Preserve the user's notification program without shell interpolation.
                let status = std::process::Command::new(program)
                    .args(&notify[1..])
                    .arg(&raw)
                    .status();
                if let Err(error) = status {
                    eprintln!("Nectra: existing Codex notification could not start: {error}");
                }
            }
        }
    }
    Ok(0)
}

fn record(runtime: &str, event: &Value) -> Option<Record> {
    let mut r = Record::default();
    let kind = event["hook_event_name"].as_str().unwrap_or("");
    match (runtime, kind) {
        ("claude", "UserPromptSubmit") | ("gemini", "BeforeAgent") => {
            r.kind = "begin".into();
            r.prompt = event["prompt"].as_str().unwrap_or("").into();
        }
        ("claude", "PreToolUse") | ("gemini", "BeforeTool") => r.kind = "busy".into(),
        ("claude", "Stop") => {
            r.kind = "answer".into();
            r.answer = event["last_assistant_message"].as_str()?.into();
            if event["background_tasks"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            {
                r.kind = "busy".into();
            }
        }
        ("gemini", "AfterAgent") => {
            r.kind = "answer".into();
            r.answer = event["prompt_response"].as_str()?.into();
        }
        ("claude", "StopFailure") => {
            r.kind = "error".into();
            r.error = format!("{} {}", event["error"], event["error_details"]);
        }
        ("codex", _) if event["type"] == "agent-turn-complete" => {
            r.kind = "answer".into();
            r.answer = event["last-assistant-message"].as_str()?.into();
            r.session = event["thread-id"].as_str().unwrap_or("").into();
            r.prompt = event["input-messages"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
        }
        _ => return None,
    }
    Some(r)
}

/// What Codex is asked each turn. A short file reference avoids Windows'
/// command-line limit and exposing chat text in argv.
fn codex_prompt(root: &Path) -> String {
    format!("Read the UTF-8 conversation at {} and answer its current_user_message. Use the history silently; do not acknowledge loading context, model routing, or the handoff. Begin directly with the answer and do not repeat completed work.", root.join("context.txt").display())
}

fn write_record(root: &Path, record: &Record) -> Result<()> {
    // Unique files avoid interleaved writes from concurrent provider hook processes.
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = format!("{stamp:030}-{}", std::process::id());
    let path = root.join(format!("{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    serde_json::to_writer(&mut file, record)?;
    file.flush()?;
    drop(file);
    fs::rename(path, root.join(format!("{name}.json")))?;
    Ok(())
}

#[derive(Default)]
pub(super) struct Capture {
    began: bool,
    work_started: bool,
    pub revision: usize,
    pub ready: bool,
    answer: String,
    prompt: String,
    earlier: Vec<(String, String)>,
    error: String,
    session: String,
    seen: std::collections::BTreeSet<PathBuf>,
}
impl Capture {
    fn refresh(&mut self, root: &Path) -> Result<()> {
        let mut paths = fs::read_dir(root)?
            .map(|e| e.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()?;
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|e| e != "json") || self.seen.contains(&path) {
                continue;
            }
            if fs::metadata(&path)?.len() > LIMIT {
                bail!("terminal handoff exceeds limit");
            }
            let r: Record = serde_json::from_slice(&fs::read(&path)?)?;
            self.seen.insert(path);
            // Rejected before the revision advances, so the pane never sees it.
            // Codex also reports its own side turns, such as titling the thread
            // (seen on 0.162.1), as completed turns. The first real one answers
            // our context file; later ones, typed in the pane, continue its thread.
            if r.kind == "answer" && !r.session.is_empty() {
                // A side turn's prompt can quote ours, so match from the start.
                let ours = if self.session.is_empty() {
                    r.prompt.trim_start().starts_with(&codex_prompt(root))
                } else {
                    r.session == self.session
                };
                if !ours {
                    continue;
                }
            }
            self.revision += 1;
            match r.kind.as_str() {
                "begin" => {
                    self.began = true;
                    if self.ready {
                        self.earlier.push((
                            std::mem::take(&mut self.prompt),
                            std::mem::take(&mut self.answer),
                        ));
                    }
                    self.prompt = r.prompt;
                    self.answer.clear();
                    self.error.clear();
                    self.ready = false;
                }
                "busy" => {
                    self.work_started = true;
                    self.ready = false;
                    self.answer.clear();
                }
                "answer" => {
                    // Codex notifications also carry the user message for each completed turn.
                    if !r.prompt.is_empty() && self.ready {
                        self.earlier.push((
                            std::mem::take(&mut self.prompt),
                            std::mem::take(&mut self.answer),
                        ));
                    }
                    if !r.prompt.is_empty() {
                        self.prompt = r.prompt;
                    }
                    if !r.session.is_empty() {
                        self.session = r.session;
                    }
                    self.answer = r.answer;
                    self.ready = !self.answer.trim().is_empty();
                    self.error.clear();
                }
                "error" => {
                    self.error = r.error;
                    self.ready = false;
                    self.answer.clear();
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn finish(&self) -> Result<String> {
        if !self.error.is_empty() {
            if let Some(error) = crate::access::ModelUnavailable::detect(
                &self.error,
                self.began && !self.work_started && self.earlier.is_empty(),
            ) {
                return Err(error.into());
            }
            // Claude's PreToolUse hook reports every tool before it runs.
            let quiet = self.began && !self.work_started && self.earlier.is_empty();
            if let Some(limit) =
                crate::limits::RateLimited::from_error(&json!({"message":self.error}), quiet)
            {
                return Err(limit.into());
            }
            if self.error.contains("authentication") || self.error.contains("oauth") {
                return Err(super::AuthenticationRequired {
                    safe_to_retry: false,
                }
                .into());
            }
            bail!("provider terminal failed: {}", self.error);
        }
        if !self.ready {
            bail!("no completed answer was captured; interrupted work was not saved or replayed. If hooks are disabled or unsupported by this CLI, use --provider-view stream");
        }
        if self.earlier.is_empty() {
            return Ok(self.answer.clone());
        }
        // Preserve follow-ups typed directly into the native UI, with explicit role labels.
        let mut turns = self.earlier.clone();
        turns.push((self.prompt.clone(), self.answer.clone()));
        Ok(format!("Completed conversation inside the provider terminal (the first user request is the current Nectra message):\n{}", serde_json::to_string(&turns.iter().enumerate().map(|(i,(user,assistant))| json!({"user":if i == 0 { "[current Nectra message]" } else { user },"assistant":assistant})).collect::<Vec<_>>())?))
    }
}

fn shell_quote(value: &str, powershell: bool) -> String {
    if powershell {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn hook_command(root: &Path, runtime: &str) -> Result<String> {
    let powershell = cfg!(windows) && runtime == "gemini";
    let executable = std::env::current_exe()?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(format!(
        "{}{} __terminal-hook {} {}",
        if powershell { "& " } else { "" },
        shell_quote(&executable, powershell),
        shell_quote(&root.to_string_lossy().replace('\\', "/"), powershell),
        runtime
    ))
}

fn add_hooks(settings: &mut Value, runtime: &str, command: &str) -> Result<()> {
    let events: &[&str] = if runtime == "claude" {
        &["UserPromptSubmit", "PreToolUse", "Stop", "StopFailure"]
    } else {
        &["BeforeAgent", "BeforeTool", "AfterAgent"]
    };
    let object = settings
        .as_object_mut()
        .context("provider settings must be an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("hooks must be an object")?;
    for event in events {
        let hooks = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("hook event must be an array")?;
        hooks.push(json!({"matcher":if runtime == "claude" {"*"} else {".*"},"hooks":[{"type":"command","command":command,"timeout":if runtime == "claude" {10} else {10000}}]}));
    }
    Ok(())
}

fn gemini_defaults() -> PathBuf {
    if let Some(p) = std::env::var_os("GEMINI_CLI_SYSTEM_DEFAULTS_PATH") {
        return p.into();
    }
    if let Some(p) = std::env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH") {
        return Path::new(&p).with_file_name("system-defaults.json");
    }
    if cfg!(windows) {
        PathBuf::from("C:/ProgramData/gemini-cli/system-defaults.json")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/GeminiCli/system-defaults.json")
    } else {
        PathBuf::from("/etc/gemini-cli/system-defaults.json")
    }
}

fn command(
    d: &Decision,
    cfg: &Config,
    root: &Path,
    session: Option<&super::ProviderSession>,
) -> Result<CommandBuilder> {
    let resume_id = session.filter(|s| s.resume).and_then(|s| s.id.clone());
    let runtime = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude,
        "codex" => &cfg.inner.codex,
        "gemini" => &cfg.inner.gemini,
        _ => bail!("no native terminal for this runtime"),
    };
    let launch = launch(&runtime.command)?;
    let mut cmd = CommandBuilder::new(launch.get_program());
    cmd.args(launch.get_args());
    let mut args = arguments(d, cfg)?;
    if d.runtime == "codex" {
        let original_notify = serde_json::to_string(&existing_codex_notify()?.unwrap_or_default())?;
        args.remove(0); // exec -> interactive CLI
        args.truncate(args.len() - 2); // structured stdin marker
        if resume_id.is_some() {
            args.insert(0, "resume".into());
        }
        let notify = [
            std::env::current_exe()?.into_os_string(),
            "__terminal-hook".into(),
            root.as_os_str().into(),
            "codex".into(),
            original_notify.into(),
        ];
        let notify: Vec<String> = notify
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        args.extend([
            "--config".into(),
            format!(
                "notify={}",
                toml::Value::Array(notify.into_iter().map(toml::Value::String).collect())
            )
            .into(),
            "--no-alt-screen".into(),
        ]);
        if let Some(id) = &resume_id {
            args.push(id.into());
        }
        args.push(codex_prompt(root).into());
    } else {
        let index = args
            .iter()
            .position(|a| a == "--output-format")
            .context("missing output mode")?;
        args.drain(index..index + 2);
        let mut settings = json!({});
        if d.runtime == "claude" {
            args.remove(0); // -p -> interactive CLI
            settings = take_claude_settings(&mut args)?;
        } else {
            match fs::read(gemini_defaults()) {
                Ok(bytes) => {
                    settings = serde_json::from_slice(&bytes)
                        .context("reading Gemini system defaults (must be valid JSON)")?
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        add_hooks(&mut settings, &d.runtime, &hook_command(root, &d.runtime)?)?;
        // Keep settings outside the handoff event directory.
        let settings_path = root.join("settings.config");
        fs::write(&settings_path, serde_json::to_vec(&settings)?)?;
        if d.runtime == "claude" {
            if let Some(s) = session {
                args = super::session_arguments(args, "claude", s)?;
            }
            args.extend([
                "--settings".into(),
                settings_path.into_os_string(),
                "--".into(),
                visible_prompt(root)?.into(),
            ]);
        } else {
            cmd.env("GEMINI_CLI_SYSTEM_DEFAULTS_PATH", settings_path);
            args.push(format!("--prompt-interactive={}", visible_prompt(root)?).into());
        }
    }
    cmd.args(args);
    cmd.cwd(std::env::current_dir()?);
    cmd.env("TERM", "xterm-256color");
    for (key, value) in crate::mcp::for_launch(&d.runtime, cfg).1 {
        cmd.env(key, value);
    }
    Ok(cmd)
}

pub fn conversation(d: &Decision, prompt: &str, cfg: &Config) -> Result<String> {
    conversation_with_session(d, prompt, cfg, None)
}

/// Terminal-view turn that keeps, or continues, a provider session.
pub fn conversation_with_session(
    d: &Decision,
    prompt: &str,
    cfg: &Config,
    session: Option<&mut super::ProviderSession>,
) -> Result<String> {
    let root = tempfile::Builder::new()
        .prefix("nectra-terminal-")
        .tempdir()?;
    fs::write(root.path().join("context.txt"), prompt)?;
    let cmd = command(d, cfg, root.path(), session.as_deref())?;
    let marker = root
        .path()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let mut capture = Capture::default();
    let outcome = pane::run(cmd, &d.model, |screen| {
        capture.refresh(root.path())?;
        // Codex sits at its prompt after a usage limit: end the turn for fallback.
        let limited = d.runtime == "codex" && screen_limit(screen, &marker).is_some();
        Ok((
            capture.revision + usize::from(limited),
            capture.ready || !capture.error.is_empty() || limited,
        ))
    })?;
    capture.refresh(root.path())?;
    if let Some(s) = session {
        if !capture.session.is_empty() {
            s.id.get_or_insert(capture.session.clone());
        }
    }
    if !capture.error.is_empty() {
        return capture.finish();
    }
    if !capture.ready {
        if let Some(limit) = terminal_limit(&d.runtime, &outcome, &capture, &marker) {
            return Err(limit.into());
        }
        if let Some(error) = crate::access::ModelUnavailable::detect(&outcome.diagnostic, false) {
            return Err(error.into());
        }
        // A provider that quits before answering (a bad flag, a login screen it
        // can't show) said why on its screen; "no answer captured" would hide it.
        if !outcome.success && !outcome.pending_input {
            let said: Vec<&str> = outcome
                .diagnostic
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .collect();
            bail!(
                "{} exited before answering. Its last output:
{}",
                d.model,
                said[said.len().saturating_sub(6)..].join(
                    "
"
                )
            );
        }
    }
    if outcome.pending_input {
        bail!("provider work was interrupted; this message was not replayed or saved as a completed answer");
    }
    let answer = capture.finish()?;
    if !outcome.success {
        bail!(
            "provider exited unsuccessfully; its last answer was not treated as a completed turn"
        );
    }
    println!("\n{answer}");
    Ok(answer)
}

/// A rate or usage limit that ended a terminal-view turn without an answer.
/// Codex's terminal view has no hook for a failed turn, so its screen is the
/// only report; Claude's failures arrive through its StopFailure hook instead.
fn terminal_limit(
    runtime: &str,
    outcome: &pane::Outcome,
    capture: &Capture,
    marker: &str,
) -> Option<crate::limits::RateLimited> {
    if runtime != "codex" || outcome.cancelled {
        return None;
    }
    let mut limit = screen_limit(&outcome.diagnostic, marker)?;
    // A completed Codex turn in this launch means its work may have run.
    limit.safe_to_retry &= capture.revision == 0 && capture.earlier.is_empty();
    Some(limit)
}

/// A limit in the last Codex error cell (`■ …`) below this launch's prompt,
/// which names `marker` (its handoff directory). Codex draws every tool call
/// and reply as a `•` cell, so none above the error means no work ran.
fn screen_limit(screen: &str, marker: &str) -> Option<crate::limits::RateLimited> {
    let lines: Vec<&str> = screen.lines().collect();
    let prompt = lines.iter().rposition(|l| l.contains(marker));
    let lines = &lines[prompt.map_or(0, |i| i + 1)..];
    let at = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with('■'))?;
    let message: Vec<&str> = std::iter::once(lines[at].trim_start().trim_start_matches('■'))
        .chain(
            lines[at + 1..]
                .iter()
                .copied()
                .take_while(|l| l.starts_with(' ') && !l.trim().is_empty()),
        )
        .map(str::trim)
        .collect();
    let quiet = prompt.is_some() && !lines[..at].iter().any(|l| l.trim_start().starts_with('•'));
    crate::limits::RateLimited::from_error(&json!({"message": message.join(" ")}), quiet)
}

fn take_claude_settings(args: &mut Vec<OsString>) -> Result<Value> {
    let mut settings = json!({});
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].to_string_lossy();
        let value = if arg == "--settings" {
            let value = args
                .get(i + 1)
                .context("--settings requires a value")?
                .to_string_lossy()
                .into_owned();
            args.drain(i..i + 2);
            Some(value)
        } else if let Some(value) = arg.strip_prefix("--settings=") {
            let value = value.to_owned();
            args.remove(i);
            Some(value)
        } else {
            None
        };
        if let Some(value) = value {
            settings = if value.trim_start().starts_with('{') {
                serde_json::from_str(&value)?
            } else {
                serde_json::from_slice(
                    &fs::read(value).context("reading explicit Claude settings")?,
                )?
            };
        } else {
            i += 1;
        }
    }
    Ok(settings)
}

fn existing_codex_notify() -> Result<Option<Vec<String>>> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".codex")))
        .context("cannot locate Codex config")?;
    read_codex_notify(&home)
}

fn read_codex_notify(home: &Path) -> Result<Option<Vec<String>>> {
    let text = match fs::read_to_string(home.join("config.toml")) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let config: toml::Value =
        toml::from_str(&text).context("reading Codex notification configuration")?;
    let mut notify = config.get("notify").cloned();
    if let Some(profile) = config.get("profile").and_then(toml::Value::as_str) {
        if let Some(value) = config
            .get("profiles")
            .and_then(|p| p.get(profile))
            .and_then(|p| p.get("notify"))
        {
            notify = Some(value.clone());
        }
        if !profile.is_empty()
            && profile
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            match fs::read_to_string(home.join(format!("{profile}.config.toml"))) {
                Ok(text) => {
                    let layer: toml::Value = toml::from_str(&text)?;
                    if let Some(value) = layer.get("notify") {
                        notify = Some(value.clone());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    notify
        .map(|n| {
            n.try_into()
                .context("Codex notify must be an array of strings")
        })
        .transpose()
}

/// Local manual integration fixture; absent from release builds and makes no model calls.
#[cfg(debug_assertions)]
pub fn demo() -> Result<i32> {
    let root = tempfile::tempdir()?;
    eprintln!("Local fixture: {}", root.path().display());
    let mut cmd = CommandBuilder::new(std::env::current_exe()?);
    cmd.args([
        OsString::from("__terminal-fixture"),
        root.path().as_os_str().to_owned(),
    ]);
    let mut capture = Capture::default();
    let outcome = pane::run(cmd, "Local test provider", |_| {
        capture.refresh(root.path())?;
        Ok((capture.revision, capture.ready))
    })?;
    if !outcome.success || outcome.pending_input {
        bail!("demo interrupted");
    }
    capture.refresh(root.path())?;
    println!("Captured clean answer: {}", capture.finish()?);
    for message in take_queued_messages() {
        println!("Queued main message: {message}");
    }
    Ok(0)
}

#[cfg(debug_assertions)]
#[allow(
    clippy::zombie_processes,
    reason = "Fixture intentionally leaves a child for the terminal job cleanup test"
)]
pub fn fixture() -> Result<i32> {
    let path = std::env::args_os()
        .nth(2)
        .context("missing fixture directory")?;
    let root = Path::new(&path);
    fs::write(root.join("fixture-state"), "started")?;
    let descendant = std::process::Command::new(std::env::current_exe()?)
        .arg("__terminal-sleeper")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    crossterm::terminal::enable_raw_mode()?;
    fs::write(root.join("fixture-state"), "raw mode enabled")?;
    let mut out = io::stdout();
    write!(
        out,
        "\x1b[2J\x1b[HNative terminal test\r\nFixture descendant: {}\r\nWriting the doc now.\r\n",
        descendant.id()
    )?;
    for frame in ["|", "/", "-", "\\", "|", "/"] {
        write!(out, "\r\x1b[2K\x1b[35mThinking {frame}\x1b[0m")?;
        out.flush()?;
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    write!(
        out,
        "\r\n\x1b[33mApprove writing design.md? Press y\x1b[0m\r\n"
    )?;
    out.flush()?;
    fs::write(root.join("fixture-state"), "waiting for approval")?;
    let mut input = io::stdin();
    let mut key = [0];
    loop {
        input.read_exact(&mut key)?;
        if key[0] == b'y' {
            break;
        }
        if key[0] == 3 {
            return Ok(1);
        }
    }
    write!(
        out,
        "\x1b[32mSaved design.md — café 👻\x1b[0m\r\nDone. Ctrl+] returns to Nectra.\r\n"
    )?;
    out.flush()?;
    write_record(
        root,
        &Record {
            kind: "answer".into(),
            answer: "Saved design.md — café 👻".into(),
            ..Default::default()
        },
    )?;
    let mut typed = Vec::new();
    loop {
        input.read_exact(&mut key)?;
        if key[0] == 3 {
            return Ok(0);
        }
        if key[0] == b'\r' && (typed == b"/quit" || typed == b"/exit") {
            return Ok(0);
        }
        typed.push(key[0]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_shot_prompts_build_native_commands_without_a_chat_session() {
        let mut cfg = Config::from_defaults().unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        cfg.inner.claude.command = executable.clone();
        cfg.inner.gemini.command = executable;
        for runtime in ["claude", "gemini"] {
            let d = Decision {
                task: "general".into(),
                provider: runtime.into(),
                runtime: runtime.into(),
                model: "test-model".into(),
                effort: String::new(),
                mode: crate::cli::Mode::Balanced,
                score: 0.0,
                score_margin: None,
                ambiguous: false,
                overridden: true,
                reason: String::new(),
                candidates: vec![],
            };
            let root = tempfile::tempdir().unwrap();
            let context = one_shot_context("hello\nsecond line");
            fs::write(root.path().join("context.txt"), &context).unwrap();
            assert_eq!(visible_prompt(root.path()).unwrap(), "hello\nsecond line");
            let payload: Value = serde_json::from_str(context.split_once('\n').unwrap().1).unwrap();
            assert_eq!(payload["conversation"], json!([]));
            assert_eq!(payload["current_user_message"], "hello\nsecond line");
            assert!(
                command(&d, &cfg, root.path(), None).is_ok(),
                "{runtime} one-shot command must build"
            );
        }
    }
    #[test]
    fn explicit_settings_and_profile_notifications_are_preserved() {
        let mut args = vec![
            OsString::from("--settings={\"permissions\":{\"defaultMode\":\"plan\"}}"),
            "--verbose".into(),
        ];
        let settings = take_claude_settings(&mut args).unwrap();
        assert_eq!(settings["permissions"]["defaultMode"], "plan");
        assert_eq!(args, vec![OsString::from("--verbose")]);
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("config.toml"),
            "notify=['original']\nprofile='work'\n",
        )
        .unwrap();
        fs::write(
            home.path().join("work.config.toml"),
            "notify=['profile-program','literal $() argument']\n",
        )
        .unwrap();
        assert_eq!(
            read_codex_notify(home.path()).unwrap().unwrap(),
            ["profile-program", "literal $() argument"]
        );
    }
    #[test]
    fn completed_answers_are_separate_from_screen_output() {
        let root = tempfile::tempdir().unwrap();
        let mut capture = Capture::default();
        write_record(
            root.path(),
            &record(
                "claude",
                &json!({"hook_event_name":"UserPromptSubmit","prompt":"hello"}),
            )
            .unwrap(),
        )
        .unwrap();
        write_record(
            root.path(),
            &record(
                "claude",
                &json!({"hook_event_name":"Stop","last_assistant_message":"Saved design.md"}),
            )
            .unwrap(),
        )
        .unwrap();
        capture.refresh(root.path()).unwrap();
        assert_eq!(capture.finish().unwrap(), "Saved design.md");
        write_record(
            root.path(),
            &record("claude", &json!({"hook_event_name":"PreToolUse"})).unwrap(),
        )
        .unwrap();
        capture.refresh(root.path()).unwrap();
        assert!(capture.finish().is_err());
    }
    #[test]
    fn provider_handoffs_and_followups() {
        assert_eq!(
            record(
                "gemini",
                &json!({"hook_event_name":"AfterAgent","prompt_response":"Done"})
            )
            .unwrap()
            .answer,
            "Done"
        );
        let root = tempfile::tempdir().unwrap();
        let mut capture = Capture::default();
        for (prompt, answer) in [("bootstrap", "First"), ("Explain the file", "Second")] {
            write_record(root.path(), &record("codex", &json!({"type":"agent-turn-complete","input-messages":[prompt],"last-assistant-message":answer})).unwrap()).unwrap();
            capture.refresh(root.path()).unwrap();
        }
        let saved = capture.finish().unwrap();
        assert!(
            saved.contains("Explain the file")
                && saved.contains("First")
                && saved.contains("Second")
        );
        assert!(!saved.contains("bootstrap"));
    }
    #[test]
    fn codex_side_turns_are_not_the_answer() {
        let root = tempfile::tempdir().unwrap();
        let ask = codex_prompt(root.path());
        let mut capture = Capture::default();
        let turn = |capture: &mut Capture, thread: &str, prompt: &str, answer: &str| {
            let event = json!({"type":"agent-turn-complete","thread-id":thread,"input-messages":[prompt],"last-assistant-message":answer});
            write_record(root.path(), &record("codex", &event).unwrap()).unwrap();
            capture.refresh(root.path()).unwrap();
        };
        turn(
            &mut capture,
            "title",
            &format!("Generate a concise, single-line task title for: {ask}"),
            r#"{"title":"Answer"}"#,
        );
        assert!(!capture.ready);
        // Nor does it count as progress the pane would react to.
        assert_eq!(capture.revision, 0);
        turn(&mut capture, "t1", &ask, "Created out");
        turn(
            &mut capture,
            "title2",
            "Generate a concise, single-line task title",
            r#"{"title":"Again"}"#,
        );
        turn(&mut capture, "t1", "and list it", "out/");
        let saved = capture.finish().unwrap();
        assert!(
            saved.contains("Created out") && saved.contains("out/"),
            "{saved}"
        );
        assert!(!saved.contains("title"), "{saved}");
        assert_eq!(capture.session, "t1");
    }
    #[test]
    fn shell_paths_are_quoted_and_existing_hooks_survive() {
        assert_eq!(shell_quote("a'b $()", true), "'a''b $()'");
        assert_eq!(shell_quote("a'b $()", false), "'a'\"'\"'b $()'");
        let mut settings =
            json!({"hooks":{"Stop":[{"hooks":[]}]},"permissions":{"defaultMode":"plan"}});
        add_hooks(&mut settings, "claude", "safe").unwrap();
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(settings["permissions"]["defaultMode"], "plan");
    }
}

#[cfg(test)]
mod access_handoff_tests {
    use super::*;
    #[test]
    fn native_rejection_retries_only_before_observed_tool_work() {
        let root = tempfile::tempdir().unwrap();
        write_record(
            root.path(),
            &Record {
                kind: "begin".into(),
                prompt: "task".into(),
                ..Default::default()
            },
        )
        .unwrap();
        write_record(
            root.path(),
            &Record {
                kind: "error".into(),
                error: "model_not_found".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut capture = Capture::default();
        capture.refresh(root.path()).unwrap();
        assert!(
            capture
                .finish()
                .unwrap_err()
                .downcast_ref::<crate::access::ModelUnavailable>()
                .unwrap()
                .safe_to_retry
        );
        write_record(
            root.path(),
            &Record {
                kind: "busy".into(),
                ..Default::default()
            },
        )
        .unwrap();
        capture.refresh(root.path()).unwrap();
        assert!(
            !capture
                .finish()
                .unwrap_err()
                .downcast_ref::<crate::access::ModelUnavailable>()
                .unwrap()
                .safe_to_retry
        );
    }

    const MARKER: &str = "nectra-terminal-abc";
    /// Codex's terminal view after its first request hit the plan's usage limit.
    const LIMITED: &str = "› Read the UTF-8 conversation at C:/t/nectra-terminal-abc/context.txt and answer\n\n■ You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro) or try\n  again at Oct 27th, 2026 3:04 PM.\n\n› Ask Codex to do anything\n";
    fn ended(screen: &str, cancelled: bool) -> pane::Outcome {
        pane::Outcome {
            diagnostic: screen.into(),
            success: true,
            pending_input: false,
            cancelled,
        }
    }

    #[test]
    fn a_usage_limit_on_the_codex_screen_is_a_safe_rate_limit() {
        let capture = Capture::default();
        let limit = terminal_limit("codex", &ended(LIMITED, false), &capture, MARKER)
            .expect("usage limit on screen");
        assert!(limit.safe_to_retry && limit.provider_wide);
        assert!(limit
            .message
            .contains("try again at Oct 27th, 2026 3:04 PM."));
        // Shown while the provider still waits, so the pane can end the turn.
        assert!(screen_limit(LIMITED, MARKER).is_some());
    }

    #[test]
    fn a_limit_after_codex_did_work_is_not_safe_to_replay() {
        let worked = LIMITED.replace("\n\n■", "\n\n• Ran cargo build\n  └ Finished\n\n■");
        let capture = Capture::default();
        let limit = terminal_limit("codex", &ended(&worked, false), &capture, MARKER).unwrap();
        assert!(!limit.safe_to_retry);
        // Our prompt scrolled away: what ran before the error can't be seen.
        let unseen = LIMITED.replace(MARKER, "elsewhere");
        let limit = terminal_limit("codex", &ended(&unseen, false), &capture, MARKER).unwrap();
        assert!(!limit.safe_to_retry);
        // A completed Codex turn means work ran in this launch.
        let answered = Capture {
            revision: 1,
            ..Capture::default()
        };
        let limit = terminal_limit("codex", &ended(LIMITED, false), &answered, MARKER).unwrap();
        assert!(!limit.safe_to_retry);
    }

    #[test]
    fn a_cancelled_turn_or_another_error_is_no_rate_limit() {
        let capture = Capture::default();
        assert!(terminal_limit("codex", &ended(LIMITED, true), &capture, MARKER).is_none());
        for error in [
            "■ stream disconnected before completion: error sending request for url",
            "■ unexpected status 401 Unauthorized: Missing bearer authentication",
            "■ Conversation interrupted - tell the model what to do differently.",
        ] {
            let screen = LIMITED.replace(
                "■ You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro) or try\n  again at Oct 27th, 2026 3:04 PM.",
                error,
            );
            assert!(terminal_limit("codex", &ended(&screen, false), &capture, MARKER).is_none());
        }
        // An error from resumed history, above this launch's prompt, is not this turn's.
        let old = format!("■ You've hit your usage limit.\n\n{LIMITED}").replace(
            "■ You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro) or try\n  again at Oct 27th, 2026 3:04 PM.",
            "",
        );
        assert!(screen_limit(&old, MARKER).is_none());
    }

    #[test]
    fn a_claude_limit_before_any_tool_ran_is_safe_to_replay() {
        let mut capture = Capture {
            began: true,
            error: "\"rate_limit\" You've hit your limit".into(),
            ..Capture::default()
        };
        let safe = |c: &Capture| {
            c.finish()
                .unwrap_err()
                .downcast::<crate::limits::RateLimited>()
                .unwrap()
                .safe_to_retry
        };
        assert!(safe(&capture));
        capture.work_started = true;
        assert!(!safe(&capture));
    }
}
