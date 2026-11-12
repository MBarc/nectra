use super::*;
use crate::classifier::TaskType;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicUsize;

/// A model stand-in whose answer depends on the prompt alone, with floats
/// that do not survive a careless text round trip.
struct Fake {
    delay: Duration,
    inside: AtomicUsize,
    most: AtomicUsize,
}
impl Fake {
    fn new(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            delay: Duration::from_millis(delay_ms),
            inside: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
        })
    }
}
fn expected(prompt: &str, previous: Option<&str>, roles: &[&str]) -> Answer {
    let h = sha_hex(&[prompt, previous.unwrap_or("-")]);
    let f = |i: usize| {
        u64::from_str_radix(&h[i * 8..i * 8 + 8], 16).unwrap() as f64 / u32::MAX as f64
    };
    Answer {
        task: if f(0) < 0.5 {
            TaskType::Debugging
        } else {
            TaskType::CodeGeneration
        },
        confidence: f(1) / 3.0,
        difficulty: f(2) * 0.1 + 1.0 / 3.0,
        new_task: previous.map(|_| f(3) / 7.0),
        role: roles.first().map(|r| r.to_string()),
    }
}
impl Answerer for Arc<Fake> {
    fn answer(&self, prompt: &str, previous: Option<&str>, roles: &[&str]) -> Result<Answer> {
        let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.inside.fetch_sub(1, Ordering::SeqCst);
        Ok(expected(prompt, previous, roles))
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    ep: Endpoint,
    fake: Arc<Fake>,
    done: Option<std::thread::JoinHandle<()>>,
}
impl Rig {
    fn endpoint() -> (tempfile::TempDir, Endpoint) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let ep = Endpoint::new(dir.path().join("l.sock")).unwrap();
        (dir, ep)
    }
    fn start(identity: Identity, load_ms: u64, delay_ms: u64, idle: Duration) -> Rig {
        let (dir, ep) = Self::endpoint();
        let fake = Fake::new(delay_ms);
        let listener = os::bind(&ep).unwrap();
        let (f, e) = (fake.clone(), ep.clone());
        let done = std::thread::spawn(move || {
            serve(listener, e, identity, false, idle, move || {
                std::thread::sleep(Duration::from_millis(load_ms));
                Ok(Box::new(f))
            })
        });
        Rig {
            _dir: dir,
            ep,
            fake,
            done: Some(done),
        }
    }
    fn ok() -> Rig {
        Self::start(Identity::current(), 0, 0, Duration::from_secs(60))
    }
    fn client(&self) -> Client {
        client(self.ep.clone(), Identity::current())
    }
    fn hello(&self) -> Hello {
        serde_json::from_str(&oneshot(&self.ep, &Request::Hello).unwrap().unwrap()).unwrap()
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = oneshot(&self.ep, &Request::Stop);
        if let Some(d) = self.done.take() {
            let _ = d.join();
        }
    }
}
fn client(ep: Endpoint, identity: Identity) -> Client {
    Client {
        endpoint: ep,
        identity,
        timeout: Duration::from_secs(5),
        hello_timeout: Duration::from_secs(1),
        load_wait: Duration::from_secs(10),
        spawn: None,
        allow_unpinned: false,
    }
}
fn ask1(c: &Client, prompt: &str, previous: Option<&str>, wait: bool) -> Outcome {
    c.call(Some((prompt, previous, &["small", "big"])), wait)
}
impl Hello {
    fn of(identity: Identity, state: &str) -> Hello {
        Hello {
            identity,
            state: state.into(),
            error: None,
            unpinned: false,
            pid: 0,
            uptime_secs: 0,
            requests: 0,
            idle_limit_secs: 0,
            idle_secs: 0,
        }
    }
}

#[test]
fn a_served_decision_is_identical_to_a_direct_one() {
    let rig = Rig::ok();
    let c = rig.client();
    let prompts: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../eval/holdout.json")).unwrap();
    assert!(prompts.len() > 20);
    for (i, case) in prompts.iter().enumerate() {
        let prompt = case["prompt"].as_str().unwrap();
        let previous = (i % 2 == 0).then_some("an earlier message");
        let Outcome::Answer(served) = ask1(&c, prompt, previous, true) else {
            panic!("no answer for {prompt}")
        };
        // Bit for bit: confidence and difficulty are floats.
        assert_eq!(
            served,
            expected(prompt, previous, &["small", "big"]),
            "{prompt}"
        );
    }
}

