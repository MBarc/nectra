//! A shared Laya per user (docs/design/LAYA-SHARED.md): one `nectra laya serve`
//! process owns the model and answers every other Nectra process over a
//! per-user unix socket (a named pipe on Windows), so the 1 GiB / 16 s load is
//! paid once. Newline-delimited JSON, one request per line:
//!
//! - `{"op":"hello"}` -> [`Hello`]: what the server is, and whether it is ready.
//! - `{"op":"ask","prompt","previous","roles","deadline_ms"}` -> a
//!   [`Answer`] or `{"error":"..."}`.
//! - `{"op":"stop"}` -> `{"ok":true}`, then the server exits.
//!
//! A client talks to a server only after its hello matches the client's own
//! identity (protocol, nectra version, hash of the pinned model files); any
//! other server is refused and the client loads the model itself, which gives
//! the same decision. The server exits after an idle period and frees the RAM.
use crate::{config::Config, jev::Answer};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as os;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

#[cfg(all(test, unix))]
mod tests;

/// Bumped when the messages change shape.
pub const PROTO: u32 = 1;
/// The longest request or reply line (a prompt with files inlined is big).
const MAX_LINE: u64 = 8 << 20;
/// Concurrent connections a server serves; more are told "busy".
const MAX_CONNS: usize = 32;
/// How long a silent connection is held.
const CONN_IDLE: Duration = Duration::from_secs(30);

/// Where a server listens: a socket path, or a pipe name on Windows.
#[derive(Clone, Debug)]
pub struct Endpoint(PathBuf);

/// What a server is. A client compares `proto`, `nectra` and `model` with its
/// own before it sends a prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub proto: u32,
    pub nectra: String,
    /// SHA-256 over the pinned hashes of the model files and the runtime.
    pub model: String,
}

