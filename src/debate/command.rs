//! User-facing debate entry points and a separate, versioned audit journal.
use super::{runner, select, Eligibility};
use crate::{
    cli::{Cli, Mode, ProviderArg},
    config::Config,
    debate::plain::{self, PlainBackend},
    executor::TurnCancellation,
    limits::Cooldowns,
    models::Registry,
    router,
    session::Session,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    Start {
        version: u32,
        prompt: &'a str,
        participants: Vec<runner::Participant>,
    },
    Phase {
        record: runner::PhaseRecord,
    },
    Complete {
        result: &'a runner::DebateResult,
    },
    Failed {
        error: String,
    },
}
struct Journal(File);
impl Journal {
    fn append(&mut self, event: &Event<'_>) -> Result<()> {
        serde_json::to_writer(&mut self.0, event)?;
        self.0.write_all(b"\n")?;
        self.0.sync_data().context("saving debate audit")
    }
}

pub struct Options {
    pub mode: Mode,
    pub provider: Option<ProviderArg>,
    pub model: Option<String>,
    pub cooldown_state: Option<std::path::PathBuf>,
}

pub fn turn(
    session: &mut Session,
    cfg: &Config,
    registry: &Registry,
    prompt: &str,
    options: Options,
    cooldowns: &mut Cooldowns,
    output: &mut dyn Write,
) -> Result<()> {
    if cfg.skip_permissions {
        bail!("debate does not accept --dangerously-skip-permissions");
    }
    let base = router::route(
        prompt,
        cfg,
        registry,
        options.mode,
        options.provider,
        options.model.clone(),
    )?;
    let backend = PlainBackend::new(cfg);
    let selected = select(&base.candidates, registry, &Eligibility {
        available: &|r| plain::supports(r),
        // A tool-less participant has nothing to deny, so capability and
        // availability are the same question here.
        read_only_capable: &|r| plain::supports(r),
        cooling_down: &|m,p| cooldowns.remaining(m,p).is_some(),
        user_constrained: options.provider.is_some() || options.model.is_some(),
    }).context("no tool-less debate participant satisfies your routing constraints; Codex is not yet verified for the tool-less adapter; no tool-bearing fallback was attempted")?;
    let (context, omitted) = session.prompt(prompt, runner::CONTEXT_BUDGET)?;
    writeln!(
        output,
        "Debate: {}{}; {} logical calls, with variable provider/tool cost.",
        selected.primary.id,
        selected
            .opponent
            .map(|p| format!(" + {}", p.id))
            .unwrap_or_default(),
        if selected.opponent.is_some() { 5 } else { 1 }
    )?;
    if omitted > 0 {
        writeln!(
            output,
            "Omitted {omitted} older turns to fit the debate context budget."
        )?;
    }
    writeln!(
        output,
        "Participants run with no tools; Nectra supplies the evidence."
    )?;
    output.flush()?;
    let cancellation = TurnCancellation::install()?;
    if cancellation.token().is_cancelled() {
        bail!("Debate cancelled; no provider was contacted.");
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let audit = session.path.with_extension(format!("debate-{stamp}.jsonl"));
    let mut journal = Journal(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&audit)?,
    );
    let mut participants = vec![runner::Participant::from(selected.primary)];
    if let Some(p) = selected.opponent {
        participants.push(runner::Participant::from(p));
    }
    journal.append(&Event::Start {
        version: 1,
        prompt,
        participants,
    })?;
    writeln!(output, "Audit: {}", audit.display())?;
    session.begin(prompt, "debate")?;
    let result = runner::run(
        &selected,
        &base,
        &context,
        &backend,
        cancellation.token(),
        |record| {
            if let Some(limit) = &record.rate_limit {
                cooldowns.record(
                    &record.model,
                    &record.provider,
                    &crate::limits::RateLimited {
                        message: record.error.clone().unwrap_or_default(),
                        provider_wide: limit.provider_wide,
                        retry_after: std::time::Duration::from_secs(limit.retry_after_seconds),
                        safe_to_retry: false,
                    },
                );
                if let Some(path) = &options.cooldown_state {
                    let _ = cooldowns.persist(path);
                }
            }
            writeln!(
                output,
                "{:?}: {} {}",
                record.phase,
                record.model,
                if record.error.is_some() {
                    "failed"
                } else {
                    "finished"
                }
            )?;
            output.flush()?;
            journal.append(&Event::Phase { record })
        },
    );
    let result = result.and_then(|result| {
        if cancellation.token().is_cancelled() {
            bail!("Debate cancelled; no completed answer was saved.");
        }
        journal.append(&Event::Complete { result: &result })?;
        Ok(result)
    });
    match result {
        Ok(result) => {
            let answer = result.rendered();
            session.complete(&answer)?;
            writeln!(output, "\n{answer}")?;
            Ok(())
        }
        Err(error) => {
            session.fail(&format!("{error:#}"))?;
            journal.append(&Event::Failed {
                error: format!("{error:#}"),
            })?;
            Err(error)
        }
    }
}

