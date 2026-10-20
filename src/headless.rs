//! `nectra run --events jsonl`: one routed task, unattended, reported as a
//! versioned stream of JSON lines on stdout. The contract is docs/headless.md.
use crate::{
    cli::Cli,
    config::Config,
    executor::{self, CancellationToken, ProviderSession},
    models::Registry,
    redact,
    router::{self, Decision},
    usage,
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

/// One model call's tokens as `usage` reports them. `cache_write` includes
/// `cache_write_5m`, the part Claude wrote at its 5-minute rate; the rest is at
/// the 1-hour rate.
#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq, Eq)]
struct Usage {
    input: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_5m: u64,
    output: u64,
}
impl Usage {
    fn fields(&mut self) -> [&mut u64; 5] {
        [
            &mut self.input,
            &mut self.cache_read,
            &mut self.cache_write,
            &mut self.cache_write_5m,
            &mut self.output,
        ]
    }
    fn add(&mut self, mut o: Usage) {
        for (a, b) in self.fields().into_iter().zip(o.fields()) {
            *a += *b;
        }
    }
    /// The growth from `before` to these running totals.
    fn since(mut self, mut before: Usage) -> Usage {
        for (a, b) in self.fields().into_iter().zip(before.fields()) {
            *a = a.saturating_sub(*b);
        }
        self
    }
    /// Field by field, the larger. A provider's counters for a call only grow,
    /// so a repeated or stale snapshot never lowers what was counted.
    fn max(&mut self, mut o: Usage) {
        for (a, b) in self.fields().into_iter().zip(o.fields()) {
            *a = (*a).max(*b);
        }
    }
    /// USD at `model`'s own price; None when it has none.
    fn cost(&self, model: &str) -> Option<f64> {
        let [input, read, write_1h, output] = usage::price(model)?;
        let write_5m = usage::price_cache_write_5m(model)?;
        let n = |v: u64| v as f64;
        Some(
            (n(self.input) * input
                + n(self.cache_read) * read
                + n(self.cache_write.saturating_sub(self.cache_write_5m)) * write_1h
                + n(self.cache_write_5m) * write_5m
                + n(self.output) * output)
                / 1e6,
        )
    }
}

/// Claude's usage object. Without the TTL split, a cache write is taken to be
/// at the 1-hour rate, Claude Code's default and the dearer one.
fn claude_usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let split = &u["cache_creation"];
    let (m5, h1) = (
        split["ephemeral_5m_input_tokens"].as_u64().unwrap_or(0),
        split["ephemeral_1h_input_tokens"].as_u64().unwrap_or(0),
    );
    Usage {
        input: n("input_tokens"),
        cache_read: n("cache_read_input_tokens"),
        cache_write: n("cache_creation_input_tokens").max(m5 + h1),
        cache_write_5m: m5,
        output: n("output_tokens"),
    }
}

/// Codex's usage object, whose input includes cached input.
fn codex_usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Usage {
        input: n("input_tokens").saturating_sub(n("cached_input_tokens")),
        cache_read: n("cached_input_tokens"),
        cache_write: n("cache_write_input_tokens"),
        cache_write_5m: 0,
        output: n("output_tokens"),
    }
}

/// One model call (Claude), log record (Codex) or per-model total (Gemini).
/// `complete` is false when the provider never gave the call's final count.
struct Call {
    routed: String,
    provider: String,
    model: String,
    tokens: Usage,
    complete: bool,
}

/// Codex's own log of the thread. Codex reports usage on `turn.completed`
/// only, as the thread's running total, so a resumed thread would count its
/// history again and an interrupted turn would count nothing. Its log
/// (rollout) has the running total after every model call, which gives each
/// call's tokens as they happen.
#[derive(Default)]
struct Rollout {
    /// Empty until a new thread's `thread.started`; None when Codex keeps no log.
    thread: Option<String>,
    path: Option<PathBuf>,
    read: u64,
    looked: Option<Instant>,
    /// The last running total read; on a resumed thread, at first the history's.
    total: Usage,
    model: Option<String>,
    counted: bool,
    /// The running total before this attempt is known: zero on a new thread,
    /// read from the log before a resumed one starts. Without it nothing is
    /// counted from the log, which would include the thread's history.
    based: bool,
}

