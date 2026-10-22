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
    borrow::Cow,
    fs,
    io::Read,
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
    /// The routing role Jev chose among those your models claim.
    pub role: Option<String>,
}

/// A routing role and the plain description Jev reads.
pub type Role = (&'static str, &'static str);

/// Classify with Jev when configured and allowed, else with local keywords.
pub fn classify(prompt: &str, cfg: &Config) -> Classification {
    classify_turn(prompt, None, cfg, &[])
}

/// The keyword classifier alone: used without a Jev key, when Jev fails, or
/// when the model is already chosen and a Jev call would be wasted.
pub fn classify_local(prompt: &str, previous: Option<&str>) -> Classification {
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
        role: None,
    }
}

/// `classify`, judging the message against the previous user message so
/// short follow-ups ("now run the tests") stay with their task, and picking a
/// role among `roles` (the ones some available model claims).
pub fn classify_turn(
    prompt: &str,
    previous: Option<&str>,
    cfg: &Config,
    roles: &[Role],
) -> Classification {
    let cwd = std::env::current_dir().ok();
    classify_turn_in(prompt, previous, cfg, roles, cwd.as_deref())
}

/// How much of each pointed-at file classification reads, and how many files.
const FILE_HEAD: u64 = 4096;
const MAX_FILES: usize = 4;

/// A prompt that hands over its task as a local text file ("Read
/// .orca-task/dispatch.md, it is your task") classifies on that file, not on
/// the pointer sentence. Returns the head of each such file, then the prompt,
/// plus a note for the routing reason (empty when nothing was read).
///
/// Conservative: only .md/.txt/.toml/.json regular files inside `root`, named
/// right after read/follow/see/open or in a short prompt that is not a
/// question, so "why does config.toml fail to parse?" still classifies on its
/// own words. Never follows URLs, network paths or symlinks.
fn with_referenced_files<'a>(prompt: &'a str, root: &Path) -> (Cow<'a, str>, String) {
    let unchanged = || (Cow::Borrowed(prompt), String::new());
    let Ok(root) = fs::canonicalize(root) else {
        return unchanged();
    };
    let short_order = prompt.len() < 400 && !prompt.trim_end().ends_with('?');
    let words: Vec<&str> = prompt.split_whitespace().collect();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for (i, raw) in words.iter().enumerate() {
        let word = raw
            .trim_matches(|c: char| "\"'`()[]<>,;:!?*".contains(c))
            .trim_end_matches('.');
        let after_verb = i > 0
            && matches!(
                words[i - 1].to_lowercase().as_str(),
                "read" | "follow" | "see" | "open"
            );
        if !(after_verb || short_order) || files.len() == MAX_FILES {
            continue;
        }
        if let Some(real) = text_file_inside(&root, word) {
            if !files.iter().any(|(_, p)| *p == real) {
                files.push((word.to_string(), real));
            }
        }
    }
    // The file goes first: it is the task, and Laya reads the opening most.
    let mut text = String::new();
    let mut used = Vec::new();
    for (name, path) in files {
        let mut head = Vec::new();
        let read = fs::File::open(&path).and_then(|f| f.take(FILE_HEAD).read_to_end(&mut head));
        if read.is_ok() {
            text.push_str(&String::from_utf8_lossy(&head));
            text.push_str("\n\n");
            used.push(name);
        }
    }
    text.push_str(prompt);
    if used.is_empty() {
        return unchanged();
    }
    let note = format!("; classified with the content of {}", used.join(", "));
    (Cow::Owned(text), note)
}

/// `word` as a regular text file under `root` (already canonical), or None.
fn text_file_inside(root: &Path, word: &str) -> Option<PathBuf> {
    // URLs and network paths are never touched.
    if word.contains("://") || word.starts_with("//") || word.starts_with("\\\\") {
        return None;
    }
    let ext = Path::new(word).extension()?.to_str()?.to_ascii_lowercase();
    if !["md", "txt", "toml", "json"].contains(&ext.as_str()) {
        return None;
    }
    let path = root.join(word);
    // A symlink is not a regular file here; canonicalize then catches ".."
    // and absolute paths that leave the working directory.
    if !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let real = fs::canonicalize(&path).ok()?;
    real.starts_with(root).then_some(real)
}