impl Identity {
    pub fn current() -> Self {
        Self {
            proto: PROTO,
            nectra: env!("CARGO_PKG_VERSION").to_string(),
            model: crate::laya::model_identity(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    #[serde(flatten)]
    pub identity: Identity,
    /// "loading", "ready" or "failed".
    pub state: String,
    pub error: Option<String>,
    /// The server was told to load an unpinned model.
    pub unpinned: bool,
    pub pid: u32,
    pub uptime_secs: u64,
    pub requests: u64,
    /// Seconds without a request after which the server exits.
    pub idle_limit_secs: u64,
    pub idle_secs: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Hello,
    Ask {
        prompt: String,
        previous: Option<String>,
        roles: Vec<String>,
        /// Milliseconds the caller still waits; a request that sat in the
        /// queue past this is dropped instead of run.
        deadline_ms: Option<u64>,
    },
    Stop,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Reply {
    Answer(Answer),
    Error { error: String },
}

/// What answers the questions: the model, or a stand-in in tests.
pub(crate) trait Answerer: Send + Sync {
    fn answer(&self, prompt: &str, previous: Option<&str>, roles: &[&str]) -> Result<Answer>;
}

/// One line of at most `MAX_LINE` bytes; None at end of input.
fn read_line(r: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let n = r.take(MAX_LINE + 1).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if n as u64 > MAX_LINE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line too long",
        ));
    }
    Ok(Some(line))
}
fn send(w: &mut impl Write, v: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

// ---------------------------------------------------------------- server

struct Shared {
    identity: Identity,
    unpinned: bool,
    idle: Duration,
    started: Instant,
    last: Mutex<Instant>,
    conns: AtomicUsize,
    requests: AtomicU64,
    stop: AtomicBool,
    /// Set once by the loader thread.
    ready: OnceLock<std::result::Result<Box<dyn Answerer>, String>>,
    /// One inference at a time: ONNX Runtime already uses every core.
    infer: Mutex<()>,
}

impl Shared {
    fn hello(&self) -> Hello {
        let (state, error) = match self.ready.get() {
            None => ("loading", None),
            Some(Ok(_)) => ("ready", None),
            Some(Err(e)) => ("failed", Some(e.clone())),
        };
        Hello {
            identity: self.identity.clone(),
            state: state.into(),
            error,
            unpinned: self.unpinned,
            pid: std::process::id(),
            uptime_secs: self.started.elapsed().as_secs(),
            requests: self.requests.load(Ordering::Relaxed),
            idle_limit_secs: self.idle.as_secs(),
            idle_secs: self.last.lock().map_or(0, |l| l.elapsed().as_secs()),
        }
    }
    fn touch(&self) {
        if let Ok(mut l) = self.last.lock() {
            *l = Instant::now();
        }
    }
    /// The idle period after which this server exits: short after a failed
    /// load, so a fixed model directory is retried soon.
    fn idle_limit(&self) -> Duration {
        match self.ready.get() {
            Some(Err(_)) => self.idle.min(Duration::from_secs(30)),
            _ => self.idle,
        }
    }
}

struct Counted<'a>(&'a AtomicUsize);
impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle(shared: &Shared, conn: os::Conn, endpoint: &Endpoint) {
    let n = shared.conns.fetch_add(1, Ordering::SeqCst);
    let _count = Counted(&shared.conns);
    let Ok(mut writer) = conn.try_clone() else {
        return;
    };
    if n >= MAX_CONNS {
        let _ = send(&mut writer, &serde_json::json!({"error": "busy"}));
        return;
    }
    let _ = conn.set_timeout(CONN_IDLE);
    let mut reader = BufReader::new(conn);
    while let Ok(Some(line)) = read_line(&mut reader) {
        let reply = match serde_json::from_str::<Request>(&line) {
            Err(e) => send(&mut writer, &serde_json::json!({"error": format!("bad request: {e}")})),
            Ok(Request::Hello) => send(&mut writer, &shared.hello()),
            Ok(Request::Stop) => {
                shared.stop.store(true, Ordering::SeqCst);
                let r = send(&mut writer, &serde_json::json!({"ok": true}));
                // Wake the acceptor so the server notices.
                let _ = os::connect(endpoint);
                r
            }
            Ok(Request::Ask {
                prompt,
                previous,
                roles,
                deadline_ms,
            }) => {
                let started = Instant::now();
                shared.touch();
                let reply = ask(shared, &prompt, previous.as_deref(), &roles, deadline_ms, started);
                shared.requests.fetch_add(1, Ordering::Relaxed);
                shared.touch();
                match reply {
                    Ok(answer) => send(&mut writer, &answer),
                    Err(e) => send(&mut writer, &serde_json::json!({"error": format!("{e:#}")})),
                }
            }
        };
        if reply.is_err() {
            break;
        }
    }
}

fn ask(
    shared: &Shared,
    prompt: &str,
    previous: Option<&str>,
    roles: &[String],
    deadline_ms: Option<u64>,
    started: Instant,
) -> Result<Answer> {
    let answerer = match shared.ready.get() {
        None => anyhow::bail!("loading"),
        Some(Err(e)) => anyhow::bail!("failed to load: {e}"),
        Some(Ok(a)) => a,
    };
    let _turn = shared.infer.lock().unwrap_or_else(|e| e.into_inner());
    if deadline_ms.is_some_and(|ms| started.elapsed() > Duration::from_millis(ms)) {
        anyhow::bail!("timed out waiting for an earlier message");
    }
    let roles: Vec<&str> = roles.iter().map(String::as_str).collect();
    let _busy = crate::laya::busy();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        answerer.answer(prompt, previous, &roles)
    }))
    .unwrap_or_else(|_| Err(anyhow::anyhow!("the model panicked")))
}

/// Serves `listener` until told to stop or idle too long. `load` runs on its
/// own thread; until it finishes, asks are answered "loading" at once.
pub(crate) fn serve(
    listener: os::Listener,
    endpoint: Endpoint,
    identity: Identity,
    unpinned: bool,
    idle: Duration,
    load: impl FnOnce() -> std::result::Result<Box<dyn Answerer>, String> + Send + 'static,
) {
    let shared = Arc::new(Shared {
        identity,
        unpinned,
        idle,
        started: Instant::now(),
        last: Mutex::new(Instant::now()),
        conns: AtomicUsize::new(0),
        requests: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        ready: OnceLock::new(),
        infer: Mutex::new(()),
    });
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(load))
                .unwrap_or_else(|_| Err("the loader panicked".into()));
            let _ = shared.ready.set(loaded);
            shared.touch();
        });
    }
    let listener = Arc::new(listener);
    {
        let (shared, endpoint, listener) = (shared.clone(), endpoint.clone(), listener.clone());
        std::thread::spawn(move || loop {
            let conn = match listener.accept() {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => continue,
                Err(_) if shared.stop.load(Ordering::SeqCst) => break,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if shared.stop.load(Ordering::SeqCst) {
                break;
            }
            let (shared, endpoint) = (shared.clone(), endpoint.clone());
            std::thread::spawn(move || handle(&shared, conn, &endpoint));
        });
    }
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let loading = shared.ready.get().is_none();
        let idle = shared.last.lock().map_or(Duration::ZERO, |l| l.elapsed());
        if !loading && shared.conns.load(Ordering::SeqCst) == 0 && idle >= shared.idle_limit() {
            break;
        }
    }
    shared.stop.store(true, Ordering::SeqCst);
    listener.cleanup();
}