#[test]
fn concurrent_clients_each_get_their_own_answer_one_inference_at_a_time() {
    let rig = Rig::start(Identity::current(), 0, 5, Duration::from_secs(60));
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let c = rig.client();
            std::thread::spawn(move || {
                for n in 0..5 {
                    let prompt = format!("client {t} message {n}");
                    match ask1(&c, &prompt, None, true) {
                        Outcome::Answer(a) => {
                            assert_eq!(a, expected(&prompt, None, &["small", "big"]))
                        }
                        other => panic!("{prompt}: {other:?}"),
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(
        rig.fake.most.load(Ordering::SeqCst),
        1,
        "inference must be serial"
    );
    assert_eq!(rig.hello().requests, 40);
}

#[test]
fn a_stale_or_mismatched_server_is_refused_before_any_prompt_is_sent() {
    let mut cases = vec![];
    let mut old = Identity::current();
    old.nectra = "0.0.1".into();
    cases.push(("version", old));
    let mut other = Identity::current();
    other.model = "0".repeat(64);
    cases.push(("model", other));
    let mut future = Identity::current();
    future.proto += 1;
    cases.push(("protocol", future));
    for (what, identity) in cases {
        let rig = Rig::start(identity, 0, 0, Duration::from_secs(60));
        let Outcome::Local(why) = ask1(&rig.client(), "secret prompt", None, true) else {
            panic!("{what}: a mismatched server was used")
        };
        assert!(why.starts_with("stale server"), "{what}: {why}");
        assert_eq!(
            rig.hello().requests,
            0,
            "{what}: a prompt reached the server"
        );
    }
}

#[test]
fn an_unpinned_server_is_refused_unless_asked_for() {
    let (_dir, ep) = Rig::endpoint();
    let listener = os::bind(&ep).unwrap();
    let e = ep.clone();
    let t = std::thread::spawn(move || {
        serve(
            listener,
            e,
            Identity::current(),
            true,
            Duration::from_secs(60),
            || Ok(Box::new(Fake::new(0))),
        )
    });
    let c = client(ep.clone(), Identity::current());
    assert!(matches!(ask1(&c, "x", None, true), Outcome::Local(w) if w.contains("unpinned")));
    let _ = oneshot(&ep, &Request::Stop);
    t.join().unwrap();
}

#[test]
fn a_dead_server_falls_back() {
    use std::os::unix::net::UnixListener;
    let local = |c: Client| matches!(ask1(&c, "x", None, true), Outcome::Local(_));
    // Nothing there at all.
    let (_d, ep) = Rig::endpoint();
    assert!(local(client(ep, Identity::current())));
    // A socket file nobody listens on (a crashed server).
    let (_d, ep) = Rig::endpoint();
    drop(UnixListener::bind(&ep.0).unwrap());
    assert!(ep.0.exists());
    assert!(local(client(ep, Identity::current())));
    // A server that hangs up on every connection.
    let (_d, ep) = Rig::endpoint();
    let l = UnixListener::bind(&ep.0).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            drop(s)
        }
    });
    assert!(local(client(ep, Identity::current())));
    // A server that accepts and never speaks.
    let (_d, ep) = Rig::endpoint();
    let l = UnixListener::bind(&ep.0).unwrap();
    std::thread::spawn(move || {
        let mut held = vec![];
        for s in l.incoming() {
            held.push(s);
        }
    });
    let started = Instant::now();
    let mut c = client(ep, Identity::current());
    c.hello_timeout = Duration::from_millis(300);
    assert!(local(c));
    assert!(started.elapsed() < Duration::from_secs(3));
    // A server that dies in the middle of a question.
    let (_d, ep) = Rig::endpoint();
    let l = UnixListener::bind(&ep.0).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            read_line(&mut r).unwrap();
            send(&mut s, &Hello::of(Identity::current(), "ready")).unwrap();
            read_line(&mut r).unwrap();
            drop(s);
        }
    });
    assert!(local(client(ep, Identity::current())));
}