/// Launch the provider and read the tool inventory it actually reports.
///
/// Costs one small model call, which is why it sits behind `--live`.
fn live_tool_inventory(cfg: &Config, probe: &crate::router::Decision) -> Result<Vec<String>> {
    let args = plain::provider_args(probe)?;
    let mut child = std::process::Command::new(&cfg.inner.claude.command)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("launching the provider for a live tool-inventory check")?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().context("provider stdin")?;
        stdin.write_all(b"Reply with exactly: OK")?;
    }
    // wait_with_output drains both pipes while waiting, so this cannot deadlock.
    let out = child
        .wait_with_output()
        .context("waiting for the live tool-inventory check")?;
    if !out.status.success() {
        bail!(
            "live check failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if event["type"] == "system" && event["subtype"] == "init" {
            return Ok(event["tools"]
                .as_array()
                .map(|t| {
                    t.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default());
        }
    }
    bail!("provider emitted no init event; cannot verify the tool inventory")
}

pub fn one_shot(
    cli: &Cli,
    cfg: &Config,
    registry: &Registry,
    prompt: &str,
    path: Option<&Path>,
) -> Result<i32> {
    if let Some(crate::cli::Commands::Debate {
        check_isolation: true,
        live,
        ..
    }) = &cli.command
    {
        let live = *live;
        if !prompt.trim().is_empty() || cli.stdin || cli.dry_run {
            bail!("--check-isolation takes no prompt, --stdin or --dry-run");
        }
        // Isolation is no longer a container question: participants are
        // launched with no tools, so the check is that the arguments really
        // expose none. It makes no model calls and needs no engine.
        let probe = crate::router::Decision {
            task: "general".into(),
            provider: "anthropic".into(),
            runtime: "claude".into(),
            model: "sonnet".into(),
            effort: String::new(),
            mode: crate::cli::Mode::Balanced,
            score: 0.0,
            score_margin: None,
            ambiguous: false,
            overridden: false,
            reason: String::new(),
            candidates: vec![],
        };
        let args = plain::provider_args(&probe)?;
        let tools = args
            .iter()
            .position(|a| a == "--tools")
            .context("launch arguments must constrain the tool set")?;
        if !args[tools + 1].is_empty() {
            bail!("tool set is not empty: {:?}", args[tools + 1]);
        }
        println!(
            "claude: launch arguments expose an empty tool set (provider behaviour NOT verified)"
        );
        if live {
            // Arguments only prove the argument builder. Ask the real CLI what
            // it actually exposed, which is the standard of evidence Codex's
            // fixtures established here.
            let inventory = live_tool_inventory(cfg, &probe)?;
            if !inventory.is_empty() {
                bail!("claude reported a non-empty tool inventory: {inventory:?}");
            }
            println!("claude: live init event reported an empty tool inventory");
        }
        println!("ollama: adapter executes no tools by construction");
        println!(
            "Tool-less check passed. {} No container required.",
            if live {
                "One small model call was made to read the live tool inventory."
            } else {
                "No model calls made; pass --live to verify against the real CLI."
            }
        );
        return Ok(0);
    }
    if cli.dry_run {
        // No Docker or provider execution; describe constrained routing only.
        let decision = router::route(
            prompt,
            cfg,
            registry,
            cli.mode.unwrap_or(cfg.inner.routing.default_mode),
            cli.provider,
            cli.model.clone(),
        )?;
        println!("{}", serde_json::to_string_pretty(&decision)?);
        return Ok(0);
    }
    let cwd = std::env::current_dir()?;
    let mut session = match path {
        Some(path) => Session::create(path, &cwd)?,
        None => Session::create_default(&cwd)?,
    };
    let mut cooldowns = Cooldowns::state_path()
        .map(|p| Cooldowns::load(&p))
        .unwrap_or_default();
    let mut output = crate::terminal::stdout();
    writeln!(output, "Session: {}", session.path.display())?;
    let result = turn(
        &mut session,
        cfg,
        registry,
        prompt,
        Options {
            mode: cli.mode.unwrap_or(cfg.inner.routing.default_mode),
            provider: cli.provider,
            model: cli.model.clone(),
            cooldown_state: Cooldowns::state_path(),
        },
        &mut cooldowns,
        &mut output,
    );
    result?;
    Ok(0)
}
