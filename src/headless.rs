//! `nectra run --events jsonl`: one routed task, unattended, reported as a
//! versioned stream of JSON lines on stdout. The contract is docs/headless.md.
use crate::{
    cli::Cli,
    config::Config,
    executor::{self, CancellationToken, ProviderSession},
    models::Registry,
    router::{self, Decision},
    usage::{self, Tokens},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Bumped on any incompatible change to an event's shape.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Events {
    Jsonl,
}

#[derive(Debug, Clone, Default, clap::Args)]
pub struct Args {
    /// Run unattended and report as JSON lines on stdout (docs/headless.md).
    #[arg(long, value_enum)]
    pub events: Option<Events>,
    /// Continue a session from an earlier result event's `session_id`.
    #[arg(long, requires = "events", conflicts_with = "no_session")]
    pub resume: Option<String>,
    /// Stop the task once its API-equivalent cost passes this many USD.
    #[arg(long, requires = "events", value_parser = positive)]
    pub max_cost_usd: Option<f64>,
    /// Stop the task after this long: seconds, or with an s, m or h suffix.
    #[arg(long, requires = "events", value_parser = duration)]
    pub deadline: Option<Duration>,
    /// Write no session record or cooldown state; the task can't be resumed.
    #[arg(long, requires = "events")]
    pub no_session: bool,
    /// Keep session records and cooldowns here instead of the user data directory.
    #[arg(long, requires = "events")]
    pub state_dir: Option<PathBuf>,
    /// Give the agent exactly these MCP servers (standard `mcpServers` JSON).
    #[arg(long, requires = "events")]
    pub mcp_config: Option<PathBuf>,
    /// Don't share MCP servers between the provider CLIs' own configs.
    #[arg(long, requires = "events")]
    pub no_mcp_share: bool,
}

fn positive(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err("expected a positive number".into()),
    }
}
fn duration(s: &str) -> Result<Duration, String> {
    let (n, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err("expected seconds, or a number with s, m or h".into()),
    };
    match n.parse::<u64>() {
        Ok(n) if n > 0 => Ok(Duration::from_secs(n * scale)),
        _ => Err("expected a positive whole number".into()),
    }
}

/// The task's cost passed `--max-cost-usd`.
#[derive(Debug)]
struct CostLimit(f64);
impl std::fmt::Display for CostLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cost limit of ${} reached; the agent was stopped",
            self.0
        )
    }
}
impl std::error::Error for CostLimit {}

/// What `--resume` needs to continue a task: kept as `<state>/runs/<id>.json`.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    v: u32,
    id: String,
    model: String,
    provider: String,
    runtime: String,
    provider_session_id: String,
    cwd: PathBuf,
}
fn record_path(dir: &Path, id: &str) -> Result<PathBuf> {
    // The id comes from the caller and names a file: no path tricks.
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        bail!("not a Nectra session id: {id}");
    }
    Ok(dir.join("runs").join(format!("{id}.json")))
}

/// One model call (Claude), turn (Codex) or per-model total (Gemini).
struct Call {
    routed: String,
    provider: String,
    model: String,
    tokens: Tokens,
}

