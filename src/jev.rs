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
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const KEY_ENV: &str = "TYPESAFE_API_KEY";
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// A small prompt-only signal for work whose consequences or coordination are
/// easy for the three-way difficulty head to understate. This deliberately
/// rewards combinations of spec shape and concrete risk language; length by
/// itself has little effect.
fn stakes_score(prompt: &str) -> f64 {
    let lower = prompt.to_lowercase();
    let length = prompt.chars().count();
    let mut score = match length {
        1_200.. => 0.20,
        700.. => 0.15,
        400.. => 0.10,
        _ => 0.0,
    };

    let requirements = prompt
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("- ")
                || line.starts_with("* ")
                // "1. " or "12) ", not a year ("2026.") or a number ("3.14").
                || line.find(['.', ')']).is_some_and(|end| {
                    (1..=2).contains(&end)
                        && line[..end].chars().all(|c| c.is_ascii_digit())
                        && line[end + 1..].starts_with(' ')
                })
        })
        .count();
    score += 0.05 * requirements.min(4) as f64;

    const CONSTRAINTS: &str = concat!(
        "must never|under concurrency|rollback|security review|security-critical|",
        "security-relevant|idempotent|idempotency|fail closed|data loss|constant time|",
        "replay|untrusted|hostile|revoke|rotation|token issuance|path traversal|",
        "zip-slip|symlink|csrf|pkce|hmac|dedupe|double-charge|double charge|double payout|",
        "processed twice|exactly once",
    );
    let constraints = CONSTRAINTS
        .split('|')
        .filter(|phrase| lower.contains(*phrase))
        .count();
    score += 0.12 * constraints.min(5) as f64;
    // Everyday words in ordinary work too ("add a migration"): a little,
    // and never enough alone to lift the cap below.
    let generic = ["transaction", "migration", "concurrent"]
        .iter()
        .filter(|w| lower.contains(*w))
        .count();
    score += 0.04 * generic as f64;

    let path_refs = lower
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && !"./\\~_-".contains(c)))
        .filter(|word| {
            // Not URLs, and not "and/or" or "i/o": a path has an extension,
            // two separators, or a relative start.
            !word.contains("://")
                && !word.starts_with("www.")
                && (".rs .py .ts .tsx .js .toml .json .md"
                    .split_whitespace()
                    .any(|suffix| word.ends_with(suffix) && word.len() > suffix.len())
                    || word.matches(['/', '\\']).count() >= 2
                    || ["./", "../", "~/"].iter().any(|p| word.starts_with(p)))
        })
        .take(2)
        .count();
    // No "client": "client and server" is one system, not two components.
    let named_components =
        " component| module| service| worker| api| database| frontend| backend| server"
            .split('|')
            .filter(|name| lower.contains(*name))
            .take(2)
            .count();
    if path_refs + named_components >= 2 {
        score += 0.15;
    }

    if constraints == 0 {
        score = score.min(0.20);
    }
    score.min(1.0)
}

