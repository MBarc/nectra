//! `nectra usage`: tokens per model, read from the providers' own session logs.
//!
//! Nectra records the provider session id with each answer. Claude Code and
//! Codex already log exact token usage per session, so the totals come from
//! there rather than from estimates. Gemini and Ollama turns are counted but
//! have no token data.
//!
//! Costs are API-equivalent: the providers' list prices for the same tokens.
//! Subscriptions don't bill per token, and how much of a plan each model uses
//! is unpublished, so a cost is a yardstick, not a bill.
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// The model `nectra usage` and chat compare costs against by default.
pub const BASELINE: &str = "opus";
const PRICES: &str = include_str!("../eval/chat-savings/prices.json");

/// USD per million tokens (input, cache read, cache write, output) for the
/// models in `eval/chat-savings/prices.json`. Claude Code logs full names
/// (`claude-opus-5-5`), which resolve to their short ids.
pub fn price(model: &str) -> Option<[f64; 4]> {
    let id = ["opus", "sonnet", "haiku"]
        .into_iter()
        .find(|s| model.starts_with("claude-") && model.contains(s))
        .unwrap_or(model);
    let prices: Value = serde_json::from_str(PRICES).ok()?;
    let n = |k: &str| prices[id][k].as_f64();
    Some([
        n("input")?,
        n("cache_read")?,
        n("cache_write")?,
        n("output")?,
    ])
}

