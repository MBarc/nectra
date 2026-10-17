use anyhow::{bail, Result};
use nectra::{
    chat::{self, Backend, ChatOptions},
    cli::Mode,
    config::Config,
    models::{Registry, Runtime},
    router::Decision,
    session::Session,
};
use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Write},
    process::{Command, Stdio},
};

fn fixture() -> (tempfile::TempDir, Config, Registry) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        include_str!("../config/nectra.toml"),
    )
    .unwrap();
    fs::write(
        dir.path().join("models.toml"),
        include_str!("../config/models.toml"),
    )
    .unwrap();
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let registry = Registry::load(&cfg).unwrap();
    (dir, cfg, registry)
}
#[derive(Default)]
struct Mock {
    deny_once: bool,
    queued: Vec<String>,
    auth_once: bool,
    logins: Vec<String>,
    prompts: Vec<String>,
    models: Vec<String>,
    codex_only: bool,
    fail_next: bool,
    limit_all: bool,
    limit_anthropic: bool,
    started_work: bool,
}
impl Backend for Mock {
    fn take_queued_messages(&mut self) -> Vec<String> {
        if self.models.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut self.queued)
        }
    }
    fn login(&mut self, provider: &str, _: &Config) -> Result<()> {
        self.logins.push(provider.into());
        Ok(())
    }
    fn available(&self, runtime: Runtime, _: &Config) -> bool {
        !self.codex_only || runtime == Runtime::Codex
    }
    fn respond(
        &mut self,
        d: &Decision,
        prompt: &str,
        _: &Config,
        output: &mut dyn Write,
    ) -> Result<String> {
        self.prompts.push(prompt.into());
        self.models.push(d.model.clone());
        if self.deny_once {
            self.deny_once = false;
            return Err(nectra::access::ModelUnavailable {
                safe_to_retry: !self.started_work,
            }
            .into());
        }
        if self.auth_once {
            self.auth_once = false;
            return Err(nectra::executor::AuthenticationRequired {
                safe_to_retry: true,
            }
            .into());
        }
        if self.limit_all || (self.limit_anthropic && d.provider == "anthropic") {
            return Err(nectra::limits::RateLimited {
                message: "usage limit reached".into(),
                provider_wide: true,
                retry_after: std::time::Duration::from_secs(60),
                safe_to_retry: !self.started_work,
            }
            .into());
        }
        if self.fail_next {
            self.fail_next = false;
            bail!("model temporarily unavailable");
        }
        let reply = format!("answer from {}", d.model);
        writeln!(output, "{reply}")?;
        Ok(reply)
    }
}
fn options() -> ChatOptions {
    ChatOptions {
        // Tests must not touch the real cooldown state on this machine.
        cooldown_state: None,
        mode: Mode::Best,
        provider: None,
        model: None,
        explain: false,
        dry_run: false,
    }
}
fn context(prompt: &str) -> serde_json::Value {
    serde_json::from_str(prompt.split_once('\n').unwrap().1).unwrap()
}

#[test]
fn debate_dry_run_and_rejected_bypass_never_reach_writable_backend() {
    for dry_run in [true, false] {
        let (dir, mut cfg, registry) = fixture();
        cfg.skip_permissions = !dry_run;
        let mut opts = options();
        opts.dry_run = dry_run;
        let mut session = Session::create(&dir.path().join("debate.jsonl"), dir.path()).unwrap();
        let mut backend = Mock::default();
        let mut output = vec![];
        chat::run_loop(
            &cfg,
            &registry,
            opts,
            &mut session,
            &mut Cursor::new("/debate on\nReview the architecture\n/quit\n"),
            &mut output,
            &mut backend,
        )
        .unwrap();
        assert!(backend.models.is_empty());
        assert!(session.exchanges.is_empty());
        let output = String::from_utf8(output).unwrap();
        if dry_run {
            assert!(output.contains("\"candidates\""));
        } else {
            assert!(output.contains("does not accept --dangerously-skip-permissions"));
        }
    }
}

