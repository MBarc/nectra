//! `nectra usage`: tokens per model, read from the providers' own session logs.
//!
//! Nectra records the provider session id with each answer. Claude Code and
//! Codex already log exact token usage per session, so the totals come from
//! there rather than from estimates. Gemini and Ollama turns are counted but
//! have no token data.
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

#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Tokens {
    /// Input not served from cache.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
}
impl Tokens {
    fn add(&mut self, o: Tokens) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ModelUsage {
    pub turns: u64,
    /// Provider sessions with token data; a resumed session counts once.
    pub sessions: u64,
    /// Turns whose provider session could not be found (or Gemini/Ollama).
    pub turns_without_data: u64,
    pub tokens: Tokens,
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
}

pub fn collect(logs: &Logs, days: Option<u64>) -> Result<BTreeMap<String, ModelUsage>> {
    let since = days.map(|d| SystemTime::now() - Duration::from_secs(d * 86_400));
    let mut by_model: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut sessions: HashSet<(String, String)> = HashSet::new();
    for path in files(&logs.nectra_sessions, |_| true)? {
        if since.is_some_and(|s| {
            fs::metadata(&path)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m < s)
        }) {
            continue;
        }
        let mut model = String::new();
        for line in BufReader::new(fs::File::open(&path)?).lines() {
            let Ok(event) = serde_json::from_str::<Value>(&line?) else {
                continue;
            };
            match event["type"].as_str() {
                Some("user") => model = event["model"].as_str().unwrap_or("").into(),
                Some("assistant") => {
                    let entry = by_model.entry(model.clone()).or_default();
                    entry.turns += 1;
                    match event["provider_session"].as_str() {
                        Some(id) => {
                            sessions.insert((model.clone(), id.into()));
                        }
                        None => entry.turns_without_data += 1,
                    }
                }
                _ => {}
            }
        }
    }
    // One walk of each provider's log folder, then lookups by session id.
    let claude = index(&logs.claude_projects, |name| name.strip_suffix(".jsonl"))?;
    let codex = index(&logs.codex_sessions, |name| {
        // rollout-<timestamp>-<thread uuid>.jsonl; the uuid is the last 36 chars.
        let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
        stem.get(stem.len().checked_sub(36)?..)
    })?;
    for (model, id) in sessions {
        let tokens = if let Some(path) = claude.get(&id) {
            claude_tokens(path)?
        } else if let Some(path) = codex.get(&id) {
            codex_tokens(path)?
        } else {
            by_model.entry(model).or_default().turns_without_data += 1;
            continue;
        };
        let entry = by_model.entry(model).or_default();
        entry.sessions += 1;
        entry.tokens.add(tokens);
    }
    Ok(by_model)
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

/// Claude Code repeats an assistant message's usage on every content block,
/// so count each message id once.
fn claude_tokens(path: &Path) -> Result<Tokens> {
    let mut seen = HashSet::new();
    let mut total = Tokens::default();
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let Ok(entry) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let message = &entry["message"];
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
        total.add(Tokens {
            input: n("input_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            output: n("output_tokens"),
        });
    }
    Ok(total)
}

/// Codex logs running totals; the last one is the session's. Its input count
/// includes cached input, which is split out here.
fn codex_tokens(path: &Path) -> Result<Tokens> {
    let mut last = Value::Null;
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if !line.contains("\"token_count\"") {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<Value>(&line) {
            let total = &entry["payload"]["info"]["total_token_usage"];
            if !total.is_null() {
                last = total.clone();
            }
        }
    }
    let n = |k: &str| last[k].as_u64().unwrap_or(0);
    let cached = n("cached_input_tokens");
    Ok(Tokens {
        input: n("input_tokens").saturating_sub(cached),
        cache_read: cached,
        cache_write: n("cache_write_input_tokens"),
        output: n("output_tokens"),
    })
}

pub fn render(by_model: &BTreeMap<String, ModelUsage>) -> String {
    let mut out = format!(
        "{:<16} {:>6} {:>9} {:>12} {:>12} {:>12} {:>10}\n",
        "MODEL", "TURNS", "SESSIONS", "INPUT", "CACHE READ", "CACHE WRITE", "OUTPUT"
    );
    let mut total = Tokens::default();
    let mut missing = 0;
    for (model, u) in by_model {
        total.add(u.tokens);
        missing += u.turns_without_data;
        let t = u.tokens;
        out += &format!(
            "{:<16} {:>6} {:>9} {:>12} {:>12} {:>12} {:>10}\n",
            model, u.turns, u.sessions, t.input, t.cache_read, t.cache_write, t.output
        );
    }
    out += &format!(
        "{:<16} {:>6} {:>9} {:>12} {:>12} {:>12} {:>10}\n",
        "total", "", "", total.input, total.cache_read, total.cache_write, total.output
    );
    if missing > 0 {
        out += &format!(
            "{missing} turns have no token data (Gemini, Ollama, ephemeral turns or deleted provider logs).\n"
        );
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
        let u = collect(&logs, None).unwrap();
        assert_eq!((u["opus"].turns, u["opus"].sessions), (2, 1));
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
        assert!(render(&u).contains("1 turns have no token data"));
    }
}
