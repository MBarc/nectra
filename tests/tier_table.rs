//! The task x tier table in the router: picks, hedging, mode and threshold
//! steering, keyword forcing, and unchanged behaviour without a table.
use nectra::{
    classifier::{requirements, TaskType},
    cli::Mode,
    config::{Config, TierHedge},
    jev::Classification,
    models::Registry,
    router,
    table::{self, Rung, TableEntry},
};
use std::fs;

fn rung(model: &str, pass: f64) -> Rung {
    Rung {
        model: model.into(),
        n: 40,
        pass_rate: pass,
        ci: [pass - 0.1, (pass + 0.05).min(1.0)],
        run: "rh-v2-test".into(),
    }
}
fn entry(tier: &str, models: &[&str]) -> TableEntry {
    TableEntry {
        kind: "debugging".into(),
        tier: tier.into(),
        ladder: models
            .iter()
            .enumerate()
            .map(|(i, m)| rung(m, 0.6 + 0.1 * i as f64))
            .collect(),
    }
}
fn example() -> Vec<TableEntry> {
    vec![
        entry("simple", &["haiku", "sonnet"]),
        entry("normal", &["sonnet", "gpt-5.6-sol", "opus"]),
        entry("demanding", &["gpt-5.6-sol", "opus"]),
    ]
}

fn setup(
    table: &[TableEntry],
    routing: &str,
    classifier: &str,
) -> (tempfile::TempDir, Config, Registry) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = include_str!("../config/nectra.toml").to_string();
    config = config.replacen("[routing]", &format!("[routing]\n{routing}"), 1);
    config.push_str(&format!("\n[classifier]\n{classifier}\n"));
    fs::write(dir.path().join("config.toml"), config).unwrap();
    let models = if table.is_empty() {
        include_str!("../config/models.toml").to_string()
    } else {
        table::splice(include_str!("../config/models.toml"), table).unwrap()
    };
    fs::write(dir.path().join("models.toml"), models).unwrap();
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let registry = Registry::load(&cfg).unwrap();
    (dir, cfg, registry)
}

fn laya(difficulty: f64, confidence: f64) -> Classification {
    Classification {
        task: TaskType::Debugging,
        req: requirements(TaskType::Debugging),
        tier: None,
        why: "laya test".into(),
        new_task: None,
        role: None,
        classifier: "laya",
        confidence: Some(confidence),
        difficulty: Some(difficulty),
    }
}
fn keywords() -> Classification {
    Classification {
        classifier: "keywords",
        confidence: None,
        difficulty: None,
        ..laya(0.0, 0.0)
    }
}
fn route(
    cls: Classification,
    prompt: &str,
    cfg: &Config,
    reg: &Registry,
    mode: Mode,
) -> router::Decision {
    router::route_with(cls, prompt, cfg, reg, mode, None, None).unwrap()
}