#[cfg(test)]
thread_local! {
    /// Where this test thread's Jev requests go instead of TypeSafe.
    static TEST_ENDPOINT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn endpoint() -> String {
    #[cfg(test)]
    if let Some(url) = TEST_ENDPOINT.with(|e| e.borrow().clone()) {
        return url;
    }
    ENDPOINT.to_string()
}

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

/// How much of each pointed-at file classification reads, in all, and how
/// many files.
const FILE_HEAD: u64 = 4096;
const TOTAL_HEAD: u64 = 8192;
const MAX_FILES: usize = 4;

/// How far into a pointed-at file classification looks for an Orca dispatch's
/// task marker.
const SCAN: u64 = 64 * 1024;
/// An Orca dispatch: a long preamble of CLI instructions, then the task.
const TASK_MARKER: &str = "=== TASK ===";

/// The part of a pointed-at file that says what to do: an Orca dispatch's
/// TASK section (to the next `=== ... ===` heading) and true, or else the
/// whole file and false.
fn task_part(file: &str) -> (&str, bool) {
    let Some(at) = file.find(TASK_MARKER) else {
        return (file, false);
    };
    let task = &file[at + TASK_MARKER.len()..];
    let end = task.find("\n=== ").unwrap_or(task.len());
    (task[..end].trim(), true)
}

/// A prompt that hands over its task as a local text file ("Read
/// .orca-task/dispatch.md, it is your task") classifies on that file, not on
/// the pointer sentence. Returns the head of each such file (an Orca
/// dispatch's TASK section, never its boilerplate preamble), then the prompt;
/// a note for the routing reason (empty when nothing was read); and the text
/// the stakes score reads: the dispatch tasks, or else the prompt alone,
/// since an arbitrary file's head says little about what is at stake.
///
/// Conservative: only .md/.txt/.toml/.json regular files inside `root`, named
/// within three words after read/follow/see/open/do/implement or as one of the
/// first two words of a short prompt. Questions ("why does config.toml fail
/// to parse", with or without "?") never expand. Never follows URLs, network
/// paths or symlinks.
fn with_referenced_files<'a>(
    prompt: &'a str,
    root: &Path,
) -> (Cow<'a, str>, String, Cow<'a, str>) {
    let unchanged = || (Cow::Borrowed(prompt), String::new(), Cow::Borrowed(prompt));
    let words: Vec<&str> = prompt.split_whitespace().collect();
    let first = words.first().map(|w| w.to_lowercase()).unwrap_or_default();
    let question = prompt.trim_end().ends_with('?')
        || [
            "why", "how", "what", "when", "where", "which", "who", "is", "are", "does", "can",
            "could", "should", "would", "will",
        ]
        .contains(&first.trim_end_matches(|c: char| !c.is_alphanumeric()));
    let Ok(root) = fs::canonicalize(root) else {
        return unchanged();
    };
    if question {
        return unchanged();
    }
    let short = prompt.len() < 400;
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for (i, raw) in words.iter().enumerate() {
        let word = raw
            .trim_matches(|c: char| "\"'`()[]<>,;:!?*".contains(c))
            .trim_end_matches('.');
        let after_verb = words[i.saturating_sub(3)..i].iter().any(|w| {
            matches!(
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric()),
                "read" | "follow" | "see" | "open" | "do" | "implement"
            )
        });
        if !(after_verb || short && i < 2) || files.len() == MAX_FILES {
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
    let mut tasks = String::new();
    let mut used = Vec::new();
    let mut budget = TOTAL_HEAD;
    for (name, path) in files {
        let mut raw = Vec::new();
        let read = fs::File::open(&path).and_then(|f| f.take(SCAN).read_to_end(&mut raw));
        let raw = String::from_utf8_lossy(&raw);
        let (part, dispatch) = task_part(&raw);
        let mut take = FILE_HEAD.min(budget) as usize;
        while !part.is_char_boundary(take.min(part.len())) {
            take -= 1;
        }
        let head = &part[..take.min(part.len())];
        if read.is_ok() && !head.is_empty() {
            budget -= head.len() as u64;
            text.push_str(head);
            text.push_str("\n\n");
            if dispatch {
                tasks.push_str(head);
                tasks.push_str("\n\n");
            }
            let what = if dispatch { "TASK, " } else { "" };
            used.push(format!("{name} ({what}{} bytes)", head.len()));
        }
        if budget == 0 {
            break;
        }
    }
    if used.is_empty() {
        return unchanged();
    }
    text.push_str(prompt);
    let stakes = if tasks.is_empty() {
        Cow::Borrowed(prompt)
    } else {
        Cow::Owned(tasks)
    };
    if std::env::var_os("NECTRA_DEBUG").is_some() {
        eprintln!(
            "nectra: debug: classifying with file content: {}",
            used.join(", ")
        );
    }
    let note = format!("; classified with the content of {}", used.join(", "));
    (Cow::Owned(text), note, stakes)
}

/// Whether `word` may be looked up on disk at all: only plain relative
/// paths. Anything with a root, a drive or a device/UNC prefix (`C:x.md`,
/// `\\?\`, `\\.\`, and mixed-slash `/\host\share`) would replace `root` on
/// join, and on Windows merely asking for its metadata can open an SMB
/// connection that leaks the user's NTLM hash. Pure: touches no filesystem.
fn plain_relative(word: &str) -> bool {
    let path = Path::new(word);
    !(word.starts_with(['/', '\\'])
        || word.contains(':')
        || path.has_root()
        || path.is_absolute()
        || matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_) | std::path::Component::RootDir)
        ))
}

