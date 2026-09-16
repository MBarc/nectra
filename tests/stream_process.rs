//! This executable doubles as a fake provider, keeping child output free of test-harness text.
use nectra::{
    cli::Mode,
    config::Config,
    executor::{conversation_with_cancellation, CancellationToken},
    router::Decision,
};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--hold") {
        let path = Path::new(&args[2]);
        for tick in 0..1500 {
            fs::write(path.join("heartbeat"), tick.to_string()).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        return;
    }
    if args.get(1).is_some_and(|s| s == "exec") {
        fixture();
        return;
    }
    if args.get(1).is_some_and(|s| s == "--driver") {
        #[cfg(windows)]
        if args.get(3).is_some_and(|s| s == "--ignore-ctrl-c") {
            // Reproduce the process attribute inherited from some launchers.
            assert_ne!(
                unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 1) },
                0
            );
        }
        signal_driver(Path::new(&args[2]));
        return;
    }
    for mode in ["ok", "cancel", "bad"] {
        exercise(mode);
    }
    exercise_terminal_interrupt(false);
    #[cfg(windows)]
    exercise_terminal_interrupt(true);
    println!("stream process scenarios passed (completion, cancellation, malformed output, terminal Ctrl+C; Windows also covers ignored Ctrl+C)");
}

fn signal_driver(root: &Path) {
    let mut cfg = Config::from_defaults().unwrap();
    cfg.inner.codex.command = std::env::current_exe().unwrap().to_string_lossy().into();
    let d = Decision {
        task: "general".into(),
        provider: "openai".into(),
        runtime: "codex".into(),
        model: "test".into(),
        effort: String::new(),
        mode: Mode::Balanced,
        score: 0.,
        score_margin: None,
        ambiguous: false,
        overridden: true,
        reason: String::new(),
        candidates: vec![],
    };
    let result = nectra::executor::conversation(
        &d,
        &format!("cancel\n{}", root.display()),
        &cfg,
        &mut Vec::new(),
    );
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    fs::write(root.join("returned"), "chat stayed alive").unwrap();
}

fn exercise_terminal_interrupt(ignore_ctrl_c: bool) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let root = tempfile::tempdir().unwrap();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(std::env::current_exe().unwrap());
    cmd.arg("--driver");
    cmd.arg(root.path());
    if ignore_ctrl_c {
        cmd.arg("--ignore-ctrl-c");
    }
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut input = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let drain = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    while !root.path().join("ready").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    input.write_all(b"\x03").unwrap();
    input.flush().unwrap();
    let mut status = None;
    while Instant::now() < deadline {
        status = child.try_wait().unwrap();
        if status.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(input);
    drop(pair.master);
    let bytes = drain.join().unwrap();
    assert!(
        status.is_some_and(|s| s.success()) && root.path().join("returned").exists(),
        "terminal interrupt did not return safely (ignored={ignore_ctrl_c}): {}",
        String::from_utf8_lossy(&bytes)
    );
}

#[allow(
    clippy::zombie_processes,
    reason = "Fixture deliberately leaves an owned descendant for cleanup verification"
)]
fn fixture() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let (mode, path) = input.split_once('\n').unwrap();
    let root = Path::new(path);
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--hold", path])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    fs::write(root.join("pid"), child.id().to_string()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !root.join("heartbeat").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    if mode == "bad" {
        println!("not-json");
    } else {
        println!("{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"reply\"}}}}");
        if mode == "ok" {
            println!("{{\"type\":\"turn.completed\"}}");
        }
    }
    std::io::stdout().flush().unwrap();
    fs::write(root.join("ready"), "ready").unwrap();
    if mode != "ok" {
        thread::sleep(Duration::from_secs(30));
    }
}

fn exercise(mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = Config::from_defaults().unwrap();
    cfg.inner.codex.command = std::env::current_exe().unwrap().to_string_lossy().into();
    let d = Decision {
        task: "general".into(),
        provider: "openai".into(),
        runtime: "codex".into(),
        model: "test".into(),
        effort: String::new(),
        mode: Mode::Balanced,
        score: 0.,
        score_margin: None,
        ambiguous: false,
        overridden: true,
        reason: String::new(),
        candidates: vec![],
    };
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let ready = root.path().join("ready");
    let cancelling = mode == "cancel";
    let worker = thread::spawn(move || {
        if !cancelling {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        signal.cancel();
    });
    let started = Instant::now();
    let mut output = Vec::new();
    let result = conversation_with_cancellation(
        &d,
        &format!("{mode}\n{}", root.path().display()),
        &cfg,
        &mut output,
        &cancel,
    );
    worker.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{mode} exceeded shutdown deadline"
    );
    if mode == "ok" {
        assert_eq!(result.unwrap(), "reply");
    } else {
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains(if mode == "cancel" {
                "cancelled"
            } else {
                "invalid JSON"
            }),
            "{error}"
        );
    }
    let before = fs::read(root.path().join("heartbeat")).unwrap();
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        before,
        fs::read(root.path().join("heartbeat")).unwrap(),
        "{mode}: descendant survived cleanup"
    );
}
