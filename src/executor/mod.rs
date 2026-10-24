use crate::{config::Config, router::Decision};
mod conversation;
mod draft;
pub mod native;
mod pane;
mod process;
#[cfg(windows)]
mod windows_job;
use anyhow::{bail, Context, Result};
pub use conversation::conversation;
pub use conversation::conversation_resumable;
pub use conversation::conversation_with_cancellation;
pub use conversation::conversation_with_session;
pub use conversation::AuthenticationRequired;
pub use conversation::{conversation_observed, DeadlineExceeded, Observer};
pub use process::TurnCancellation;
pub use process::{output_until, CancellationToken};
use std::{
    env,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Resolve native executables, including the native payload of the Windows npm Codex package.
/// Deliberately do not dispatch prompts through cmd.exe or PowerShell shims.
pub fn resolve(command: &str) -> Result<PathBuf> {
    let p = Path::new(command);
    if p.is_absolute() || p.components().count() > 1 {
        if p.is_file() && native(p) {
            return Ok(p.to_path_buf());
        }
        bail!("runtime executable not found or not native: {command}");
    }
    let path = env::var_os("PATH").unwrap_or_default();
    // agy's installer puts it in %LOCALAPPDATA%/agy/bin and does not always
    // add that to PATH, so a fresh install would look missing.
    let agy_home = (command == "agy")
        .then(|| env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("agy/bin")))
        .flatten();
    for dir in env::split_paths(&path)
        .chain(agy_home)
        .filter(|p| p.is_absolute())
    {
        let exact = dir.join(command);
        if exact.is_file() && native(&exact) {
            return Ok(exact);
        }
        if cfg!(windows) {
            let exe = dir.join(format!("{command}.exe"));
            if exe.is_file() {
                return Ok(exe);
            }
            if command == "codex" {
                let arch = if cfg!(target_arch = "aarch64") {
                    "arm64"
                } else {
                    "x64"
                };
                let target = if cfg!(target_arch = "aarch64") {
                    "aarch64-pc-windows-msvc"
                } else {
                    "x86_64-pc-windows-msvc"
                };
                for root in [
                    dir.join("node_modules/@openai/codex/node_modules/@openai"),
                    dir.join("node_modules/@openai"),
                ] {
                    for leaf in ["bin/codex.exe", "codex/codex.exe"] {
                        let exe = root.join(format!("codex-win32-{arch}/vendor/{target}/{leaf}"));
                        if exe.is_file() {
                            return Ok(exe);
                        }
                    }
                }
            }
        }
    }
    bail!("{command} is unavailable; install its native CLI or set an absolute executable path in config")
}
fn native(path: &Path) -> bool {
    !cfg!(windows)
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// Launch npm Gemini through Node directly on Windows, never through a shell shim.
pub fn launch(command: &str) -> Result<Command> {
    let path = Path::new(command);
    if path.is_file()
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("js"))
    {
        let mut cmd = Command::new(resolve("node")?);
        cmd.arg(path);
        return Ok(cmd);
    }
    if let Ok(path) = resolve(command) {
        return Ok(Command::new(path));
    }
    if cfg!(windows) && command == "gemini" {
        for dir in
            env::split_paths(&env::var_os("PATH").unwrap_or_default()).filter(|p| p.is_absolute())
        {
            // The entry point moves between releases (dist/index.js, then
            // bundle/gemini.js by 0.64), so read it from the package.
            let package = dir.join("node_modules/@google/gemini-cli");
            let bin = std::fs::read(package.join("package.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|p| p["bin"]["gemini"].as_str().map(String::from));
            let script = package.join(bin.as_deref().unwrap_or("dist/index.js"));
            if script.is_file() {
                let mut cmd = Command::new(resolve("node")?);
                cmd.arg(script);
                return Ok(cmd);
            }
        }
    }
    bail!("{command} is unavailable; install the provider CLI (Gemini npm installs also need Node.js), or configure its executable/JS entry point")
}

pub fn login(provider: &str, cfg: &Config) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!("provider login needs an interactive terminal");
    }
    let (runtime, args): (&str, &[&str]) = match provider {
        "anthropic" | "claude" => (&cfg.inner.claude.command, &["auth", "login"]),
        "openai" | "codex" => (&cfg.inner.codex.command, &["login"]),
        "google" | "gemini" | "agy" => match crate::models::google_runtime(cfg) {
            crate::models::Runtime::Gemini => {
                crate::terminal::line("Opening Gemini CLI. Use /auth to sign in again, then /quit to return to Nectra.")?;
                (&cfg.inner.gemini.command, &[])
            }
            _ => {
                crate::terminal::line("Opening Antigravity (agy). Sign in there, then exit it to return to Nectra.")?;
                (&cfg.inner.agy.command, &[])
            }
        },
        "ollama" => bail!("local Ollama needs no provider login. For a protected server, update the environment variable named by ollama.token_env and restart Nectra"),
        _ => bail!("login is supported for anthropic, openai, and google"),
    };
    crate::terminal::line("Opening provider login. Credentials stay with the provider CLI.")?;
    let status = launch(runtime)?
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        bail!("provider login exited with {status}");
    }
    crate::terminal::line(
        "Login workflow closed. Send a new message to verify access; failed work was not replayed.",
    )?;
    Ok(())
}

