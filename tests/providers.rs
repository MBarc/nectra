use clap::Parser;
use nectra::{
    cli::{Cli, Mode, ProviderArg},
    config::Config,
    executor,
    models::Registry,
    router,
};
#[test]
fn permissions_are_translated_per_runtime_and_never_persisted() {
    let mut cfg = Config::from_defaults().unwrap();
    cfg.inner.providers = Some(vec!["anthropic".into(), "openai".into(), "google".into()]);
    let reg = Registry::load(&cfg).unwrap();
    for (provider, flag) in [
        (ProviderArg::Anthropic, "--dangerously-skip-permissions"),
        (
            ProviderArg::Openai,
            "--dangerously-bypass-approvals-and-sandbox",
        ),
        (ProviderArg::Google, "--approval-mode=yolo"),
    ] {
        let decision =
            router::route("hello", &cfg, &reg, Mode::Best, Some(provider), None).unwrap();
        cfg.skip_permissions = false;
        assert!(!executor::arguments(&decision, &cfg)
            .unwrap()
            .iter()
            .any(|a| a == flag));
        cfg.skip_permissions = true;
        let args = executor::arguments(&decision, &cfg).unwrap();
        assert!(args.iter().any(|a| a == flag));
        assert!(!args.iter().any(|a| a == "hello"));
    }
    assert!(!cfg.pretty_toml().unwrap().contains("skip_permissions"));
    assert!(
        Cli::try_parse_from(["nectra", "chat", "--dangerously-skip-permissions"])
            .unwrap()
            .dangerously_skip_permissions
    );
}
#[test]
fn gemini_dependency_is_checked_only_when_selected() {
    let dir = tempfile::tempdir().unwrap();
    let mut checked = vec![];
    nectra::setup::configure(
        &dir.path().join("config.toml"),
        Some("google"),
        &mut std::io::empty(),
        &mut vec![],
        &mut |command| {
            checked.push(command.to_string());
            Ok(command.into())
        },
    )
    .unwrap();
    assert_eq!(checked, ["gemini"]);
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let reg = Registry::load(&cfg).unwrap();
    assert!(reg
        .models
        .iter()
        .filter(|m| m.enabled)
        .all(|m| m.provider.as_str() == "google"));
}

#[test]
fn color_mode_does_not_decorate_machine_output_and_login_needs_terminal() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["--color", "always", "route", "hello"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["provider", "list", "--color", "always"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.contains(&27));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["provider", "list"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!output.stdout.contains(&27));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["provider", "login", "google"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
}
