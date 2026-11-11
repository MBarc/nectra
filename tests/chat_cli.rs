// Own binary on purpose: spawning a child forks the whole process, and in the
// shared chat.rs binary the forked child briefly held sibling tests' session
// file locks (flock is per open file description), making their resume fail.
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn cli_chat_creates_and_resumes_without_model_calls() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    fs::write(&cfg, include_str!("../config/nectra.toml")).unwrap();
    fs::write(
        dir.path().join("models.toml"),
        include_str!("../config/models.toml"),
    )
    .unwrap();
    let path = dir.path().join("cli.jsonl");
    for flag in ["--session", "--resume"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nectra"))
            .current_dir(dir.path())
            .arg("--config")
            .arg(&cfg)
            .args(["chat", flag])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"/help\n/session\n/quit\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8(out.stdout)
            .unwrap()
            .contains("Session saved:"));
    }
}