/// Prompts travel over stdin: no shell interpolation, flag injection or command-line length limit.
pub fn arguments(d: &Decision, cfg: &Config) -> Result<Vec<OsString>> {
    if cfg.no_tools && !answers_only(&d.runtime) {
        bail!(
            "--no-tools: {} can't be launched without tools, so it is refused",
            d.runtime
        );
    }
    // Claude's read-only launch is already tool-less.
    let read_only = cfg.read_only || cfg.no_tools;
    let mut args: Vec<OsString> = match d.runtime.as_str() {
        "claude" => {
            let mut args = vec![
                "-p".into(),
                "--model".into(),
                d.model.clone().into(),
                "--output-format".into(),
                "text".into(),
            ];
            if !d.effort.is_empty() {
                args.extend(["--effort".into(), d.effort.clone().into()]);
            }
            args
        }
        "codex" => {
            let mut args = vec!["exec".into(), "--model".into(), d.model.clone().into()];
            if !d.effort.is_empty() {
                args.extend([
                    OsString::from("--config"),
                    format!("model_reasoning_effort=\"{}\"", d.effort).into(),
                ]);
            }
            args
        }
        "gemini" => vec![
            "--model".into(),
            d.model.clone().into(),
            "--output-format".into(),
            "text".into(),
        ],
        // `-p` takes the next argument as the prompt, so `--print=` (empty) it
        // is, and the prompt travels on stdin as a stream-json user message.
        "agy" => {
            let mut args = vec![
                "--print=".into(),
                "--input-format".into(),
                "stream-json".into(),
                "--model".into(),
                d.model.clone().into(),
                "--output-format".into(),
                "text".into(),
            ];
            if !d.effort.is_empty() {
                args.extend(["--effort".into(), d.effort.clone().into()]);
            }
            args
        }
        _ => bail!("unsupported runtime"),
    };
    let runtime = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude,
        "codex" => &cfg.inner.codex,
        "gemini" => &cfg.inner.gemini,
        "agy" => &cfg.inner.agy,
        _ => bail!("unsupported runtime"),
    };
    // Both replace the configured permissions with their own.
    let override_permissions = cfg.skip_permissions || read_only;
    // Any extra argument could reopen a tool surface (`--mcp-config` adds servers
    // that `--strict-mcp-config` then keeps), so a read-only Claude launch takes none.
    let extras: &[String] = if read_only && d.runtime == "claude" {
        &[]
    } else {
        &runtime.extra_args
    };
    let mut extras = extras.iter();
    while let Some(arg) = extras.next() {
        let flag = arg.split('=').next().unwrap_or(arg);
        if override_permissions
            && [
                "--sandbox",
                "-s",
                "--ask-for-approval",
                "-a",
                "--permission-mode",
                "--approval-mode",
            ]
            .contains(&flag)
        {
            if !arg.contains('=') {
                let _ = extras.next();
            }
            continue;
        }
        if override_permissions
            && [
                "--full-auto",
                "--yolo",
                "-y",
                "--dangerously-skip-permissions",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
            .contains(&flag)
        {
            continue;
        }
        args.push(arg.into());
    }
    args.extend(crate::mcp::for_launch(&d.runtime, cfg).0);
    if read_only {
        // Claude gets no tools at all: `--tools ""` alone still leaves every MCP
        // server's tools (seen live on 2.1.302). Codex has no tool-less mode, so
        // it gets its read-only sandbox (which on Windows still permits deletion).
        let flags: &[&str] = match d.runtime.as_str() {
            "claude" => &["--tools", "", "--strict-mcp-config"],
            "codex" => &["--sandbox", "read-only"],
            _ => bail!("{} has no read-only launch", d.runtime),
        };
        args.extend(flags.iter().map(OsString::from));
        if cfg.no_tools {
            // Settings run shell of their own: hooks, apiKeyHelper and the like.
            // No user, project or local settings, no hooks from those, and no
            // slash commands or skills (a command's `!` lines run). Managed
            // (admin) settings still apply, hooks included: they are part of
            // the machine's image, not something a request controls.
            // Checked live on 2.1.310: a project UserPromptSubmit hook didn't run.
            args.extend(
                [
                    "--setting-sources",
                    "",
                    "--settings",
                    r#"{"disableAllHooks":true}"#,
                    "--disable-slash-commands",
                ]
                .map(OsString::from),
            );
        }
    } else if cfg.skip_permissions {
        args.push(
            match d.runtime.as_str() {
                "claude" => "--dangerously-skip-permissions",
                "codex" => "--dangerously-bypass-approvals-and-sandbox",
                "gemini" => "--approval-mode=yolo",
                "agy" => "--dangerously-skip-permissions",
                _ => unreachable!(),
            }
            .into(),
        );
    }
    if d.runtime == "codex" {
        args.extend(["--".into(), "-".into()]);
    }
    Ok(args)
}
/// Whether `--no-tools` can hold this runtime to answers. Claude has a tool-less
/// launch and Ollama is chat-only. Codex's read-only sandbox still reads files
/// and runs commands (with its tool features off it still offered a shell call,
/// on 0.162.1), and agy's and Gemini's plan modes still read files.
pub fn answers_only(runtime: &str) -> bool {
    matches!(runtime, "claude" | "ollama")
}

