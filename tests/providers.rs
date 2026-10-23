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
        (ProviderArg::Google, "--dangerously-skip-permissions"),
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
fn a_read_only_launch_drops_every_configured_permission() {
    let mut cfg = Config::from_defaults().unwrap();
    cfg.inner.providers = Some(vec!["anthropic".into(), "openai".into()]);
    cfg.inner.claude.extra_args = vec![
        "--permission-mode".into(),
        "acceptEdits".into(),
        "--mcp-config".into(),
        "servers.json".into(),
    ];
    cfg.inner.codex.extra_args = vec!["--sandbox".into(), "workspace-write".into()];
    cfg.skip_permissions = true;
    cfg.read_only = true;
    let reg = Registry::load(&cfg).unwrap();
    let args = |provider| {
        let d = router::route("hello", &cfg, &reg, Mode::Best, Some(provider), None).unwrap();
        let args = executor::arguments(&d, &cfg).unwrap();
        // Resuming the outgoing session is how a handoff note is asked for.
        let session = executor::ProviderSession {
            resume: true,
            id: Some("a1b2c3d4-0000-4000-8000-000000000002".into()),
        };
        executor::session_arguments(args, &d.runtime, &session)
            .unwrap()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let claude = args(ProviderArg::Anthropic);
    let tools = claude.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(claude[tools + 1], "");
    // Without this, every MCP server's tools stay available.
    assert!(claude.contains(&"--strict-mcp-config".to_string()));
    let codex = args(ProviderArg::Openai);
    assert!(
        codex.contains(&"sandbox_mode=\"read-only\"".to_string()),
        "{codex:?}"
    );
    for args in [claude, codex] {
        assert!(
            !args.iter().any(|a| a.contains("dangerously")
                || a.contains("acceptEdits")
                || a.contains("servers.json")
                || a.contains("workspace-write")),
            "{args:?}"
        );
    }
}
#[test]
fn google_dependency_is_checked_only_when_selected() {
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
    assert_eq!(checked, ["agy"]);
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