#[test]
fn the_table_picks_the_cheapest_rung_of_the_tier() {
    let (_d, cfg, reg) = setup(&example(), "", "");
    let d = route(laya(0.4, 0.9), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert_eq!(d.model, "sonnet");
    let ladder = d.ladder.expect("table pick recorded");
    assert_eq!(
        (ladder.rung, ladder.ladder_len, ladder.tier.as_str()),
        (0, 3, "normal")
    );
    assert!(
        d.reason.contains("table debugging x normal"),
        "{}",
        d.reason
    );
}

#[test]
fn without_a_table_nothing_changes() {
    let (_d, cfg, reg) = setup(&[], "", "");
    assert!(reg.table.is_empty());
    let d = route(laya(0.4, 0.9), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert!(d.ladder.is_none());
    assert_eq!(d.escalation, 0);
    let json = serde_json::to_value(&d).unwrap();
    assert!(json.get("ladder").is_none() && json.get("escalation").is_none());
    // Same answer as a table that has no entry for this kind.
    let other = vec![TableEntry {
        kind: "writing".into(),
        ..entry("normal", &["sonnet"])
    }];
    let (_d2, cfg2, reg2) = setup(&other, "", "");
    let again = route(
        laya(0.4, 0.9),
        "fix the crash",
        &cfg2,
        &reg2,
        Mode::Balanced,
    );
    assert_eq!(again.model, d.model);
    assert!(again.ladder.is_none());
}

#[test]
fn an_unsure_tier_starts_one_rung_higher_by_default() {
    let (_d, cfg, reg) = setup(&example(), "", "");
    let d = route(laya(0.4, 0.3), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert_eq!(d.model, "gpt-5.6-sol");
    assert!(d.ladder.unwrap().hedged);
    // Close to a tier boundary (simple_below 0.2 by default) hedges too.
    let near = route(laya(0.22, 0.9), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert_eq!(near.ladder.unwrap().tier, "normal");
    assert_eq!(near.model, "gpt-5.6-sol");
}

#[test]
fn the_hedge_is_config_driven() {
    let (_d, cfg, reg) = setup(&example(), "tier_hedge = \"off\"", "");
    let d = route(laya(0.4, 0.3), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert_eq!(d.model, "sonnet");
    let (_d, cfg, reg) = setup(&example(), "hedge_confidence_below = 0.2", "");
    let d = route(laya(0.4, 0.3), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert_eq!(d.model, "sonnet", "0.3 is above a 0.2 threshold");
    let (_d, cfg, reg) = setup(&example(), "tier_hedge = \"top2\"", "");
    assert_eq!(cfg.inner.routing.tier_hedge, TierHedge::Top2);
    // Simple at 0.19 is a hair under simple_below: top-2 takes normal's pick.
    let d = route(laya(0.19, 0.9), "fix the crash", &cfg, &reg, Mode::Balanced);
    assert_eq!(
        (d.model.as_str(), d.ladder.unwrap().tier),
        ("sonnet", "normal".to_string())
    );
}

#[test]
fn mode_fast_takes_the_cheapest_rung() {
    let (_d, cfg, reg) = setup(&example(), "", "");
    let d = route(laya(0.4, 0.3), "fix the crash", &cfg, &reg, Mode::Fast);
    assert_eq!(d.model, "sonnet", "no hedge up in fast mode");
    assert_eq!(d.ladder.unwrap().rung, 0);
}

#[test]
fn simple_below_steers_the_table() {
    let prompt = "fix the crash";
    let (_d, cfg, reg) = setup(&example(), "", "simple_below = 0.1");
    let a = route(laya(0.15, 0.95), prompt, &cfg, &reg, Mode::Balanced);
    assert_eq!(a.ladder.as_ref().unwrap().tier, "normal");
    let (_d, cfg, reg) = setup(&example(), "", "simple_below = 0.3");
    let b = route(laya(0.15, 0.95), prompt, &cfg, &reg, Mode::Balanced);
    assert_eq!(
        (b.ladder.unwrap().tier, b.model.as_str()),
        ("simple".to_string(), "haiku")
    );
}

#[test]
fn complex_forces_demanding_only_on_the_keywords_backend() {
    let (_d, cfg, reg) = setup(&example(), "", "");
    let prompt = "this is a complex bug";
    let k = route(keywords(), prompt, &cfg, &reg, Mode::Balanced);
    assert_eq!(k.ladder.unwrap().tier, "demanding");
    let l = route(laya(0.4, 0.95), prompt, &cfg, &reg, Mode::Balanced);
    assert_eq!(
        l.ladder.unwrap().tier,
        "normal",
        "Laya's difficulty decides, not the word"
    );
}

#[test]
fn a_pinned_model_skips_the_table() {
    let (_d, cfg, reg) = setup(&example(), "", "");
    let d = router::route_with(
        laya(0.4, 0.9),
        "fix the crash",
        &cfg,
        &reg,
        Mode::Balanced,
        None,
        Some("haiku".into()),
    )
    .unwrap();
    assert_eq!(d.model, "haiku");
    assert!(d.ladder.is_none());
}

#[test]
fn a_retry_steps_up_and_stops_at_the_top() {
    let (_d, cfg, reg) = setup(&example(), "max_escalations = 5", "");
    let step = |above: &str, n| {
        router::route_summarized_steered(
            laya(0.4, 0.9),
            "fix the crash",
            &cfg,
            &reg,
            Mode::Balanced,
            None,
            None,
            &table::Steer {
                above: Some(above.into()),
                escalations: n,
            },
        )
    };
    let (d, summary) = step("sonnet", 1).unwrap();
    assert_eq!((d.model.as_str(), d.escalation), ("gpt-5.6-sol", 1));
    assert!(summary.contains("escalated"), "{summary}");
    assert_eq!(step("gpt-5.6-sol", 2).unwrap().0.model, "opus");
    assert!(step("opus", 3).is_err());
}

#[test]
fn a_table_naming_an_unknown_model_is_refused_at_load() {
    let bad = vec![entry("normal", &["sonnet", "no-such-model"])];
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        include_str!("../config/nectra.toml"),
    )
    .unwrap();
    fs::write(
        dir.path().join("models.toml"),
        table::splice(include_str!("../config/models.toml"), &bad).unwrap(),
    )
    .unwrap();
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let err = Registry::load(&cfg).unwrap_err();
    assert!(format!("{err:#}").contains("no-such-model"), "{err:#}");
}

#[test]
fn the_documented_example_imports_and_routes() {
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
    let json = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/examples/tier-table.example.json");
    let nectra = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
            .arg("--config")
            .arg(dir.path().join("config.toml"))
            .args(args)
            .output()
            .unwrap()
    };
    let before = fs::read_to_string(dir.path().join("models.toml")).unwrap();
    let dry = nectra(&[
        "registry",
        "table",
        "import",
        "--dry-run",
        json.to_str().unwrap(),
    ]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("models.toml")).unwrap(),
        before
    );
    for _ in 0..2 {
        let out = nectra(&["registry", "table", "import", json.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let after = fs::read_to_string(dir.path().join("models.toml")).unwrap();
    assert!(after.starts_with(&before), "the models are untouched");
    assert_eq!(
        after.matches("[[table]]").count(),
        3,
        "a second import replaces the first"
    );
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    assert_eq!(Registry::load(&cfg).unwrap().table.len(), 3);
    let show = nectra(&["registry", "table", "show"]);
    assert!(String::from_utf8_lossy(&show.stdout).contains("debugging x normal"));
    // A table naming an unknown model is refused and leaves the file alone.
    let bad = dir.path().join("bad.json");
    fs::write(
        &bad,
        fs::read_to_string(&json)
            .unwrap()
            .replace("\"opus\"", "\"ghost\""),
    )
    .unwrap();
    let out = nectra(&["registry", "table", "import", bad.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ghost"));
    assert_eq!(
        fs::read_to_string(dir.path().join("models.toml")).unwrap(),
        after
    );
}
