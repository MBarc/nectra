use nectra::terminal::{self, Theme};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn preference_roundtrip_does_not_modify_provider_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "provider settings stay intact").unwrap();
    let path = terminal::theme_path(Some(&config)).unwrap();
    assert_eq!(terminal::load_theme(&path).unwrap(), Theme::Default);
    for theme in [Theme::Hacker, Theme::IceBlue, Theme::Default] {
        terminal::save_theme(&path, theme).unwrap();
        assert_eq!(terminal::load_theme(&path).unwrap(), theme);
    }
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "provider settings stay intact"
    );
    assert_ne!(
        terminal::theme_path(Some(&dir.path().join("ui.toml"))).unwrap(),
        dir.path().join("ui.toml")
    );
}

#[test]
fn cli_saves_themes_and_launch_override_does_not_replace_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_nectra"))
            .args(args)
            .arg("--config")
            .arg(&config)
            .output()
            .unwrap()
    };
    assert!(run(&["theme", "hacker"]).status.success());
    assert_eq!(
        terminal::load_theme(&dir.path().join("ui.toml")).unwrap(),
        Theme::Hacker
    );
    let output = run(&[
        "provider", "list", "--theme", "ice-blue", "--color", "always",
    ]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("38;5;"));
    assert_eq!(
        terminal::load_theme(&dir.path().join("ui.toml")).unwrap(),
        Theme::Hacker
    );
    assert!(!run(&["theme", "unknown"]).status.success());
    let output = run(&["provider", "list", "--color", "never"]);
    assert!(!output.stdout.contains(&27));
}

#[test]
fn in_chat_theme_change_is_saved_but_not_added_to_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, include_str!("../config/nectra.toml")).unwrap();
    fs::write(
        dir.path().join("models.toml"),
        include_str!("../config/models.toml"),
    )
    .unwrap();
    let session = dir.path().join("chat.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["chat", "--config"])
        .arg(&config)
        .arg("--session")
        .arg(&session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/theme ice-blue\n/theme\n/quit\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("ice-blue (saved)"));
    assert_eq!(
        terminal::load_theme(&dir.path().join("ui.toml")).unwrap(),
        Theme::IceBlue
    );
    assert!(!fs::read_to_string(session).unwrap().contains("/theme"));
    assert!(!result.stderr.contains(&27));
}