/// A provider-side conversation Nectra can continue on the next turn.
///
/// Resuming keeps the provider's own context (files read, tool output) and its
/// prompt cache, so a follow-up costs only the new message instead of a replay.
#[derive(Debug, Clone, Default)]
pub struct ProviderSession {
    /// Continue this existing session instead of starting a new one.
    pub resume: bool,
    /// Claude: assigned before launch. Codex, agy: captured from the provider's events.
    pub id: Option<String>,
}
impl ProviderSession {
    /// Claude, Codex and agy expose a session id Nectra can target safely.
    pub fn supported(runtime: &str) -> bool {
        matches!(runtime, "claude" | "codex" | "agy")
    }
    pub fn start(runtime: &str) -> Self {
        Self {
            resume: false,
            id: (runtime == "claude").then(new_session_id),
        }
    }
    pub fn resume(id: &str) -> Self {
        Self {
            resume: true,
            id: Some(id.to_string()),
        }
    }
}

/// `codex exec resume` has no equivalent for these `codex exec` flags.
const CODEX_NOT_RESUMABLE: [&str; 4] = ["--add-dir", "--approve-for-me", "--cd", "-C"];

/// Whether this runtime's configured flags survive a resumed launch.
pub fn can_resume(runtime: &str, cfg: &Config) -> bool {
    runtime != "codex"
        || !cfg.inner.codex.extra_args.iter().any(|a| {
            CODEX_NOT_RESUMABLE.contains(&a.split('=').next().unwrap_or(a))
                    // -C takes its value attached too: -Cpath
                    || (a.starts_with("-C") && !a.starts_with("--"))
        })
}

