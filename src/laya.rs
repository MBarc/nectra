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
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, OnceLock,
    },
    time::{Duration, UNIX_EPOCH},
};

/// Each retrained model gets a new release tag; its files are pinned below.
/// `NECTRA_LAYA_RELEASE` names a mirror instead: the pins still apply.
const RELEASE: &str = "https://github.com/MBarc/nectra/releases/download/laya-model-1-alpha.2";

/// Where the nectra-laya package keeps the model, used when the config names none.
const SYSTEM_MODEL_DIR: &str = "/usr/lib/nectra/laya";

/// (path in the model directory, release asset, sha256)
const MODEL_FILES: &[(&str, &str, &str)] = &[
    (
        "model.onnx",
        "model.onnx",
        "4e3c05b03761f4b8c40de6d50656c06153d2d9b34d09c593b8688ce006d81ab8",
    ),
    (
        "model.onnx.data",
        "model.onnx.data",
        "f3f862bde4de1a7a638244dcd2dcfc0b1b5b85aa6ee9e7ae4eeba3cd8e853848",
    ),
    (
        "rl_agent_config.json",
        "rl_agent_config.json",
        "8d1770285b84b6206a2c20ac0fa46112cca51ab4357ae7a2edf75e495d1ea590",
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
        .or_else(|| {
            let system = Path::new(SYSTEM_MODEL_DIR);
            (cfg!(target_os = "linux") && system.join("model.onnx").is_file())
                .then(|| system.to_path_buf())
        })
        .or_else(|| dirs::data_local_dir().map(|d| d.join("nectra").join("laya")))
}

fn release() -> String {
    std::env::var("NECTRA_LAYA_RELEASE").unwrap_or_else(|_| RELEASE.into())
}

type Pin = (&'static str, &'static str, &'static str);

static ENGINE: OnceLock<std::result::Result<LayaEngine, String>> = OnceLock::new();
static WARMING: AtomicBool = AtomicBool::new(false);
/// Threads currently inside ONNX Runtime (the loader, an inference).
static BUSY: AtomicUsize = AtomicUsize::new(0);

struct Busy;
impl Busy {
    fn enter() -> Self {
        BUSY.fetch_add(1, Ordering::SeqCst);
        Busy
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Ends the process. ONNX Runtime's teardown at exit would run beside a
/// thread still loading or inferring (a quit mid-load, or after a timeout),
/// so then the process leaves without running it, after flushing output.
pub fn exit(code: i32) -> ! {
    if BUSY.load(Ordering::SeqCst) > 0 {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        #[cfg(unix)]
        unsafe {
            libc::_exit(code)
        }
    }
    std::process::exit(code)
}

/// The directory to load from: where it really is, so a symlink or rename
/// after the checks cannot point the load somewhere else. Always absolute:
/// ort tries a relative runtime path against its own exe's directory first.
fn resolved(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    if let Ok(real) = fs::canonicalize(dir) {
        return real;
    }
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Loaded once per process; a failure is remembered so every message falls
/// back to keywords instead of retrying the load. The model's files must match
/// their pins unless `allow_unpinned_model` says otherwise. One throwaway
/// message is answered before the engine is published, so ONNX Runtime's
/// first-run setup is part of the load and never of a message's budget.
fn engine(cfg: &Config) -> &'static std::result::Result<LayaEngine, String> {
    ENGINE.get_or_init(|| {
        let engine = model_dir(cfg)
            .context("cannot find a data directory")
            .and_then(|dir| {
                load(
                    &dir,
                    cfg.inner.classifier.allow_unpinned_model,
                    !prepacking(cfg),
                )
            })
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
    let busy = Busy::enter();
    std::thread::spawn(move || {
        let _busy = busy;
        guarded(&cfg);
    });
}

/// [`engine`], with a panic remembered as a failed load: otherwise the cell
/// stays empty and "still loading" forever, and messages use keywords.
fn guarded(cfg: &Config) {
    if let Err(panic) = std::panic::catch_unwind(|| engine(cfg)) {
        let why = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        let _ = ENGINE.set(Err(format!("loader panicked: {why}")));
    }
}

/// Loads the model now, on this thread, for a process that answers many
/// prompts (`run --serve`): none of them routes with keywords while it loads.
pub fn preload(cfg: &Config) {
    if cfg.inner.classifier.backend == crate::config::ClassifierBackend::Laya {
        let _busy = Busy::enter();
        guarded(cfg);
    }
}

/// The default per-message budget for this machine: a slow one (2 or fewer
/// cores, or no AVX2) needs about 2.5 s a message, so 1000 ms would never
/// answer.
fn auto_timeout_ms(cores: usize, avx2: bool) -> u64 {
    if cores <= 2 || !avx2 {
        5000
    } else {
        1000
    }
}

/// `classifier.laya_timeout_ms`, or the machine's default.
pub fn timeout_ms(cfg: &Config) -> u64 {
    cfg.inner.classifier.laya_timeout_ms.unwrap_or_else(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        #[cfg(target_arch = "x86_64")]
        let avx2 = is_x86_feature_detected!("avx2");
        #[cfg(not(target_arch = "x86_64"))]
        let avx2 = true;
        auto_timeout_ms(cores, avx2)
    })
}

/// Prepacked weights cost about 400 MB of private memory; without them the
/// weights stay in the evictable, shared file mapping. Only worth it where
/// memory is short: under 4 GiB.
fn auto_prepacking(mem_total_kib: Option<u64>) -> bool {
    !mem_total_kib.is_some_and(|kib| kib < 4 * 1024 * 1024)
}

/// `classifier.laya_prepacking`, or the machine's default (Linux reads MemTotal).
fn prepacking(cfg: &Config) -> bool {
    cfg.inner.classifier.laya_prepacking.unwrap_or_else(|| {
        let total = fs::read_to_string("/proc/meminfo").ok().and_then(|m| {
            m.lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        });
        auto_prepacking(total)
    })
}

/// What `nectra classifier laya` (or a package's post-install) verified in
/// full: each file's pin, size and modification time at that moment.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Stamped {
    path: String,
    sha256: String,
    size: u64,
    mtime_ns: u128,
}

const STAMP: &str = ".nectra-verified.json";

fn file_state(path: &Path) -> Option<(u64, u128)> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), mtime.as_nanos()))
}

