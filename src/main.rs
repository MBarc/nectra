use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser};
use nectra::{
    chat,
    cli::{ClassifierCommands, Cli, Commands},
    config, executor, models, router,
};
use serde::{Deserialize, Serialize};
use std::io::{IsTerminal, Read, Write};

fn main() {
    match app() {
        Ok(code) => nectra::laya::exit(code),
        Err(error) => {
            nectra::terminal::error(&format!("nectra: {error:#}"));
            nectra::laya::exit(1);
        }
    }
}
fn app() -> Result<i32> {
    #[cfg(debug_assertions)]
    match std::env::args().nth(1).as_deref() {
        Some("__terminal-demo") => return nectra::executor::native::demo(),
        Some("__terminal-fixture") => return nectra::executor::native::fixture(),
        Some("__terminal-sleeper") => {
            std::thread::sleep(std::time::Duration::from_secs(60));
            return Ok(0);
        }
        _ => {}
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("__terminal-hook")) {
        return nectra::executor::native::hook_main();
    }
    let cli = Cli::parse();
    nectra::terminal::set_mode(cli.color);
    let theme_path = nectra::terminal::theme_path(cli.config.as_deref())?;
    if let Some(Commands::Theme { name }) = &cli.command {
        if let Some(theme) = name {
            nectra::terminal::save_theme(&theme_path, *theme)?;
            nectra::terminal::set_theme(*theme);
        } else {
            nectra::terminal::set_theme(nectra::terminal::load_theme(&theme_path)?);
        }
        nectra::terminal::line(&format!(
            "Theme: {}\nAvailable themes: default, hacker, ice-blue\nPreferences: {}",
            nectra::terminal::theme().name(),
            theme_path.display()
        ))?;
        return Ok(0);
    }
    nectra::terminal::set_theme(match cli.theme {
        Some(theme) => theme,
        None => nectra::terminal::load_theme(&theme_path)?,
    });
    if let Some(Commands::Classifier {
        backend,
        command,
        model_dir,
        verify_only,
    }) = &cli.command
    {
        match (backend, command) {
            (_, Some(ClassifierCommands::Fetch { dir })) => {
                nectra::laya::fetch_to(dir, &mut nectra::terminal::stdout())?
            }
            (Some(backend), None) => nectra::setup::classifier_setup(
                cli.config.as_deref(),
                *backend,
                model_dir.as_deref(),
                *verify_only,
            )?,
            (None, None) => unreachable!("clap requires a backend"),
        }
        return Ok(0);
    }
    if let Some(Commands::Ollama {
        url,
        models,
        policy,
    }) = &cli.command
    {
        nectra::setup::ollama_setup(cli.config.as_deref(), url, models, *policy)?;
        return Ok(0);
    }
    if cli.command.is_none()
        && cli.prompt.is_empty()
        && !cli.stdin
        && !std::io::stdin().is_terminal()
    {
        Cli::command().print_help()?;
        println!();
        return Ok(0);
    }
    if let Some(Commands::Setup {
        providers,
        if_needed,
    }) = &cli.command
    {
        nectra::setup::run(cli.config.as_deref(), providers.as_deref(), *if_needed)?;
        return Ok(0);
    }
    if let Some(Commands::Provider { command }) = &cli.command {
        nectra::setup::manage(cli.config.as_deref(), command)?;
        return Ok(0);
    }
    if let Some(Commands::Usage {
        days,
        json,
        session,
        direct,
        baseline,
    }) = &cli.command
    {
        let mut logs = nectra::usage::Logs::default_paths()?;
        if let Some(path) = session {
            logs.nectra_sessions = path.clone();
        }
        let usage = nectra::usage::collect(&logs, *days, *direct)?;
        if *json {
            println!("{}", serde_json::to_string_pretty(&usage)?);
        } else {
            print!("{}", nectra::usage::render(&usage, baseline));
        }
        return Ok(0);
    }
    let chatting = matches!(cli.command, Some(Commands::Chat { .. }))
        || (cli.command.is_none() && cli.prompt.is_empty() && !cli.stdin);
    // The built-in defaults assume providers this user may not have, so a
    // first interactive chat sets up instead of failing on them later.
    if chatting
        && cli.config.is_none()
        && std::io::stdin().is_terminal()
        && dirs::config_dir().is_some_and(|d| !d.join("nectra/config.toml").exists())
    {
        nectra::setup::run(None, None, false)?;
    }
    if let Some(Commands::Skills { link }) = &cli.command {
        let home = dirs::home_dir().context("no home directory")?;
        if *link {
            let linked = nectra::skills::link(&home)?;
            for line in &linked {
                println!("Linked {line}");
            }
            println!(
                "{} skill link(s) created. Skills written for one CLI's tools may still need edits to work in another.",
                linked.len()
            );
        }
        let project = std::env::current_dir()?;
        let mut scopes = vec![("user", nectra::skills::scan(&home)?)];
        if project != home {
            scopes.push(("project", nectra::skills::scan(&project)?));
        }
        print!("{}", nectra::skills::render(&scopes));
        return Ok(0);
    }
    let mut cfg = config::Config::load(cli.config.as_deref())?;
    if let Some(Commands::Mcp { sync }) = &cli.command {
        let home = dirs::home_dir().context("no home directory")?;
        if *sync {
            let added = nectra::mcp::sync(&home, &cfg)?;
            for line in &added {
                println!("Added {line}");
            }
            println!(
                "{} server(s) added. Servers that need a sign-in ask for it in each CLI.",
                added.len()
            );
        }
        print!("{}", nectra::mcp::render(&nectra::mcp::scan(&home)?));
        if !*sync {
            println!("shared: Nectra adds it when it launches that CLI. Gemini can't take servers at launch; `nectra mcp --sync` adds them to every CLI's own config.");
        }
        return Ok(0);
    }
    if cfg.embedded {
        nectra::terminal::error(
            "nectra: no setup found, so the built-in defaults apply; run `nectra setup` to choose your providers",
        );
    }
    cfg.skip_permissions = cli.dangerously_skip_permissions;
    cfg.mcp_home = dirs::home_dir();
    cfg.provider_view = cli.provider_view;
    match &cli.command {
        Some(Commands::Config { print }) => {
            if *print {
                println!("{}", cfg.pretty_toml()?);
            } else {
                println!("{}", cfg.path.display());
            }
            return Ok(0);
        }
        Some(Commands::Doctor) => {
            executor::doctor(&cfg)?;
            return Ok(0);
        }
        Some(Commands::Eval {
            corpus,
            min_tier_accuracy,
        }) => return evaluate(corpus, *min_tier_accuracy, &cfg),
        _ => {}
    }
    // `run --events jsonl` reports a refused index as its result, like any
    // other setup failure; everything else stops here.
    let knowledge = match cli
        .knowledge
        .as_ref()
        .or(cfg.inner.knowledge.index.as_ref())
    {
        Some(index) => nectra::knowledge::load(index).map(|block| cfg.knowledge = Some(block)),
        None => Ok(()),
    };
    let headless = matches!(&cli.command, Some(Commands::Run { headless, .. })
        if headless.events.is_some() && !cli.dry_run);
    let knowledge = match headless {
        true => knowledge,
        false => Ok(knowledge?),
    };
    let registry = models::Registry::load(&cfg)?;
    if cli.continue_latest && !chatting {
        bail!("--continue applies to chat only: run `nectra -c` or `nectra chat -c`");
    }
    if chatting {
        return chat::run(&cli, &cfg, &registry);
    }
    if matches!(cli.command, Some(Commands::Models)) {
        let mut output = nectra::terminal::stdout();
        writeln!(
            output,
            "{:<10} {:<18} {:<8} {:<8} {:<7} PREFERS",
            "PROVIDER", "MODEL", "RUNTIME", "STATUS", "EFFORT"
        )?;
        for m in &registry.models {
            writeln!(output, "{}", nectra::models::roster_line(m))?;
        }
        writeln!(
            output,
            "
Model access is verified on first use. Run `nectra doctor` to check provider CLIs."
        )?;
        if let Some(notice) = nectra::models::legacy_registry_notice(&registry) {
            writeln!(
                output,
                "
{}
Registry: {}",
                notice,
                cfg.registry_path().display()
            )?;
        }
        return Ok(0);
    }
    let (words, route_only) = match &cli.command {
        Some(Commands::Route { prompt }) => (prompt, true),
        Some(Commands::Run { prompt, .. }) => (prompt, false),
        Some(Commands::Debate { prompt, .. }) => (prompt, false),
        _ => (&cli.prompt, false),
    };
    if let Some(Commands::Run { prompt, headless }) = &cli.command {
        // Headless reads its own prompt, inside its deadline.
        if headless.events.is_some() && !cli.dry_run {
            return nectra::headless::run(&cli, &cfg, &registry, prompt, headless, knowledge);
        }
    }
    let prompt = if cli.stdin {
        if !words.is_empty() {
            bail!("use either --stdin or positional prompt text");
        }
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading prompt from stdin")?;
        text
    } else {
        words.join(" ")
    };
    if let Some(Commands::Debate { session, .. }) = &cli.command {
        return nectra::debate::command::one_shot(
            &cli,
            &cfg,
            &registry,
            &prompt,
            session.as_deref(),
        );
    }
    let decision = router::route(
        &prompt,
        &cfg,
        &registry,
        cli.mode.unwrap_or(cfg.inner.routing.default_mode),
        cli.provider,
        cli.model,
    )?;
    if cli.explain {
        eprintln!(
            "Nectra -> {} / {} task={} score={:.3}",
            decision.runtime, decision.model, decision.task, decision.score
        );
        eprintln!("Reason: {}", decision.reason);
    }
    if route_only || cli.dry_run {
        println!("{}", serde_json::to_string_pretty(&decision)?);
        return Ok(0);
    }
    executor::execute(&decision, &prompt, &cfg)
}
/// One labelled prompt. `task` is scored when present; `tier` (simple,
/// normal or demanding) scores difficulty and demanding routing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalCase {
    prompt: String,
    task: Option<String>,
    tier: Option<String>,
    /// Where the prompt came from; informational.
    #[serde(default)]
    #[allow(dead_code)]
    source: Option<String>,
}
fn parse_cases(text: &str) -> Result<Vec<EvalCase>> {
    let cases: Vec<EvalCase> = serde_json::from_str(text)?;
    if cases.is_empty() {
        bail!("evaluation corpus is empty");
    }
    if let Some(i) = cases
        .iter()
        .position(|c| c.task.is_none() && c.tier.is_none())
    {
        bail!("case {i} has neither `task` nor `tier`");
    }
    if let Some(t) = cases
        .iter()
        .filter_map(|c| c.tier.as_deref())
        .find(|t| !["simple", "normal", "demanding"].contains(t))
    {
        bail!("unknown tier {t:?}");
    }
    Ok(cases)
}
#[derive(Serialize)]
struct Failure {
    prompt: String,
    expected: String,
    actual: String,
}
/// Gold `demanding` against a prediction.
#[derive(Serialize, Default, Debug, PartialEq)]
struct Confusion {
    tp: usize,
    fp: usize,
    #[serde(rename = "fn")]
    fn_: usize,
}
impl Confusion {
    fn add(&mut self, gold: bool, predicted: bool) {
        match (gold, predicted) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fn_ += 1,
            (false, false) => {}
        }
    }
}
#[derive(Serialize)]
struct EvalReport {
    total: usize,
    passed: usize,
    accuracy: f64,
    /// Prompts the classifier marked demanding (Demanding tier or role).
    demanding_tier: usize,
    /// Prompts the router sent to a model that claims the demanding role:
    /// how often the strongest model actually runs.
    demanding_model: usize,
    /// Prompts the model did not answer (deadline, load failure), scored
    /// by the keyword classifier instead.
    keyword_fallbacks: usize,
    failures: Vec<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<TierReport>,
}
#[derive(Serialize, Default)]
struct TierReport {
    total: usize,
    passed: usize,
    accuracy: f64,
    /// Against gold `demanding`: the classifier's demanding tier or role,
    demanding_tier: Confusion,
    /// and the router's pick of a demanding model.
    demanding_model: Confusion,
    failures: Vec<Failure>,
}
fn evaluate(
    path: &std::path::Path,
    min_tier_accuracy: Option<f64>,
    cfg: &config::Config,
) -> Result<i32> {
    let cases = parse_cases(&std::fs::read_to_string(path).context("reading evaluation corpus")?)?;
    let registry = models::Registry::load(cfg)?;
    let demanding_ids: Vec<&str> = registry
        .models
        .iter()
        .filter(|m| m.prefers.iter().any(|r| r == "demanding"))
        .map(|m| m.id.as_str())
        .collect();
    let mut failures = Vec::new();
    let (mut total, mut tiers) = (0, None::<TierReport>);
    let (mut demanding_tier, mut demanding_model, mut keyword_fallbacks) = (0, 0, 0);
    for c in cases {
        // Every role offered, and no file lookups: the prompt alone decides.
        let got = nectra::jev::classify_turn_in(&c.prompt, None, cfg, &nectra::policy::ROLES, None);
        // Only the keyword classifier leaves the tier unset.
        keyword_fallbacks += usize::from(got.tier.is_none());
        let actual_tier = got
            .tier
            .map_or("none".to_string(), |t| format!("{t:?}").to_lowercase());
        let marked = actual_tier == "demanding" || got.role.as_deref() == Some("demanding");
        let task = got.task.to_string();
        let model = router::route_with(
            got,
            &c.prompt,
            cfg,
            &registry,
            nectra::cli::Mode::Balanced,
            None,
            None,
        )?
        .model;
        let picked = demanding_ids.contains(&model.as_str());
        demanding_tier += usize::from(marked);
        demanding_model += usize::from(picked);
        if let Some(expected) = c.task {
            total += 1;
            if task != expected {
                failures.push(Failure {
                    prompt: c.prompt.clone(),
                    expected,
                    actual: task,
                });
            }
        }
        if let Some(tier) = c.tier {
            let t = tiers.get_or_insert_with(TierReport::default);
            t.total += 1;
            t.demanding_tier.add(tier == "demanding", marked);
            t.demanding_model.add(tier == "demanding", picked);
            if actual_tier == tier {
                t.passed += 1;
            } else {
                t.failures.push(Failure {
                    prompt: c.prompt,
                    expected: tier,
                    actual: actual_tier,
                });
            }
        }
    }
    if let Some(t) = &mut tiers {
        t.accuracy = t.passed as f64 / t.total as f64;
    }
    let passed = total - failures.len();
    // Tier accuracy fails the run only against an explicit threshold.
    let tiers_ok = match (&tiers, min_tier_accuracy) {
        (Some(t), Some(min)) => t.accuracy >= min,
        _ => true,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&EvalReport {
            total,
            passed,
            accuracy: if total == 0 {
                0.0
            } else {
                passed as f64 / total as f64
            },
            demanding_tier,
            demanding_model,
            keyword_fallbacks,
            failures,
            tier: tiers,
        })?
    );
    Ok(if passed == total && tiers_ok { 0 } else { 1 })
}
#[cfg(test)]
mod eval_tests {
    use super::*;

    #[test]
    fn confusion_counts_each_cell() {
        let mut c = Confusion::default();
        for (gold, got) in [
            (true, true),
            (true, true),
            (false, true),
            (true, false),
            (false, false),
        ] {
            c.add(gold, got);
        }
        assert_eq!(
            c,
            Confusion {
                tp: 2,
                fp: 1,
                fn_: 1
            }
        );
    }

    #[test]
    fn cases_need_a_label() {
        assert!(parse_cases(r#"[{"prompt": "hi", "task": "general"}]"#).is_ok());
        assert!(parse_cases(r#"[{"prompt": "hi", "tier": "simple"}]"#).is_ok());
        let err = parse_cases(r#"[{"prompt": "hi"}]"#)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("neither"), "{err}");
        assert!(parse_cases(r#"[{"prompt": "hi", "tier": "hard"}]"#).is_err());
        assert!(parse_cases("[]").is_err());
    }
}