#[test]
fn reauthentication_restores_provider_without_replaying_prior_work() {
    let (dir, mut cfg, reg) = fixture();
    // "hello again" continues the task GPT answered; test re-login on its own.
    cfg.inner.routing.sticky = false;
    let mut session = Session::create(&dir.path().join("auth.jsonl"), dir.path()).unwrap();
    let mut backend = Mock {
        auth_once: true,
        ..Default::default()
    };
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("hello\n/login anthropic\nhello again\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.logins, ["anthropic"]);
    assert_eq!(backend.models.len(), 3);
    // Short chit-chat goes to the cheap model.
    assert_eq!(backend.models[0], "haiku");
    assert_eq!(backend.models[2], "haiku");
    assert_eq!(session.exchanges.len(), 2);
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("needs authentication"));
}

#[test]
fn rate_limits_fail_over_and_keep_context() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("limits.jsonl"), dir.path()).unwrap();
    let mut backend = Mock {
        limit_anthropic: true,
        ..Default::default()
    };
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("hello\nhello again\n/quit\n"),
        &mut vec![],
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.models.len(), 3);
    assert_eq!(backend.models[0], "haiku");
    assert!(backend.models[1..].iter().all(|m| m.starts_with("gpt-")));
    assert_eq!(backend.prompts[0], backend.prompts[1]);
    assert_eq!(session.exchanges.len(), 2);
}

#[test]
fn exhaustion_and_partial_work_do_not_replay_forever() {
    for started_work in [false, true] {
        let (dir, cfg, reg) = fixture();
        let mut session = Session::create(&dir.path().join("limits.jsonl"), dir.path()).unwrap();
        let mut backend = Mock {
            limit_all: true,
            started_work,
            ..Default::default()
        };
        chat::run_loop(
            &cfg,
            &reg,
            options(),
            &mut session,
            &mut Cursor::new("hello\n/models\n/quit\n"),
            &mut vec![],
            &mut backend,
        )
        .unwrap();
        assert_eq!(backend.models.len(), if started_work { 1 } else { 2 });
        assert!(session.exchanges.is_empty());
    }
}