/// A random version 4 UUID. Not security sensitive: it only names a session.
pub fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut b = [0u8; 16];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for (i, chunk) in b.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Codex and agy ids go on the command line: only a UUID-shaped one (hex and
/// `-`, never leading with `-`) can't be read as a flag.
fn flag_safe(id: &str) -> Result<()> {
    if id.is_empty()
        || id.starts_with('-')
        || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        bail!("not a provider session id: {id}");
    }
    Ok(())
}

/// Adds session flags to `arguments` output for a print-mode launch.
pub fn session_arguments(
    mut args: Vec<OsString>,
    runtime: &str,
    session: &ProviderSession,
) -> Result<Vec<OsString>> {
    if let ("codex" | "agy", Some(id), true) = (runtime, &session.id, session.resume) {
        flag_safe(id)?;
    }
    match (runtime, &session.id) {
        ("claude", Some(id)) => {
            let flag = if session.resume {
                "--resume"
            } else {
                "--session-id"
            };
            args.extend([flag.into(), id.into()]);
        }
        ("codex", Some(id)) if session.resume => {
            // `exec resume` rejects --sandbox; the config key means the same.
            // --color is cosmetic and also rejected.
            let mut kept = Vec::with_capacity(args.len());
            let mut it = args.into_iter();
            while let Some(arg) = it.next() {
                let text = arg.to_string_lossy().into_owned();
                if text == "--" {
                    // Everything after the delimiter is positional.
                    kept.push(arg);
                    kept.extend(it.by_ref());
                    break;
                }
                let (flag, inline) = match text.split_once('=') {
                    Some((f, v)) => (f.to_string(), Some(v.to_string())),
                    // -s takes its value attached too: -sworkspace-write
                    None if text.starts_with("-s") && text.len() > 2 && !text.starts_with("--") => {
                        ("-s".to_string(), Some(text[2..].to_string()))
                    }
                    None => (text.clone(), None),
                };
                if matches!(flag.as_str(), "--sandbox" | "-s" | "--color") {
                    let value = match inline {
                        Some(v) => v,
                        None => it
                            .next()
                            .map(|v| v.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    };
                    if flag != "--color" {
                        kept.extend([
                            "--config".into(),
                            format!("sandbox_mode=\"{value}\"").into(),
                        ]);
                    }
                    continue;
                }
                kept.push(arg);
            }
            args = kept;
            // exec <opts> -- - becomes exec resume <opts> -- <id> -
            args.insert(1, "resume".into());
            let stdin = args.pop().context("missing stdin marker")?;
            args.extend([id.into(), stdin]);
        }
        ("codex", None) if session.resume => bail!("codex session id missing"),
        ("agy", Some(id)) if session.resume => args.extend(["--conversation".into(), id.into()]),
        ("agy", None) if session.resume => bail!("agy conversation id missing"),
        _ => {}
    }
    Ok(args)
}

pub fn execute(d: &Decision, prompt: &str, cfg: &Config) -> Result<i32> {
    if native::enabled(cfg, &d.runtime)? {
        native::conversation(d, &native::one_shot_context(prompt), cfg)?;
        return Ok(0);
    }
    let activity = crate::activity::Activity::start(&d.model);
    if d.runtime == "ollama" {
        let mut stdout = std::io::stdout();
        crate::ollama::conversation(&d.model, prompt, cfg, &mut activity.writer(&mut stdout))?;
        return Ok(0);
    }
    if d.runtime == "agy" {
        // Its input is a JSON message and its output events: the stream path parses both.
        conversation(d, prompt, cfg, &mut std::io::stdout())?;
        return Ok(0);
    }
    let runtime = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude,
        "codex" => &cfg.inner.codex,
        "gemini" => &cfg.inner.gemini,
        _ => bail!("unsupported runtime"),
    };
    run_child(
        launch(&runtime.command)?
            .args(arguments(d, cfg)?)
            .envs(crate::mcp::for_launch(&d.runtime, cfg).1),
        prompt,
    )
}
fn run_child(command: &mut Command, prompt: &str) -> Result<i32> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("launching agent")?;
    crate::activity::set_phase(crate::activity::Phase::Waiting);
    let mut stdout = child.stdout.take().context("opening agent output")?;
    let mut stderr = child.stderr.take().context("opening agent diagnostics")?;
    let out_activity = crate::activity::current();
    let err_activity = out_activity.clone();
    let out_reader = thread::spawn(move || -> std::io::Result<()> {
        let mut buffer = [0; 8192];
        loop {
            let count = stdout.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            crate::activity::reply(&out_activity, &buffer[..count])?;
        }
        Ok(())
    });
    let err_reader = thread::spawn(move || -> std::io::Result<()> {
        let mut buffer = [0; 8192];
        loop {
            let count = stderr.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            crate::activity::diagnostic(&err_activity, &buffer[..count])?;
        }
        Ok(())
    });
    let written = child
        .stdin
        .take()
        .context("opening agent stdin")?
        .write_all(prompt.as_bytes());
    let status = child.wait().context("waiting for agent")?;
    let output = out_reader
        .join()
        .map_err(|_| anyhow::anyhow!("output reader panicked"))?;
    let diagnostics = err_reader
        .join()
        .map_err(|_| anyhow::anyhow!("diagnostics reader panicked"))?;
    let code = status.code().unwrap_or(1);
    if code == 0 {
        written.context("sending prompt to agent")?;
        output.context("reading agent output")?;
        diagnostics.context("reading agent diagnostics")?;
    }
    Ok(code)
}