#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Tokens {
    /// Input not served from cache.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
}
impl Tokens {
    pub fn add(&mut self, o: Tokens) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
    }
    /// USD at `price` (from [`price`]).
    pub fn cost(&self, price: [f64; 4]) -> f64 {
        let t = [self.input, self.cache_read, self.cache_write, self.output];
        t.iter().zip(price).map(|(&n, p)| n as f64 * p).sum::<f64>() / 1e6
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ModelUsage {
    pub turns: u64,
    /// Provider sessions with token data; a resumed session counts once.
    pub sessions: u64,
    /// Turns whose provider session could not be found (or Gemini/Ollama).
    pub turns_without_data: u64,
    /// Answered turns whose provider session has token data: cost per turn uses these.
    pub turns_with_data: u64,
    /// Attempts that failed; their tokens are included when their session is known.
    pub failed: u64,
    pub tokens: Tokens,
    /// Turns that moved the chat here from another model.
    pub switches: u64,
    /// Turns that started with the previous model's handoff note.
    pub handoffs: u64,
    /// `/again` retries sent here.
    pub again: u64,
    /// Turns by what the router took them for ("small code change", ...).
    pub kinds: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub models: BTreeMap<String, ModelUsage>,
    /// Chats that couldn't be read: on Windows, one open in a running Nectra is locked.
    pub unreadable: u64,
    /// With `--direct`: Claude Code and Codex sessions Nectra didn't start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct: Option<BTreeMap<String, ModelUsage>>,
}

/// Where the provider logs live; injectable so tests never read real ones.
pub struct Logs {
    pub nectra_sessions: PathBuf,
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
}
impl Logs {
    pub fn default_paths() -> Result<Self> {
        let home = dirs::home_dir().context("no home directory")?;
        Ok(Self {
            nectra_sessions: dirs::data_local_dir()
                .context("no local data directory")?
                .join("nectra/sessions"),
            claude_projects: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"))
                .join("projects"),
            codex_sessions: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"))
                .join("sessions"),
        })
    }
    /// One walk of each provider's log folder, for lookups by session id.
    fn index(&self) -> Result<(HashMap<String, PathBuf>, HashMap<String, PathBuf>)> {
        let claude = index(&self.claude_projects, |name| name.strip_suffix(".jsonl"))?;
        let codex = index(&self.codex_sessions, |name| {
            // rollout-<timestamp>-<thread uuid>.jsonl; the uuid is the last 36 chars.
            let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
            stem.get(stem.len().checked_sub(36)?..)
        })?;
        Ok((claude, codex))
    }
}

pub fn collect(logs: &Logs, days: Option<u64>, direct: bool) -> Result<Report> {
    let since = days.map(|d| SystemTime::now() - Duration::from_secs(d * 86_400));
    let recent = |path: &Path| {
        since.is_none_or(|s| {
            fs::metadata(path)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= s)
        })
    };
    let mut report = Report::default();
    let by_model = &mut report.models;
    // Answered turns per (model, provider session).
    let mut sessions: HashMap<(String, String), u64> = HashMap::new();
    // A single session file counts that chat alone.
    let chats = if logs.nectra_sessions.is_file() {
        vec![logs.nectra_sessions.clone()]
    } else {
        files(&logs.nectra_sessions, |_| true)?
    };
    // Every chat's sessions are Nectra's, whichever chats this report counts.
    let mut ours: HashSet<String> = HashSet::new();
    for path in chats {
        let Ok(text) = fs::read_to_string(&path) else {
            report.unreadable += 1;
            continue;
        };
        let counted = recent(&path);
        let mut model = String::new();
        let mut answered_by: Option<String> = None;
        for line in text.lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(id) = event["provider_session"].as_str() {
                ours.insert(id.into());
            }
            if !counted {
                continue;
            }
            match event["type"].as_str() {
                Some("user") => {
                    model = event["model"].as_str().unwrap_or("").into();
                    let entry = by_model.entry(model.clone()).or_default();
                    if answered_by.as_ref().is_some_and(|m| m != &model) {
                        entry.switches += 1;
                    }
                    entry.handoffs += u64::from(event["handoff"] == true);
                    // 0.4.4 logs mark only retries that replaced an answer.
                    entry.again += u64::from(event["again"] == true || event["replaces"] == true);
                    if let Some(why) = event["why"].as_str() {
                        let kind = why.split(" · ").next().unwrap_or(why);
                        *entry.kinds.entry(kind.into()).or_default() += 1;
                    }
                }
                Some("assistant") => {
                    answered_by = Some(model.clone());
                    let entry = by_model.entry(model.clone()).or_default();
                    entry.turns += 1;
                    match event["provider_session"].as_str() {
                        Some(id) => {
                            *sessions.entry((model.clone(), id.into())).or_default() += 1;
                        }
                        None => entry.turns_without_data += 1,
                    }
                }
                Some("failed") => {
                    let entry = by_model.entry(model.clone()).or_default();
                    entry.failed += 1;
                    match event["provider_session"].as_str() {
                        Some(id) => {
                            sessions.entry((model.clone(), id.into())).or_default();
                        }
                        None => entry.turns_without_data += 1,
                    }
                }
                Some("clear") => answered_by = None,
                _ => {}
            }
        }
    }
    let (claude, codex) = logs.index()?;
    for ((model, id), turns) in &sessions {
        let entry = by_model.entry(model.clone()).or_default();
        let Some(tokens) = session_tokens(&claude, &codex, id)? else {
            entry.turns_without_data += 1;
            continue;
        };
        entry.sessions += 1;
        entry.turns_with_data += turns;
        entry.tokens.add(tokens);
    }
    if direct {
        let mut out: BTreeMap<String, ModelUsage> = BTreeMap::new();
        // Tokens go to the model that used them; a session's turns and its
        // count go to the model that wrote most.
        let mut add = |per_model: BTreeMap<String, Tokens>, turns: u64| {
            let Some(main) = per_model
                .iter()
                .max_by_key(|(_, t)| t.output)
                .map(|(m, _)| m.clone())
            else {
                return;
            };
            if turns == 0 {
                return;
            }
            for (model, tokens) in per_model {
                out.entry(model).or_default().tokens.add(tokens);
            }
            let entry = out.entry(main).or_default();
            entry.turns += turns;
            entry.turns_with_data += turns;
            entry.sessions += 1;
        };
        // Subagent logs (agent-*.jsonl) aren't counted on either side.
        for (id, path) in &claude {
            if ours.contains(id) || id.starts_with("agent-") || !recent(path) {
                continue;
            }
            let (per_model, turns) = claude_log(path)?;
            add(per_model, turns);
        }
        for (id, path) in &codex {
            if ours.contains(id) || !recent(path) {
                continue;
            }
            let (per_model, turns) = codex_log(path)?;
            add(per_model, turns);
        }
        report.direct = Some(out);
    }
    Ok(report)
}