#[test]
fn switching_providers_keeps_one_conversation() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("thread.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    let mut output = vec![];
    chat::run_loop(&cfg, &reg, options(), &mut session, &mut Cursor::new("/model opus\nRemember our project is Pebble.\n/model gpt-5.6-sol\nWhat is the project called?\n/quit\n"), &mut output, &mut backend).unwrap();
    assert_eq!(backend.models, ["opus", "gpt-5.6-sol"]);
    let second = context(&backend.prompts[1]);
    assert_eq!(
        second["conversation"][0]["user"],
        "Remember our project is Pebble."
    );
    assert_eq!(second["conversation"][0]["assistant"], "answer from opus");
    assert_eq!(session.exchanges.len(), 2);
    let path = session.path.clone();
    drop(session);
    let resumed = Session::resume(&path, dir.path()).unwrap();
    assert_eq!(resumed.exchanges[1].model, "gpt-5.6-sol");
}
#[test]
fn automatic_routing_reconsiders_each_new_message() {
    let (dir, cfg, mut reg) = fixture();
    reg.models
        .retain(|m| m.id == "opus" || m.id == "gpt-5.6-sol");
    // Controlled profiles isolate task-sensitive selection from the editable shipped policy.
    for model in &mut reg.models {
        let coding = model.runtime == Runtime::Codex;
        model.capabilities.coding = if coding { 1.0 } else { 0.1 };
        model.capabilities.tool_use = if coding { 1.0 } else { 0.1 };
        model.capabilities.writing = if coding { 0.1 } else { 1.0 };
        model.capabilities.reasoning = 0.9;
        model.capabilities.long_context = 0.9;
        model.capabilities.speed = 0.8;
        model.capabilities.cost_efficiency = 0.8;
    }
    let mut session = Session::create(&dir.path().join("auto.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("draft a memo\nimplement a function for the API\n/quit\n"),
        &mut vec![],
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.models, ["opus", "gpt-5.6-sol"]);
    assert_eq!(
        context(&backend.prompts[1])["conversation"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn unavailable_runtime_is_excluded_and_failed_turn_does_not_pollute_history() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("failed.jsonl"), dir.path()).unwrap();
    let mut backend = Mock {
        codex_only: true,
        fail_next: true,
        ..Mock::default()
    };
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("debug first\ntry a different task\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    assert!(backend.models.iter().all(|m| m.starts_with("gpt-")));
    assert!(context(&backend.prompts[1])["conversation"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(session.exchanges.len(), 1);
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("session remains open"));
}
#[test]
fn paste_clear_and_preview_are_local_session_operations() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("paste.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/paste\nline one\n/model literal\n/send\n/clear\nnext\n/quit\n"),
        &mut vec![],
        &mut backend,
    )
    .unwrap();
    assert_eq!(
        context(&backend.prompts[0])["current_user_message"],
        "line one\n/model literal\n"
    );
    assert!(context(&backend.prompts[1])["conversation"]
        .as_array()
        .unwrap()
        .is_empty());
    let count = session.exchanges.len();
    let mut preview = options();
    preview.dry_run = true;
    chat::run_loop(
        &cfg,
        &reg,
        preview,
        &mut session,
        &mut Cursor::new("write a memo\n/quit\n"),
        &mut vec![],
        &mut backend,
    )
    .unwrap();
    assert_eq!(session.exchanges.len(), count);
    assert_eq!(backend.prompts.len(), 2);
}
#[test]
fn resume_recovers_partial_tail_and_does_not_replay_interrupted_work() {
    let (dir, _, _) = fixture();
    let path = dir.path().join("recover.jsonl");
    let mut session = Session::create(&path, dir.path()).unwrap();
    session.begin("first", "opus").unwrap();
    session.complete("complete answer").unwrap();
    session.begin("unfinished", "gpt-5.6-sol").unwrap();
    drop(session);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"assis")
        .unwrap();
    let mut resumed = Session::resume(&path, dir.path()).unwrap();
    assert!(resumed.recovered_tail);
    assert_eq!(resumed.interrupted.as_deref(), Some("unfinished"));
    assert_eq!(resumed.exchanges.len(), 1);
    resumed.begin("new turn", "opus").unwrap();
    resumed.complete("new answer").unwrap();
    drop(resumed);
    assert_eq!(
        Session::resume(&path, dir.path()).unwrap().exchanges.len(),
        2
    );
}
#[test]
fn an_interrupted_turn_is_not_resumed_into() {
    let (dir, _, _) = fixture();
    let path = dir.path().join("crash.jsonl");
    let mut session = Session::create(&path, dir.path()).unwrap();
    session.begin("first", "opus").unwrap();
    session.complete_in("answer", Some("s1")).unwrap();
    drop(session);
    let mut session = Session::resume(&path, dir.path()).unwrap();
    assert_eq!(session.provider_session, Some(("opus".into(), "s1".into())));
    // A crash mid-turn may have left unsaved work in s1.
    session.begin("second", "opus").unwrap();
    drop(session);
    let session = Session::resume(&path, dir.path()).unwrap();
    assert_eq!(session.provider_session, None);
}
#[test]
fn session_refuses_overwrite_concurrent_access_and_wrong_directory() {
    let (dir, _, _) = fixture();
    let path = dir.path().join("locked.jsonl");
    let session = Session::create(&path, dir.path()).unwrap();
    assert!(Session::create(&path, dir.path()).is_err());
    assert!(Session::resume(&path, dir.path()).is_err());
    drop(session);
    let other = tempfile::tempdir().unwrap();
    assert!(Session::resume(&path, other.path()).is_err());
    assert!(Session::resume(&path, dir.path()).is_ok());
}
#[test]
fn complete_corrupt_record_is_not_silently_repaired() {
    let (dir, _, _) = fixture();
    let path = dir.path().join("corrupt.jsonl");
    drop(Session::create(&path, dir.path()).unwrap());
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"invalid complete record\n")
        .unwrap();
    let before = fs::read(&path).unwrap();
    assert!(Session::resume(&path, dir.path()).is_err());
    assert_eq!(before, fs::read(&path).unwrap());
}
#[test]
fn context_budget_keeps_opening_and_recent_turns_without_splitting_unicode() {
    let (dir, _, _) = fixture();
    let mut session = Session::create(&dir.path().join("context.jsonl"), dir.path()).unwrap();
    for i in 0..12 {
        session.begin(&format!("request {i}"), "opus").unwrap();
        session.complete(&"café ".repeat(40)).unwrap();
    }
    let (prompt, omitted) = session.prompt("current", 1600).unwrap();
    assert!(prompt.len() <= 1600);
    assert!(omitted > 0);
    let value = context(&prompt);
    assert_eq!(value["conversation"][0]["user"], "request 0");
    assert_eq!(
        value["conversation"].as_array().unwrap().last().unwrap()["user"],
        "request 11"
    );
    assert_eq!(value["current_user_message"], "current");
    assert!(session.prompt(&"x".repeat(2000), 1600).is_err());
}
#[test]
fn cli_chat_creates_and_resumes_without_model_calls() {
    let (dir, cfg, _) = fixture();
    let path = dir.path().join("cli.jsonl");
    for flag in ["--session", "--resume"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nectra"))
            .current_dir(dir.path())
            .arg("--config")
            .arg(&cfg.path)
            .args(["chat", flag])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"/help\n/session\n/quit\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8(out.stdout)
            .unwrap()
            .contains("Session saved:"));
    }
}