/// `nectra laya serve`: the real model. Returns when the server has stopped.
pub fn run_server(cfg: &Config, idle_override: Option<u64>) -> Result<()> {
    let ep = os::default_endpoint(true)?.context("no place for the socket")?;
    let listener = match os::activated() {
        Some(l) => l,
        None => os::bind(&ep)?,
    };
    let idle = Duration::from_secs(idle_override.unwrap_or(cfg.inner.classifier.laya_idle_secs));
    let unpinned = cfg.inner.classifier.allow_unpinned_model;
    println!(
        "{}",
        serde_json::json!({"event": "listening", "socket": ep.display(), "pid": std::process::id()})
    );
    let cfg = cfg.clone();
    serve(listener, ep, Identity::current(), unpinned, idle, move || {
        crate::laya::served_engine(&cfg)
    });
    Ok(())
}

// ---------------------------------------------------------------- client

pub(crate) struct Client {
    pub endpoint: Endpoint,
    pub identity: Identity,
    /// Budget for one request once the server is ready.
    pub timeout: Duration,
    /// The hello must come back within this.
    pub hello_timeout: Duration,
    /// How long a one-shot caller waits for a starting server.
    pub load_wait: Duration,
    /// Starts a detached server; None when the caller must not.
    pub spawn: Option<Box<dyn Fn() -> std::io::Result<Option<std::process::Child>> + Send + Sync>>,
    /// Accept a server that loaded an unpinned model.
    pub allow_unpinned: bool,
}

/// What a request to the service came to.
#[derive(Debug)]
pub(crate) enum Outcome {
    Answer(Answer),
    /// A hello-only call found the server ready.
    Ready,
    /// Use the model in this process instead (the reason).
    Local(String),
    /// The caller falls back to keywords (the reason, in the words the route line shows).
    Failed(String),
}

/// A server this call started: waited for on a thread once the call is done,
/// so a long-lived caller keeps no zombie when the server later exits.
struct Reap(Option<std::process::Child>);
impl Drop for Reap {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            if !matches!(c.try_wait(), Ok(Some(_))) {
                std::thread::spawn(move || {
                    let _ = c.wait();
                });
            }
        }
    }
}

enum Step {
    Absent,
    Loading,
    Ready,
    Answer(Answer),
    Outcome(Outcome),
}

const IN_HELLO: u8 = 0;
const IN_ASK: u8 = 1;

struct AskReq<'a> {
    prompt: &'a str,
    previous: Option<&'a str>,
    roles: &'a [&'a str],
}