/// API-equivalent cost of one chat's provider sessions, and of the same tokens
/// at `baseline`'s prices. None when no session has priced token data.
pub fn chat_cost(
    logs: &Logs,
    sessions: &[(String, String)],
    baseline: &str,
) -> Result<Option<(f64, f64)>> {
    let Some(base) = price(baseline) else {
        return Ok(None);
    };
    let (claude, codex) = logs.index()?;
    let (mut cost, mut at_baseline, mut priced) = (0.0, 0.0, false);
    for (model, id) in sessions {
        let Some(p) = price(model) else { continue };
        let Some(tokens) = session_tokens(&claude, &codex, id)? else {
            continue;
        };
        priced = true;
        cost += tokens.cost(p);
        at_baseline += tokens.cost(base);
    }
    Ok(priced.then_some((cost, at_baseline)))
}

fn session_tokens(
    claude: &HashMap<String, PathBuf>,
    codex: &HashMap<String, PathBuf>,
    id: &str,
) -> Result<Option<Tokens>> {
    let per_model = if let Some(path) = claude.get(id) {
        claude_log(path)?.0
    } else if let Some(path) = codex.get(id) {
        codex_log(path)?.0
    } else {
        return Ok(None);
    };
    let mut total = Tokens::default();
    for t in per_model.into_values() {
        total.add(t);
    }
    // A log with no usage records yet is no data, not a free turn.
    Ok((total != Tokens::default()).then_some(total))
}

fn files(dir: &Path, keep: impl Fn(&str) -> bool) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".jsonl") && keep(n))
            {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn index(dir: &Path, id: impl Fn(&str) -> Option<&str>) -> Result<HashMap<String, PathBuf>> {
    Ok(files(dir, |n| id(n).is_some())?
        .into_iter()
        .filter_map(|p| {
            let key = id(p.file_name()?.to_str()?)?.to_string();
            Some((key, p))
        })
        .collect())
}

/// Tokens per model in a Claude Code session, and the messages you typed.
/// Claude Code repeats an assistant message's usage on every content block,
/// so each message id counts once.
fn claude_log(path: &Path) -> Result<(BTreeMap<String, Tokens>, u64)> {
    let mut seen = HashSet::new();
    let mut by_model: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut typed = 0;
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let Ok(entry) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let message = &entry["message"];
        if entry["type"] == "user" && entry["isMeta"] != true && entry["isSidechain"] != true {
            // Tool results and Claude Code's own <command>/<reminder> records aren't typed.
            let content = &message["content"];
            let text = content.as_str().is_some_and(|t| !t.starts_with('<'))
                || content.as_array().is_some_and(|blocks| {
                    blocks.iter().any(|b| b["type"] == "text")
                        && !blocks.iter().any(|b| b["type"] == "tool_result")
                });
            typed += u64::from(text);
            continue;
        }
        let usage = &message["usage"];
        if entry["type"] != "assistant" || usage.is_null() {
            continue;
        }
        if let Some(id) = message["id"].as_str() {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }
        let n = |k: &str| usage[k].as_u64().unwrap_or(0);
        let model = message["model"].as_str().unwrap_or("unknown").to_string();
        by_model.entry(model).or_default().add(Tokens {
            input: n("input_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            output: n("output_tokens"),
        });
    }
    Ok((by_model, typed))
}

/// A Codex session's tokens per model, and its turns. Codex logs running
/// totals, so each model gets the growth while it was in use; input counts
/// include cached input, which is split out here.
fn codex_log(path: &Path) -> Result<(BTreeMap<String, Tokens>, u64)> {
    let mut by_model: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut model = String::from("unknown");
    let mut seen = Tokens::default();
    let mut turns = 0;
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if line.contains("\"task_started\"") {
            turns += 1;
        }
        let counts = line.contains("\"token_count\"");
        if !counts && !line.contains("\"turn_context\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(m) = entry["payload"]["model"].as_str() {
            model = m.into();
        }
        let total = &entry["payload"]["info"]["total_token_usage"];
        if !counts || total.is_null() {
            continue;
        }
        let n = |k: &str| total[k].as_u64().unwrap_or(0);
        let cached = n("cached_input_tokens");
        let now = Tokens {
            input: n("input_tokens").saturating_sub(cached),
            cache_read: cached,
            cache_write: n("cache_write_input_tokens"),
            output: n("output_tokens"),
        };
        by_model.entry(model.clone()).or_default().add(Tokens {
            input: now.input.saturating_sub(seen.input),
            cache_read: now.cache_read.saturating_sub(seen.cache_read),
            cache_write: now.cache_write.saturating_sub(seen.cache_write),
            output: now.output.saturating_sub(seen.output),
        });
        seen = now;
    }
    Ok((by_model, turns))
}