/// `word` as a regular text file under `root` (already canonical), or None.
fn text_file_inside(root: &Path, word: &str) -> Option<PathBuf> {
    // URLs, network paths and anything absolute are never touched.
    if word.contains("://")
        || word.starts_with("//")
        || word.starts_with("\\\\")
        || !plain_relative(word)
    {
        return None;
    }
    let ext = Path::new(word).extension()?.to_str()?.to_ascii_lowercase();
    if !["md", "txt", "toml", "json"].contains(&ext.as_str()) {
        return None;
    }
    let path = root.join(word);
    // A symlink is not a regular file here; canonicalize then catches ".."
    // escapes that leave the working directory.
    if !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let real = fs::canonicalize(&path).ok()?;
    real.starts_with(root).then_some(real)
}

/// `classify_turn`, with the directory pointed-at files are read from
/// (`None` reads none: `nectra eval` scores the prompt alone). `--no-tools`
/// reads none either: that turn promises no file access, and Jev would carry
/// the file heads off the machine.
pub fn classify_turn_in(
    prompt: &str,
    previous: Option<&str>,
    cfg: &Config,
    roles: &[Role],
    root: Option<&Path>,
) -> Classification {
    let (text, note, stakes_text) = match root.filter(|_| !cfg.no_tools) {
        Some(root) => with_referenced_files(prompt, root),
        None => (Cow::Borrowed(prompt), String::new(), Cow::Borrowed(prompt)),
    };
    classify_text(&text, &note, &stakes_text, previous, cfg, roles)
}

#[cfg(test)]
thread_local! {
    /// A stand-in Laya for this test thread: the difficulty it answers, and
    /// every text it was asked to classify.
    static TEST_LAYA: std::cell::RefCell<Option<(f64, Vec<String>)>> = const { std::cell::RefCell::new(None) };
}

fn ask_laya(text: &str, previous: Option<&str>, roles: &[Role], cfg: &Config) -> Result<Answer> {
    #[cfg(test)]
    if let Some(difficulty) = TEST_LAYA.with(|l| {
        l.borrow_mut().as_mut().map(|(d, seen)| {
            seen.push(text.to_string());
            *d
        })
    }) {
        return Ok(Answer {
            task: TaskType::CodeGeneration,
            confidence: 1.0,
            difficulty,
            new_task: None,
            role: None,
        });
    }
    crate::laya::ask(text, previous, roles, cfg)
}

