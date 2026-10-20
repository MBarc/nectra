//! Optional local classifier: a Laya model fine-tuned on Nectra's questions,
//! run on this machine with ONNX Runtime. Free and offline; prompts never
//! leave the machine. `nectra classifier laya` downloads it.
use crate::{
    classifier::TaskType,
    config::Config,
    jev::{Answer, Role, TASK_CRITERIA},
};
use ::laya::{LayaEngine, LayaOptions, Question, QuestionSet};
use anyhow::{bail, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Each retrained model gets a new release tag; its files are pinned below.
const RELEASE: &str = "https://github.com/MBarc/nectra/releases/download/laya-model-1";

/// (path in the model directory, release asset, sha256)
const MODEL_FILES: &[(&str, &str, &str)] = &[
    ("model.onnx", "model.onnx", "TODO"),
    ("model.onnx.data", "model.onnx.data", "TODO"),
    ("rl_agent_config.json", "rl_agent_config.json", "TODO"),
    ("tokenizer/tokenizer.json", "tokenizer.json", "TODO"),
    (
        "tokenizer/tokenizer_config.json",
        "tokenizer_config.json",
        "TODO",
    ),
    (
        "ONNXRUNTIME-LICENSE",
        "ONNXRUNTIME-LICENSE",
        "c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674",
    ),
];

/// Microsoft's ONNX Runtime 1.31.0 CPU library for this platform, loaded at
/// run time. Intel Macs and ARM Windows/Linux have no official build.
#[cfg(all(windows, target_arch = "x86_64"))]
const RUNTIME: Option<(&str, &str, &str)> = Some((
    "onnxruntime.dll",
    "onnxruntime-win-x64.dll",
    "0c42dc4e278f0968fdf9ebf1b1e54a74c657a9942e70803e453c91993451cf4c",
));
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUNTIME: Option<(&str, &str, &str)> = Some((
    "libonnxruntime.so",
    "libonnxruntime-linux-x64.so",
    "469849977cb60744ea160927e1a3a0643fd4c55c0aff743251c6d293402e334e",
));
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const RUNTIME: Option<(&str, &str, &str)> = Some((
    "libonnxruntime.dylib",
    "libonnxruntime-macos-arm64.dylib",
    "2a50a63e7f41f8d4d551d3ee47248df3864f6540109fce8ad520ca3b51af4b70",
));
#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
)))]
const RUNTIME: Option<(&str, &str, &str)> = None;

/// The questions exactly as the model was trained on them: bare labels, no
/// descriptions, which keeps the input (and so the latency) small. Task options
/// are `jev::TASK_CRITERIA` in order and role options `policy::ROLES`;
/// `eval/laya/criteria.json` is the training copy.
const TASK_Q: &str = "What task is this?";
const DIFFICULTY_Q: &str = "How hard is it?";
const DIFFICULTY_LEVELS: [&str; 3] = ["easy", "moderate", "hard"];
const ROLE_Q: &str = "Which model should handle it?";
const NEW_TASK_Q: &str = "Is current_message a new task?";

pub fn model_dir(cfg: &Config) -> Option<PathBuf> {
    cfg.inner
        .classifier
        .model_dir
        .clone()
        .or_else(|| dirs::data_local_dir().map(|d| d.join("nectra").join("laya")))
}

/// Loaded once per process (a few seconds); a failure is remembered so every
/// message falls back to keywords instead of retrying the load.
fn engine(cfg: &Config) -> Result<&'static LayaEngine> {
    static ENGINE: OnceLock<std::result::Result<LayaEngine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| load(cfg).map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn load(cfg: &Config) -> Result<LayaEngine> {
    let (runtime, _, _) = RUNTIME.context("no local model runtime for this platform")?;
    let dir = model_dir(cfg).context("cannot find a data directory")?;
    if !dir.join(runtime).is_file() || !dir.join("model.onnx").is_file() {
        bail!("local model not downloaded; run `nectra classifier laya`");
    }
    // ort reads this when it first loads the runtime library.
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        std::env::set_var("ORT_DYLIB_PATH", dir.join(runtime));
    }
    Ok(LayaEngine::create(LayaOptions {
        model_directory: Some(dir),
        ..Default::default()
    })?)
}