/// Records the current size and mtime of `files`, which the caller has just
/// hashed against their pins.
fn write_stamp(dir: &Path, files: &[&Pin]) -> Result<()> {
    let stamped: Vec<Stamped> = files
        .iter()
        .map(|&&(path, _, sha256)| {
            let (size, mtime_ns) =
                file_state(&dir.join(path)).with_context(|| format!("reading {path}"))?;
            Ok(Stamped {
                path: path.into(),
                sha256: sha256.into(),
                size,
                mtime_ns,
            })
        })
        .collect::<Result<_>>()?;
    let partial = dir.join(format!("{STAMP}.part"));
    fs::write(&partial, serde_json::to_vec_pretty(&stamped)?)?;
    fs::rename(&partial, dir.join(STAMP))?;
    Ok(())
}

/// Whether the stamp may stand in for hashing: only when nobody but root can
/// have changed the directory, the stamp or any file since it was written.
/// Anything a normal user can write, a normal user can alter and re-date, so
/// a user-owned install is always hashed in full on load.
#[cfg(unix)]
fn stamp_trusted(dir: &Path, files: &[&Pin]) -> bool {
    stamp_trusted_for(dir, files, 0)
}
/// `owner` is root (0) outside tests.
#[cfg(unix)]
fn stamp_trusted_for(dir: &Path, files: &[&Pin], owner: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    let sealed = |path: &Path, want_dir: bool| {
        fs::symlink_metadata(path).is_ok_and(|m| {
            m.uid() == owner && m.mode() & 0o022 == 0 && m.is_dir() == want_dir && !m.is_symlink()
        })
    };
    // Every ancestor too: whoever can write a parent can rename the sealed
    // directory away and put back same-size, backdated files. A sticky parent
    // (/tmp) only lets an entry's owner rename it. `load` goes through the
    // canonical path, so these are the real parents.
    let Ok(real) = fs::canonicalize(dir) else {
        return false;
    };
    let parents_safe = real.ancestors().skip(1).all(|p| {
        fs::metadata(p).is_ok_and(|m| {
            (m.uid() == owner || m.uid() == 0) && (m.mode() & 0o022 == 0 || m.mode() & 0o1000 != 0)
        })
    });
    parents_safe
        && sealed(&real, true)
        && sealed(&real.join("tokenizer"), true)
        && sealed(&real.join(STAMP), false)
        && files
            .iter()
            .all(|&&(path, _, _)| sealed(&real.join(path), false))
}
#[cfg(not(unix))]
fn stamp_trusted(_: &Path, _: &[&Pin]) -> bool {
    false
}

