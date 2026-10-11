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
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub const KEY_ENV: &str = "TYPESAFE_API_KEY";
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// How much model a prompt needs; drives the cheap/strong routing roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Simple,
    Normal,
    Demanding,
}

#[derive(Clone)]
pub struct Classification {
    pub task: TaskType,
    pub req: Requirements,
    pub tier: Option<Tier>,
    pub why: String,
    /// Whether the message starts a new task rather than continuing the
    /// previous one. None without a previous message.
    pub new_task: Option<bool>,
}

/// Classify with Jev when configured and allowed, else with local keywords.
pub fn classify(prompt: &str, cfg: &Config) -> Classification {
    classify_turn(prompt, None, cfg)
}

/// `classify`, judging the message against the previous user message so
/// short follow-ups ("now run the tests") stay with their task.
pub fn classify_turn(prompt: &str, previous: Option<&str>, cfg: &Config) -> Classification {
    let local = || {
        let (task, _, _) = classifier::classify(prompt);
        let (text, new_task) = match previous {
            // A generic follow-up borrows the previous task for classification.
            Some(prev) if task == TaskType::General => (format!("{prev}\n{prompt}"), Some(false)),
            Some(prev) => (
                prompt.to_string(),
                Some(classifier::classify(prev).0 != task),
            ),
            None => (prompt.to_string(), None),
        };
        let (task, req, why) = classifier::classify(&text);
        Classification {
            task,
            req,
            tier: None,
            why,
            new_task,
        }
    };
    let c = &cfg.inner.classifier;
    let key = api_key(&cfg.path);
    let Some(key) = key.filter(|_| allowed(cfg)) else {
        return local();
    };
    match ask(prompt, previous, &key, c.timeout_ms) {
        Ok(Answer {
            task,
            confidence,
            difficulty,
            new_task,
        }) => {
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
                new_task: new_task.map(|p| p >= 0.5),
            }
        }
        Err(e) => {
            let mut l = local();
            l.why = format!("{}; jev unavailable: {e:#}", l.why);
            l
        }
    }
}

/// Whether this configuration may send prompts to Jev at all. A local-only
/// Ollama setup chose privacy, so its prompts never leave the machine.
pub fn allowed(cfg: &Config) -> bool {
    cfg.inner.classifier.backend != ClassifierBackend::Local
        && !(cfg.ollama_enabled()
            && cfg
                .inner
                .ollama
                .as_ref()
                .is_some_and(|o| o.policy == crate::ollama::LocalPolicy::LocalOnly))
}

/// The key file lives beside config.toml, not in it, so `nectra config --print`
/// never shows it.
pub fn key_path(config_path: &Path) -> Option<PathBuf> {
    config_path.parent().map(|d| d.join("typesafe.key"))
}

/// `TYPESAFE_API_KEY` wins over the key file saved by `nectra setup`.
pub fn api_key(config_path: &Path) -> Option<String> {
    std::env::var(KEY_ENV)
        .ok()
        .or_else(|| fs::read_to_string(key_path(config_path)?).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// One tiny real call, so setup can reject a mistyped key.
pub fn verify(key: &str) -> Result<()> {
    ask("hello", None, key, 10_000).map(|_| ())
}

/// Saves the key readable only by the current user (the per-user config
/// directory already is on Windows).
pub fn save_key(config_path: &Path, key: &str) -> Result<PathBuf> {
    let path = key_path(config_path).context("configuration path has no directory")?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

struct Answer {
    task: TaskType,
    confidence: f64,
    /// Normalised to 0..1.
    difficulty: f64,
    /// Probability the message starts a new task; None without a previous message.
    new_task: Option<f64>,
}

fn ask(prompt: &str, previous: Option<&str>, key: &str, timeout_ms: u64) -> Result<Answer> {
    let (state, subject) = match previous {
        Some(prev) => (
            json!({ "previous_message": prev, "current_message": prompt }),
            "current_message (previous_message is context for short follow-ups)",
        ),
        None => (json!(prompt), "the message"),
    };
    let mut body = json!({
        "state": state,
        "model": "jev-latest",
        "questions": {
            "task": {
                "type": "choice",
                "instructions": format!("Which kind of task does {subject} ask an AI assistant to do?"),
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
                "instructions": format!("How capable does the AI model need to be to do the task in {subject} well?"),
                "criteria": [
                    "easy: a small fast model handles it",
                    "moderate: a mid-tier model handles it",
                    "hard: needs the strongest model"
                ]
            }
        }
    });
    if previous.is_some() {
        body["questions"]["new_task"] = json!({
            "type": "noul",
            "instructions": "Does current_message start a new, separate task instead of continuing the work in previous_message?",
            "criteria": {
                "true": "A different piece of work: a new goal, topic or deliverable",
                "false": "A follow-up on the same work: a fix, next step, tweak, question or confirmation about it"
            }
        });
    }
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

fn parse(resp: &Value) -> Result<Answer> {
    let a = &resp["answers"];
    let task: TaskType = serde_json::from_value(a["task"]["choice"].clone())
        .context("jev returned an unknown task type")?;
    let Some(score) = a["difficulty"]["score"].as_f64() else {
        bail!("jev returned no difficulty score");
    };
    Ok(Answer {
        task,
        confidence: a["task"]["confidence"].as_f64().unwrap_or(0.0),
        // Three levels score 0..2.
        difficulty: (score / 2.0).clamp(0.0, 1.0),
        new_task: a["new_task"]["noul"].as_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_a_real_response_shape() {
        let resp = json!({"model":"jev-1.13.0","answers":{
            "task":{"type":"choice","choice":"debugging","confidence":0.91,"probabilities":{}},
            "difficulty":{"type":"score","score":1.5,"confidence":0.4}}});
        let a = parse(&resp).unwrap();
        assert_eq!(a.task, TaskType::Debugging);
        assert!((a.confidence - 0.91).abs() < 1e-9 && (a.difficulty - 0.75).abs() < 1e-9);
        assert!(a.new_task.is_none());
        let resp = json!({"answers":{"task":{"choice":"general"},"difficulty":{"score":0.0},
            "new_task":{"type":"noul","noul":0.12}}});
        assert_eq!(parse(&resp).unwrap().new_task, Some(0.12));
    }
    #[test]
    fn local_follow_ups_continue_and_new_task_types_switch() {
        std::env::remove_var(KEY_ENV);
        let cfg = Config::from_defaults().unwrap();
        let prev = Some("debug this failing race condition");
        let c = classify_turn("thanks, do that", prev, &cfg);
        assert_eq!((c.task, c.new_task), (TaskType::Debugging, Some(false)));
        let c = classify_turn("write a short bio for my profile", prev, &cfg);
        assert_eq!(c.new_task, Some(true));
        assert_eq!(classify_turn("hello", None, &cfg).new_task, None);
    }
    #[test]
    fn rejects_unknown_task() {
        let resp = json!({"answers":{"task":{"choice":"poetry"},"difficulty":{"score":0.0}}});
        assert!(parse(&resp).is_err());
    }
    #[test]
    fn saved_key_is_found_beside_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        save_key(
            &cfg,
            "  sk-test
",
        )
        .unwrap();
        std::env::remove_var(KEY_ENV);
        assert_eq!(api_key(&cfg).as_deref(), Some("sk-test"));
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