fn classify_text(
    text: &str,
    note: &str,
    stakes_text: &str,
    previous: Option<&str>,
    cfg: &Config,
    roles: &[Role],
) -> Classification {
    // Every backend classifies on the expanded text. Jev already receives
    // full prompts, so it may see the file heads too; a local-only setup never
    // reaches Jev (`allowed`).
    let local = || {
        let mut l = classify_local(text, previous);
        l.why.push_str(note);
        l
    };
    let c = &cfg.inner.classifier;
    // The local model runs on this machine, so even a local-only setup may use it.
    let (source, answer) = if c.backend == ClassifierBackend::Laya {
        ("laya", ask_laya(text, previous, roles, cfg))
    } else {
        let Some(key) = api_key(&cfg.path).filter(|_| allowed(cfg)) else {
            return local();
        };
        ("jev", ask(text, previous, roles, &key, c.timeout_ms))
    };
    match answer {
        Ok(Answer {
            task,
            confidence,
            difficulty: raw_difficulty,
            new_task,
            role,
        }) => {
            // `[routing] stakes_weight`, fitted on eval/laya/long-spec-tuning.json
            // by tools/laya/sweep-stakes; 0 turns the signal off.
            let weight = cfg.inner.routing.stakes_weight;
            let stakes = (source == "laya" && weight > 0.0).then(|| stakes_score(stakes_text));
            let difficulty = stakes.map_or(raw_difficulty, |score| {
                (raw_difficulty + weight * score).min(1.0)
            });
            let difficulty_summary = stakes.map_or_else(
                || format!("{difficulty:.2}"),
                |score| {
                    format!(
                        "{raw_difficulty:.2} + {weight:.3} x stakes {score:.2} = {difficulty:.2}"
                    )
                },
            );
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
                    "{source} classified prompt as {task} (confidence {confidence:.2}, difficulty {difficulty_summary} -> {tier:?}{}){}",
                    role.as_deref().map(|r| format!(", role {r}")).unwrap_or_default(),
                    note
                ),
                new_task: new_task.map(|p| p >= 0.5),
                // Only roles offered can come back; reject anything else.
                role: role.filter(|r| roles.iter().any(|(name, _)| name == r)),
            }
        }
        Err(e) => {
            let mut l = local();
            // Laya's errors already name themselves ("Laya still loading").
            let reason = if source == "laya" {
                format!("{e:#}")
            } else {
                format!("{source} unavailable: {e:#}")
            };
            l.why = format!("keywords · {reason}; {}", l.why);
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
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".typesafe.key.{}-{nonce}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(key.as_bytes())?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
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
        .post(endpoint())
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
    fn stakes_reward_risky_specs_without_treating_length_as_risk() {
        let routine = "Update the README with a short setup example. ".repeat(30);
        let risky = "Implement the payment worker under concurrency. The operation must never run twice; make it idempotent, use one database transaction, and rollback on failure.";
        assert_eq!(stakes_score(&routine), 0.20);
        assert!(stakes_score(risky) >= 0.60, "{}", stakes_score(risky));
        assert!(stakes_score("write a limerick") < 0.10);
    }
    #[test]
    fn stakes_ignore_years_decimals_slashes_urls_and_everyday_words() {
        let risky = "Fix the handler; it must never double charge.";
        let base = stakes_score(risky);
        // Lines starting with a year or a decimal are not numbered requirements.
        assert_eq!(stakes_score(&format!("{risky}\n2026. was a good year\n3.14 is pi")), base);
        assert!(stakes_score(&format!("{risky}\n1. one\n2) two")) > base);
        // "and/or", "I/O" and URLs are not paths; two real paths are.
        let paths = |words: &str| stakes_score(&format!("{risky} {words}")) > base;
        assert!(!paths("and/or I/O"));
        assert!(!paths("https://a.example/x/y.md www.example.com/a/b"));
        assert!(paths("src/router.rs and ./scripts/gate"));
        // Client and server are one system, not two components.
        assert!(!paths("the client and the server"));
        // Everyday words alone stay under the cap.
        assert_eq!(stakes_score(&"Add a migration and a transaction for concurrent jobs. ".repeat(30)), 0.20);
        assert!(stakes_score("Add a migration") < 0.05);
    }
    #[test]
    fn stakes_and_blend_never_exceed_one() {
        let everything = format!(
            "{}\n1. a\n2. b\n3. c\n4. d\nsrc/a.rs src/b.rs",
            "must never under concurrency rollback security review idempotent data loss replay hostile ".repeat(20)
        );
        assert_eq!(stakes_score(&everything), 1.0);
        let mut cfg = Config::from_defaults().unwrap();
        cfg.inner.classifier.backend = ClassifierBackend::Laya;
        cfg.inner.routing.stakes_weight = 1.0;
        TEST_LAYA.with(|l| *l.borrow_mut() = Some((0.95, Vec::new())));
        let c = classify_turn_in(&everything, None, &cfg, &[], None);
        TEST_LAYA.with(|l| *l.borrow_mut() = None);
        assert!(c.why.contains("= 1.00 -> Demanding"), "{}", c.why);
    }
    #[test]
    fn stakes_weight_zero_leaves_laya_difficulty_alone() {
        let risky = "Implement the payment worker under concurrency. It must never double charge; make it idempotent and rollback on failure.";
        let mut cfg = Config::from_defaults().unwrap();
        cfg.inner.classifier.backend = ClassifierBackend::Laya;
        let tier = |cfg: &Config| {
            TEST_LAYA.with(|l| *l.borrow_mut() = Some((0.5, Vec::new())));
            let c = classify_turn_in(risky, None, cfg, &[], None);
            TEST_LAYA.with(|l| *l.borrow_mut() = None);
            c
        };
        let on = tier(&cfg);
        assert_eq!(on.tier, Some(Tier::Demanding), "{}", on.why);
        cfg.inner.routing.stakes_weight = 0.0;
        let off = tier(&cfg);
        assert_eq!(off.tier, Some(Tier::Normal), "{}", off.why);
        assert!(off.why.contains("difficulty 0.50 -> Normal") && !off.why.contains("stakes"), "{}", off.why);
    }
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(key_path(&cfg).unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
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
            "why does config.toml fail to parse",
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
    fn no_tools_never_reads_pointed_at_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secrets.json"), r#"{"token": "hunter2"}"#).unwrap();
        std::env::remove_var(KEY_ENV);
        let mut cfg = Config::from_defaults().unwrap();
        cfg.no_tools = true;
        let c = classify_turn_in("Read secrets.json", None, &cfg, &[], Some(dir.path()));
        assert!(!c.why.contains("secrets.json"), "{}", c.why);
        cfg.no_tools = false;
        let c = classify_turn_in("Read secrets.json", None, &cfg, &[], Some(dir.path()));
        assert!(c.why.contains("secrets.json"), "{}", c.why);
    }
    /// The body of one HTTP request to `server`, answered with a 500 so
    /// classification falls back to keywords.
    fn capture_request(server: &std::net::TcpListener) -> String {
        use std::io::Write;
        let (mut s, _) = server.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            let Some(end) = text.find("\r\n\r\n") else {
                continue;
            };
            let len = text
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if n == 0 || buf.len() >= end + 4 + len {
                s.write_all(b"HTTP/1.1 500 x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .unwrap();
                return text;
            }
        }
    }
    #[test]
    fn no_tools_jev_request_carries_no_file_content() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secrets.json"), r#"{"token": "hunter2"}"#).unwrap();
        fs::write(dir.path().join("typesafe.key"), "test-key").unwrap();
        let mut cfg = Config::from_defaults().unwrap();
        cfg.path = dir.path().join("config.toml");
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        TEST_ENDPOINT.with(|e| *e.borrow_mut() = Some(url));
        let body = |cfg: &Config| {
            std::thread::scope(|s| {
                let got = s.spawn(|| capture_request(&server));
                classify_turn_in("Read secrets.json", None, cfg, &[], Some(dir.path()));
                got.join().unwrap()
            })
        };
        // With tools the file goes to Jev, so the intercept can see a leak.
        assert!(body(&cfg).contains("hunter2"));
        cfg.no_tools = true;
        let sent = body(&cfg);
        assert!(
            sent.contains("Read secrets.json") && !sent.contains("hunter2"),
            "{sent}"
        );
        TEST_ENDPOINT.with(|e| *e.borrow_mut() = None);
    }
    /// An Orca dispatch the size of a real one: the TASK starts past FILE_HEAD.
    fn dispatch(task: &str) -> String {
        let preamble = "Run `orca orchestration send --type status` to report. Never use the Bash tool; security review of the CLI is done.\n".repeat(45);
        format!("You are an Orca worker.\n{preamble}\n=== AFTER YOU SEND worker_done ===\nStop.\n\n=== TASK ===\n{task}\n")
    }
    #[test]
    fn a_dispatch_classifies_on_its_task_not_its_preamble() {
        let task = "Implement the payment worker under concurrency. It must never double charge; make it idempotent and rollback on failure.";
        let file = dispatch(task);
        assert!(file.find(TASK_MARKER).unwrap() > FILE_HEAD as usize);
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".orca-task")).unwrap();
        fs::write(dir.path().join(".orca-task/dispatch-x.md"), &file).unwrap();
        let (text, note, stakes) = with_referenced_files(POINTER, dir.path());
        assert!(text.starts_with(task) && !text.contains("orca orchestration send --type"), "{text}");
        assert!(note.contains("TASK"), "{note}");
        assert_eq!(stakes.trim(), task);
        // Laya sees the task; stakes scores it, not the preamble's boilerplate.
        let mut cfg = Config::from_defaults().unwrap();
        cfg.inner.classifier.backend = ClassifierBackend::Laya;
        TEST_LAYA.with(|l| *l.borrow_mut() = Some((0.5, Vec::new())));
        let c = classify_turn_in(POINTER, None, &cfg, &[], Some(dir.path()));
        let seen = TEST_LAYA.with(|l| l.borrow_mut().take().unwrap().1);
        assert!(seen[0].starts_with(task), "{}", seen[0]);
        assert!(c.why.contains(&format!("stakes {:.2}", stakes_score(task))), "{}", c.why);
        assert_eq!(c.tier, Some(Tier::Demanding));
        // A plain file is read for Laya, but stakes scores the prompt alone.
        fs::write(dir.path().join(".orca-task/dispatch-x.md"), file.replace(TASK_MARKER, "TASK")).unwrap();
        let (text, _, stakes) = with_referenced_files(POINTER, dir.path());
        assert!(text.starts_with("You are an Orca worker.") && stakes == POINTER);
    }
    #[test]
    fn oversized_files_are_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!("{}{SPEC}", "x".repeat(FILE_HEAD as usize));
        fs::write(dir.path().join("big.md"), big).unwrap();
        let (text, note, _) = with_referenced_files("Read big.md", dir.path());
        assert!(note.contains("big.md"));
        assert!(text.len() <= "Read big.md".len() + 2 + FILE_HEAD as usize);
        assert!(!text.contains("architecture"));
    }
    #[test]
    fn only_pointers_expand() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".orca-task")).unwrap();
        for f in [
            ".orca-task/dispatch-x.md",
            "CHANGELOG.md",
            "config.toml",
            "spec.md",
        ] {
            fs::write(dir.path().join(f), SPEC).unwrap();
        }
        let expands = |p: &str| !with_referenced_files(p, dir.path()).1.is_empty();
        assert!(expands(POINTER));
        assert!(expands(
            "Read .orca-task/dispatch-x.md (your Orca dispatch: preamble and TASK)... Report with orca orchestration send"
        ));
        assert!(expands("please implement the spec.md plan"));
        assert!(expands("spec.md is your task"));
        assert!(!expands("Add a release section to CHANGELOG.md"));
        assert!(!expands("why does config.toml fail to parse"));
        assert!(!expands("why does config.toml fail to parse?"));
        assert!(!expands("What is in spec.md"));
        assert!(!expands("the build broke after someone edited config.toml"));
    }
    #[test]
    fn file_content_is_capped_in_total() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a.md", "b.md", "c.md", "d.md"] {
            fs::write(dir.path().join(f), "y".repeat(FILE_HEAD as usize)).unwrap();
        }
        let prompt = "Read a.md, b.md, c.md, d.md";
        let (text, note, _) = with_referenced_files(prompt, dir.path());
        assert!(
            text.len() <= prompt.len() + 4 + TOTAL_HEAD as usize,
            "{}",
            text.len()
        );
        assert!(note.contains("b.md") && !note.contains("c.md"), "{note}");
    }
    #[test]
    fn paths_escaping_the_root_are_refused() {
        let outside = tempfile::tempdir().unwrap();
        let spec = outside.path().join("spec.md");
        fs::write(&spec, SPEC).unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("inside.md"), SPEC).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let name = outside.path().file_name().unwrap().to_str().unwrap();
        assert!(text_file_inside(&root, "inside.md").is_some());
        for word in [
            spec.display().to_string(),
            format!("../{name}/spec.md"),
            format!(r"..\{name}\spec.md"),
            "https://example.com/spec.md".into(),
            "//server/share/spec.md".into(),
            r"\\server\share\spec.md".into(),
        ] {
            assert!(text_file_inside(&root, &word).is_none(), "{word}");
        }
        // Refused by the pure pre-check, before any filesystem call can open
        // an SMB connection.
        for word in [
            spec.display().to_string(),
            r"/\localhost\c$\spec.md".into(),
            r"\/localhost/c$/spec.md".into(),
            r"/\127.0.0.1\c$\spec.md".into(),
            r"\\server\share\spec.md".into(),
            "//server/share/spec.md".into(),
            r"\\?\C:\spec.md".into(),
            r"\\.\C:\spec.md".into(),
            r"\?\C:\spec.md".into(),
            r"\.\C:\spec.md".into(),
            "C:s.md".into(),
            "/etc/spec.md".into(),
        ] {
            assert!(!plain_relative(&word), "{word}");
        }
        for word in ["inside.md", "a/b.md", r"a\b.md", "../x/spec.md"] {
            assert!(plain_relative(word), "{word}");
        }
    }
}