/// The stamp's entries, or none when it is untrusted, missing or unreadable.
fn read_stamp(dir: &Path, trusted: bool) -> Vec<Stamped> {
    if !trusted {
        return Vec::new();
    }
    fs::read(dir.join(STAMP))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The pinned files in `files` that don't match their hash: an error, or with
/// `allow_unpinned`, the list of them. A file whose pin, size and mtime match
/// a `stamp` entry is not hashed again.
fn check_pins(
    dir: &Path,
    files: &[&Pin],
    allow_unpinned: bool,
    stamp: &[Stamped],
) -> Result<Vec<&'static str>> {
    let mut unpinned = Vec::new();
    for &&(path, _, sha256) in files {
        let verified = file_state(&dir.join(path)).is_some_and(|(size, mtime_ns)| {
            stamp.iter().any(|s| {
                s.path == path && s.sha256 == sha256 && s.size == size && s.mtime_ns == mtime_ns
            })
        });
        if !verified && hash_file(&dir.join(path)).ok().as_deref() != Some(sha256) {
            if !allow_unpinned {
                bail!("{path} does not match its pinned hash; run `nectra classifier laya` again");
            }
            unpinned.push(path);
        }
    }
    Ok(unpinned)
}

fn load(dir: &Path, allow_unpinned: bool, prepacking_off: bool) -> Result<LayaEngine> {
    let dir = &resolved(dir);
    let pin = RUNTIME.context("no local model runtime for this platform")?;
    let (runtime, _, _) = pin;
    if !dir.join(runtime).is_file() || !dir.join("model.onnx").is_file() {
        bail!("local model not downloaded; run `nectra classifier laya`");
    }
    let staged_runtime = stage_runtime(dir, runtime, pin.2, allow_unpinned)?;
    let all: Vec<&Pin> = MODEL_FILES.iter().chain([&pin]).collect();
    let stamp = read_stamp(dir, stamp_trusted(dir, &all));
    // Everything ONNX Runtime parses or runs is checked before it loads. The
    // raw weights, most of the hashing time, are checked while it loads, and
    // the engine is dropped if they don't match.
    let (weights, rest): (Vec<&Pin>, Vec<&Pin>) = all
        .into_iter()
        .partition(|(path, _, _)| *path == "model.onnx.data");
    let mut unpinned = check_pins(dir, &rest, allow_unpinned, &stamp)?;
    if hash_file(&staged_runtime)? != pin.2 {
        unpinned.push(runtime.to_string());
    }
    // Always the runtime the pins checked, by its full path: left to itself,
    // ort would look for it on the library path (or `ORT_DYLIB_PATH`), and
    // panic where that fails, inside a call that can't unwind.
    // Never hand the loader the mutable, user-controlled runtime path that was
    // just verified. Copy the verified native library into a private staging
    // directory and bind ORT to that artifact for the lifetime of the engine.
    // This closes the verify-then-replace window for native code; model data is
    // still checked while the engine is constructed below.
    let _environment = ort::init_from(&staged_runtime)
        .with_context(|| format!("loading ONNX Runtime from {}", dir.display()))?;
    let engine = std::thread::scope(|s| {
        let weights = s.spawn(|| check_pins(dir, &weights, allow_unpinned, &stamp));
        let engine = LayaEngine::create(LayaOptions {
            model_directory: Some(dir.to_path_buf()),
            disable_prepacking: prepacking_off,
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

/// Copy the native runtime before hashing it. The cache is keyed by the
/// resulting hash, so a replacement between verification and loading cannot
/// silently turn into a different native library. Unlike TempDir, the cache
/// survives Windows DLL unload semantics.
fn stage_runtime(
    dir: &Path,
    runtime: &str,
    expected: &str,
    allow_unpinned: bool,
) -> Result<PathBuf> {
    let actual_source = dir.join(runtime);
    let cache = dirs::cache_dir()
        .context("no per-user cache directory for ONNX Runtime")?
        .join("nectra/laya/ort");
    fs::create_dir_all(&cache)?;
    let expected_target = cache.join(format!("{expected}-{runtime}"));
    if expected_target.is_file() && hash_file(&expected_target)? == expected {
        return Ok(expected_target);
    }
    let tmp = cache.join(format!(".{runtime}.{}.tmp", std::process::id()));
    fs::copy(&actual_source, &tmp)
        .with_context(|| format!("staging verified runtime from {}", actual_source.display()))?;
    let actual = hash_file(&tmp)?;
    if actual != expected && !allow_unpinned {
        let _ = fs::remove_file(&tmp);
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    let target = cache.join(format!("{actual}-{runtime}"));
    if !target.is_file() {
        fs::rename(&tmp, &target)?;
    } else {
        let _ = fs::remove_file(&tmp);
    }
    Ok(target)
}

/// Hashes every file already in `dir` against its pin and writes the stamp
/// that lets later loads skip the hashing. Downloads nothing; for a package's
/// post-install step, run as root.
pub fn verify_dir(dir: &Path, output: &mut dyn Write) -> Result<()> {
    let runtime = RUNTIME.context(
        "the local model needs ONNX Runtime, which has no official build for this platform",
    )?;
    let all: Vec<&Pin> = MODEL_FILES.iter().chain([&runtime]).collect();
    check_pins(dir, &all, false, &[])?;
    write_stamp(dir, &all)?;
    writeln!(output, "Verified {} and wrote {STAMP}.", dir.display())?;
    Ok(())
}

/// Runs `f` on its own thread and gives up after `deadline`. A late answer
/// is dropped; inference can't be interrupted, so the thread finishes alone.
fn within<T: Send + 'static>(
    deadline: Duration,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    // Counted before the thread starts, so `exit` can't miss it.
    let busy = Busy::enter();
    std::thread::spawn(move || {
        let _busy = busy;
        tx.send(f())
    });
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
    within(Duration::from_millis(timeout_ms(cfg)), move || {
        ask_with(engine, &prompt, previous.as_deref(), &roles)
    })
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
    download_from(&release(), &dir, output)?;
    Ok(dir)
}

/// `nectra classifier fetch --dir`: only downloads and checks the files, for
/// the nectra-laya package, which runs it as root without a user config.
pub fn fetch_to(dir: &Path, output: &mut dyn Write) -> Result<()> {
    fetch_all(&release(), dir, output)?;
    writeln!(output, "Local model files verified in {}.", dir.display())?;
    Ok(())
}

fn fetch_all(release: &str, dir: &Path, output: &mut dyn Write) -> Result<()> {
    let runtime = RUNTIME.context(
        "the local model needs ONNX Runtime, which has no official build for this platform",
    )?;
    let client = http_client(IDLE_TIMEOUT)?;
    for &(path, asset, sha256) in MODEL_FILES.iter().chain([&runtime]) {
        let target = dir.join(path);
        if target.is_file() && hash_file(&target)? == sha256 {
            continue;
        }
        fs::create_dir_all(target.parent().context("no parent directory")?)?;
        writeln!(output, "Downloading {asset}...")?;
        fetch(&client, &format!("{release}/{asset}"), &target, sha256)
            .with_context(|| format!("downloading {asset}"))?;
    }
    let all: Vec<&Pin> = MODEL_FILES.iter().chain([&runtime]).collect();
    // Every file now matches its pin. Only root's stamp is ever trusted, so a
    // user-written one would be dead weight; as root (the nectra-laya postinst)
    // this fetch is the verify, and writes the stamp.
    #[cfg(unix)]
    let root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let root = false;
    if root {
        if let Err(e) = write_stamp(dir, &all) {
            writeln!(
                output,
                "Could not write {STAMP}: {e:#}; every load will hash the model."
            )?;
        }
    }
    Ok(())
}

fn download_from(release: &str, dir: &Path, output: &mut dyn Write) -> Result<Answer> {
    fetch_all(release, dir, output)?;
    let started = std::time::Instant::now();
    let answer = ask_with(
        &load(dir, false, false)?,
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

/// A stalled server fails the download after this long without data, so the
/// nectra-laya postinst can't hang dpkg. Not a total limit: the model is 700 MB.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The blocking client's `timeout` covers connecting, waiting for the headers
/// and each `read` of the body, so for a download it is an idle timeout.
fn http_client(idle: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(idle)
        .timeout(idle)
        .build()?)
}

/// Writes `target` only if the whole download matches `sha256`; a failed or
/// interrupted download leaves nothing behind.
fn fetch(client: &reqwest::blocking::Client, url: &str, target: &Path, sha256: &str) -> Result<()> {
    let partial = target.with_extension("part");
    let result =
        fetch_part(client, url, &partial, sha256).and_then(|()| Ok(fs::rename(&partial, target)?));
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn fetch_part(
    client: &reqwest::blocking::Client,
    url: &str,
    partial: &Path,
    sha256: &str,
) -> Result<()> {
    let mut response = client.get(url).send()?.error_for_status()?;
    let mut file = fs::File::create(partial)?;
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
        bail!("checksum mismatch: expected {sha256}, got {got}");
    }
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
    fn fetch_dir_refuses_files_that_miss_their_pins() {
        if RUNTIME.is_none() {
            return;
        }
        // A mirror serving the wrong bytes under every asset name.
        let served = tempfile::tempdir().unwrap();
        for (_, asset, _) in MODEL_FILES.iter().chain(RUNTIME.as_ref()) {
            fs::write(served.path().join(asset), b"not the pinned file").unwrap();
        }
        let url = serve(served.path().to_path_buf());
        let dir = tempfile::tempdir().unwrap();
        let err = fetch_all(&url, &dir.path().join("laya"), &mut Vec::new()).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        let files: Vec<_> = walk(dir.path());
        assert!(files.is_empty(), "nothing written: {files:?}");
    }
    fn walk(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .flat_map(|e| {
                let path = e.unwrap().path();
                if path.is_dir() {
                    walk(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }
    /// Promises 1000 bytes and sends 10, then hangs up or (`stall`) goes quiet
    /// with the connection open; either way nothing may be kept.
    fn short_download(stall: bool) -> anyhow::Error {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/asset", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = std::io::Read::read(&mut stream, &mut [0; 4096]);
            write!(
                stream,
                "HTTP/1.1 200 OK
Content-Length: 1000
Connection: close

0123456789"
            )
            .unwrap();
            if stall {
                std::thread::sleep(Duration::from_secs(60));
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let err = fetch(
            &http_client(Duration::from_secs(1)).unwrap(),
            &url,
            &dir.path().join("asset"),
            &hex(&Sha256::digest(b"genuine")),
        )
        .unwrap_err();
        assert!(!format!("{err:#}").contains("checksum"), "{err:#}");
        assert_eq!(
            walk(dir.path()),
            Vec::<PathBuf>::new(),
            "no partial file kept"
        );
        err
    }
    #[test]
    fn cleans_up_an_interrupted_download() {
        short_download(false);
    }
    #[test]
    fn gives_up_on_a_stalled_download() {
        let started = std::time::Instant::now();
        let err = short_download(true);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}: {err:#}",
            started.elapsed()
        );
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
        let err = load(dir.path(), false, false).err().expect("must refuse");
        assert!(format!("{err:#}").contains("pinned hash"), "{err:#}");
        // Only an explicit opt-in loads unpinned files, and it names every one.
        let all: Vec<_> = MODEL_FILES.iter().chain(RUNTIME.as_ref()).collect();
        assert!(check_pins(dir.path(), &all, false, &[]).is_err());
        let unpinned = check_pins(dir.path(), &all, true, &[]).unwrap();
        assert_eq!(unpinned, all.iter().map(|f| f.0).collect::<Vec<_>>());
        // A file that matches its pin is not reported.
        let real = dir.path().join("only");
        fs::write(&real, b"abc").unwrap();
        let pin = (
            "only",
            "",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        assert!(check_pins(dir.path(), &[&pin], false, &[])
            .unwrap()
            .is_empty());
    }
    #[test]
    fn a_relative_model_dir_loads_by_its_absolute_path() {
        assert!(resolved(Path::new("laya-model")).is_absolute());
    }
    #[test]
    fn a_runtime_that_wont_load_is_an_error_not_a_panic() {
        if RUNTIME.is_none() {
            return;
        }
        // ort remembers the first runtime it loads for the whole process, so
        // the check runs in a fresh one whatever other tests loaded.
        if std::env::var_os("NECTRA_ORT_CHILD").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "laya::tests::a_runtime_that_wont_load_is_an_error_not_a_panic",
                    "--test-threads=1",
                ])
                .env("NECTRA_ORT_CHILD", "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{stdout}");
            assert!(stdout.contains("1 passed"), "{stdout}");
            return;
        }
        // Past the pins (allowed unpinned), so ONNX Runtime itself is loaded:
        // from this directory, and a failure comes back as an error.
        let dir = tempfile::tempdir().unwrap();
        for (path, _, _) in MODEL_FILES.iter().chain(RUNTIME.as_ref()) {
            let target = dir.path().join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, b"not a library").unwrap();
        }
        let err = load(dir.path(), true, false).err().expect("must fail");
        let err = format!("{err:#}");
        assert!(err.contains("loading ONNX Runtime from"), "{err}");
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
    /// Two small "model files" with real pins, and the stamp for them.
    fn stamped_dir() -> (tempfile::TempDir, Vec<Pin>) {
        let dir = tempfile::tempdir().unwrap();
        let pins = ["one", "two"].map(|name| {
            fs::write(dir.path().join(name), name).unwrap();
            let sha: &'static str = Box::leak(hex(&Sha256::digest(name)).into_boxed_str());
            (name, "", sha)
        });
        let pins = pins.to_vec();
        write_stamp(dir.path(), &pins.iter().collect::<Vec<_>>()).unwrap();
        (dir, pins)
    }
    fn set_mtime(path: &Path, to: std::time::SystemTime) {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(to)
            .unwrap();
    }
    #[test]
    fn a_stamped_file_is_not_hashed_again_but_a_changed_mtime_is() {
        let (dir, pins) = stamped_dir();
        let files: Vec<&Pin> = pins.iter().collect();
        let stamp = read_stamp(dir.path(), true);
        assert_eq!(stamp.len(), 2);
        assert!(check_pins(dir.path(), &files, false, &stamp).is_ok());
        // Same size, new mtime: rehashed, found wrong, refused.
        let old = fs::metadata(dir.path().join("one"))
            .unwrap()
            .modified()
            .unwrap();
        fs::write(dir.path().join("one"), "ONE").unwrap();
        set_mtime(&dir.path().join("one"), old + Duration::from_secs(5));
        let err = check_pins(dir.path(), &files, false, &stamp).unwrap_err();
        assert!(format!("{err:#}").contains("one does not match"), "{err:#}");
        // Same size and the old mtime restored passes: this is why the stamp
        // is only trusted where nobody but root can write.
        set_mtime(&dir.path().join("one"), old);
        assert!(check_pins(dir.path(), &files, false, &stamp).is_ok());
        // A different size is rehashed too.
        fs::write(dir.path().join("two"), "twotwo").unwrap();
        set_mtime(&dir.path().join("two"), old);
        assert!(check_pins(dir.path(), &files, false, &stamp).is_err());
    }
    #[test]
    fn a_missing_or_untrusted_stamp_means_a_full_check() {
        let (dir, pins) = stamped_dir();
        let files: Vec<&Pin> = pins.iter().collect();
        let old = fs::metadata(dir.path().join("one"))
            .unwrap()
            .modified()
            .unwrap();
        // Tamper, then restore size and mtime: only a stamp can hide it.
        fs::write(dir.path().join("one"), "ONE").unwrap();
        set_mtime(&dir.path().join("one"), old);
        // Untrusted stamp (a user-owned install) is ignored: refused.
        assert!(read_stamp(dir.path(), false).is_empty());
        assert!(check_pins(dir.path(), &files, false, &read_stamp(dir.path(), false)).is_err());
        // Missing stamp: refused.
        fs::remove_file(dir.path().join(STAMP)).unwrap();
        assert!(read_stamp(dir.path(), true).is_empty());
        assert!(check_pins(dir.path(), &files, false, &read_stamp(dir.path(), true)).is_err());
        // A stamp recorded for other pins does not vouch for these.
        let (other, _) = stamped_dir();
        let old = fs::metadata(other.path().join("one"))
            .unwrap()
            .modified()
            .unwrap();
        fs::write(other.path().join("one"), "ONE").unwrap();
        set_mtime(&other.path().join("one"), old);
        let mut stamp = read_stamp(other.path(), true);
        assert!(
            check_pins(other.path(), &files, false, &stamp).is_ok(),
            "the stamp hides it"
        );
        stamp[0].sha256 = "0".repeat(64);
        stamp[1].sha256 = "0".repeat(64);
        assert!(check_pins(other.path(), &files, false, &stamp).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn only_a_root_sealed_directory_is_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, pins) = stamped_dir();
        fs::create_dir(dir.path().join("tokenizer")).unwrap();
        let files: Vec<&Pin> = pins.iter().collect();
        // Never for a user-owned directory (these tests run as a user), and
        // never when others can write it, whoever owns it.
        let root = unsafe { libc::geteuid() } == 0;
        assert_eq!(stamp_trusted(dir.path(), &files), root);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!stamp_trusted(dir.path(), &files));
    }
    #[cfg(unix)]
    #[test]
    fn a_writable_parent_untrusts_a_sealed_directory() {
        use std::os::unix::fs::PermissionsExt;
        let me = unsafe { libc::geteuid() };
        let top = tempfile::tempdir().unwrap();
        let sealed = top.path().join("sealed");
        fs::create_dir(&sealed).unwrap();
        for name in ["one", "two", STAMP] {
            fs::write(sealed.join(name), name).unwrap();
        }
        fs::create_dir(sealed.join("tokenizer")).unwrap();
        let pins = [("one", "", ""), ("two", "", "")];
        let files: Vec<&Pin> = pins.iter().collect();
        let mode = |p: &Path, m| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
        // Explicit modes: the umask (0002 on some machines) must not decide.
        for name in ["one", "two", STAMP] {
            mode(&sealed.join(name), 0o644);
        }
        mode(&sealed.join("tokenizer"), 0o755);
        mode(&sealed, 0o755);
        mode(top.path(), 0o755);
        // `me` stands in for root: sealed, so trusted.
        assert!(stamp_trusted_for(&sealed, &files, me));
        // Under a parent others can write it could be renamed away and replaced.
        mode(top.path(), 0o777);
        assert!(!stamp_trusted_for(&sealed, &files, me));
        // Sticky (like /tmp) only lets an entry's owner rename it.
        mode(top.path(), 0o1777);
        assert!(stamp_trusted_for(&sealed, &files, me));
        // A symlink to it is judged by where it really is.
        let link = tempfile::tempdir().unwrap();
        let via = link.path().join("via");
        std::os::unix::fs::symlink(&sealed, &via).unwrap();
        mode(top.path(), 0o777);
        assert!(!stamp_trusted_for(&via, &files, me));
        mode(top.path(), 0o755);
    }
    #[test]
    fn slow_machines_get_a_longer_budget_and_less_memory_use() {
        assert_eq!(auto_timeout_ms(2, true), 5000);
        assert_eq!(auto_timeout_ms(8, false), 5000);
        assert_eq!(auto_timeout_ms(8, true), 1000);
        assert!(!auto_prepacking(Some(2 * 1024 * 1024)));
        assert!(auto_prepacking(Some(8 * 1024 * 1024)));
        assert!(auto_prepacking(None));
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
        load(dir.path(), false, false).unwrap();
        println!("verified load: {} ms", started.elapsed().as_millis());
        fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("model.onnx.data"))
            .unwrap()
            .write_all(b"\0")
            .unwrap();
        let err = load(dir.path(), false, false).err().expect("must refuse");
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
        let engine = load(
            &dir,
            unpinned,
            std::env::var_os("NECTRA_LAYA_NO_PREPACK").is_some(),
        )
        .unwrap();
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
    fn nectra_deb_breaks_older_models() {
        // packaging/build-debs.sh versions nectra-laya from RELEASE the same way.
        let tag = RELEASE.rsplit('/').next().unwrap();
        let version = tag.trim_start_matches("laya-model-").replace('-', "~");
        let breaks = format!("breaks = \"nectra-laya (<< {version})\"");
        assert!(
            include_str!("../Cargo.toml").contains(&breaks),
            "Cargo.toml must say {breaks}"
        );
    }
    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