fn classify_turn_in(
    prompt: &str,
    previous: Option<&str>,
    cfg: &Config,
    roles: &[Role],
    root: Option<&Path>,
) -> Classification {
    let (text, note) = match root {
        Some(root) => with_referenced_files(prompt, root),
        None => (Cow::Borrowed(prompt), String::new()),
    };
    // File content stays on this machine: the keyword classifier and Laya
    // see it, Jev gets the prompt alone.
    let local = || {
        let mut l = classify_local(&text, previous);
        l.why.push_str(&note);
        l
    };
    let c = &cfg.inner.classifier;
    // The local model runs on this machine, so even a local-only setup may use it.
    let (source, answer) = if c.backend == ClassifierBackend::Laya {
        ("laya", crate::laya::ask(&text, previous, roles, cfg))
    } else {
        let Some(key) = api_key(&cfg.path).filter(|_| allowed(cfg)) else {
            return local();
        };
        ("jev", ask(prompt, previous, roles, &key, c.timeout_ms))
    };
    match answer {
        Ok(Answer {
            task,
            confidence,
            difficulty,
            new_task,
            role,
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
                    "{source} classified prompt as {task} (confidence {confidence:.2}, difficulty {difficulty:.2} -> {tier:?}{}){}",
                    role.as_deref().map(|r| format!(", role {r}")).unwrap_or_default(),
                    if source == "laya" { note.as_str() } else { "" }
                ),
                new_task: new_task.map(|p| p >= 0.5),
                // Only roles offered can come back; reject anything else.
                role: role.filter(|r| roles.iter().any(|(name, _)| name == r)),
            }
        }
        Err(e) => {
            let mut l = local();
            l.why = format!("{}; {source} unavailable: {e:#}", l.why);
            l
        }
    }
}