#[test]
fn natural_model_request_overrides_pin_for_one_turn_only() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("natural.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    let mut opts = options();
    opts.model = Some("sonnet".into());
    chat::run_loop(&cfg, &reg, opts, &mut session,
        &mut Cursor::new("use opus and plan out how you would build a shed\ncontinue\nExplain when to use opus\nDo not use opus\nPlease use OPUS: review it\n/quit\n"),
        &mut Vec::new(), &mut backend).unwrap();
    assert_eq!(
        backend.models,
        ["opus", "sonnet", "sonnet", "sonnet", "opus"]
    );
    assert!(session.exchanges[0].user.contains("use opus and"));
}

#[test]
fn follow_ups_stay_on_the_model_doing_the_task() {
    for sticky in [true, false] {
        let (dir, mut cfg, reg) = fixture();
        cfg.inner.routing.sticky = sticky;
        let mut session = Session::create(&dir.path().join("sticky.jsonl"), dir.path()).unwrap();
        let mut backend = Mock::default();
        chat::run_loop(
            &cfg,
            &reg,
            options(),
            &mut session,
            &mut Cursor::new("use opus to write tests for this module\nthanks, do that\n/quit\n"),
            &mut Vec::new(),
            &mut backend,
        )
        .unwrap();
        assert_eq!(backend.models[0], "opus");
        // Without stickiness the follow-up is re-routed like a fresh message.
        assert_eq!(backend.models[1] == "opus", sticky, "{:?}", backend.models);
    }
}

#[test]
fn unavailable_explicit_model_reports_and_uses_another() {
    let (dir, cfg, reg) = fixture();
    let mut session =
        Session::create(&dir.path().join("unavailable-request.jsonl"), dir.path()).unwrap();
    let mut backend = Mock {
        codex_only: true,
        ..Default::default()
    };
    let mut output = Vec::new();
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("use opus and plan a shed\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.models.len(), 1);
    assert_ne!(backend.models[0], "opus");
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("selecting another model"));
}

