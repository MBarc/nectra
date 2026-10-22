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
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, OnceLock,
    },
    time::Duration,
};

/// Each retrained model gets a new release tag; its files are pinned below.
const RELEASE: &str = "https://github.com/MBarc/nectra/releases/download/laya-model-1";

/// (path in the model directory, release asset, sha256)
const MODEL_FILES: &[(&str, &str, &str)] = &[
    (
        "model.onnx",
        "model.onnx",
        "ccbfb5b0f5d284ace041eb579be5f5008181b8c92eae91f72a79771d7085d9ff",
    ),
    (
        "model.onnx.data",
        "model.onnx.data",
        "21941c6610821126b30ad3cf83c01556e0131be6b35f05f7d9c98fc4cbe0d5d3",
    ),
    (
        "rl_agent_config.json",
        "rl_agent_config.json",
        "0d19b2d11e67ded687edf10333f4d26421434662f835cec8b2e53f1b5acc58a3",
    ),
    (
        "tokenizer/tokenizer.json",
        "tokenizer.json",
        "dbba919c6e5e492bc22beb80abb64e6fcc7e8744dc696e8388b8c977767afa4b",
    ),
    (
        "tokenizer/tokenizer_config.json",
        "tokenizer_config.json",
        "b85452b1f3cf89ef52e104452e6f68b1219af8c55e33d18e4a13027d4a548e0b",
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

type Pin = (&'static str, &'static str, &'static str);

static ENGINE: OnceLock<std::result::Result<LayaEngine, String>> = OnceLock::new();
static WARMING: AtomicBool = AtomicBool::new(false);

/// Loaded once per process; a failure is remembered so every message falls
/// back to keywords instead of retrying the load. The model's files must match
/// their pins unless `allow_unpinned_model` says otherwise. One throwaway
/// message is answered before the engine is published, so ONNX Runtime's
/// first-run setup is part of the load and never of a message's budget.
fn engine(cfg: &Config) -> &'static std::result::Result<LayaEngine, String> {
    ENGINE.get_or_init(|| {
        let engine = model_dir(cfg)
            .context("cannot find a data directory")
            .and_then(|dir| load(&dir, cfg.inner.classifier.allow_unpinned_model))
            .map_err(|e| format!("{e:#}"))?;
        ask_with(&engine, "hello", None, &[]).map_err(|e| format!("{e:#}"))?;
        Ok(engine)
    })
}

/// Loads the model in the background so the first messages don't wait for it.
/// Until it is ready, messages use keywords (`ask` says "still loading").
pub fn warm(cfg: &Config) {
    if cfg.inner.classifier.backend != crate::config::ClassifierBackend::Laya
        || WARMING.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let cfg = cfg.clone();
    std::thread::spawn(move || {
        engine(&cfg);
    });
}

/// The pinned files in `files` that don't match their hash: an error, or with
/// `allow_unpinned`, the list of them.
fn check_pins(dir: &Path, files: &[&Pin], allow_unpinned: bool) -> Result<Vec<&'static str>> {
    let mut unpinned = Vec::new();
    for &&(path, _, sha256) in files {
        if hash_file(&dir.join(path)).ok().as_deref() != Some(sha256) {
            if !allow_unpinned {
                bail!("{path} does not match its pinned hash; run `nectra classifier laya` again");
            }
            unpinned.push(path);
        }
    }
    Ok(unpinned)
}

fn load(dir: &Path, allow_unpinned: bool) -> Result<LayaEngine> {
    let pin = RUNTIME.context("no local model runtime for this platform")?;
    let (runtime, _, _) = pin;
    if !dir.join(runtime).is_file() || !dir.join("model.onnx").is_file() {
        bail!("local model not downloaded; run `nectra classifier laya`");
    }
    // Everything ONNX Runtime parses or runs is checked before it loads. The
    // raw weights, most of the hashing time, are checked while it loads, and
    // the engine is dropped if they don't match.
    let (weights, rest): (Vec<&Pin>, Vec<&Pin>) = MODEL_FILES
        .iter()
        .chain([&pin])
        .partition(|(path, _, _)| *path == "model.onnx.data");
    let mut unpinned = check_pins(dir, &rest, allow_unpinned)?;
    // ort reads this when it first loads the runtime library: always the one
    // checked above, never another found through the environment.
    std::env::set_var("ORT_DYLIB_PATH", dir.join(runtime));
    let engine = std::thread::scope(|s| {
        let weights = s.spawn(|| check_pins(dir, &weights, allow_unpinned));
        let engine = LayaEngine::create(LayaOptions {
            model_directory: Some(dir.to_path_buf()),
            ..Default::default()
        });
        unpinned.extend(weights.join().expect("hashing panicked")?);
        anyhow::Ok(engine?)
    })?;
    if !unpinned.is_empty() {
        eprintln!(
            "Nectra: WARNING: loaded an UNPINNED local model from {} (classifier.allow_unpinned_model = true); not the published files: {}",
            dir.display(),
            unpinned.join(", ")
        );
    }
    Ok(engine)
}

/// Runs `f` on its own thread and gives up after `deadline`. A late answer
/// is dropped; inference can't be interrupted, so the thread finishes alone.
fn within<T: Send + 'static>(
    deadline: Duration,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(deadline).map_err(|_| {
        anyhow::anyhow!(
            "Laya timed out: no answer within {} ms",
            deadline.as_millis()
        )
    })?
}