pub fn check_runtime(command: &str) -> Result<std::path::PathBuf> {
    let mut launch = launch(command)?;
    let path = PathBuf::from(launch.get_program());
    let seconds = if launch.get_args().next().is_some_and(|arg| {
        Path::new(arg)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("js"))
    }) {
        60
    } else {
        10
    };
    let mut child = launch
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    bail!("version check exited with {status}");
                }
                return Ok(path);
            }
            Ok(None) if start.elapsed() < Duration::from_secs(seconds) => {
                thread::sleep(Duration::from_millis(50))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                result.context("checking CLI process")?;
                bail!("version check timed out after {seconds} seconds");
            }
        }
    }
}

/// The runtime (and its name) that Google's models launch.
pub fn google_config(cfg: &Config) -> &crate::config::RuntimeConfig {
    match crate::models::google_runtime(cfg) {
        crate::models::Runtime::Gemini => &cfg.inner.gemini,
        _ => &cfg.inner.agy,
    }
}
pub fn google_name(cfg: &Config) -> &'static str {
    match crate::models::google_runtime(cfg) {
        crate::models::Runtime::Gemini => "Gemini CLI",
        _ => "Antigravity CLI (agy)",
    }
}

pub fn doctor(cfg: &Config) -> Result<()> {
    let mut ok = true;
    if cfg.ollama_enabled() {
        let result = cfg
            .inner
            .ollama
            .as_ref()
            .context("Ollama selected but not configured")
            .and_then(|o| o.check());
        match result {
            Ok(()) => crate::terminal::line(
                "[OK] Ollama: server and selected local models verified (chat only)",
            )?,
            Err(e) => {
                crate::terminal::error(&format!("Ollama: {e:#}"));
                ok = false;
            }
        }
    }
    for (id, name, runtime) in [
        ("anthropic", "Claude Code", &cfg.inner.claude),
        ("openai", "Codex CLI", &cfg.inner.codex),
        ("google", google_name(cfg), google_config(cfg)),
    ] {
        if id == "google" && cfg.inner.providers.is_none() {
            continue;
        }
        if cfg
            .inner
            .providers
            .as_ref()
            .is_some_and(|providers| !providers.iter().any(|p| p == id))
        {
            crate::terminal::line(&format!("{name}: not selected (skipped)"))?;
            continue;
        }
        let result = check_runtime(&runtime.command).and_then(|path| {
            crate::terminal::line(&format!("[OK] {name}: {}", path.display()))?;
            Ok(())
        });
        if let Err(error) = result {
            crate::terminal::error(&format!("{name}: {error:#}"));
            ok = false;
        }
    }
    if !ok {
        bail!("one or more CLIs unavailable; routes remain usable offline");
    }
    crate::terminal::line(
        "Executable checks passed. Authentication and model access were not tested.",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_flags_start_and_resume_each_provider() {
        use super::{session_arguments, ProviderSession};
        let os = |v: &[&str]| {
            v.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        let claude = os(&["-p", "--model", "opus"]);
        let start = ProviderSession::start("claude");
        let id = start.id.clone().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_eq!(
            session_arguments(claude.clone(), "claude", &start).unwrap(),
            os(&["-p", "--model", "opus", "--session-id", &id])
        );
        assert_eq!(
            session_arguments(claude, "claude", &ProviderSession::resume("abc")).unwrap(),
            os(&["-p", "--model", "opus", "--resume", "abc"])
        );
        let codex = os(&["exec", "--model", "gpt-5.6-sol", "--json", "--", "-"]);
        assert_eq!(
            session_arguments(codex.clone(), "codex", &ProviderSession::start("codex")).unwrap(),
            codex
        );
        assert_eq!(
            session_arguments(
                codex,
                "codex",
                &ProviderSession::resume("a1b2c3d4-0000-4000-8000-000000000001")
            )
            .unwrap(),
            os(&[
                "exec",
                "resume",
                "--model",
                "gpt-5.6-sol",
                "--json",
                "--",
                "a1b2c3d4-0000-4000-8000-000000000001",
                "-"
            ])
        );
        // `exec resume` rejects --sandbox and --color; sandbox becomes config.
        let sandboxed = os(&[
            "exec",
            "--model",
            "m",
            "--sandbox",
            "workspace-write",
            "--color=never",
            "-s=read-only",
            "-sdanger-full-access",
            "--",
            "-",
        ]);
        assert_eq!(
            session_arguments(
                sandboxed,
                "codex",
                &ProviderSession::resume("a1b2c3d4-0000-4000-8000-000000000001")
            )
            .unwrap(),
            os(&[
                "exec",
                "resume",
                "--model",
                "m",
                "--config",
                "sandbox_mode=\"workspace-write\"",
                "--config",
                "sandbox_mode=\"read-only\"",
                "--config",
                "sandbox_mode=\"danger-full-access\"",
                "--",
                "a1b2c3d4-0000-4000-8000-000000000001",
                "-"
            ])
        );
    }

    use super::*;
    use std::io::Read;
    #[test]
    #[ignore = "subprocess fixture only"]
    fn child_fixture() {
        let Ok(mode) = env::var("NECTRA_TEST_CHILD") else {
            return;
        };
        if mode == "early_exit" {
            std::process::exit(23);
        }
        let mut prompt = String::new();
        std::io::stdin().read_to_string(&mut prompt).unwrap();
        assert_eq!(prompt, "--help & café\n".repeat(20000));
        std::process::exit(0);
    }
    #[test]
    fn child_receives_large_unicode_prompt_and_eof() {
        let mut cmd = Command::new(env::current_exe().unwrap());
        cmd.args(["--exact", "executor::tests::child_fixture", "--ignored"])
            .env("NECTRA_TEST_CHILD", "read");
        assert_eq!(
            run_child(&mut cmd, &"--help & café\n".repeat(20000)).unwrap(),
            0
        );
    }
    #[test]
    fn child_exit_code_survives_broken_pipe() {
        let mut cmd = Command::new(env::current_exe().unwrap());
        cmd.args(["--exact", "executor::tests::child_fixture", "--ignored"])
            .env("NECTRA_TEST_CHILD", "early_exit");
        assert_eq!(run_child(&mut cmd, &"x".repeat(1000000)).unwrap(), 23);
    }
}
