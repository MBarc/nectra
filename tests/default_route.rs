//! The default route is haiku (docs/routing.md, eval/calibration/CLASSIFIER-VS-CASCADE.md).
//! The user's own steering, retries after a failure, the measured table and Laya
//! are unchanged, and the last two only apply when opted in.
use nectra::{
    classifier::{requirements, TaskType},
    cli::Mode,
    config::{ClassifierBackend, Config},
    jev::Classification,
    models::Registry,
    router,
    table::{self, Steer},
};
use std::fs;

fn load(config_extra: &str, table: &[table::TableEntry]) -> (tempfile::TempDir, Config, Registry) {
    let dir = tempfile::tempdir().unwrap();
    // `config_extra` is `key = value` lines that replace the shipped ones.
    let mut config = include_str!("../config/nectra.toml").to_string();
    for line in config_extra.lines() {
        let key = line.split('=').next().unwrap().trim();
        let old = config
            .lines()
            .find(|l| l.starts_with(&format!("{key} =")))
            .unwrap()
            .to_string();
        config = config.replace(&old, line);
    }
    fs::write(dir.path().join("config.toml"), config).unwrap();
    let models = if table.is_empty() {
        include_str!("../config/models.toml").to_string()
    } else {
        table::splice(include_str!("../config/models.toml"), table).unwrap()
    };
    fs::write(dir.path().join("models.toml"), models).unwrap();
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let reg = Registry::load(&cfg).unwrap();
    (dir, cfg, reg)
}

fn ladder() -> Vec<table::TableEntry> {
    let rung = |m: &str, p: f64| table::Rung {
        model: m.into(),
        n: 30,
        pass_rate: p,
        ci: [0.5, 0.99],
        run: "t1".into(),
        cost_usd: None,
        basis: None,
    };
    table::KINDS
        .iter()
        .flat_map(|k| {
            ["simple", "normal", "demanding"].map(|tier| table::TableEntry {
                kind: (*k).into(),
                tier: tier.into(),
                ladder: vec![rung("sonnet", 0.8), rung("opus", 0.95)],
            })
        })
        .collect()
}

fn laya(difficulty: f64) -> Classification {
    Classification {
        task: TaskType::Debugging,
        req: requirements(TaskType::Debugging),
        tier: None,
        why: "laya test".into(),
        new_task: None,
        role: None,
        classifier: "laya",
        confidence: Some(0.95),
        difficulty: Some(difficulty),
    }
}

const PROMPTS: [&str; 6] = [
    "fix the failing test in parser.rs",
    "debug this failing function",
    "write a design doc for the sync service",
    "summarize the trade-offs between sqlite and postgres",
    "refactor this module to use iterators",
    "thanks, that worked",
];

#[test]
fn a_plain_request_goes_to_haiku_by_default() {
    let (_d, cfg, reg) = load("", &[]);
    for prompt in PROMPTS {
        let d = router::route(prompt, &cfg, &reg, Mode::Balanced, None, None).unwrap();
        assert_eq!(d.model, "haiku", "{prompt}: {}", d.reason);
        assert!(d.ladder.is_none() && !d.overridden, "{}", d.reason);
        assert!(d.reason.contains("routing.default_model"), "{}", d.reason);
    }
}

#[test]
fn the_users_own_steering_still_chooses() {
    let (_d, cfg, reg) = load("", &[]);
    let go = |prompt: &str, mode, model: Option<&str>| {
        router::route(prompt, &cfg, &reg, mode, None, model.map(Into::into))
            .unwrap()
            .model
    };
    // /model and --model.
    assert_eq!(go(PROMPTS[0], Mode::Balanced, Some("opus")), "opus");
    // A mode other than balanced.
    assert_ne!(go(PROMPTS[0], Mode::Best, None), "haiku");
    // Demanding wording in the prompt.
    assert_ne!(
        go(
            "complex end to end rewrite of the scheduler",
            Mode::Balanced,
            None
        ),
        "haiku"
    );
}

#[test]
fn a_retry_after_a_failure_never_goes_back_to_haiku() {
    let (_d, cfg, reg) = load("", &[]);
    let cls = laya(0.4);
    let steer = Steer {
        above: Some("haiku".into()),
        escalations: 1,
        attempted: vec!["haiku".into()],
        ..Steer::default()
    };
    let (d, _) = router::route_summarized_steered(
        cls,
        PROMPTS[0],
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
        &steer,
    )
    .unwrap();
    assert_ne!(d.model, "haiku", "{}", d.reason);
}

#[test]
fn an_empty_default_model_brings_back_the_scoring_heuristic() {
    let (_d, cfg, reg) = load("default_model = \"\"", &[]);
    let d = router::route(PROMPTS[1], &cfg, &reg, Mode::Balanced, None, None).unwrap();
    assert_eq!(d.model, "gpt-5.6-luna", "{}", d.reason);
}

#[test]
fn a_default_model_that_is_not_eligible_falls_back_to_scoring() {
    // --provider openai leaves haiku out of the candidates.
    let (_d, cfg, reg) = load("", &[]);
    let d = router::route(
        PROMPTS[0],
        &cfg,
        &reg,
        Mode::Balanced,
        Some(nectra::cli::ProviderArg::Openai),
        None,
    )
    .unwrap();
    assert_ne!(d.model, "haiku");
    assert!(d.model.starts_with("gpt"), "{}", d.model);
}

#[test]
fn the_measured_table_is_ignored_unless_opted_in() {
    let table = ladder();
    // A table in models.toml, default config: haiku, no ladder.
    let (_d, cfg, reg) = load("", &table);
    let d = router::route_with(
        laya(0.4),
        PROMPTS[0],
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
    )
    .unwrap();
    assert_eq!(d.model, "haiku", "{}", d.reason);
    assert!(d.ladder.is_none());
    // Default model dropped but the table still off: scoring, no ladder.
    let (_d, cfg, reg) = load("default_model = \"\"", &table);
    let d = router::route_with(
        laya(0.4),
        PROMPTS[0],
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
    )
    .unwrap();
    assert!(d.ladder.is_none(), "{}", d.reason);
    // Opted in: the table picks its first rung.
    let (_d, cfg, reg) = load("default_model = \"\"\nuse_table = true", &table);
    let d = router::route_with(
        laya(0.4),
        PROMPTS[0],
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
    )
    .unwrap();
    assert_eq!(d.model, "sonnet", "{}", d.reason);
    assert!(d.ladder.is_some());
}

#[test]
fn laya_as_the_backend_turns_the_default_model_off() {
    let (_d, mut cfg, reg) = load("", &[]);
    cfg.inner.classifier.backend = ClassifierBackend::Laya;
    let d = router::route_with(
        laya(0.4),
        PROMPTS[1],
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        None,
    )
    .unwrap();
    assert_eq!(d.model, "gpt-5.6-luna", "{}", d.reason);
    // And by default the backend is not Laya.
    let (_d, cfg, _r) = load("", &[]);
    assert_ne!(cfg.inner.classifier.backend, ClassifierBackend::Laya);
}