#[test]
fn queued_messages_route_in_order_with_completed_context() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("queue.jsonl"), dir.path()).unwrap();
    let mut backend = Mock {
        queued: vec![
            "use sonnet and review it".into(),
            "use opus and finalize it".into(),
        ],
        ..Default::default()
    };
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("plan a shed\n/quit\n"),
        &mut Vec::new(),
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.models, ["opus", "sonnet", "opus"]);
    assert_eq!(session.exchanges.len(), 3);
    assert_eq!(
        context(&backend.prompts[1])["conversation"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        context(&backend.prompts[2])["conversation"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(backend.prompts[0].contains("do not acknowledge receiving context"));
}

#[test]
fn rejected_explicit_model_falls_back_without_repeating_started_work() {
    for started_work in [false, true] {
        let (dir, cfg, reg) = fixture();
        let mut session = Session::create(&dir.path().join("access.jsonl"), dir.path()).unwrap();
        let mut backend = Mock {
            deny_once: true,
            started_work,
            ..Default::default()
        };
        let mut output = Vec::new();
        chat::run_loop(&cfg, &reg, options(), &mut session,
            &mut Cursor::new("use gpt-6-astra and plan the design\n/models\nuse gpt-6-astra and continue\n/quit\n"), &mut output, &mut backend).unwrap();
        assert_eq!(backend.models[0], "gpt-6-astra");
        assert!(backend.models[1..].iter().all(|m| m != "gpt-6-astra"));
        assert_eq!(backend.models.len(), if started_work { 2 } else { 3 });
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("model rejected"));
    }
}

/// Keeps provider sessions like the native backend: Claude ids arrive assigned,
/// Codex ids are captured from the turn.
#[derive(Default)]
struct Sessions {
    turns: Vec<(String, bool, Option<String>, String)>,
}
impl Backend for Sessions {
    fn available(&self, _: Runtime, _: &Config) -> bool {
        true
    }
    fn respond(&mut self, _: &Decision, _: &str, _: &Config, _: &mut dyn Write) -> Result<String> {
        unreachable!("chat always offers a session to Claude and Codex")
    }
    fn respond_session(
        &mut self,
        d: &Decision,
        prompt: &str,
        _: &Config,
        _: &mut dyn Write,
        session: Option<&mut nectra::executor::ProviderSession>,
    ) -> Result<String> {
        let s = session.expect("session offered");
        if d.runtime == "codex" && s.id.is_none() {
            s.id = Some("thread-1".into());
        }
        self.turns
            .push((d.model.clone(), s.resume, s.id.clone(), prompt.into()));
        Ok(format!("answer from {}", d.model))
    }
}

#[test]
fn same_model_follow_ups_resume_and_switches_replay_the_transcript() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("thread.jsonl"), dir.path()).unwrap();
    let mut backend = Sessions::default();
    let mut output = vec![];
    chat::run_loop(&cfg, &reg, options(), &mut session, &mut Cursor::new(
        // Each /model switch comes with a new task, so no handoff note is asked for.
        "/model opus\nfirst\nsecond\n/model gpt-5.6-sol\nwrite a short bio for my profile\nmake it shorter\n/model opus\nreview this diff for bugs\n/quit\n"),
        &mut output, &mut backend).unwrap();
    let t = &backend.turns;
    // opus starts a session, then continues it with only the new message.
    assert!(!t[0].1 && t[0].2.is_some());
    assert!(t[1].1 && t[1].2 == t[0].2);
    assert_eq!(context(&t[1].3)["current_user_message"], "second");
    assert!(context(&t[1].3).get("conversation").is_none());
    // Switching provider replays everything; Codex's captured thread resumes next.
    assert!(!t[2].1);
    assert_eq!(
        context(&t[2].3)["conversation"].as_array().unwrap().len(),
        2
    );
    assert!(t[3].1 && t[3].2.as_deref() == Some("thread-1"));
    // Back to opus: its old session misses the Codex turns, so start fresh.
    assert!(!t[4].1 && t[4].2 != t[0].2);
    assert_eq!(
        context(&t[4].3)["conversation"].as_array().unwrap().len(),
        4
    );
}

#[test]
fn backends_without_sessions_keep_replaying_the_transcript() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("thread.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/model opus\nfirst\nsecond\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    assert_eq!(
        context(&backend.prompts[1])["conversation"][0]["user"],
        "first"
    );
}

