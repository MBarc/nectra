//! The structured routing-decision log (schema in docs/routing.md).
//!
//! Every routing decision becomes one [`DecisionRecord`]; what happened to it
//! afterwards becomes [`OutcomeRecord`]s. Chat writes both into the session log
//! and headless puts the record in its `route` event. `nectra log decisions`
//! reads them back. The prompt is stored as a hash; its text only with the
//! explicit `[routing] log_prompt_text` opt-in.
use crate::{jev::Classification, router::Decision};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bump when a field is removed or changes meaning; adding fields does not.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CandidateRecord {
    pub model: String,
    pub provider: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionRecord {
    pub schema_version: u32,
    /// Unique per decision, stable across the log: outcomes refer to it.
    pub id: String,
    /// Unix seconds.
    pub ts: u64,
    /// "chat" or "run".
    pub source: String,
    /// Hex SHA-256 of the prompt text as routed.
    pub prompt_hash: String,
    /// Only with the opt-in; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
    /// Laya (or Jev, or keyword) task kind, e.g. "debugging".
    pub task_kind: String,
    /// "laya", "jev" or "keywords".
    pub classifier: String,
    /// "simple", "normal" or "demanding"; null for keywords.
    pub tier: Option<String>,
    /// Difficulty 0..1 after the stakes signal; null for keywords.
    pub difficulty: Option<f64>,
    /// The classifier's confidence in the task kind; null for keywords.
    pub confidence: Option<f64>,
    /// Routing role the classifier chose, if it chose one.
    pub role: Option<String>,
    /// Score gap between the pick and the runner-up; null with one candidate.
    pub margin: Option<f32>,
    pub mode: String,
    pub candidates: Vec<CandidateRecord>,
    pub pick: String,
    pub pick_provider: String,
    pub pick_score: f32,
    pub reason: String,
    /// The user pinned a model or provider.
    pub overridden: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    /// The turn ran but the tests it was checked with failed (writers that run tests).
    FailedTests,
    /// The agent or its tool failed, or returned no answer.
    ToolError,
    /// The provider's rate or usage limit stopped it.
    Limit,
    /// The user sent `/again`.
    UserAgain,
    /// The user switched to another model for the next message.
    UserSwitchModel,
    /// The user pinned a provider or mode for the next message.
    Override,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutcomeRecord {
    /// The decision this is about.
    pub decision: String,
    pub outcome: Outcome,
    /// The decision that followed from this outcome (a fallback, a retry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
}

pub fn prompt_hash(prompt: &str) -> String {
    Sha256::digest(prompt.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn new_id(prompt_hash: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seed = format!(
        "{prompt_hash}{nanos}{}{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let digest: String = Sha256::digest(seed.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("d-{digest}")
}

impl DecisionRecord {
    /// `text` is stored only when `log_text` is set.
    pub fn new(
        source: &str,
        prompt: &str,
        log_text: bool,
        classification: &Classification,
        decision: &Decision,
    ) -> Self {
        let hash = prompt_hash(prompt);
        Self {
            schema_version: SCHEMA_VERSION,
            id: new_id(&hash),
            ts: now(),
            source: source.into(),
            prompt_text: log_text.then(|| prompt.to_string()),
            prompt_hash: hash,
            task_kind: decision.task.clone(),
            classifier: classification.classifier.into(),
            tier: classification.tier.map(|t| format!("{t:?}").to_lowercase()),
            difficulty: classification.difficulty,
            confidence: classification.confidence,
            role: classification.role.clone(),
            margin: decision.score_margin,
            mode: format!("{:?}", decision.mode).to_lowercase(),
            candidates: decision
                .candidates
                .iter()
                .map(|c| CandidateRecord {
                    model: c.model.clone(),
                    provider: c.provider.clone(),
                    score: c.score,
                })
                .collect(),
            pick: decision.model.clone(),
            pick_provider: decision.provider.clone(),
            pick_score: decision.score,
            reason: decision.reason.clone(),
            overridden: decision.overridden,
        }
    }
}

/// One decision with what became of it, as `nectra log decisions` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct LoggedDecision {
    #[serde(flatten)]
    pub decision: DecisionRecord,
    /// The latest outcome recorded; null while the turn is unfinished.
    pub outcome: Option<Outcome>,
    pub follow_up: Option<String>,
    /// The chat session file it came from.
    pub session: String,
}

/// Read every decision from the session logs in `dir`, newest last. `since` is
/// unix seconds; a log that can't be read is skipped.
pub fn read_dir(dir: &std::path::Path, since: u64) -> Vec<LoggedDecision> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    paths.sort();
    let mut all = vec![];
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        all.extend(read_log(&text, &path.display().to_string()));
    }
    all.retain(|d| d.decision.ts >= since);
    all.sort_by_key(|d| d.decision.ts);
    all
}

/// The decisions and outcomes in one session log's text.
pub fn read_log(text: &str, session: &str) -> Vec<LoggedDecision> {
    let mut out: Vec<LoggedDecision> = vec![];
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("user") => {
                if let Ok(decision) = serde_json::from_value(event["decision"].clone()) {
                    out.push(LoggedDecision {
                        decision,
                        outcome: None,
                        follow_up: None,
                        session: session.into(),
                    });
                }
            }
            Some("outcome") => {
                let Ok(o) = serde_json::from_value::<OutcomeRecord>(event["outcome"].clone())
                else {
                    continue;
                };
                if let Some(d) = out.iter_mut().find(|d| d.decision.id == o.decision) {
                    d.outcome = Some(o.outcome);
                    d.follow_up = o.follow_up.or(d.follow_up.take());
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse `--since`: unix seconds, or a span such as "7d", "12h", "30m".
pub fn parse_since(text: &str, now: u64) -> anyhow::Result<u64> {
    let t = text.trim();
    if let Ok(secs) = t.parse::<u64>() {
        return Ok(secs);
    }
    let (n, unit) = t.split_at(t.len().saturating_sub(1));
    let n: u64 = n
        .parse()
        .map_err(|_| anyhow::anyhow!("--since takes unix seconds or a span like 7d, 12h, 30m"))?;
    let per = match unit {
        "d" => 86400,
        "h" => 3600,
        "m" => 60,
        _ => anyhow::bail!("--since takes unix seconds or a span like 7d, 12h, 30m"),
    };
    Ok(now.saturating_sub(n * per))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, ts: u64) -> DecisionRecord {
        DecisionRecord {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            ts,
            source: "chat".into(),
            prompt_hash: prompt_hash("fix the bug"),
            prompt_text: None,
            task_kind: "debugging".into(),
            classifier: "laya".into(),
            tier: Some("normal".into()),
            difficulty: Some(0.5),
            confidence: Some(0.9),
            role: Some("coding".into()),
            margin: Some(0.1),
            mode: "balanced".into(),
            candidates: vec![CandidateRecord {
                model: "a".into(),
                provider: "p".into(),
                score: 1.0,
            }],
            pick: "a".into(),
            pick_provider: "p".into(),
            pick_score: 1.0,
            reason: "r".into(),
            overridden: false,
        }
    }

    #[test]
    fn the_record_has_a_stable_shape_and_no_prompt_text() {
        let v = serde_json::to_value(record("d-1", 5)).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert!(v.get("prompt_text").is_none());
        assert_eq!(v["prompt_hash"].as_str().unwrap().len(), 64);
        for key in [
            "id",
            "ts",
            "source",
            "task_kind",
            "classifier",
            "tier",
            "difficulty",
            "confidence",
            "role",
            "margin",
            "mode",
            "candidates",
            "pick",
            "pick_provider",
            "pick_score",
            "reason",
            "overridden",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(
            serde_json::to_value(Outcome::UserSwitchModel).unwrap(),
            "user_switch_model"
        );
    }

    #[test]
    fn outcomes_attach_to_their_decision_and_link_the_follow_up() {
        let line = |v: serde_json::Value| format!("{v}\n");
        let mut log = String::new();
        log += &line(
            serde_json::json!({"type":"user","text":"x","model":"a","decision":record("d-1",10)}),
        );
        log += &line(
            serde_json::json!({"type":"outcome","outcome":{"decision":"d-1","outcome":"limit"}}),
        );
        log += &line(
            serde_json::json!({"type":"user","text":"x","model":"b","decision":record("d-2",11)}),
        );
        log += &line(
            serde_json::json!({"type":"outcome","outcome":{"decision":"d-1","outcome":"limit","follow_up":"d-2"}}),
        );
        log += &line(
            serde_json::json!({"type":"outcome","outcome":{"decision":"d-2","outcome":"completed"}}),
        );
        // An old-format user record has no decision and is skipped.
        log += &line(serde_json::json!({"type":"user","text":"y","model":"a"}));
        let got = read_log(&log, "s");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].outcome, Some(Outcome::Limit));
        assert_eq!(got[0].follow_up.as_deref(), Some("d-2"));
        assert_eq!(got[1].outcome, Some(Outcome::Completed));
    }

    #[test]
    fn since_takes_seconds_or_a_span() {
        assert_eq!(parse_since("1000", 5).unwrap(), 1000);
        assert_eq!(parse_since("2d", 200_000).unwrap(), 200_000 - 172_800);
        assert!(parse_since("soon", 0).is_err());
    }
}