/// Token usage as the providers report it, per call. Claude's result
/// `modelUsage` adds up the whole session on a resumed one, so Claude is
/// counted from each message's own usage instead.
#[derive(Default)]
struct Ledger {
    calls: Vec<Call>,
    /// Claude: each message id's call, and the message each stream (the main
    /// agent, or a subagent under its tool use id) is in.
    claude: HashMap<String, usize>,
    streams: HashMap<String, usize>,
    /// Codex and Gemini, this attempt: the agent did paid work, and the
    /// provider's final count of it.
    worked: bool,
    reported: Option<Usage>,
    resumed: bool,
    rollout: Rollout,
    /// Claude Code's own `total_cost_usd`, for cross-checking: on a resumed
    /// session it covers the whole session.
    provider_cost: Option<f64>,
}
impl Ledger {
    /// A new attempt on `d`. `thread` is a resumed Codex thread, whose log so
    /// far is history; `logged` says whether Codex keeps a log at all.
    fn begin(&mut self, d: &Decision, thread: Option<&str>, logged: bool) {
        self.worked = false;
        self.reported = None;
        self.resumed = thread.is_some();
        self.rollout = Rollout::default();
        if d.runtime == "codex" && logged {
            self.rollout.thread = Some(thread.unwrap_or_default().to_string());
            self.rollout.based = thread.is_none() || self.read_rollout(d, false);
        }
    }
    fn call(&mut self, d: &Decision, model: &str, tokens: Usage, complete: bool) -> usize {
        self.calls.push(Call {
            routed: d.model.clone(),
            provider: d.provider.clone(),
            model: model.to_string(),
            tokens,
            complete,
        });
        self.calls.len() - 1
    }
    fn observe(&mut self, d: &Decision, event: &Value) {
        let kind = event["type"].as_str().unwrap_or("");
        match d.runtime.as_str() {
            "claude" => self.claude(d, event),
            "codex" => {
                match kind {
                    "thread.started" if self.rollout.thread.as_deref() == Some("") => {
                        self.rollout.thread = event["thread_id"].as_str().map(String::from);
                    }
                    "item.started" | "item.completed" => self.worked = true,
                    "turn.completed" => self.reported = Some(codex_usage(&event["usage"])),
                    _ => {}
                }
                self.read_rollout(d, true);
            }
            "gemini" => match kind {
                "result" => {
                    let mut total = Usage::default();
                    for (model, u) in event["stats"]["models"].as_object().into_iter().flatten() {
                        let n = |k: &str| u[k].as_u64().unwrap_or(0);
                        let t = Usage {
                            input: n("input"),
                            cache_read: n("cached"),
                            output: n("output_tokens"),
                            ..Usage::default()
                        };
                        total.add(t);
                        // Gemini's stream stats leave out thinking tokens.
                        self.call(d, model, t, false);
                    }
                    self.reported = Some(total);
                }
                "init" => {}
                _ => self.worked = true,
            },
            _ => {}
        }
    }
    fn claude(&mut self, d: &Decision, event: &Value) {
        // A subagent's messages stream under its tool use id.
        let stream = event["parent_tool_use_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let snapshot = |ledger: &mut Ledger, m: &Value, stopped: bool| -> Option<usize> {
            let id = m["id"].as_str()?.to_string();
            let index = match ledger.claude.get(&id) {
                Some(&i) => i,
                None => {
                    let model = m["model"].as_str().unwrap_or("unknown");
                    let i = ledger.call(d, model, Usage::default(), false);
                    ledger.claude.insert(id, i);
                    i
                }
            };
            let call = &mut ledger.calls[index];
            call.tokens.max(claude_usage(&m["usage"]));
            call.complete |= stopped;
            Some(index)
        };
        match event["type"].as_str() {
            Some("stream_event") => {
                let e = &event["event"];
                match e["type"].as_str() {
                    Some("message_start") => {
                        if let Some(i) = snapshot(self, &e["message"], false) {
                            self.streams.insert(stream, i);
                        }
                    }
                    // A delta's counts are running totals for its stream's message.
                    Some(kind @ ("message_delta" | "message_stop")) => {
                        if let Some(&i) = self.streams.get(&stream) {
                            let call = &mut self.calls[i];
                            call.tokens.max(claude_usage(&e["usage"]));
                            call.complete |=
                                kind == "message_stop" || !e["delta"]["stop_reason"].is_null();
                        }
                    }
                    _ => {}
                }
            }
            Some("result") => {
                if let Some(cost) = event["total_cost_usd"].as_f64() {
                    *self.provider_cost.get_or_insert(0.0) += cost;
                }
            }
            // Whole messages, which also cover calls whose raw stream isn't
            // forwarded (a subagent's); final once they carry a stop reason.
            Some("assistant") => {
                let m = &event["message"];
                snapshot(self, m, !m["stop_reason"].is_null());
            }
            _ => {}
        }
    }
    /// Counts the calls Codex has logged since the last read. With `count`
    /// false, only takes the running total so far as the baseline. Whether
    /// the log was read.
    fn read_rollout(&mut self, d: &Decision, count: bool) -> bool {
        use std::io::{Read, Seek};
        let r = &mut self.rollout;
        let Some(thread) = r.thread.as_deref().filter(|t| !t.is_empty()) else {
            return false;
        };
        if count && !r.based {
            return false;
        }
        if r.path.is_none() {
            // Codex writes the log as the thread starts; look at most every 2 s.
            if r.looked
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
            {
                return false;
            }
            r.looked = Some(Instant::now());
            r.path = usage::codex_rollout(thread);
        }
        let Some(path) = &r.path else { return false };
        let mut text = vec![];
        let read = std::fs::File::open(path).and_then(|mut f| {
            f.seek(std::io::SeekFrom::Start(r.read))?;
            f.read_to_end(&mut text)
        });
        // Only whole lines; a line still being written is read next time.
        let end = match (read, text.iter().rposition(|&b| b == b'\n')) {
            (Ok(_), Some(end)) => end,
            (Ok(_), None) => return true,
            (Err(_), _) => return false,
        };
        r.read += end as u64 + 1;
        let mut found = vec![];
        for line in text[..end].split(|&b| b == b'\n') {
            let Ok(entry) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let p = &entry["payload"];
            if entry["type"] == "turn_context" {
                r.model = p["model"].as_str().map(String::from).or(r.model.take());
            }
            let total = &p["info"]["total_token_usage"];
            if p["type"] != "token_count" || total.is_null() {
                continue;
            }
            let total = codex_usage(total);
            let call = total.since(r.total);
            r.total = total;
            if count && call != Usage::default() {
                found.push((r.model.clone().unwrap_or_else(|| d.model.clone()), call));
            }
        }
        if !found.is_empty() {
            self.rollout.counted = true;
            self.worked = true;
        }
        for (model, call) in found {
            self.call(d, &model, call, true);
        }
        true
    }
    /// The attempt on `d` ended: reads what Codex logged last, and marks paid
    /// work whose count never arrived.
    fn finish(&mut self, d: &Decision) {
        match d.runtime.as_str() {
            "claude" => return,
            "codex" => {
                self.rollout.looked = None;
                self.read_rollout(d, true);
                // Without a log, `turn.completed` is exact on a new thread only.
                if let Some(u) = self.reported {
                    if !self.rollout.counted && !self.resumed {
                        self.call(d, &d.model, u, true);
                    }
                }
                // The log has every finished call; a stopped turn's last may be missing.
                if self.reported.is_some() && (self.rollout.counted || !self.resumed) {
                    return;
                }
            }
            _ => {
                if self.reported.is_some() {
                    return;
                }
            }
        }
        if self.worked || self.reported.is_some() {
            let model = self
                .rollout
                .model
                .clone()
                .unwrap_or_else(|| d.model.clone());
            self.call(d, &model, Usage::default(), false);
        }
    }
    /// Per (routed model, reporting model), in first-seen order: tokens, and
    /// whether every call's final count arrived.
    fn models(&self) -> Vec<(&Call, Usage, bool)> {
        let mut out: Vec<(&Call, Usage, bool)> = vec![];
        for c in &self.calls {
            match out
                .iter_mut()
                .find(|(o, ..)| o.routed == c.routed && o.model == c.model)
            {
                Some((_, t, complete)) => {
                    t.add(c.tokens);
                    *complete &= c.complete;
                }
                None => out.push((c, c.tokens, c.complete)),
            }
        }
        out
    }
    /// The priced part of the cost, and whether every model with tokens has
    /// its own price.
    fn cost(&self) -> (f64, bool) {
        self.models()
            .iter()
            .fold((0.0, true), |(sum, all), (c, t, _)| {
                match t.cost(&c.model) {
                    Some(cost) => (sum + cost, all),
                    None => (sum, all && *t == Usage::default()),
                }
            })
    }
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

/// The session id to report: the resumed one, or a new record for this task,
/// written privately and whole.
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
    let dir = path.parent().context("no runs directory")?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    // A temporary file is private (0600 on Unix) and renamed into place whole.
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(&serde_json::to_vec_pretty(&record)?)?;
    file.persist(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(record.id))
}