/// Token usage as the provider streams it. Claude's result `modelUsage` adds up
/// the whole session on a resumed one, so Claude is counted from each
/// message's own usage instead.
#[derive(Default)]
struct Ledger {
    calls: Vec<Call>,
    claude_messages: HashMap<String, usize>,
    current: Option<usize>,
}
fn tokens(u: &Value, input: &str, cache_read: &str, cache_write: &str, output: &str) -> Tokens {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Tokens {
        input: n(input),
        cache_read: n(cache_read),
        cache_write: n(cache_write),
        output: n(output),
    }
}
impl Ledger {
    fn observe(&mut self, d: &Decision, event: &Value) {
        let call = |model: &str, tokens| Call {
            routed: d.model.clone(),
            provider: d.provider.clone(),
            model: model.to_string(),
            tokens,
        };
        let claude = |u: &Value| {
            tokens(
                u,
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
                "output_tokens",
            )
        };
        match (d.runtime.as_str(), event["type"].as_str().unwrap_or("")) {
            ("claude", "stream_event") => {
                let e = &event["event"];
                match e["type"].as_str() {
                    Some("message_start") => {
                        let m = &e["message"];
                        let model = m["model"].as_str().unwrap_or(&d.model);
                        let id = m["id"].as_str().unwrap_or_default().to_string();
                        let index = *self.claude_messages.entry(id).or_insert_with(|| {
                            self.calls.push(call(model, Tokens::default()));
                            self.calls.len() - 1
                        });
                        self.calls[index].tokens = claude(&m["usage"]);
                        self.current = Some(index);
                    }
                    // Counts in a delta are running totals for the message.
                    Some("message_delta") => {
                        if let Some(c) = self.current.and_then(|i| self.calls.get_mut(i)) {
                            let (u, t) = (&e["usage"], &mut c.tokens);
                            for (field, key) in [
                                (&mut t.input, "input_tokens"),
                                (&mut t.cache_read, "cache_read_input_tokens"),
                                (&mut t.cache_write, "cache_creation_input_tokens"),
                                (&mut t.output, "output_tokens"),
                            ] {
                                if let Some(n) = u[key].as_u64() {
                                    *field = n;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            ("codex", "turn.completed") => {
                let u = &event["usage"];
                let mut t = tokens(
                    u,
                    "input_tokens",
                    "cached_input_tokens",
                    "cache_write_input_tokens",
                    "output_tokens",
                );
                // Codex counts cached input inside input_tokens.
                t.input = t.input.saturating_sub(t.cache_read);
                self.calls.push(call(&d.model, t));
            }
            ("gemini", "result") => {
                for (model, u) in event["stats"]["models"].as_object().into_iter().flatten() {
                    let t = tokens(u, "input", "cached", "", "output_tokens");
                    self.calls.push(call(model, t));
                }
            }
            _ => {}
        }
    }
    /// Per (routed model, reporting model), in first-seen order.
    fn models(&self) -> Vec<(&Call, Tokens)> {
        let mut out: Vec<(&Call, Tokens)> = vec![];
        for c in &self.calls {
            match out
                .iter_mut()
                .find(|(o, _)| o.routed == c.routed && o.model == c.model)
            {
                Some((_, t)) => t.add(c.tokens),
                None => out.push((c, c.tokens)),
            }
        }
        out
    }
    /// The priced part of the cost, and whether every call was priced.
    fn cost(&self) -> (f64, bool) {
        self.models()
            .iter()
            .fold((0.0, true), |(sum, all), (c, t)| match price(c) {
                Some(p) => (sum + t.cost(p), all),
                None => (sum, all && *t == Tokens::default()),
            })
    }
}
fn price(c: &Call) -> Option<[f64; 4]> {
    usage::price(&c.model).or_else(|| usage::price(&c.routed))
}

fn emit(kind: &str, mut fields: Value) -> Result<()> {
    fields["v"] = json!(VERSION);
    fields["type"] = json!(kind);
    let mut out = std::io::stdout().lock();
    writeln!(out, "{fields}")?;
    out.flush()?;
    Ok(())
}

/// The tool start/end events a provider event carries, as (phase, name, id).
fn tools(runtime: &str, event: &Value, names: &mut HashMap<String, String>) -> Vec<Value> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let mut found = vec![];
    match (runtime, event["type"].as_str().unwrap_or("")) {
        ("claude", kind @ ("assistant" | "user")) => {
            for block in event["message"]["content"].as_array().into_iter().flatten() {
                match (kind, block["type"].as_str()) {
                    ("assistant", Some("tool_use")) => {
                        names.insert(s(&block["id"]), s(&block["name"]));
                        found.push(json!({"phase":"start","name":block["name"],"id":block["id"]}));
                    }
                    ("user", Some("tool_result")) => {
                        let id = s(&block["tool_use_id"]);
                        found.push(json!({"phase":"end","name":names.get(&id),"id":id,
                            "is_error":block["is_error"].as_bool().unwrap_or(false)}));
                    }
                    _ => {}
                }
            }
        }
        ("codex", kind @ ("item.started" | "item.completed")) => {
            let item = &event["item"];
            if !matches!(
                item["type"].as_str(),
                Some("agent_message" | "reasoning") | None
            ) {
                let phase = if kind == "item.started" {
                    "start"
                } else {
                    "end"
                };
                found.push(json!({"phase":phase,"name":item["type"],"id":item["id"]}));
            }
        }
        ("gemini", "tool_use") => {
            names.insert(s(&event["tool_id"]), s(&event["tool_name"]));
            found.push(json!({"phase":"start","name":event["tool_name"],"id":event["tool_id"]}));
        }
        ("gemini", "tool_result") => {
            let id = s(&event["tool_id"]);
            found.push(json!({"phase":"end","name":names.get(&id),"id":id,
                "is_error":event["status"] == "error"}));
        }
        _ => {}
    }
    found
}

/// The error class of a failed attempt, and whether another model may take it.
fn classify(error: &anyhow::Error, cancel: &CancellationToken) -> (&'static str, bool) {
    if cancel.is_cancelled() {
        return ("cancelled", false);
    }
    if error.is::<CostLimit>() {
        return ("cost_limit", false);
    }
    if error.is::<executor::DeadlineExceeded>() {
        return ("deadline", false);
    }
    if let Some(e) = error.downcast_ref::<crate::access::ModelUnavailable>() {
        return ("model_unavailable", e.safe_to_retry);
    }
    if let Some(e) = error.downcast_ref::<executor::AuthenticationRequired>() {
        return ("auth", e.safe_to_retry);
    }
    if let Some(e) = error.downcast_ref::<crate::limits::RateLimited>() {
        return ("rate_limited", e.safe_to_retry);
    }
    ("agent_error", false)
}
/// Nectra's own failures exit 1, like any other Nectra error; the agent's 3.
fn exit_code(class: Option<&str>) -> i32 {
    match class {
        None => 0,
        Some("harness_error" | "no_model") => 1,
        Some("cost_limit" | "deadline") => 4,
        Some("cancelled") => 130,
        Some(_) => 3,
    }
}

/// On Unix, SIGTERM (systemd stopping the unit) cancels the turn like Ctrl+C,
/// which stops the provider's whole process group.
#[cfg(unix)]
fn cancel_on_sigterm(token: &CancellationToken) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static TERM: AtomicBool = AtomicBool::new(false);
    extern "C" fn handle(_: libc::c_int) {
        TERM.store(true, Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, handle as *const () as libc::sighandler_t);
    }
    let token = token.clone();
    std::thread::spawn(move || loop {
        if TERM.load(Ordering::SeqCst) {
            token.cancel();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    });
}

/// The session id to report: the resumed one, or a new record for this task.
fn remember(
    state: Option<&Path>,
    resumed: Option<&Record>,
    d: &Decision,
    session: &Option<ProviderSession>,
) -> Result<Option<String>> {
    if let Some(r) = resumed {
        return Ok(Some(r.id.clone()));
    }
    let (Some(state), Some(provider_session_id)) =
        (state, session.as_ref().and_then(|s| s.id.clone()))
    else {
        return Ok(None);
    };
    let record = Record {
        v: VERSION,
        id: executor::new_session_id(),
        model: d.model.clone(),
        provider: d.provider.clone(),
        runtime: d.runtime.clone(),
        provider_session_id,
        cwd: std::env::current_dir()?,
    };
    let path = record_path(state, &record.id)?;
    std::fs::create_dir_all(path.parent().context("no runs directory")?)?;
    std::fs::write(&path, serde_json::to_vec_pretty(&record)?)?;
    Ok(Some(record.id))
}

pub fn run(cli: &Cli, cfg: &Config, registry: &Registry, prompt: &str, args: &Args) -> Result<i32> {
    let started = Instant::now();
    let mut cfg = cfg.clone();
    if args.no_mcp_share {
        cfg.mcp_home = None;
    }
    let turn = executor::TurnCancellation::install()?;
    let cancel = turn.token();
    #[cfg(unix)]
    cancel_on_sigterm(cancel);
    let mut ledger = Ledger::default();
    let mut attempts: Vec<Value> = vec![];
    let mut session_id = None;
    let mut last: Option<Decision> = None;
    let outcome = (|| -> Result<std::result::Result<String, (&'static str, String)>> {
        let fail = |class, e: anyhow::Error| Ok(Err((class, format!("{e:#}"))));
        if prompt.trim().is_empty() {
            return fail("harness_error", anyhow::anyhow!("prompt cannot be empty"));
        }
        if let Some(path) = &args.mcp_config {
            let home = cfg.mcp_home.clone().or_else(dirs::home_dir);
            match crate::mcp::Explicit::load(path, home.as_deref()) {
                Ok(explicit) => cfg.mcp_config = Some(explicit),
                Err(e) => return fail("harness_error", e),
            }
        }
        let state = match (&args.state_dir, args.no_session) {
            (_, true) => None,
            (Some(dir), _) => Some(dir.clone()),
            (None, _) => Some(
                dirs::data_local_dir()
                    .context("no data directory")?
                    .join("nectra"),
            ),
        };
        let resumed: Option<Record> = match &args.resume {
            None => None,
            Some(id) => {
                let read = (|| -> Result<Record> {
                    let path = record_path(state.as_deref().context("no state directory")?, id)?;
                    let text = std::fs::read_to_string(&path)
                        .with_context(|| format!("no Nectra session {id} in {}", path.display()))?;
                    let record: Record = serde_json::from_str(&text)?;
                    if record.cwd != std::env::current_dir()? {
                        bail!(
                            "session {id} ran in {}; resume it from there",
                            record.cwd.display()
                        );
                    }
                    Ok(record)
                })();
                match read {
                    Ok(r) => Some(r),
                    Err(e) => return fail("harness_error", e),
                }
            }
        };
        let cooldown_path = state.as_ref().map(|d| d.join("cooldowns.json"));
        let mut cooldowns = cooldown_path
            .as_deref()
            .map(crate::limits::Cooldowns::load)
            .unwrap_or_default();
        let mut tried: Vec<String> = vec![];
        let mut names = HashMap::new();
        loop {
            let available = Registry {
                models: registry
                    .models
                    .iter()
                    .filter(|m| {
                        let runtime = m.runtime.as_str();
                        // Ollama runs no tools and reports no usage: not an agent.
                        runtime != "ollama"
                            && !tried.contains(&m.id)
                            && cooldowns.remaining(&m.id, m.provider.as_str()).is_none()
                            && cfg.mcp_config.as_ref().is_none_or(|e| e.supports(runtime))
                            && (args.max_cost_usd.is_none() || usage::price(&m.id).is_some())
                    })
                    .cloned()
                    .collect(),
            };
            let pinned = resumed
                .as_ref()
                .map(|r| r.model.clone())
                .or(cli.model.clone());
            let decision = match router::route(
                prompt,
                &cfg,
                &available,
                cli.mode.unwrap_or(cfg.inner.routing.default_mode),
                if resumed.is_some() {
                    None
                } else {
                    cli.provider
                },
                pinned.clone(),
            ) {
                Ok(d) => d,
                Err(e) => {
                    let e = match tried.last() {
                        Some(_) => e.context(format!("after trying {}", tried.join(", "))),
                        None => e,
                    };
                    return fail("no_model", e);
                }
            };
            tried.push(decision.model.clone());
            let resumable = ProviderSession::supported(&decision.runtime)
                && executor::can_resume(&decision.runtime, &cfg);
            let mut provider_session = match &resumed {
                Some(r) => Some(ProviderSession::resume(&r.provider_session_id)),
                None if state.is_some() && resumable => {
                    Some(ProviderSession::start(&decision.runtime))
                }
                None => None,
            };
            emit(
                "route",
                json!({
                    "attempt": tried.len(),
                    "model": decision.model,
                    "provider": decision.provider,
                    "runtime": decision.runtime,
                    "task": decision.task,
                    "mode": decision.mode,
                    "score": decision.score,
                    "reason": decision.reason,
                    "resumed": resumed.is_some(),
                }),
            )?;
            let remaining = match args.deadline.map(|d| d.checked_sub(started.elapsed())) {
                Some(None) => {
                    last = Some(decision);
                    return fail("deadline", executor::DeadlineExceeded.into());
                }
                Some(Some(left)) => Some(left),
                None => None,
            };
            let command = match decision.runtime.as_str() {
                "claude" => &cfg.inner.claude.command,
                "codex" => &cfg.inner.codex.command,
                _ => &cfg.inner.gemini.command,
            };
            // A missing CLI is Nectra's environment, not the agent: try another.
            if let Err(e) = executor::launch(command) {
                attempts.push(json!({"model": decision.model, "error_class": "harness_error", "error": format!("{e:#}")}));
                last = Some(decision);
                if pinned.is_none() {
                    continue;
                }
                return fail("harness_error", e);
            }
            let result = executor::conversation_observed(
                &decision,
                prompt,
                &cfg,
                &mut std::io::sink(),
                cancel,
                provider_session.as_mut(),
                remaining,
                &mut |event, text| {
                    ledger.observe(&decision, event);
                    if let Some(text) = text.filter(|t| !t.is_empty()) {
                        emit("text", json!({"model": decision.model, "text": text}))?;
                    }
                    for mut tool in tools(&decision.runtime, event, &mut names) {
                        tool["model"] = json!(decision.model);
                        emit("tool", tool)?;
                    }
                    match args.max_cost_usd {
                        Some(max) if ledger.cost().0 > max => Err(CostLimit(max).into()),
                        _ => Ok(()),
                    }
                },
            );
            let error = match result {
                Ok(reply) => {
                    attempts.push(json!({"model": decision.model, "error_class": null}));
                    session_id = remember(
                        state.as_deref(),
                        resumed.as_ref(),
                        &decision,
                        &provider_session,
                    )?;
                    last = Some(decision);
                    return Ok(Ok(reply));
                }
                Err(e) => e,
            };
            let (class, retry) = classify(&error, cancel);
            attempts.push(json!({"model": decision.model, "error_class": class, "error": format!("{error:#}")}));
            if let Some(limit) = error.downcast_ref::<crate::limits::RateLimited>() {
                cooldowns.record(&decision.model, &decision.provider, limit);
                if let Some(path) = &cooldown_path {
                    let _ = cooldowns.persist(path);
                }
            }
            // A resumed or pinned task stays on its model.
            if retry && pinned.is_none() {
                last = Some(decision);
                continue;
            }
            // A failed task can still be resumed, e.g. after a cost stop.
            session_id = remember(
                state.as_deref(),
                resumed.as_ref(),
                &decision,
                &provider_session,
            )?;
            last = Some(decision);
            return fail(class, error);
        }
    })()?;
    let (class, text, error) = match outcome {
        Ok(text) => (None, Some(text), None),
        Err((class, error)) => (Some(class), None, Some(error)),
    };
    let (cost, priced) = ledger.cost();
    let mut total = Tokens::default();
    let models: Vec<Value> = ledger
        .models()
        .into_iter()
        .map(|(c, t)| {
            total.add(t);
            json!({
                "model": c.model,
                "routed_as": c.routed,
                "provider": c.provider,
                "usage": t,
                "cost_usd": price(c).map(|p| t.cost(p)),
            })
        })
        .collect();
    let code = exit_code(class);
    emit(
        "result",
        json!({
            "is_error": class.is_some(),
            "error_class": class,
            "error": error,
            "text": text,
            "session_id": session_id,
            "model": last.as_ref().map(|d| &d.model),
            "provider": last.as_ref().map(|d| &d.provider),
            "usage": total,
            "cost_usd": priced.then_some(cost),
            "models": models,
            "attempts": attempts,
            "route": last,
            "duration_ms": started.elapsed().as_millis() as u64,
            "exit_code": code,
        }),
    )?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(runtime: &str, model: &str) -> Decision {
        Decision {
            task: "general".into(),
            provider: "anthropic".into(),
            runtime: runtime.into(),
            model: model.into(),
            effort: String::new(),
            mode: crate::cli::Mode::Fast,
            score: 1.0,
            score_margin: None,
            ambiguous: false,
            overridden: false,
            reason: String::new(),
            candidates: vec![],
        }
    }

    #[test]
    fn claude_usage_sums_each_message_once() {
        let d = decision("claude", "haiku");
        let mut l = Ledger::default();
        let start = |id: &str, i, cr, cw, o| {
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":id,"model":"claude-haiku-5-5",
                "usage":{"input_tokens":i,"cache_read_input_tokens":cr,"cache_creation_input_tokens":cw,"output_tokens":o}}}})
        };
        let delta = |o| json!({"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":o}}});
        // Live shape from Claude Code 2.1.304: two messages, deltas carry running output.
        for e in [
            start("m1", 2, 16670, 21673, 5),
            delta(167),
            start("m2", 2, 38343, 1720, 2),
            delta(14),
            // A cumulative modelUsage in the result must not be added on top.
            json!({"type":"result","modelUsage":{"claude-haiku-5-5":{"inputTokens":999}}}),
        ] {
            l.observe(&d, &e);
        }
        let models = l.models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].0.model, "claude-haiku-5-5");
        assert_eq!(
            models[0].1,
            Tokens {
                input: 4,
                cache_read: 55013,
                cache_write: 23393,
                output: 181
            }
        );
        // Claude Code itself reported $0.00531963 for this run.
        let (cost, priced) = l.cost();
        assert!(priced && (cost - 0.00531963).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn codex_cached_input_is_not_counted_twice_and_models_stay_apart() {
        let mut l = Ledger::default();
        l.observe(
            &decision("codex", "gpt-5.6-luna"),
            &json!({"type":"turn.completed","usage":{"input_tokens":29493,"cached_input_tokens":25856,"cache_write_input_tokens":0,"output_tokens":139}}),
        );
        l.observe(
            &decision("gemini", "gemini-x"),
            &json!({"type":"result","status":"success","stats":{"models":{"gemini-x":{"input":7,"cached":3,"output_tokens":2}}}}),
        );
        let models = l.models();
        assert_eq!(models[0].1.input, 3637);
        assert_eq!(models[0].1.cache_read, 25856);
        assert_eq!(
            models[1].1,
            Tokens {
                input: 7,
                cache_read: 3,
                cache_write: 0,
                output: 2
            }
        );
        // Gemini has no price: the total is not claimed as complete.
        assert!(!l.cost().1);
    }

    #[test]
    fn tool_events_pair_start_and_end() {
        let mut names = HashMap::new();
        let start = tools(
            "claude",
            &json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash"}]}}),
            &mut names,
        );
        assert_eq!(
            start,
            vec![json!({"phase":"start","name":"Bash","id":"t1"})]
        );
        let end = tools(
            "claude",
            &json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}),
            &mut names,
        );
        assert_eq!(end[0]["name"], "Bash");
        assert_eq!(end[0]["phase"], "end");
        let codex = tools(
            "codex",
            &json!({"type":"item.started","item":{"id":"item_0","type":"command_execution"}}),
            &mut names,
        );
        assert_eq!(codex[0]["name"], "command_execution");
        assert!(tools(
            "codex",
            &json!({"type":"item.completed","item":{"type":"agent_message"}}),
            &mut names
        )
        .is_empty());
    }

    #[test]
    fn errors_map_to_classes_and_exit_codes() {
        let cancel = CancellationToken::default();
        let class = |e: anyhow::Error| classify(&e, &cancel);
        assert_eq!(class(CostLimit(1.0).into()), ("cost_limit", false));
        assert_eq!(
            class(executor::DeadlineExceeded.into()),
            ("deadline", false)
        );
        assert_eq!(
            class(
                crate::access::ModelUnavailable {
                    safe_to_retry: true
                }
                .into()
            ),
            ("model_unavailable", true)
        );
        assert_eq!(class(anyhow::anyhow!("boom")), ("agent_error", false));
        cancel.cancel();
        assert_eq!(classify(&anyhow::anyhow!("x"), &cancel).0, "cancelled");
        assert_eq!(exit_code(None), 0);
        assert_eq!(exit_code(Some("no_model")), 1);
        assert_eq!(exit_code(Some("auth")), 3);
        assert_eq!(exit_code(Some("deadline")), 4);
    }

    #[test]
    fn flags_parse_and_session_ids_cannot_escape() {
        assert_eq!(duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(duration("10m").unwrap(), Duration::from_secs(600));
        assert!(duration("0").is_err() && duration("5d").is_err());
        assert!(positive("0").is_err() && positive("nan").is_err());
        let dir = Path::new("state");
        assert!(record_path(dir, "../../etc/passwd").is_err());
        assert!(record_path(dir, "").is_err());
        assert!(record_path(dir, &executor::new_session_id()).is_ok());
    }
}
