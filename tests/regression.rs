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
        m.id = format!("custom-{}", m.id);
        // Equal scores without bundled responsibility preferences. Roles are
        // declared per model now, so clearing them is what neutralises the
        // bonus; renaming the id alone no longer does.
        m.prefers.clear();
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
fn generalization_corpus_holds_its_floor() {
    // eval/tasks.json is tuned against: every fix is validated on it, so it
    // reports 100% and cannot measure generalization. This corpus is the
    // control. DO NOT tune the classifier against these prompts -- raising the
    // floor legitimately means adding new unseen prompts and re-measuring.
    // The floor is deliberately well below 100%: purely lexical matching cannot
    // classify a prompt containing none of its keywords, and most failures here
    // are exactly that.
    const FLOOR: f64 = 0.50;

    let (_dir, cfg, _) = fixture();
    let out = binary()
        .arg("--config")
        .arg(&cfg.path)
        .arg("eval")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/eval/generalization.json"
        ))
        .output()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("eval emits a JSON report");
    let accuracy = report["accuracy"].as_f64().unwrap();
    assert!(
        accuracy >= FLOOR,
        "generalization fell to {accuracy:.3}, below the {FLOOR:.2} floor;          a change that helps eval/tasks.json but hurts unseen prompts is overfitting"
    );
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
        // Balanced tries the cheap coder first; best mode keeps the strong one.
        ("debug this failing function", "gpt-5.6-luna"),
        ("complex end to end architecture", "gpt-6-astra"),
    ] {
        let d = router::route(prompt, &cfg, &reg, Mode::Balanced, None, None).unwrap();
        assert_eq!(d.model, expected, "{prompt}");
        if prompt.contains("shed") {
            assert_eq!(d.task, "planning");
        }
    }
    let best = router::route(
        "debug this failing function",
        &cfg,
        &reg,
        Mode::Best,
        None,
        None,
    );
    assert_eq!(best.unwrap().model, "gpt-5.6-sol");
}

#[test]
fn work_on_code_never_goes_to_a_chat_only_model_when_one_can_act() {
    use nectra::ollama::{LocalModel, LocalPolicy, OllamaConfig};
    for policy in [LocalPolicy::PreferLocal, LocalPolicy::LocalOnly] {
        let mut cfg = Config::from_defaults().unwrap();
        cfg.inner.ollama = Some(OllamaConfig {
            base_url: "http://localhost:11434".into(),
            models: vec![LocalModel {
                name: "test:latest".into(),
                context_tokens: 8192,
            }],
            policy,
            timeout_seconds: 2,
            token_env: None,
        });
        let reg = Registry::load(&cfg).unwrap();
        let route = |p: &str| router::route(p, &cfg, &reg, Mode::Balanced, None, None).unwrap();
        // Chat still prefers local.
        assert_eq!(route("hello there").provider, "ollama");
        let fix = route("fix the failing test in parser.rs");
        if policy == LocalPolicy::PreferLocal {
            assert_ne!(fix.runtime, "ollama", "{policy:?}: {}", fix.reason);
        } else {
            // Local-only has no model that can act: Ollama answers, and says so.
            assert_eq!(fix.runtime, "ollama");
            assert!(fix.reason.contains("can only describe"), "{}", fix.reason);
        }
    }
}

#[test]
fn short_chit_chat_goes_to_the_simple_model_offline() {
    let (_dir, cfg, reg) = fixture();
    for prompt in ["thanks, that worked", "ok great", "hi"] {
        let d = router::route(prompt, &cfg, &reg, Mode::Balanced, None, None).unwrap();
        assert_eq!(d.model, "haiku", "{prompt}: {}", d.reason);
    }
    let d = router::route(
        "rename userId to accountId",
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
    )
    .unwrap();
    assert_eq!(d.task, "refactoring");
    // A short prompt that names hard work is not cheap work.
    let d = router::route(
        "fix the race condition",
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
    )
    .unwrap();
    assert_ne!(d.model, "haiku");
}

#[test]
fn continue_finds_the_newest_session_for_this_directory_only() {
    let sessions = tempfile::tempdir().unwrap();
    let here = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let path = |n: &str| sessions.path().join(format!("{n}-1.jsonl"));
    assert_eq!(
        nectra::session::Session::latest_in(sessions.path(), here.path()).unwrap(),
        None
    );
    drop(nectra::session::Session::create(&path("100"), here.path()).unwrap());
    drop(nectra::session::Session::create(&path("200"), here.path()).unwrap());
    drop(nectra::session::Session::create(&path("300"), elsewhere.path()).unwrap());
    fs::write(sessions.path().join("400-1.jsonl"), "not json\n").unwrap();
    assert_eq!(
        nectra::session::Session::latest_in(sessions.path(), here.path()).unwrap(),
        Some(path("200"))
    );
}

#[test]
fn a_partial_config_section_takes_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "registry = \"models.toml\"\n[claude]\ncommand = \"claude\"\n[codex]\ncommand = \"codex\"\n[routing]\nsticky = false\n[classifier]\nbackend = \"local\"\n",
    )
    .unwrap();
    let cfg = Config::from_path(&path).unwrap();
    assert!(!cfg.inner.routing.sticky);
    assert!(cfg.inner.routing.resume_sessions);
    assert_eq!(cfg.inner.routing.confidence_margin, 0.035);
    assert_eq!(cfg.inner.classifier.timeout_ms, 1500);
}
