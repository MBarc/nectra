//! Optional Jev (TypeSafe System One) classifier.
//!
//! One call answers two questions about the prompt: which task type it is and
//! how capable a model it needs. Any failure (no key, offline, timeout, bad
//! answer) falls back to the local keyword classifier, so routing never
//! depends on the network.
use crate::{
    classifier::{self, Requirements, TaskType},
    config::{ClassifierBackend, Config},
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const KEY_ENV: &str = "TYPESAFE_API_KEY";
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// How much model a prompt needs; drives the cheap/strong routing roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Simple,
    Normal,
    Demanding,
}

pub struct Classification {
    pub task: TaskType,
    pub req: Requirements,
    pub tier: Option<Tier>,
    pub why: String,
}

/// Classify with Jev when configured and allowed, else with local keywords.
pub fn classify(prompt: &str, cfg: &Config) -> Classification {
    let local = || {
        let (task, req, why) = classifier::classify(prompt);
        Classification {
            task,
            req,
            tier: None,
            why,
        }
    };
    let c = &cfg.inner.classifier;
    let key = std::env::var(KEY_ENV).ok().filter(|k| !k.trim().is_empty());
    // A local-only Ollama setup chose privacy; never send its prompts out.
    let local_only = cfg.ollama_enabled()
        && cfg
            .inner
            .ollama
            .as_ref()
            .is_some_and(|o| o.policy == crate::ollama::LocalPolicy::LocalOnly);
    let Some(key) = key.filter(|_| c.backend != ClassifierBackend::Local && !local_only) else {
        return local();
    };
    match ask(prompt, &key, c.timeout_ms) {
        Ok((task, confidence, difficulty)) => {
            let tier = if difficulty < c.simple_below {
                Tier::Simple
            } else if difficulty > c.demanding_above {
                Tier::Demanding
            } else {
                Tier::Normal
            };
            Classification {
                task,
                req: classifier::requirements(task),
                tier: Some(tier),
                why: format!(
                    "jev classified prompt as {task} (confidence {confidence:.2}, difficulty {difficulty:.2} -> {tier:?})"
                ),
            }
        }
        Err(e) => {
            let mut l = local();
            l.why = format!("{}; jev unavailable: {e:#}", l.why);
            l
        }
    }
}

/// Returns task, its confidence, and difficulty normalised to 0..1.
fn ask(prompt: &str, key: &str, timeout_ms: u64) -> Result<(TaskType, f64, f64)> {
    let body = json!({
        "state": prompt,
        "model": "jev-latest",
        "questions": {
            "task": {
                "type": "choice",
                "instructions": "Which kind of task is the user asking an AI assistant to do?",
                "criteria": {
                    "code_generation": "Write new code, features, scripts, endpoints, tests or flags",
                    "debugging": "Find and fix why something is broken, failing, crashing or behaving wrongly",
                    "refactoring": "Restructure, rename or clean up existing code without changing behaviour",
                    "code_review": "Review, audit or critique existing code, a diff or a PR for quality or security",
                    "architecture": "Design system structure, components, data models or big technical trade-offs",
                    "research": "Find out facts, compare products or look up the current state of something",
                    "writing": "Produce or polish prose: emails, bios, stories, docs, letters",
                    "planning": "Lay out steps, order of work or a project plan for building or doing something",
                    "data_analysis": "Analyse data, logs, CSVs, statistics or metrics to find patterns",
                    "general": "Chit-chat, thanks, trivial questions, simple explanations or questions about the assistant itself"
                }
            },
            "difficulty": {
                "type": "score",
                "instructions": "How capable does the AI model need to be to do this task well?",
                "criteria": [
                    "easy: a small fast model handles it",
                    "moderate: a mid-tier model handles it",
                    "hard: needs the strongest model"
                ]
            }
        }
    });
    let resp: Value = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?
        .post(ENDPOINT)
        .bearer_auth(key)
        .json(&body)
        .send()?
        .error_for_status()?
        .json()?;
    parse(&resp)
}

fn parse(resp: &Value) -> Result<(TaskType, f64, f64)> {
    let a = &resp["answers"];
    let task: TaskType = serde_json::from_value(a["task"]["choice"].clone())
        .context("jev returned an unknown task type")?;
    let confidence = a["task"]["confidence"].as_f64().unwrap_or(0.0);
    let Some(score) = a["difficulty"]["score"].as_f64() else {
        bail!("jev returned no difficulty score");
    };
    // Three levels score 0..2.
    Ok((task, confidence, (score / 2.0).clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_a_real_response_shape() {
        let resp = json!({"model":"jev-1.13.0","answers":{
            "task":{"type":"choice","choice":"debugging","confidence":0.91,"probabilities":{}},
            "difficulty":{"type":"score","score":1.5,"confidence":0.4}}});
        let (task, conf, diff) = parse(&resp).unwrap();
        assert_eq!(task, TaskType::Debugging);
        assert!((conf - 0.91).abs() < 1e-9 && (diff - 0.75).abs() < 1e-9);
    }
    #[test]
    fn rejects_unknown_task() {
        let resp = json!({"answers":{"task":{"choice":"poetry"},"difficulty":{"score":0.0}}});
        assert!(parse(&resp).is_err());
    }
    #[test]
    fn falls_back_to_local_without_key() {
        std::env::remove_var(KEY_ENV);
        let cfg = Config::from_defaults().unwrap();
        let c = classify("debug this failing race condition", &cfg);
        assert_eq!(c.task, TaskType::Debugging);
        assert!(c.tier.is_none() && c.why.starts_with("local heuristic"));
    }
}
