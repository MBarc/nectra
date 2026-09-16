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
pub use conversation::conversation_with_cancellation;
pub use conversation::AuthenticationRequired;
pub use process::CancellationToken;
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
    for dir in env::split_paths(&path).filter(|p| p.is_absolute()) {
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
            let script = dir.join("node_modules/@google/gemini-cli/dist/index.js");
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
        "google" | "gemini" => {
            crate::terminal::line("Opening Gemini CLI. Use /auth to sign in again, then /quit to return to Nectra.")?;
            (&cfg.inner.gemini.command, &[])
        }
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
        _ => bail!("unsupported runtime"),
    };
    let runtime = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude,
        "codex" => &cfg.inner.codex,
        "gemini" => &cfg.inner.gemini,
        _ => bail!("unsupported runtime"),
    };
    let mut extras = runtime.extra_args.iter();
    while let Some(arg) = extras.next() {
        let flag = arg.split('=').next().unwrap_or(arg);
        if cfg.skip_permissions
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
        if cfg.skip_permissions
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
    if cfg.skip_permissions {
        args.push(
            match d.runtime.as_str() {
                "claude" => "--dangerously-skip-permissions",
                "codex" => "--dangerously-bypass-approvals-and-sandbox",
                "gemini" => "--approval-mode=yolo",
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
pub fn execute(d: &Decision, prompt: &str, cfg: &Config) -> Result<i32> {
    if d.runtime != "ollama" && native::enabled(cfg)? {
        native::conversation(d, &native::one_shot_context(prompt), cfg)?;
        return Ok(0);
    }
    let activity = crate::activity::Activity::start(&d.model);
    if d.runtime == "ollama" {
        let mut stdout = std::io::stdout();
        crate::ollama::conversation(&d.model, prompt, cfg, &mut activity.writer(&mut stdout))?;
        return Ok(0);
    }
    let runtime = match d.runtime.as_str() {
        "claude" => &cfg.inner.claude,
        "codex" => &cfg.inner.codex,
        "gemini" => &cfg.inner.gemini,
        _ => bail!("unsupported runtime"),
    };
    run_child(launch(&runtime.command)?.args(arguments(d, cfg)?), prompt)
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
        ("google", "Gemini CLI", &cfg.inner.gemini),
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