#[test]
fn switching_mid_task_hands_over_a_note_from_the_outgoing_model() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("handoff.jsonl"), dir.path()).unwrap();
    let mut backend = Sessions::default();
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/model opus\nfirst\n/model gpt-5.6-sol\ncontinue with that\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    let t = &backend.turns;
    // The outgoing opus session is resumed once, only to write the note.
    assert_eq!(t[1].0, "opus");
    assert!(t[1].1 && t[1].2 == t[0].2);
    assert_eq!(t[1].3, Session::HANDOFF_REQUEST);
    let incoming = context(&t[2].3);
    assert_eq!(t[2].0, "gpt-5.6-sol");
    assert_eq!(incoming["handoff_note"], "answer from opus");
    assert_eq!(incoming["current_user_message"], "continue with that");
    // The note is internal: only the two real exchanges are saved.
    assert_eq!(session.exchanges.len(), 2);
    let output = String::from_utf8(output).unwrap();
    assert!(
        output.contains("gpt-5.6-sol (openai) · ") && output.contains(" · pinned with /model> ")
    );
    assert!(output.contains("Switched from opus, which wrote gpt-5.6-sol a handoff note"));
}

#[test]
fn mode_best_escapes_a_sticky_cheap_model() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("mode.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    chat::run_loop(
        &cfg,
        &reg,
        ChatOptions {
            mode: Mode::Balanced,
            ..options()
        },
        &mut session,
        &mut Cursor::new(
            "fix the failing test in parser.rs\n/mode best\nnow do that again\n/quit\n",
        ),
        &mut vec![],
        &mut backend,
    )
    .unwrap();
    // Balanced tries the cheap coder. The follow-up would stay on it, but after
    // /mode best it routes afresh to the strong coder.
    assert_eq!(backend.models, ["gpt-5.6-luna", "gpt-5.6-sol"]);
}

#[test]
fn a_mode_change_before_a_new_task_does_not_unstick_its_follow_up() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("stale.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    chat::run_loop(
        &cfg,
        &reg,
        ChatOptions {
            mode: Mode::Balanced,
            ..options()
        },
        &mut session,
        &mut Cursor::new(
            "hello\n/mode best\ncomplex end to end rewrite of the scheduler code\nnow do that again\n/quit\n",
        ),
        &mut vec![],
        &mut backend,
    )
    .unwrap();
    // /mode applied to the new task (Astra); its follow-up stays there.
    assert_eq!(backend.models[2], backend.models[1], "{:?}", backend.models);
}

#[test]
fn a_resumed_chat_continues_the_provider_session() {
    let (dir, cfg, reg) = fixture();
    let path = dir.path().join("again.jsonl");
    let mut first = Sessions::default();
    {
        let mut session = Session::create(&path, dir.path()).unwrap();
        chat::run_loop(
            &cfg,
            &reg,
            options(),
            &mut session,
            &mut Cursor::new("/model opus\nfirst\n/quit\n"),
            &mut Vec::new(),
            &mut first,
        )
        .unwrap();
    }
    let mut session = Session::resume(&path, dir.path()).unwrap();
    let mut second = Sessions::default();
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/model opus\nsecond\n/quit\n"),
        &mut Vec::new(),
        &mut second,
    )
    .unwrap();
    // `nectra -c` resumes opus's own session with just the new message,
    // keeping its tool context and prompt cache.
    let t = &second.turns[0];
    assert!(t.1 && t.2 == first.turns[0].2, "{:?}", t.2);
    assert!(context(&t.3).get("conversation").is_none());
}