#[test]
fn a_loading_server_answers_keywords_for_chat_and_waits_for_one_shot() {
    let rig = Rig::start(Identity::current(), 600, 0, Duration::from_secs(60));
    let c = rig.client();
    let started = Instant::now();
    assert!(matches!(ask1(&c, "x", None, false), Outcome::Failed(w) if w == "Laya still loading"));
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "must not wait for the load"
    );
    assert!(matches!(ask1(&c, "x", None, true), Outcome::Answer(_)));
}

#[test]
fn a_failed_load_is_reported_not_retried_in_process() {
    let (_d, ep) = Rig::endpoint();
    let listener = os::bind(&ep).unwrap();
    let e = ep.clone();
    let t = std::thread::spawn(move || {
        serve(
            listener,
            e,
            Identity::current(),
            false,
            Duration::from_secs(60),
            || Err("model.onnx.data does not match its pin".into()),
        )
    });
    let c = client(ep.clone(), Identity::current());
    std::thread::sleep(Duration::from_millis(200));
    match ask1(&c, "x", None, true) {
        Outcome::Failed(w) => assert!(
            w.starts_with("Laya failed to load: model.onnx.data"),
            "{w}"
        ),
        other => panic!("{other:?}"),
    }
    let _ = oneshot(&ep, &Request::Stop);
    t.join().unwrap();
}

#[test]
fn a_slow_answer_times_out_at_the_callers_budget() {
    let rig = Rig::start(Identity::current(), 0, 800, Duration::from_secs(60));
    let mut c = rig.client();
    c.timeout = Duration::from_millis(200);
    let started = Instant::now();
    match ask1(&c, "x", None, true) {
        Outcome::Failed(w) => assert_eq!(w, "Laya timed out: no answer within 200 ms"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_millis(700));
}

#[test]
fn a_missing_server_is_started_on_demand() {
    let (_dir, ep) = Rig::endpoint();
    let started = Arc::new(AtomicUsize::new(0));
    let (e2, s2) = (ep.clone(), started.clone());
    let mut c = client(ep.clone(), Identity::current());
    c.spawn = Some(Box::new(move || {
        s2.fetch_add(1, Ordering::SeqCst);
        let listener = os::bind(&e2).unwrap();
        let e3 = e2.clone();
        std::thread::spawn(move || {
            serve(
                listener,
                e3,
                Identity::current(),
                false,
                Duration::from_secs(60),
                || {
                    std::thread::sleep(Duration::from_millis(300));
                    Ok(Box::new(Fake::new(0)))
                },
            )
        });
        Ok(None)
    }));
    // Chat: keywords while it starts, the server started once.
    assert!(matches!(ask1(&c, "x", None, false), Outcome::Failed(w) if w == "Laya still loading"));
    // One-shot: waits for it.
    let Outcome::Answer(a) = ask1(&c, "hello", None, true) else {
        panic!()
    };
    assert_eq!(a, expected("hello", None, &["small", "big"]));
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let _ = oneshot(&ep, &Request::Stop);
}

#[test]
fn the_server_exits_when_idle_and_removes_its_socket() {
    let rig = Rig::start(Identity::current(), 0, 0, Duration::from_millis(400));
    let c = rig.client();
    assert!(matches!(ask1(&c, "x", None, true), Outcome::Answer(_)));
    let started = Instant::now();
    while rig.ep.0.exists() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!rig.ep.0.exists(), "socket left behind");
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "exited while it was in use"
    );
    // Gone means the caller falls back, or starts it again.
    assert!(matches!(ask1(&c, "x", None, true), Outcome::Local(_)));
}