/// Runs `work` on its own thread and waits for it until the deadline or a
/// cancel, so neither a stdin that never closes nor a slow router outlasts them.
fn bounded<T: Send + 'static>(
    until: Option<Instant>,
    cancel: &CancellationToken,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(work()));
    loop {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        if until.is_some_and(|u| Instant::now() >= u) {
            return Err(executor::DeadlineExceeded.into());
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => bail!("worker thread failed"),
        }
    }
}

type Outcome = std::result::Result<String, (&'static str, String)>;

pub fn run(
    cli: &Cli,
    cfg: &Config,
    registry: &Registry,
    words: &[String],
    args: &Args,
) -> Result<i32> {
    let started = Instant::now();
    let until = args.deadline.map(|d| started + d);
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
    let mut warnings: Vec<String> = vec![];
    let mut session_id = None;
    let mut last: Option<Decision> = None;
    let error = |e: &anyhow::Error| redact::redact(&format!("{e:#}"));
    // Before the run starts, anything but a guard is Nectra's own failure.
    let early = |e: &anyhow::Error| match classify(e, cancel).0 {
        class @ ("cancelled" | "deadline") => class,
        _ => "harness_error",
    };
    let run = || -> Result<Outcome> {
        let fail = |class, e: anyhow::Error| Ok(Err((class, error(&e))));
        let prompt = match (cli.stdin, words.is_empty()) {
            (true, false) => {
                let e = anyhow::anyhow!("use either --stdin or positional prompt text");
                return fail("harness_error", e);
            }
            (true, true) => {
                let read = bounded(until, cancel, || {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                        .context("reading prompt from stdin")?;
                    Ok(text)
                });
                match read {
                    Ok(text) => text,
                    Err(e) => return fail(early(&e), e),
                }
            }
            (false, _) => words.join(" "),
        };
        if prompt.trim().is_empty() {
            return fail("harness_error", anyhow::anyhow!("prompt cannot be empty"));
        }
        if let Some(path) = &args.mcp_config {
            let codex = registry
                .models
                .iter()
                .any(|m| m.runtime.as_str() == "codex")
                .then_some(cfg.inner.codex.command.as_str());
            match crate::mcp::Explicit::load(path, codex) {
                Ok(explicit) => {
                    if let (Some(_), Some(why)) = (codex, explicit.codex_excluded()) {
                        eprintln!(
                            "nectra: --mcp-config: Codex models skipped: {}",
                            redact::redact(why)
                        );
                    }
                    cfg.mcp_config = Some(explicit);
                }
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
            let route = {
                let (prompt, cfg, pinned) = (prompt.clone(), cfg.clone(), pinned.clone());
                let mode = cli.mode.unwrap_or(cfg.inner.routing.default_mode);
                let provider = if resumed.is_some() {
                    None
                } else {
                    cli.provider
                };
                bounded(until, cancel, move || {
                    router::route(&prompt, &cfg, &available, mode, provider, pinned)
                })
            };
            let decision = match route {
                Ok(d) => d,
                Err(e) => {
                    let class = match early(&e) {
                        "harness_error" => "no_model",
                        class => class,
                    };
                    let e = match tried.last() {
                        Some(_) => e.context(format!("after trying {}", tried.join(", "))),
                        None => e,
                    };
                    return fail(class, e);
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
            let remaining = match until.map(|u| u.checked_duration_since(Instant::now())) {
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
                attempts.push(json!({"model": decision.model, "error_class": "harness_error", "error": error(&e)}));
                last = Some(decision);
                if pinned.is_none() {
                    continue;
                }
                return fail("harness_error", e);
            }
            ledger.begin(
                &decision,
                resumed.as_ref().map(|r| r.provider_session_id.as_str()),
                provider_session.is_some(),
            );
            let result = executor::conversation_observed(
                &decision,
                &prompt,
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
            ledger.finish(&decision);
            // A task that ran keeps its session, even when it failed (a cost
            // stop can be continued); failing to save it only loses --resume.
            let mut keep = || match remember(
                state.as_deref(),
                resumed.as_ref(),
                &decision,
                &provider_session,
            ) {
                Ok(id) => session_id = id,
                Err(e) => warnings.push(format!(
                    "session not saved, so it can't be resumed: {}",
                    error(&e)
                )),
            };
            let e = match result {
                Ok(reply) => {
                    keep();
                    attempts.push(json!({"model": decision.model, "error_class": null}));
                    last = Some(decision);
                    return Ok(Ok(reply));
                }
                Err(e) => e,
            };
            let (class, retry) = classify(&e, cancel);
            attempts
                .push(json!({"model": decision.model, "error_class": class, "error": error(&e)}));
            if let Some(limit) = e.downcast_ref::<crate::limits::RateLimited>() {
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
            keep();
            last = Some(decision);
            return fail(class, e);
        }
    };
    // Whatever fails from here on, the caller still gets its one result event.
    let outcome: Outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => Err(("harness_error", error(&e))),
        Err(_) => Err(("harness_error", "Nectra panicked; this is a bug".into())),
    };
    let (class, text, error) = match outcome {
        Ok(text) => (None, Some(text), None),
        Err((class, error)) => (Some(class), None, Some(error)),
    };
    let (cost, priced) = ledger.cost();
    let mut total = Usage::default();
    let mut complete = true;
    let models: Vec<Value> = ledger
        .models()
        .into_iter()
        .map(|(c, t, done)| {
            total.add(t);
            complete &= done;
            json!({
                "model": c.model,
                "routed_as": c.routed,
                "provider": c.provider,
                "usage": t,
                "usage_complete": done,
                "cost_usd": t.cost(&c.model),
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
            "usage_complete": complete,
            "cost_usd": priced.then_some(cost),
            "provider_cost_usd": ledger.provider_cost,
            "models": models,
            "attempts": attempts,
            "route": last,
            "warnings": warnings,
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

    fn start(id: &str, parent: Option<&str>, usage: Value) -> Value {
        json!({"type":"stream_event","parent_tool_use_id":parent,"event":{"type":"message_start",
            "message":{"id":id,"model":"claude-haiku-5-5","usage":usage}}})
    }
    fn delta(parent: Option<&str>, output: u64, stop: bool) -> Value {
        json!({"type":"stream_event","parent_tool_use_id":parent,"event":{"type":"message_delta",
            "delta":{"stop_reason": if stop { json!("end_turn") } else { Value::Null }},
            "usage":{"output_tokens":output}}})
    }
    fn usage(input: u64, read: u64, write: u64, output: u64) -> Value {
        json!({"input_tokens":input,"cache_read_input_tokens":read,
            "cache_creation_input_tokens":write,"output_tokens":output})
    }
    fn total(l: &Ledger) -> (Usage, bool) {
        l.models()
            .iter()
            .fold((Usage::default(), true), |(mut t, c), m| {
                t.add(m.1);
                (t, c && m.2)
            })
    }

    #[test]
    fn claude_usage_sums_each_message_once() {
        let d = decision("claude", "haiku");
        let mut l = Ledger::default();
        // Live shape from Claude Code 2.1.304: two messages; the final delta
        // carries the message's output.
        for e in [
            start("m1", None, usage(2, 16670, 21673, 5)),
            delta(None, 167, true),
            start("m2", None, usage(2, 38343, 1720, 2)),
            delta(None, 14, true),
            // A cumulative modelUsage in the result must not be added on top.
            json!({"type":"result","modelUsage":{"claude-haiku-5-5":{"inputTokens":999}}}),
        ] {
            l.observe(&d, &e);
        }
        l.finish(&d);
        let models = l.models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].0.model, "claude-haiku-5-5");
        assert_eq!(
            models[0].1,
            Usage {
                input: 4,
                cache_read: 55013,
                cache_write: 23393,
                cache_write_5m: 0,
                output: 181
            }
        );
        assert!(models[0].2, "every call stopped");
        // Claude Code itself reported $0.00531963 for this run (1-hour writes).
        let (cost, priced) = l.cost();
        assert!(priced && (cost - 0.00531963).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn claude_cache_writes_are_priced_by_their_ttl() {
        let d = decision("claude", "haiku");
        let mut l = Ledger::default();
        let mut u = usage(0, 0, 3_000_000, 0);
        u["cache_creation"] =
            json!({"ephemeral_5m_input_tokens":2_000_000,"ephemeral_1h_input_tokens":1_000_000});
        l.observe(&d, &start("m1", None, u));
        l.observe(&d, &delta(None, 0, true));
        let (t, complete) = total(&l);
        assert_eq!(
            (t.cache_write, t.cache_write_5m, complete),
            (3_000_000, 2_000_000, true)
        );
        // haiku: 2M at the 5-minute $0.125, 1M at the 1-hour $0.20.
        assert!((l.cost().0 - 0.45).abs() < 1e-9, "{}", l.cost().0);
    }

    #[test]
    fn replayed_and_interleaved_claude_messages_count_once_each() {
        let d = decision("claude", "haiku");
        let mut l = Ledger::default();
        for e in [
            start("m1", None, usage(1, 10, 0, 1)),
            delta(None, 100, false),
            // A subagent's message streams while the main one is open.
            start("s1", Some("toolu_1"), usage(5, 0, 0, 1)),
            delta(Some("toolu_1"), 40, true),
            // A replayed start must not lower m1's output back to 1.
            start("m1", None, usage(1, 10, 0, 1)),
            delta(None, 120, true),
            // The whole-message snapshot of a call already counted adds nothing.
            json!({"type":"assistant","message":{"id":"m1","model":"claude-haiku-5-5",
                "stop_reason":"end_turn","usage":usage(1, 10, 0, 120)}}),
        ] {
            l.observe(&d, &e);
        }
        let (t, complete) = total(&l);
        assert_eq!(
            (t.input, t.cache_read, t.output, complete),
            (6, 10, 160, true)
        );
        assert_eq!(l.calls.len(), 2);
    }

    #[test]
    fn interrupted_claude_call_counts_its_spend_and_is_flagged() {
        let d = decision("claude", "haiku");
        let mut l = Ledger::default();
        l.observe(&d, &start("m1", None, usage(3, 0, 50_000, 1)));
        l.finish(&d);
        let (t, complete) = total(&l);
        assert_eq!(t.cache_write, 50_000, "the call's input is real spend");
        assert!(!complete, "its output never arrived");
        assert!(l.cost().0 > 0.0);
    }

    #[test]
    fn a_model_without_a_price_is_never_given_another_ones() {
        let d = decision("claude", "haiku");
        let mut l = Ledger::default();
        l.observe(&d, &start("m1", None, usage(1, 0, 0, 1)));
        let mut other = start("m2", None, usage(1000, 0, 0, 1));
        other["event"]["message"]["model"] = json!("claude-fable-5-1");
        l.observe(&d, &other);
        assert!(
            !l.cost().1,
            "fable has no price: the cost is unknown, not haiku's"
        );
        let models = l.models();
        assert_eq!(models[1].0.model, "claude-fable-5-1");
        assert_eq!(models[1].1.cost(&models[1].0.model), None);
    }

    fn rollout(lines: &[Value]) -> String {
        lines.iter().map(|l| format!("{l}\n")).collect()
    }
    fn counted(total_input: u64, cached: u64, output: u64) -> Value {
        json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":
            {"input_tokens":total_input,"cached_input_tokens":cached,"output_tokens":output}}}})
    }

    #[test]
    fn resumed_codex_thread_counts_only_its_own_calls() {
        let d = decision("codex", "gpt-5.6-luna");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-x-thread-1.jsonl");
        // As Codex 0.162.1 writes it: the kind is the entry's, not the payload's.
        // The answering model differs from the routed one, which must not be used.
        let context =
            json!({"type":"turn_context","payload":{"turn_id":"t","model":"gpt-5.6-sol"}});
        // Turn 1 as Codex 0.162.1 logged it: running totals after each call.
        std::fs::write(
            &path,
            rollout(&[
                context.clone(),
                counted(15188, 11392, 216),
                counted(30697, 26368, 288),
            ]),
        )
        .unwrap();
        let mut l = Ledger::default();
        l.begin(&d, Some("thread-1"), true);
        l.rollout.path = Some(path.clone());
        l.rollout.read = 0;
        l.rollout.based = l.read_rollout(&d, false);
        // Turn 2 appends two calls; turn.completed repeats the thread's total.
        let more = rollout(&[
            context,
            counted(46314, 41728, 513),
            counted(63403, 57216, 657),
        ]);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(more.as_bytes())
            .unwrap();
        l.observe(
            &d,
            &json!({"type":"item.completed","item":{"type":"agent_message"}}),
        );
        l.observe(&d, &json!({"type":"turn.completed","usage":{"input_tokens":63403,"cached_input_tokens":57216,"output_tokens":657}}));
        l.finish(&d);
        let (t, complete) = total(&l);
        // (63403 - 30697) input, of which (57216 - 26368) cached; 657 - 288 output.
        assert_eq!(
            (t.input + t.cache_read, t.cache_read, t.output),
            (32706, 30848, 369)
        );
        assert!(complete);
        assert_eq!(l.calls.len(), 2, "one entry per model call");
        assert_eq!(
            l.models()[0].0.model,
            "gpt-5.6-sol",
            "the model that answered"
        );
    }

    #[test]
    fn a_resumed_codex_thread_without_its_baseline_never_counts_history() {
        let d = decision("codex", "gpt-5.6-luna");
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::default();
        // The log can't be found as the attempt starts...
        l.begin(&d, Some("019a0000-0000-7000-8000-00000000dead"), true);
        // ...and turns up later, history and this turn's call together.
        let path = dir
            .path()
            .join("rollout-x-019a0000-0000-7000-8000-00000000dead.jsonl");
        std::fs::write(
            &path,
            rollout(&[counted(30697, 26368, 288), counted(46314, 41728, 513)]),
        )
        .unwrap();
        l.rollout.path = Some(path);
        l.observe(
            &d,
            &json!({"type":"item.completed","item":{"type":"agent_message"}}),
        );
        l.observe(&d, &json!({"type":"turn.completed","usage":{"input_tokens":46314,"cached_input_tokens":41728,"output_tokens":513}}));
        l.finish(&d);
        let (t, complete) = total(&l);
        assert_eq!(t, Usage::default(), "history is never counted as this turn");
        assert!(!complete, "this turn's usage is unknown");
    }

    #[test]
    fn the_active_rollout_of_a_thread_is_found_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = "019a0000-0000-7000-8000-0000000000aa";
        let meta = |id: &str| format!("{}\n", json!({"type":"session_meta","payload":{"id":id}}));
        let day = dir.path().join("2026/10/20");
        std::fs::create_dir_all(&day).unwrap();
        let first = day.join(format!("rollout-2026-10-20T08-00-00-{id}.jsonl"));
        std::fs::write(&first, meta(id)).unwrap();
        assert_eq!(usage::codex_rollout_in(dir.path(), id), Some(first));
        // `thread/revert` writes a new file for the same thread, with a suffix.
        std::thread::sleep(Duration::from_millis(20));
        let reverted = day.join(format!("rollout-2026-10-20T09-00-00-{id}_r2.jsonl"));
        std::fs::write(&reverted, meta(id)).unwrap();
        assert_eq!(usage::codex_rollout_in(dir.path(), id), Some(reverted));
        // A file whose name matches but which logs another thread is not it.
        let other = "019a0000-0000-7000-8000-0000000000bb";
        std::fs::write(
            day.join(format!("rollout-2026-10-20T10-00-00-{other}.jsonl")),
            meta(id),
        )
        .unwrap();
        assert_eq!(usage::codex_rollout_in(dir.path(), other), None);
    }

    #[test]
    fn interrupted_codex_turn_keeps_logged_calls_and_is_flagged() {
        let d = decision("codex", "gpt-5.6-luna");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-x-thread-2.jsonl");
        std::fs::write(&path, rollout(&[counted(1000, 800, 50)])).unwrap();
        let mut l = Ledger::default();
        l.begin(&d, None, true);
        l.observe(&d, &json!({"type":"thread.started","thread_id":"thread-2"}));
        l.rollout.path = Some(path);
        l.observe(
            &d,
            &json!({"type":"item.started","item":{"type":"command_execution"}}),
        );
        // The process is killed: no turn.completed.
        l.finish(&d);
        let (t, complete) = total(&l);
        assert_eq!((t.input, t.cache_read, t.output), (200, 800, 50));
        assert!(!complete, "the call in flight was never counted");

        // Without a log at all, a turn that worked is flagged, never free.
        let mut l = Ledger::default();
        l.begin(&d, None, false);
        l.observe(
            &d,
            &json!({"type":"item.started","item":{"type":"command_execution"}}),
        );
        l.finish(&d);
        assert!(!total(&l).1);
        // A turn rejected before any work costs nothing and is complete.
        let mut l = Ledger::default();
        l.begin(&d, None, false);
        l.observe(
            &d,
            &json!({"type":"turn.failed","error":{"message":"usage limit"}}),
        );
        l.finish(&d);
        assert_eq!(total(&l), (Usage::default(), true));
    }

    #[test]
    fn codex_cached_input_is_not_counted_twice_and_models_stay_apart() {
        let mut l = Ledger::default();
        let codex = decision("codex", "gpt-5.6-luna");
        l.begin(&codex, None, false);
        l.observe(
            &codex,
            &json!({"type":"turn.completed","usage":{"input_tokens":29493,"cached_input_tokens":25856,"cache_write_input_tokens":0,"output_tokens":139}}),
        );
        l.finish(&codex);
        let gemini = decision("gemini", "gemini-x");
        l.begin(&gemini, None, false);
        l.observe(
            &gemini,
            &json!({"type":"result","status":"success","stats":{"models":{"gemini-x":{"input":7,"cached":3,"output_tokens":2}}}}),
        );
        l.finish(&gemini);
        let models = l.models();
        assert_eq!(models[0].1.input, 3637);
        assert_eq!(models[0].1.cache_read, 25856);
        assert!(models[0].2);
        assert_eq!(
            models[1].1,
            Usage {
                input: 7,
                cache_read: 3,
                output: 2,
                ..Usage::default()
            }
        );
        assert!(!models[1].2, "Gemini leaves out thinking tokens");
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