/// The same questions as Jev's, in the short form the model was trained on.
/// `laya_timeout_ms` bounds inference only, never the load. Without `warm`
/// (one-shot commands such as `route` and `eval`) the first call waits for it.
/// A load failure and a timeout are reported as what they are.
pub(crate) fn ask(
    prompt: &str,
    previous: Option<&str>,
    roles: &[Role],
    cfg: &Config,
) -> Result<Answer> {
    let engine = match ENGINE.get() {
        None if WARMING.load(Ordering::SeqCst) => bail!("Laya still loading"),
        found => found.unwrap_or_else(|| engine(cfg)),
    }
    .as_ref()
    .map_err(|e| anyhow::anyhow!("Laya failed to load: {e}"))?;
    let (prompt, previous, roles) = (
        prompt.to_string(),
        previous.map(str::to_string),
        roles.to_vec(),
    );
    within(
        Duration::from_millis(cfg.inner.classifier.laya_timeout_ms),
        move || ask_with(engine, &prompt, previous.as_deref(), &roles),
    )
}

fn ask_with(
    engine: &LayaEngine,
    prompt: &str,
    previous: Option<&str>,
    roles: &[Role],
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
    let result = engine.predict(state, &questions)?;
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
    let dir = model_dir(cfg).context("cannot find a data directory")?;
    download_from(RELEASE, &dir, output)?;
    Ok(dir)
}