#[test]
fn again_replaces_the_answer_with_a_stronger_models_and_says_why() {
    let (dir, cfg, reg) = fixture();
    let path = dir.path().join("again-stronger.jsonl");
    let mut opts = options();
    opts.mode = Mode::Fast;
    let mut backend = Mock::default();
    let mut output = vec![];
    {
        let mut session = Session::create(&path, dir.path()).unwrap();
        chat::run_loop(
            &cfg,
            &reg,
            opts,
            &mut session,
            &mut Cursor::new("thanks, what time zone is UTC?\n/again\n/again haiku\n/quit\n"),
            &mut output,
            &mut backend,
        )
        .unwrap();
    }
    let output = String::from_utf8(output).unwrap();
    let [first, second, third] = &backend.models[..] else {
        panic!("{:?}", backend.models)
    };
    assert_ne!(first, second);
    assert_eq!(third, "haiku");
    // Each turn says why it went where it did.
    assert!(output.contains(&format!("\n{first} (")), "{output}");
    assert!(output.contains(&format!("/again: best available other than {first}")));
    assert!(output.contains("kept in the log as replaced"));
    // A retry never sees the answer it replaces, and only the last one counts.
    for prompt in &backend.prompts[1..] {
        assert!(!prompt.contains("answer from"), "{prompt}");
    }
    let session = Session::resume(&path, dir.path()).unwrap();
    assert_eq!(session.exchanges.len(), 1);
    assert_eq!(session.exchanges[0].model, "haiku");
    assert_eq!(session.exchanges[0].user, "thanks, what time zone is UTC?");
}

#[test]
fn again_resends_an_unanswered_message_and_needs_something_to_resend() {
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("again-failed.jsonl"), dir.path()).unwrap();
    let mut backend = Mock {
        codex_only: true,
        ..Mock::default()
    };
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/again\n/again nope\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Nothing to send again yet."));
    assert!(output.contains("Unknown, disabled or unavailable model"));
    assert!(backend.models.is_empty());
}

#[test]
fn again_ignores_a_pin_and_keeps_the_answer_when_nothing_else_can_run() {
    // A pin would only point back at the model that answered.
    let (dir, cfg, reg) = fixture();
    let mut session = Session::create(&dir.path().join("again-pin.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/model sonnet\nexplain lifetimes\n/again\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    assert_eq!(
        backend.models.len(),
        2,
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(backend.models[0], "sonnet");
    assert_ne!(backend.models[1], "sonnet");
    assert!(String::from_utf8(output)
        .unwrap()
        .contains(" · pinned with /model> "));

    // With one usable model, /again can't run, so the answer stays.
    let (dir, cfg, mut reg) = fixture();
    reg.models.retain(|m| m.id == "haiku");
    let mut session = Session::create(&dir.path().join("again-one.jsonl"), dir.path()).unwrap();
    let mut backend = Mock::default();
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("hello\n/again\n/quit\n"),
        &mut Vec::new(),
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.models, ["haiku"]);
    assert_eq!(session.exchanges.len(), 1);
}

#[test]
fn again_tracks_the_unanswered_message_across_clear_and_resume() {
    let (dir, cfg, reg) = fixture();
    let path = dir.path().join("again-resume.jsonl");
    {
        let mut session = Session::create(&path, dir.path()).unwrap();
        let mut backend = Mock::default();
        chat::run_loop(
            &cfg,
            &reg,
            options(),
            &mut session,
            &mut Cursor::new("/model haiku\nfirst question\n/quit\n"),
            &mut Vec::new(),
            &mut backend,
        )
        .unwrap();
        // A failed second turn: the model errors out and nothing else is allowed.
        let mut backend = Mock {
            limit_all: true,
            ..Mock::default()
        };
        chat::run_loop(
            &cfg,
            &reg,
            options(),
            &mut session,
            &mut Cursor::new("second question\n/quit\n"),
            &mut Vec::new(),
            &mut backend,
        )
        .unwrap();
    }
    // Reopened: /again resends the failed message and replaces nothing.
    let mut session = Session::resume(&path, dir.path()).unwrap();
    let mut backend = Mock::default();
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("/again\n/quit\n"),
        &mut Vec::new(),
        &mut backend,
    )
    .unwrap();
    assert_eq!(backend.models.len(), 1);
    assert_eq!(session.exchanges.len(), 2);
    assert_eq!(session.exchanges[1].user, "second question");

    // After /clear there is nothing to resend.
    let mut backend = Mock {
        limit_all: true,
        ..Mock::default()
    };
    let mut output = vec![];
    chat::run_loop(
        &cfg,
        &reg,
        options(),
        &mut session,
        &mut Cursor::new("third\n/clear\n/again\n/quit\n"),
        &mut output,
        &mut backend,
    )
    .unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Nothing to send again yet."));
}