/// Whether this configuration may send prompts to Jev at all. A local-only
/// Ollama setup chose privacy, so its prompts never leave the machine.
pub fn allowed(cfg: &Config) -> bool {
    cfg.inner.classifier.backend == ClassifierBackend::Auto
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
    ask("hello", None, &[], key, 10_000).map(|_| ())
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

/// The task types and the plain descriptions Jev chooses between. The local
/// model is trained on the same list (`eval/laya/criteria.json`).
pub const TASK_CRITERIA: [(&str, &str); 10] = [
    (
        "code_generation",
        "Write new code, features, scripts, endpoints, tests or flags",
    ),
    (
        "debugging",
        "Find and fix why something is broken, failing, crashing or behaving wrongly",
    ),
    (
        "refactoring",
        "Restructure, rename or clean up existing code without changing behaviour",
    ),
    (
        "code_review",
        "Review, audit or critique existing code, a diff or a PR for quality or security",
    ),
    (
        "architecture",
        "Design system structure, components, data models or big technical trade-offs",
    ),
    (
        "research",
        "Find out facts, compare products or look up the current state of something",
    ),
    (
        "writing",
        "Produce or polish prose: emails, bios, stories, docs, letters",
    ),
    (
        "planning",
        "Lay out steps, order of work or a project plan for building or doing something",
    ),
    (
        "data_analysis",
        "Analyse data, logs, CSVs, statistics or metrics to find patterns",
    ),
    (
        "general",
        "Chit-chat, thanks, trivial questions, simple explanations or questions about the assistant itself",
    ),
];

pub(crate) struct Answer {
    pub task: TaskType,
    pub confidence: f64,
    /// Normalised to 0..1.
    pub difficulty: f64,
    /// Probability the message starts a new task; None without a previous message.
    pub new_task: Option<f64>,
    pub role: Option<String>,
}

fn ask(
    prompt: &str,
    previous: Option<&str>,
    roles: &[Role],
    key: &str,
    timeout_ms: u64,
) -> Result<Answer> {
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
                "criteria": TASK_CRITERIA.iter().map(|(name, about)| (name.to_string(), json!(about))).collect::<serde_json::Map<_, _>>()
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
    // Only the roles your models claim, so Jev can't pick one nobody holds.
    if roles.len() > 1 {
        body["questions"]["role"] = json!({
            "type": "choice",
            "instructions": format!("Which kind of model should handle {subject}?"),
            "criteria": roles.iter().map(|(name, about)| (name.to_string(), json!(about))).collect::<serde_json::Map<_, _>>(),
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
        role: a["role"]["choice"].as_str().map(String::from),
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
        let resp = json!({"answers":{"task":{"choice":"general"},"difficulty":{"score":0.0},
            "role":{"type":"choice","choice":"simple"}}});
        assert_eq!(parse(&resp).unwrap().role.as_deref(), Some("simple"));
    }
    #[test]
    fn local_follow_ups_continue_and_new_task_types_switch() {
        std::env::remove_var(KEY_ENV);
        let cfg = Config::from_defaults().unwrap();
        let prev = Some("debug this failing race condition");
        let c = classify_turn("thanks, do that", prev, &cfg, &[]);
        assert_eq!((c.task, c.new_task), (TaskType::Debugging, Some(false)));
        let c = classify_turn("write a short bio for my profile", prev, &cfg, &[]);
        assert_eq!(c.new_task, Some(true));
        assert_eq!(classify_turn("hello", None, &cfg, &[]).new_task, None);
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

    const SPEC: &str =
        "Design the system architecture: components, data model and the scalability tradeoffs.";
    const POINTER: &str =
        "Read .orca-task/dispatch-x.md (your Orca dispatch: preamble and TASK). Report with orca orchestration send";

    fn turn(prompt: &str, root: &Path) -> Classification {
        std::env::remove_var(KEY_ENV);
        classify_turn_in(
            prompt,
            None,
            &Config::from_defaults().unwrap(),
            &[],
            Some(root),
        )
    }

    #[test]
    fn a_pointer_to_a_spec_classifies_as_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".orca-task")).unwrap();
        fs::write(dir.path().join(".orca-task/dispatch-x.md"), SPEC).unwrap();
        let alone = classify_local(POINTER, None).task;
        assert_ne!(alone, TaskType::Architecture);
        let c = turn(POINTER, dir.path());
        assert_eq!(c.task, TaskType::Architecture);
        assert!(c.why.contains("dispatch-x.md"), "{}", c.why);
    }
    #[test]
    fn files_outside_the_working_directory_are_ignored() {
        let outside = tempfile::tempdir().unwrap();
        let spec = outside.path().join("spec.md");
        fs::write(&spec, SPEC).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let abs = format!("Read {} and do it", spec.display());
        let rel = format!(
            "Read ../{}/spec.md and do it",
            outside.path().file_name().unwrap().to_str().unwrap()
        );
        for prompt in [abs, rel] {
            let c = turn(&prompt, dir.path());
            assert_eq!(c.task, classify_local(&prompt, None).task, "{prompt}");
            assert!(!c.why.contains("spec.md"), "{}", c.why);
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_files_are_ignored() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("spec.md"), SPEC).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("spec.md"), dir.path().join("spec.md"))
            .unwrap();
        assert!(!turn("Read spec.md", dir.path()).why.contains("spec.md"));
    }
    #[test]
    fn a_question_about_a_file_classifies_on_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/router.rs"), SPEC).unwrap();
        fs::write(dir.path().join("config.toml"), SPEC).unwrap();
        for prompt in [
            "why does src/router.rs panic?",
            "why does config.toml fail to parse?",
        ] {
            let c = turn(prompt, dir.path());
            assert_eq!(c.task, TaskType::Debugging, "{prompt}");
            assert!(c.why.starts_with("local heuristic") && !c.why.contains("content of"));
        }
    }
    #[test]
    fn a_missing_file_falls_back_to_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let c = turn(POINTER, dir.path());
        assert_eq!(c.task, classify_local(POINTER, None).task);
        assert!(!c.why.contains("content of"));
    }
    #[test]
    fn oversized_files_are_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!("{}{SPEC}", "x".repeat(FILE_HEAD as usize));
        fs::write(dir.path().join("big.md"), big).unwrap();
        let (text, note) = with_referenced_files("Read big.md", dir.path());
        assert!(note.contains("big.md"));
        assert!(text.len() <= "Read big.md".len() + 2 + FILE_HEAD as usize);
        assert!(!text.contains("architecture"));
    }
}