fn download_from(release: &str, dir: &Path, output: &mut dyn Write) -> Result<Answer> {
    let runtime = RUNTIME.context(
        "the local model needs ONNX Runtime, which has no official build for this platform",
    )?;
    fs::create_dir_all(dir.join("tokenizer"))?;
    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    for &(path, asset, sha256) in MODEL_FILES.iter().chain([&runtime]) {
        let target = dir.join(path);
        if target.is_file() && hash_file(&target)? == sha256 {
            continue;
        }
        writeln!(output, "Downloading {asset}...")?;
        fetch(&client, &format!("{release}/{asset}"), &target, sha256)
            .with_context(|| format!("downloading {asset}"))?;
    }
    let started = std::time::Instant::now();
    let answer = ask_with(
        &load(dir, false)?,
        "write a hello world script in python",
        None,
        &[],
    )?;
    writeln!(
        output,
        "Local model ready in {} ({} in {} ms).",
        dir.display(),
        answer.task,
        started.elapsed().as_millis()
    )?;
    Ok(answer)
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
    /// Serves `dir` over HTTP/1.1 on localhost, one request per connection,
    /// as a stand-in for the GitHub release.
    fn serve(dir: PathBuf) -> String {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                while reader.read_line(&mut String::new()).unwrap() > 2 {}
                let name = line.split(' ').nth(1).unwrap().trim_start_matches('/');
                match fs::File::open(dir.join(name)) {
                    Ok(mut file) => {
                        let len = file.metadata().unwrap().len();
                        write!(
                            stream,
                            "HTTP/1.1 200 OK
Content-Length: {len}
Connection: close

"
                        )
                        .unwrap();
                        let _ = std::io::copy(&mut file, &mut stream);
                    }
                    Err(_) => write!(
                        stream,
                        "HTTP/1.1 404 Not Found
Content-Length: 0
Connection: close

"
                    )
                    .unwrap(),
                }
            }
        });
        url
    }
    #[test]
    fn every_file_has_a_pinned_hash() {
        for (path, _, sha256) in MODEL_FILES.iter().chain(RUNTIME.as_ref()) {
            assert!(
                sha256.len() == 64
                    && sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{path} has no pinned sha256: {sha256}"
            );
        }
    }
    #[test]
    fn refuses_a_tampered_download() {
        let served = tempfile::tempdir().unwrap();
        fs::write(served.path().join("asset"), b"tampered").unwrap();
        let url = serve(served.path().to_path_buf());
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("asset");
        let client = reqwest::blocking::Client::new();
        let err = fetch(
            &client,
            &format!("{url}/asset"),
            &target,
            &hex(&Sha256::digest(b"genuine")),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("checksum mismatch"), "{err}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "nothing kept");
        fetch(
            &client,
            &format!("{url}/asset"),
            &target,
            &hex(&Sha256::digest(b"tampered")),
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"tampered");
    }
    #[test]
    fn refuses_to_load_a_changed_file() {
        if RUNTIME.is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for (path, _, _) in MODEL_FILES.iter().chain(RUNTIME.as_ref()) {
            let target = dir.path().join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, b"not the pinned file").unwrap();
        }
        let err = load(dir.path(), false).err().expect("must refuse");
        assert!(format!("{err:#}").contains("pinned hash"), "{err:#}");
        // Only an explicit opt-in loads unpinned files, and it names every one.
        let all: Vec<_> = MODEL_FILES.iter().chain(RUNTIME.as_ref()).collect();
        assert!(check_pins(dir.path(), &all, false).is_err());
        let unpinned = check_pins(dir.path(), &all, true).unwrap();
        assert_eq!(unpinned, all.iter().map(|f| f.0).collect::<Vec<_>>());
        // A file that matches its pin is not reported.
        let real = dir.path().join("only");
        fs::write(&real, b"abc").unwrap();
        let pin = (
            "only",
            "",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        assert!(check_pins(dir.path(), &[&pin], false).unwrap().is_empty());
    }
    #[test]
    fn a_failed_load_is_not_a_timeout() {
        // The only test that touches the process-wide engine.
        let dir = tempfile::tempdir().unwrap();
        let cfg_file = dir.path().join("config.toml");
        fs::write(&cfg_file, include_str!("../config/nectra.toml")).unwrap();
        let mut cfg = Config::from_path(&cfg_file).unwrap();
        cfg.inner.classifier.model_dir = Some(dir.path().to_path_buf());
        for _ in 0..2 {
            let err = format!("{:#}", ask("hi", None, &[], &cfg).err().expect("must fail"));
            assert!(err.contains("Laya failed to load"), "{err}");
            assert!(!err.contains("timed out"), "{err}");
        }
    }
    #[test]
    fn gives_up_at_the_deadline() {
        let slow = within(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Ok(1)
        });
        assert!(format!("{:#}", slow.unwrap_err()).contains("50 ms"));
        assert_eq!(within(Duration::from_millis(500), || Ok(2)).unwrap(), 2);
    }
    /// The whole fetch path against a local copy of the release:
    /// `NECTRA_LAYA_ASSETS=<dir staged by scripts/release-laya.sh> cargo test --release -- --ignored fetches`
    #[test]
    #[ignore = "needs the release assets in NECTRA_LAYA_ASSETS"]
    fn fetches_verifies_and_classifies() {
        let assets = PathBuf::from(std::env::var_os("NECTRA_LAYA_ASSETS").unwrap());
        let url = serve(assets.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let answer = download_from(&url, dir.path(), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        println!("{output}");
        assert_eq!(answer.task, TaskType::CodeGeneration, "{output}");
        assert_eq!(output.matches("Downloading").count(), MODEL_FILES.len() + 1);

        // A tampered asset on the server is refused and never lands.
        let tampered = tempfile::tempdir().unwrap();
        for entry in fs::read_dir(&assets).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), tampered.path().join(entry.file_name())).unwrap();
        }
        fs::write(tampered.path().join("tokenizer_config.json"), b"{}").unwrap();
        let url = serve(tampered.path().to_path_buf());
        let fresh = tempfile::tempdir().unwrap();
        let err = download_from(&url, fresh.path(), &mut Vec::new())
            .err()
            .expect("must refuse");
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(!fresh
            .path()
            .join("tokenizer/tokenizer_config.json")
            .exists());

        // So are weights changed on disk after the download, which are checked
        // while the engine loads.
        let started = std::time::Instant::now();
        load(dir.path(), false).unwrap();
        println!("verified load: {} ms", started.elapsed().as_millis());
        fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("model.onnx.data"))
            .unwrap()
            .write_all(b"\0")
            .unwrap();
        let err = load(dir.path(), false).err().expect("must refuse");
        assert!(format!("{err:#}").contains("model.onnx.data"), "{err:#}");
    }
    /// Load, first-message and steady-state latency on dev.json (the first
    /// `NECTRA_LAYA_N` messages), with the process's memory:
    /// `NECTRA_LAYA_MODEL=<model dir> cargo test --release -- --ignored latency --nocapture`
    #[test]
    #[ignore = "needs a model in NECTRA_LAYA_MODEL"]
    fn latency() {
        let dir = PathBuf::from(std::env::var_os("NECTRA_LAYA_MODEL").unwrap());
        let unpinned = std::env::var_os("NECTRA_LAYA_UNPINNED").is_some();
        let started = std::time::Instant::now();
        let engine = load(&dir, unpinned).unwrap();
        let load_ms = started.elapsed().as_millis();
        let loaded = memory();
        let dev: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../eval/laya/dev.json")).unwrap();
        let limit = std::env::var("NECTRA_LAYA_N")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(dev.len());
        let mut ms: Vec<u128> = dev
            .iter()
            .take(limit)
            .map(|c| {
                let started = std::time::Instant::now();
                ask_with(&engine, c["prompt"].as_str().unwrap(), None, &[]).unwrap();
                started.elapsed().as_millis()
            })
            .collect();
        let first = ms.remove(0);
        ms.sort();
        let at = |q: f64| ms[((ms.len() - 1) as f64 * q) as usize];
        println!(
            "load {load_ms} ms; first message {first} ms; then n={} median {} p95 {} max {} ms; over 300 ms: {}",
            ms.len(),
            at(0.5),
            at(0.95),
            ms[ms.len() - 1],
            ms.iter().filter(|&&m| m > 300).count()
        );
        println!(
            "memory after load: {loaded}; after {} messages: {}",
            ms.len() + 1,
            memory()
        );
    }
    /// This process's resident memory on Linux, split into private (`RssAnon`,
    /// which the kernel can only swap) and file-backed (`RssFile`, the mapped
    /// weights, which it can drop and re-read).
    fn memory() -> String {
        fs::read_to_string("/proc/self/status")
            .map(|s| {
                s.lines()
                    .filter(|l| {
                        ["VmRSS", "RssAnon", "RssFile", "VmHWM"]
                            .iter()
                            .any(|k| l.starts_with(k))
                    })
                    .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "n/a".into())
    }
    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