#[test]
fn the_socket_is_private_and_only_one_server_binds_it() {
    let rig = Rig::ok();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&rig.ep.0), 0o600);
    assert_eq!(mode(rig.ep.0.parent().unwrap()), 0o700);
    let second = os::bind(&rig.ep)
        .err()
        .expect("a second server must not bind");
    assert!(
        format!("{second:#}").contains("another Laya server"),
        "{second:#}"
    );
    // The first is untouched by the attempt.
    assert!(matches!(
        ask1(&rig.client(), "x", None, true),
        Outcome::Answer(_)
    ));
}

#[test]
fn a_directory_open_to_others_or_a_symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let open = dir.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ep = Endpoint::new(open.join("l.sock")).unwrap();
    assert!(os::bind(&ep).is_err());
    let private = dir.path().join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&private, &link).unwrap();
    let ep = Endpoint::new(link.join("l.sock")).unwrap();
    assert!(os::bind(&ep).is_err());
    // A planted regular file is never replaced by the server.
    let ep = Endpoint::new(private.join("l.sock")).unwrap();
    std::fs::write(&ep.0, b"x").unwrap();
    assert!(os::bind(&ep).is_err());
    assert_eq!(std::fs::read(&ep.0).unwrap(), b"x");
    // And a client will not talk to it.
    let c = client(ep, Identity::current());
    assert!(matches!(ask1(&c, "x", None, true), Outcome::Local(_)));
}

#[test]
fn oversize_and_malformed_requests_do_not_hurt_the_server() {
    let rig = Rig::ok();
    let mut conn = os::connect(&rig.ep).unwrap();
    conn.write_all(b"not json\n").unwrap();
    let mut r = BufReader::new(conn.try_clone().unwrap());
    assert!(read_line(&mut r).unwrap().unwrap().contains("bad request"));
    let big = vec![b'a'; (MAX_LINE + 10) as usize];
    let _ = conn.write_all(&big);
    drop(conn);
    assert!(matches!(
        ask1(&rig.client(), "x", None, true),
        Outcome::Answer(_)
    ));
}

#[test]
fn stop_ends_the_server() {
    let rig = Rig::ok();
    assert!(oneshot(&rig.ep, &Request::Stop)
        .unwrap()
        .unwrap()
        .contains("true"));
    let started = Instant::now();
    while rig.ep.0.exists() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!rig.ep.0.exists());
}

/// The served answer for every held-out prompt equals the one this process's
/// own engine gives, with the real model:
/// `NECTRA_LAYA_MODEL=<model dir> cargo test --release -- --ignored served_real`
#[test]
#[ignore = "needs a model in NECTRA_LAYA_MODEL"]
fn served_real_model_matches_in_process() {
    let dir = PathBuf::from(std::env::var_os("NECTRA_LAYA_MODEL").unwrap());
    let mut cfg = Config::from_defaults().unwrap();
    cfg.inner.classifier.model_dir = Some(dir);
    cfg.inner.classifier.backend = crate::config::ClassifierBackend::Laya;
    let (_d, ep) = Rig::endpoint();
    let listener = os::bind(&ep).unwrap();
    let (e, cfg2) = (ep.clone(), cfg.clone());
    let server = std::thread::spawn(move || {
        serve(
            listener,
            e,
            Identity::current(),
            false,
            Duration::from_secs(120),
            move || crate::laya::served_engine(&cfg2),
        )
    });
    let c = client(ep.clone(), Identity::current());
    let roles = ["fast_cheap", "strong_general", "coding_specialist"];
    let prompts: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../eval/holdout.json")).unwrap();
    let engine = match crate::laya::served_engine(&cfg) {
        Ok(e) => e,
        Err(e) => panic!("model does not load: {e}"),
    };
    let mut checked = 0;
    for (i, case) in prompts.iter().enumerate() {
        let prompt = case["prompt"].as_str().unwrap();
        let previous = (i % 3 == 0).then_some("fix the failing build");
        let Outcome::Answer(served) = c.call(Some((prompt, previous, &roles)), true) else {
            panic!("no served answer for {prompt}")
        };
        let direct = engine.answer(prompt, previous, &roles).unwrap();
        assert_eq!(served, direct, "{prompt}");
        checked += 1;
    }
    println!("{checked} held-out prompts: served == in-process");
    let _ = oneshot(&ep, &Request::Stop);
    server.join().unwrap();
}