/// The same questions as Jev's, in the short form the model was trained on.
pub(crate) fn ask(
    prompt: &str,
    previous: Option<&str>,
    roles: &[Role],
    cfg: &Config,
) -> Result<Answer> {
    let state = match previous {
        Some(prev) => json!({ "previous_message": prev, "current_message": prompt }),
        None => json!(prompt),
    };
    let mut questions = QuestionSet::new()
        .with(
            "task",
            Question::choice(TASK_Q, TASK_CRITERIA.map(|(t, _)| (t, "")))?,
        )?
        .with(
            "difficulty",
            Question::score(DIFFICULTY_Q, DIFFICULTY_LEVELS)?,
        )?;
    if previous.is_some() {
        questions = questions.with("new_task", Question::noul(NEW_TASK_Q, "", ""))?;
    }
    if roles.len() > 1 {
        questions = questions.with(
            "role",
            Question::choice(ROLE_Q, roles.iter().map(|(name, _)| (*name, "")))?,
        )?;
    }
    let result = engine(cfg)?.predict(state, &questions)?;
    let task = result.get("task").context("no task answer")?;
    let choice = task.as_choice().context("task answer is not a choice")?;
    let score = result
        .get("difficulty")
        .and_then(|a| a.as_score())
        .context("no difficulty answer")?;
    Ok(Answer {
        task: serde_json::from_value::<TaskType>(json!(choice.choice))
            .context("unknown task type")?,
        confidence: task.confidence(),
        // Three levels score 0..2.
        difficulty: (score.score / 2.0).clamp(0.0, 1.0),
        new_task: result
            .get("new_task")
            .and_then(|a| a.as_noul())
            .map(|n| n.probability),
        role: result
            .get("role")
            .and_then(|a| a.as_choice())
            .map(|c| c.choice.clone()),
    })
}

/// Downloads any missing or changed model file, checks every hash, and makes
/// sure the model loads and answers.
pub fn download(cfg: &Config, output: &mut dyn Write) -> Result<PathBuf> {
    let runtime = RUNTIME.context(
        "the local model needs ONNX Runtime, which has no official build for this platform",
    )?;
    let dir = model_dir(cfg).context("cannot find a data directory")?;
    fs::create_dir_all(dir.join("tokenizer"))?;
    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    for &(path, asset, sha256) in MODEL_FILES.iter().chain([&runtime]) {
        let target = dir.join(path);
        if target.is_file() && hash_file(&target)? == sha256 {
            continue;
        }
        writeln!(output, "Downloading {asset}...")?;
        fetch(&client, &format!("{RELEASE}/{asset}"), &target, sha256)
            .with_context(|| format!("downloading {asset}"))?;
    }
    let started = std::time::Instant::now();
    let answer = ask("write a hello world script in python", None, &[], cfg)?;
    writeln!(
        output,
        "Local model ready in {} ({} in {} ms).",
        dir.display(),
        answer.task,
        started.elapsed().as_millis()
    )?;
    Ok(dir)
}

fn fetch(client: &reqwest::blocking::Client, url: &str, target: &Path, sha256: &str) -> Result<()> {
    let mut response = client.get(url).send()?.error_for_status()?;
    let partial = target.with_extension("part");
    let mut file = fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
    }
    file.sync_all()?;
    drop(file);
    let got = hex(&hasher.finalize());
    if got != sha256 {
        let _ = fs::remove_file(&partial);
        bail!("checksum mismatch: expected {sha256}, got {got}");
    }
    fs::rename(&partial, target)?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn criteria_file_matches_the_code() {
        // tools/laya/retrain trains on eval/laya/criteria.json; the model only
        // answers what it was trained on, so the file must match what we ask.
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../eval/laya/criteria.json")).unwrap();
        let pairs = |v: &serde_json::Value| -> Vec<(String, String)> {
            serde_json::from_value(v.clone()).unwrap()
        };
        let owned = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let msg = "update eval/laya/criteria.json and retrain (tools/laya/retrain)";
        assert_eq!(
            pairs(&c["tasks"]),
            owned(&crate::jev::TASK_CRITERIA),
            "{msg}"
        );
        assert_eq!(pairs(&c["roles"]), owned(&crate::policy::ROLES), "{msg}");
        let q = &c["questions"];
        assert_eq!(q["task"], TASK_Q, "{msg}");
        assert_eq!(q["difficulty"], DIFFICULTY_Q, "{msg}");
        assert_eq!(q["difficulty_levels"], json!(DIFFICULTY_LEVELS), "{msg}");
        assert_eq!(q["role"], ROLE_Q, "{msg}");
        assert_eq!(q["new_task"], NEW_TASK_Q, "{msg}");
    }
    #[test]
    fn task_labels_are_nectras() {
        for (t, _) in TASK_CRITERIA {
            assert!(serde_json::from_value::<TaskType>(json!(t)).is_ok(), "{t}");
        }
    }
    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