pub fn usd(x: f64) -> String {
    if x >= 0.1 {
        format!("${x:.2}")
    } else {
        format!("${x:.3}")
    }
}

/// Rows for one table, with its priced cost and the turns that cost covers.
fn table(by_model: &BTreeMap<String, ModelUsage>) -> (String, f64, u64, Tokens) {
    let mut out = format!(
        "{:<16} {:>6} {:>9} {:>12} {:>12} {:>12} {:>10} {:>9}\n",
        "MODEL", "TURNS", "SESSIONS", "INPUT", "CACHE READ", "CACHE WRITE", "OUTPUT", "COST"
    );
    let (mut cost, mut priced_turns, mut priced_tokens) = (0.0, 0, Tokens::default());
    let mut total = Tokens::default();
    for (model, u) in by_model {
        total.add(u.tokens);
        let t = u.tokens;
        let price = price(model);
        if let Some(p) = price {
            cost += t.cost(p);
            priced_turns += u.turns_with_data;
            priced_tokens.add(t);
        }
        out += &format!(
            "{:<16} {:>6} {:>9} {:>12} {:>12} {:>12} {:>10} {:>9}\n",
            model,
            u.turns,
            u.sessions,
            t.input,
            t.cache_read,
            t.cache_write,
            t.output,
            price.map_or("-".into(), |p| usd(t.cost(p)))
        );
    }
    out += &format!(
        "{:<16} {:>6} {:>9} {:>12} {:>12} {:>12} {:>10} {:>9}\n",
        "total",
        "",
        "",
        total.input,
        total.cache_read,
        total.cache_write,
        total.output,
        usd(cost)
    );
    (out, cost, priced_turns, priced_tokens)
}

