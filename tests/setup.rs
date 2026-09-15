use anyhow::{bail, Result};
use nectra::{
    config::Config,
    models::Registry,
    setup::{configure, parse_providers},
};
use std::{fs, io::Cursor, path::PathBuf};

fn installed(command: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(command))
}

#[test]
fn selection_disables_unselected_providers_and_preserves_custom_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let mut checked = vec![];
    configure(
        &path,
        Some("openai,mistral,xai"),
        &mut Cursor::new(""),
        &mut vec![],
        &mut |command| {
            checked.push(command.to_string());
            installed(command)
        },
    )
    .unwrap();
    assert_eq!(checked, ["codex"]);
    let cfg = Config::from_path(&path).unwrap();
    let models = Registry::load(&cfg).unwrap();
    assert!(models
        .models
        .iter()
        .all(|m| m.enabled == (m.provider.as_str() == "openai")));
    let mut cfg = cfg;
    cfg.inner.codex.command = "custom-codex".into();
    fs::write(&path, cfg.pretty_toml().unwrap()).unwrap();
    let registry_before = fs::read(dir.path().join("models.toml")).unwrap();
    configure(
        &path,
        Some("anthropic"),
        &mut Cursor::new(""),
        &mut vec![],
        &mut installed,
    )
    .unwrap();
    assert_eq!(
        Config::from_path(&path).unwrap().inner.codex.command,
        "custom-codex"
    );
    assert_eq!(
        fs::read(dir.path().join("models.toml")).unwrap(),
        registry_before
    );
}

#[test]
fn missing_cli_blocks_save_and_leaves_existing_config_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    configure(
        &path,
        Some("none"),
        &mut Cursor::new(""),
        &mut vec![],
        &mut installed,
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    let mut output = vec![];
    let error = configure(
        &path,
        Some("openai"),
        &mut Cursor::new(""),
        &mut output,
        &mut |_| bail!("not installed"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("incomplete"));
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("MISSING OR UNUSABLE"));
    assert_eq!(fs::read(&path).unwrap(), before);
    let fresh = dir.path().join("fresh/config.toml");
    assert!(configure(
        &fresh,
        Some("anthropic"),
        &mut Cursor::new(""),
        &mut vec![],
        &mut |_| bail!("bad version")
    )
    .is_err());
    assert!(!fresh.exists());
}

#[test]
fn wizard_reprompts_invalid_input_and_cancels_without_writing() {
    assert_eq!(
        parse_providers("2, OpenAI, 1").unwrap(),
        ["openai", "anthropic"]
    );
    assert!(parse_providers("none,openai").is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let mut output = vec![];
    configure(
        &path,
        None,
        &mut Cursor::new("bogus\n\n"),
        &mut output,
        &mut installed,
    )
    .unwrap();
    assert!(!path.exists());
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Unknown provider"));
    configure(
        &path,
        None,
        &mut Cursor::new("2\n"),
        &mut vec![],
        &mut installed,
    )
    .unwrap();
    assert_eq!(
        Config::from_path(&path).unwrap().inner.providers.unwrap(),
        ["openai"]
    );
}

#[test]
fn unsupported_only_and_empty_selections_never_enable_cloud_fallback() {
    for selection in ["mistral,xai", "none"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        configure(
            &path,
            Some(selection),
            &mut Cursor::new(""),
            &mut vec![],
            &mut |_| panic!("unselected CLI must not be checked"),
        )
        .unwrap();
        let cfg = Config::from_path(&path).unwrap();
        assert!(Registry::load(&cfg)
            .unwrap()
            .models
            .iter()
            .all(|m| !m.enabled));
    }
}

#[test]
fn setup_command_can_save_none_without_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["setup", "--providers", "none", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let before = fs::read(&path).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["setup", "--if-needed", "--providers", "openai", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(before, fs::read(path).unwrap());
}

#[test]
fn provider_commands_preserve_other_selections_and_reject_unknown_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    for (operation, id, expected) in [
        ("add", "mistral", vec!["mistral"]),
        ("add", "xai", vec!["mistral", "xai"]),
        ("add", "mistral", vec!["mistral", "xai"]),
        ("remove", "mistral", vec!["xai"]),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
            .args(["provider", operation, id, "--config"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            Config::from_path(&path).unwrap().inner.providers.unwrap(),
            expected
        );
    }
    let before = fs::read(&path).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["provider", "add", "bogus", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["provider", "list", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("ollama"));
    assert_eq!(fs::read(&path).unwrap(), before);
}
