use clap::Parser;
use nectra::{
    classifier,
    cli::{Cli, Mode, ProviderArg},
    config::Config,
    executor,
    models::{Registry, Runtime},
    router,
};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

fn fixture() -> (tempfile::TempDir, Config, Registry) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        include_str!("../config/nectra.toml"),
    )
    .unwrap();
    fs::write(
        dir.path().join("models.toml"),
        include_str!("../config/models.toml"),
    )
    .unwrap();
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let reg = Registry::load(&cfg).unwrap();
    (dir, cfg, reg)
}
#[test]
fn reserved_runtime_options_rejected() {
    let (dir, _, _) = fixture();
    for arg in [
        "--model=other",
        "-mother",
        "--config",
        "--output-format=json",
        "--",
    ] {
        let text = include_str!("../config/nectra.toml")
            .replace("extra_args = []", &format!("extra_args = [\"{arg}\"]"));
        let path = dir.path().join("reserved.toml");
        fs::write(&path, text).unwrap();
        assert!(Config::from_path(&path).is_err(), "{arg}");
    }
}
#[test]
fn substring_false_positives_are_general() {
    for prompt in [
        "rapid contest",
        "planet memoir",
        "prefix butterfly",
        "apiculture",
    ] {
        assert_eq!(
            classifier::classify(prompt).0.to_string(),
            "general",
            "{prompt}"
        );
    }
}
#[test]
fn punctuation_and_case_preserve_phrases() {
    assert_eq!(
        classifier::classify("DEBUG a race-condition!")
            .0
            .to_string(),
        "debugging"
    );
    assert_eq!(
        classifier::classify("Review this pull\nrequest")
            .0
            .to_string(),
        "code_review"
    );
}
#[test]
fn overrides_and_conflicts() {
    let (_dir, cfg, reg) = fixture();
    let d = router::route(
        "write a memo",
        &cfg,
        &reg,
        Mode::Fast,
        None,
        Some("gpt-5.6-sol".into()),
    )
    .unwrap();
    assert_eq!(d.model, "gpt-5.6-sol");
    assert!(d.overridden);
    assert_eq!(d.score_margin, None);
    assert!(!d.ambiguous);
    assert!(router::route(
        "task",
        &cfg,
        &reg,
        Mode::Best,
        Some(ProviderArg::Anthropic),
        Some("gpt-5.6-sol".into())
    )
    .is_err());
}
#[test]
fn disabled_and_unknown_models_fail() {
    let (_dir, cfg, mut reg) = fixture();
    for m in &mut reg.models {
        m.enabled = false;
    }
    assert!(router::route("task", &cfg, &reg, Mode::Fast, None, None).is_err());
}
#[test]
fn empty_prompt_fails() {
    let (_dir, cfg, reg) = fixture();
    assert!(router::route(" \n ", &cfg, &reg, Mode::Fast, None, None).is_err());
}
#[test]
fn invalid_registry_fails_closed() {
    let (_dir, _, mut reg) = fixture();
    reg.models[0].runtime = Runtime::Codex;
    assert!(reg.validate().is_err());
}
#[test]
fn duplicate_ids_and_nonfinite_scores_rejected() {
    let (_dir, _, mut reg) = fixture();
    reg.models.push(reg.models[0].clone());
    assert!(reg.validate().is_err());
    reg.models.pop();
    reg.models[0].capabilities.coding = f32::NAN;
    assert!(reg.validate().is_err());
}
#[test]
fn ties_are_independent_of_registry_order() {
    let (_dir, cfg, mut reg) = fixture();
    let cap = reg.models[0].capabilities.clone();
    for m in &mut reg.models {
        m.capabilities = cap.clone();
        m.id = format!("custom-{}", m.id); // Equal scores without bundled responsibility preferences.
    }
    let a = router::route("task", &cfg, &reg, Mode::Fast, None, None).unwrap();
    reg.models.reverse();
    let b = router::route("task", &cfg, &reg, Mode::Fast, None, None).unwrap();
    assert_eq!(a.model, b.model);
    assert!(a.ambiguous);
    assert_eq!(a.score_margin, Some(0.0));
}
#[test]
fn config_validation_and_relative_registry() {
    let (dir, cfg, _) = fixture();
    assert_eq!(cfg.registry_path(), dir.path().join("models.toml"));
    for text in [
        include_str!("../config/nectra.toml").replace("balanced", "typo"),
        include_str!("../config/nectra.toml").replace("0.035", "nan"),
        include_str!("../config/nectra.toml").replace("registry =", "regsitry ="),
    ] {
        fs::write(dir.path().join("invalid.toml"), text).unwrap();
        assert!(Config::from_path(&dir.path().join("invalid.toml")).is_err());
    }
}
#[test]
fn embedded_registry_ignores_cwd_files() {
    let (_dir, mut cfg, _) = fixture();
    cfg.embedded = true;
    cfg.inner.registry = "Cargo.toml".into();
    Registry::load(&cfg).unwrap().validate().unwrap();
}
#[test]
fn prompt_is_never_an_argument() {
    let (_dir, cfg, reg) = fixture();
    let d = router::route(
        "--help & echo injected",
        &cfg,
        &reg,
        Mode::Fast,
        Some(ProviderArg::Openai),
        None,
    )
    .unwrap();
    let args = executor::arguments(&d, &cfg).unwrap();
    assert_eq!(&args[args.len() - 2..], ["--", "-"]);
    assert!(!args
        .iter()
        .any(|a| a.to_string_lossy().contains("injected")));
}
#[test]
fn cli_has_no_implicit_mode_override() {
    assert_eq!(
        Cli::try_parse_from(["nectra", "route", "task"])
            .unwrap()
            .mode,
        None
    );
    assert_eq!(
        Cli::try_parse_from(["nectra", "--mode", "best", "route", "task"])
            .unwrap()
            .mode,
        Some(Mode::Best)
    );
}
fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nectra"))
}
#[test]
fn cli_uses_configured_mode_and_cli_override() {
    let (dir, cfg, _) = fixture();
    fs::write(
        &cfg.path,
        include_str!("../config/nectra.toml").replace("balanced", "fast"),
    )
    .unwrap();
    for (options, expected) in [(vec![], "fast"), (vec!["--mode", "best"], "best")] {
        let out = binary()
            .current_dir(dir.path())
            .arg("--config")
            .arg(&cfg.path)
            .args(options)
            .args(["route", "debug a bug"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(json["mode"], expected);
    }
}
#[test]
fn stdin_route_and_conflicting_input() {
    let (_dir, cfg, _) = fixture();
    let mut child = binary()
        .arg("--config")
        .arg(&cfg.path)
        .args(["route", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("debug this race condition — café".as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["task"], "debugging");
    assert!(!binary()
        .arg("--config")
        .arg(&cfg.path)
        .args(["route", "--stdin", "extra"])
        .output()
        .unwrap()
        .status
        .success());
}
#[test]
fn config_inspection_survives_missing_registry() {
    let (_dir, cfg, _) = fixture();
    fs::remove_file(cfg.registry_path()).unwrap();
    assert!(binary()
        .arg("--config")
        .arg(&cfg.path)
        .args(["config", "--print"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!binary()
        .arg("--config")
        .arg(&cfg.path)
        .args(["route", "task"])
        .output()
        .unwrap()
        .status
        .success());
}
#[test]
fn evaluation_corpus_passes() {
    let (_dir, cfg, _) = fixture();
    let out = binary()
        .arg("--config")
        .arg(&cfg.path)
        .arg("eval")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/eval/tasks.json"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn responsibility_matrix_and_physical_planning() {
    let (_, cfg, reg) = fixture();
    for (prompt, expected) in [
        ("summarize this briefly", "haiku"),
        ("simple function implementation", "gpt-5.6-luna"),
        ("write a memo", "sonnet"),
        ("plan how to build a shed", "opus"),
        ("debug this failing function", "gpt-5.6-sol"),
        ("complex end to end architecture", "gpt-6-astra"),
    ] {
        let d = router::route(prompt, &cfg, &reg, Mode::Balanced, None, None).unwrap();
        assert_eq!(d.model, expected, "{prompt}");
        if prompt.contains("shed") {
            assert_eq!(d.task, "planning");
        }
    }
}