pub fn render(report: &Report, baseline: &str) -> String {
    let models = &report.models;
    let (mut out, cost, turns, tokens) = table(models);
    out += "COST is API-equivalent: list prices for the same tokens. Plans don't bill per token.\n";
    if let Some(base) = price(baseline).filter(|_| cost > 0.0) {
        let at_base = tokens.cost(base);
        out += &format!(
            "The same tokens at {baseline} prices: {}, so routing cost {:.0}% of that.\n",
            usd(at_base),
            100.0 * cost / at_base
        );
    }
    let sum = |f: fn(&ModelUsage) -> u64| models.values().map(f).sum::<u64>();
    let (switches, handoffs, again) = (sum(|u| u.switches), sum(|u| u.handoffs), sum(|u| u.again));
    if switches + again > 0 {
        out += &format!(
            "{switches} model switches ({handoffs} with a handoff note), {again} /again retries.\n"
        );
    }
    let missing = sum(|u| u.turns_without_data);
    if missing > 0 {
        out += &format!(
            "{missing} turns have no token data (Gemini, Ollama, ephemeral turns or deleted provider logs).\n"
        );
    }
    if report.unreadable > 0 {
        out += &format!(
            "{} chats couldn't be read and are left out{}; a chat open in a running Nectra is locked on Windows.\n",
            report.unreadable,
            if report.direct.is_some() { ", so their sessions may show as direct use" } else { "" }
        );
    }
    if let Some(direct) = &report.direct {
        let (rows, direct_cost, direct_turns, _) = table(direct);
        out += "\nDirect use: Claude Code and Codex sessions Nectra didn't start\n";
        out += &rows;
        if turns > 0 && direct_turns > 0 {
            out += &format!(
                "Per turn at list prices: {} through Nectra, {} direct. The tasks differ, so this is a rough comparison.\n",
                usd(cost / turns as f64),
                usd(direct_cost / direct_turns as f64)
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_come_from_each_providers_own_log() {
        let dir = tempfile::tempdir().unwrap();
        let logs = Logs {
            nectra_sessions: dir.path().join("nectra"),
            claude_projects: dir.path().join("claude"),
            codex_sessions: dir.path().join("codex/2026/10/11"),
        };
        for p in [
            &logs.nectra_sessions,
            &logs.claude_projects.join("proj"),
            &logs.codex_sessions,
        ] {
            fs::create_dir_all(p).unwrap();
        }
        let thread = "01a12cfd-191a-7db2-ac0c-7eb7a9f406a8";
        fs::write(
            logs.nectra_sessions.join("s.jsonl"),
            [
                r#"{"type":"header","version":1,"cwd":"x"}"#,
                r#"{"type":"user","text":"a","model":"opus"}"#,
                r#"{"type":"assistant","text":"b","provider_session":"c1"}"#,
                r#"{"type":"user","text":"c","model":"opus"}"#,
                r#"{"type":"assistant","text":"d","provider_session":"c1"}"#,
                r#"{"type":"user","text":"c2","model":"opus"}"#,
                r#"{"type":"failed","error":"x","provider_session":"c1"}"#,
                r#"{"type":"user","text":"e","model":"gpt-5.6-sol"}"#,
                &format!(r#"{{"type":"assistant","text":"f","provider_session":"{thread}"}}"#),
                r#"{"type":"user","text":"g","model":"gemini-2.5-pro"}"#,
                r#"{"type":"assistant","text":"h"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        // The same message id twice (two content blocks) counts once.
        fs::write(logs.claude_projects.join("proj/c1.jsonl"), [
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":7}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":7}}}"#,
            r#"{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":1,"cache_read_input_tokens":200,"cache_creation_input_tokens":0,"output_tokens":3}}}"#,
        ].join("\n")).unwrap();
        fs::write(logs.codex_sessions.join(format!("rollout-2026-10-11T18-02-27-{thread}.jsonl")), [
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":5}}}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"cached_input_tokens":200,"output_tokens":9}}}}"#,
        ].join("\n")).unwrap();
        let u = collect(&logs, None, false).unwrap().models;
        assert_eq!(
            (u["opus"].turns, u["opus"].sessions, u["opus"].failed),
            (2, 1, 1)
        );
        assert_eq!(
            u["opus"].tokens,
            Tokens {
                input: 11,
                cache_read: 300,
                cache_write: 5,
                output: 10
            }
        );
        assert_eq!(
            u["gpt-5.6-sol"].tokens,
            Tokens {
                input: 100,
                cache_read: 200,
                cache_write: 0,
                output: 9
            }
        );
        assert_eq!(u["gemini-2.5-pro"].turns_without_data, 1);
        let one = Logs {
            nectra_sessions: logs.nectra_sessions.join("s.jsonl"),
            ..logs
        };
        assert_eq!(
            collect(&one, None, false).unwrap().models["opus"].sessions,
            1
        );
        let report = Report {
            models: u,
            ..Default::default()
        };
        assert!(render(&report, BASELINE).contains("1 turns have no token data"));
    }

    #[test]
    fn routing_cost_and_direct_use_are_reported_side_by_side() {
        let dir = tempfile::tempdir().unwrap();
        let logs = Logs {
            nectra_sessions: dir.path().join("nectra"),
            claude_projects: dir.path().join("claude/proj"),
            codex_sessions: dir.path().join("codex"),
        };
        for p in [
            &logs.nectra_sessions,
            &logs.claude_projects,
            &logs.codex_sessions,
        ] {
            fs::create_dir_all(p).unwrap();
        }
        let luna = "01a12cfd-191a-7db2-ac0c-7eb7a9f406a8";
        fs::write(
            logs.nectra_sessions.join("s.jsonl"),
            [
                r#"{"type":"user","text":"a","model":"sonnet","why":"writing or explaining · preferred for this"}"#,
                r#"{"type":"assistant","text":"b","provider_session":"c1"}"#,
                r#"{"type":"user","text":"c","model":"gpt-5.6-luna","why":"small code change · best fit available","handoff":true}"#,
                &format!(r#"{{"type":"assistant","text":"d","provider_session":"{luna}"}}"#),
                r#"{"type":"user","text":"c","model":"opus","replaces":true,"again":true}"#,
                r#"{"type":"assistant","text":"e"}"#,
                r#"{"type":"clear"}"#,
                r#"{"type":"user","text":"f","model":"haiku"}"#,
                r#"{"type":"assistant","text":"g","provider_session":"e1"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        fs::write(logs.claude_projects.join("c1.jsonl"),
            r#"{"type":"assistant","message":{"id":"m1","model":"claude-sonnet-5-5","usage":{"input_tokens":1000000,"output_tokens":0}}}"#).unwrap();
        fs::write(logs.codex_sessions.join(format!("rollout-2026-10-17T10-00-00-{luna}.jsonl")),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000000,"cached_input_tokens":0,"output_tokens":0}}}}"#).unwrap();
        // Run without Nectra: two typed messages (tool results and reminders aren't).
        fs::write(logs.claude_projects.join("d1.jsonl"), [
            r#"{"type":"user","message":{"role":"user","content":"fix it"}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<system-reminder>x</system-reminder>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"and this"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","model":"claude-opus-5-5","usage":{"input_tokens":1000000,"output_tokens":0}}}"#,
        ].join("\n")).unwrap();
        fs::write(logs.codex_sessions.join("rollout-2026-10-17T11-00-00-11111111-2222-3333-4444-555555555555.jsonl"), [
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000000,"cached_input_tokens":1000000,"output_tokens":10}}}}"#,
            // Switched models mid-session: luna is charged only for what it added.
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-luna"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":3000000,"cached_input_tokens":1000000,"output_tokens":10}}}}"#,
        ].join("\n")).unwrap();

        // A provider log with no usage yet is no data, not a free turn.
        fs::write(
            logs.claude_projects.join("e1.jsonl"),
            r#"{"type":"user","message":{"content":"f"}}"#,
        )
        .unwrap();
        let report = collect(&logs, None, true).unwrap();
        assert_eq!(
            (
                report.models["haiku"].turns_with_data,
                report.models["haiku"].turns_without_data
            ),
            (0, 1)
        );
        let m = &report.models;
        assert_eq!(
            (m["gpt-5.6-luna"].switches, m["gpt-5.6-luna"].handoffs),
            (1, 1)
        );
        assert_eq!((m["opus"].switches, m["opus"].again), (1, 1));
        assert_eq!(m["haiku"].switches, 0, "a cleared chat starts over");
        assert_eq!(m["sonnet"].kinds["writing or explaining"], 1);
        let direct = report.direct.as_ref().unwrap();
        assert_eq!(
            (direct["claude-opus-5-5"].turns, direct["gpt-5.6-sol"].turns),
            (2, 1)
        );
        assert!(
            !direct.contains_key("claude-sonnet-5-5"),
            "Nectra's own sessions aren't direct"
        );
        assert_eq!(
            (
                direct["gpt-5.6-luna"].tokens.input,
                direct["gpt-5.6-luna"].turns
            ),
            (1_000_000, 0)
        );
        assert_eq!(direct["gpt-5.6-sol"].tokens.input, 1_000_000);

        // 1M input tokens: sonnet $2.00 + luna $0.20; the same at opus: $8.00.
        let text = render(&report, BASELINE);
        assert!(text.contains("$2.20"), "{text}");
        assert!(
            text.contains("at opus prices: $8.00, so routing cost 28% of that"),
            "{text}"
        );
        assert!(
            text.contains("2 model switches (1 with a handoff note), 1 /again retries"),
            "{text}"
        );
        // Direct: opus $4.00 + sol $5.50 + luna $0.20 over 3 turns. Nectra: $2.20
        // over the 2 turns with token data, not the opus and haiku turns without.
        assert!(
            text.contains("$1.10 through Nectra, $3.23 direct"),
            "{text}"
        );
        let chat = [("sonnet".to_string(), "c1".to_string())];
        let (cost, at_opus) = chat_cost(&logs, &chat, BASELINE).unwrap().unwrap();
        assert!((cost - 2.0).abs() < 1e-9 && (at_opus - 4.0).abs() < 1e-9);
    }
}