impl Client {
    /// One connection: hello, check, then (with a request) the question.
    fn step(&self, req: Option<&AskReq>) -> Step {
        let (tx, rx) = mpsc::channel();
        let phase = Arc::new(AtomicU8::new(IN_HELLO));
        let work = {
            let (ep, id, phase) = (self.endpoint.clone(), self.identity.clone(), phase.clone());
            let (timeout, hello_t, allow) = (self.timeout, self.hello_timeout, self.allow_unpinned);
            let body = req.map(|r| {
                (
                    r.prompt.to_string(),
                    r.previous.map(str::to_string),
                    r.roles.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
            });
            move || {
                let _ = tx.send(exchange(&ep, &id, allow, body, timeout, hello_t, &phase));
            }
        };
        std::thread::spawn(work);
        // Backstop for transports with no read timeout.
        match rx.recv_timeout(self.hello_timeout + self.timeout + Duration::from_millis(500)) {
            Ok(step) => step,
            Err(_) if phase.load(Ordering::SeqCst) == IN_ASK => Step::Outcome(Outcome::Failed(
                format!("Laya timed out: no answer within {} ms", self.timeout.as_millis()),
            )),
            Err(_) => Step::Outcome(Outcome::Local("the server does not answer".into())),
        }
    }

    /// Waits (or not) for a ready server, starting one when none is running.
    /// With `req`, asks it too.
    pub fn call(&self, req: Option<(&str, Option<&str>, &[&str])>, wait: bool) -> Outcome {
        let req = req.map(|(prompt, previous, roles)| AskReq {
            prompt,
            previous,
            roles,
        });
        let give_up = Instant::now() + self.load_wait;
        let mut child = Reap(None);
        let mut started = false;
        loop {
            let exited = child
                .0
                .as_mut()
                .is_some_and(|c| matches!(c.try_wait(), Ok(Some(_))));
            match self.step(req.as_ref()) {
                Step::Answer(a) => return Outcome::Answer(a),
                Step::Outcome(o) => return o,
                Step::Ready => return Outcome::Ready,
                Step::Loading if !wait => return Outcome::Failed("Laya still loading".into()),
                Step::Loading => {}
                Step::Absent => {
                    if exited {
                        return Outcome::Local("the server exited".into());
                    }
                    if !started {
                        started = true;
                        match self.spawn.as_ref().map(|s| s()) {
                            None => return Outcome::Local("no Laya service running".into()),
                            Some(Err(e)) => {
                                return Outcome::Local(format!("cannot start the server: {e}"))
                            }
                            Some(Ok(c)) => child.0 = c,
                        }
                    }
                    if !wait {
                        return Outcome::Failed("Laya still loading".into());
                    }
                }
            }
            if Instant::now() >= give_up {
                return Outcome::Local("the server did not become ready".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn exchange(
    ep: &Endpoint,
    me: &Identity,
    allow_unpinned: bool,
    req: Option<(String, Option<String>, Vec<String>)>,
    timeout: Duration,
    hello_timeout: Duration,
    phase: &AtomicU8,
) -> Step {
    use std::io::ErrorKind::*;
    let conn = match os::connect(ep) {
        Ok(c) => c,
        Err(e) if matches!(e.kind(), NotFound | ConnectionRefused) => return Step::Absent,
        Err(e) => return Step::Outcome(Outcome::Local(format!("cannot connect: {e}"))),
    };
    let broken = |e: std::io::Error| Step::Outcome(Outcome::Local(format!("the server failed: {e}")));
    let _ = conn.set_timeout(hello_timeout);
    let Ok(mut writer) = conn.try_clone() else {
        return Step::Outcome(Outcome::Local("cannot duplicate the connection".into()));
    };
    let mut reader = BufReader::new(conn);
    if let Err(e) = send(&mut writer, &Request::Hello) {
        return broken(e);
    }
    let hello = match read_line(&mut reader) {
        Ok(Some(line)) => match serde_json::from_str::<Hello>(&line) {
            Ok(h) => h,
            Err(_) => return Step::Outcome(Outcome::Local("the server speaks another protocol".into())),
        },
        Ok(None) => return Step::Outcome(Outcome::Local("the server hung up".into())),
        Err(e) => return broken(e),
    };
    if hello.identity != *me {
        return Step::Outcome(Outcome::Local(format!(
            "stale server (nectra {}, protocol {}, model {}; this is nectra {}, protocol {}, model {})",
            hello.identity.nectra,
            hello.identity.proto,
            &hello.identity.model[..hello.identity.model.len().min(12)],
            me.nectra,
            me.proto,
            &me.model[..me.model.len().min(12)],
        )));
    }
    if hello.unpinned && !allow_unpinned {
        return Step::Outcome(Outcome::Local("the server runs an unpinned model".into()));
    }
    match hello.state.as_str() {
        "ready" => {}
        "failed" => {
            return Step::Outcome(Outcome::Failed(format!(
                "Laya failed to load: {}",
                hello.error.unwrap_or_default()
            )))
        }
        _ => return Step::Loading,
    }
    let Some((prompt, previous, roles)) = req else {
        return Step::Ready;
    };
    phase.store(IN_ASK, Ordering::SeqCst);
    let _ = reader.get_ref().set_timeout(timeout);
    let ask = Request::Ask {
        prompt,
        previous,
        roles,
        deadline_ms: Some(timeout.as_millis() as u64),
    };
    if let Err(e) = send(&mut writer, &ask) {
        return broken(e);
    }
    match read_line(&mut reader) {
        Ok(Some(line)) => match serde_json::from_str::<Reply>(&line) {
            Ok(Reply::Answer(a)) => Step::Answer(a),
            Ok(Reply::Error { error }) if error == "loading" => Step::Loading,
            Ok(Reply::Error { error }) if error.starts_with("timed out") => Step::Outcome(
                Outcome::Failed(format!("Laya timed out: no answer within {} ms", timeout.as_millis())),
            ),
            Ok(Reply::Error { error }) => Step::Outcome(Outcome::Failed(format!("Laya failed: {error}"))),
            Err(_) => Step::Outcome(Outcome::Local("the server sent an unreadable reply".into())),
        },
        Ok(None) => Step::Outcome(Outcome::Local("the server hung up".into())),
        Err(e) if matches!(e.kind(), WouldBlock | TimedOut) => Step::Outcome(Outcome::Failed(
            format!("Laya timed out: no answer within {} ms", timeout.as_millis()),
        )),
        Err(e) => broken(e),
    }
}

// ---------------------------------------------------------------- commands

/// The client the process-wide settings describe, or None when the service
/// is off or cannot be used (unpinned model, no place for a socket).
pub(crate) fn client_for(cfg: &Config) -> Option<Client> {
    use crate::config::LayaService;
    let c = &cfg.inner.classifier;
    let off = std::env::var("NECTRA_LAYA_SERVICE").is_ok_and(|v| v == "off");
    if off || c.laya_service == LayaService::Off || c.allow_unpinned_model {
        return None;
    }
    let endpoint = os::default_endpoint(false).ok()??;
    let config_path = cfg.path.clone();
    Some(Client {
        endpoint,
        identity: Identity::current(),
        timeout: Duration::from_millis(crate::laya::timeout_ms(cfg)),
        hello_timeout: Duration::from_secs(2),
        load_wait: Duration::from_secs(180),
        spawn: Some(Box::new(move || spawn_server(&config_path).map(Some))),
        allow_unpinned: false,
    })
}

/// The endpoint `laya status|stop` talk to, even when its directory does not exist yet.
fn status_endpoint() -> Result<Option<Endpoint>> {
    os::default_endpoint(false)
}

/// Starts `nectra laya serve` detached, with no console and no terminal.
fn spawn_server(config: &std::path::Path) -> std::io::Result<std::process::Child> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    if config.as_os_str() != "<embedded>" {
        cmd.arg("--config").arg(config);
    }
    cmd.args(["laya", "serve"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session of its own: it outlives this terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            })
        };
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    cmd.spawn()
}

/// `nectra laya status`: the server's hello, or that none runs. Reports a
/// server this build would refuse, with the reason, rather than hiding it.
pub fn status() -> Result<serde_json::Value> {
    let Some(ep) = status_endpoint()? else {
        return Ok(serde_json::json!({"running": false}));
    };
    let Some(hello) = oneshot(&ep, &Request::Hello)? else {
        return Ok(serde_json::json!({"running": false, "socket": ep.display()}));
    };
    let hello: Hello = serde_json::from_str(&hello).context("unreadable hello")?;
    let mine = Identity::current();
    let mut v = serde_json::to_value(&hello)?;
    v["running"] = true.into();
    v["socket"] = ep.display().into();
    v["compatible"] = (hello.identity == mine).into();
    if hello.identity != mine {
        v["this_build"] = serde_json::to_value(&mine)?;
    }
    Ok(v)
}

/// `nectra laya stop`: asks the server to exit and waits for it to go.
pub fn stop() -> Result<serde_json::Value> {
    let Some(ep) = status_endpoint()? else {
        return Ok(serde_json::json!({"stopped": false, "running": false}));
    };
    if oneshot(&ep, &Request::Stop)?.is_none() {
        return Ok(serde_json::json!({"stopped": false, "running": false}));
    }
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if os::connect(&ep).is_err() {
            return Ok(serde_json::json!({"stopped": true}));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(serde_json::json!({"stopped": false, "running": true, "error": "still running after 5 s"}))
}

/// One request, one reply line; None when nothing listens.
fn oneshot(ep: &Endpoint, req: &Request) -> Result<Option<String>> {
    let conn = match os::connect(ep) {
        Ok(c) => c,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => {
            return Ok(None)
        }
        Err(e) => return Err(e).context("connecting to the Laya server"),
    };
    let _ = conn.set_timeout(Duration::from_secs(5));
    let mut writer = conn.try_clone()?;
    send(&mut writer, req)?;
    Ok(read_line(&mut BufReader::new(conn))?)
}

/// SHA-256 over strings, hex.
pub(crate) fn sha_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
