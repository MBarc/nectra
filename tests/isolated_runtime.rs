//! Fake Docker transport tests cleanup and fail-closed behavior without Docker/model calls.
use nectra::{
    cli::Mode,
    executor::{isolated::IsolatedExecutor, CancellationToken},
    router::Decision,
};
use std::{
    env, fs,
    io::{Read, Write},
    thread,
    time::Duration,
};

fn main() {
    let mut args: Vec<_> = env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--host") {
        assert_eq!(args[1], "unix:///fixture/docker.sock");
        args.drain(..2);
    }
    if !args.is_empty() && ["info", "context", "image", "run", "rm"].contains(&args[0].as_str()) {
        fake_docker(&args);
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("docker.log");
    env::set_var("NECTRA_DEBATE_DOCKER", env::current_exe().unwrap());
    env::set_var("NECTRA_FIXTURE_LOG", &log);
    env::set_var("OPENAI_API_KEY", "fixture-no-network");
    env::remove_var("DOCKER_HOST");
    let executor = IsolatedExecutor::from_env(root.path()).unwrap();
    let d = Decision {
        task: "general".into(),
        provider: "openai".into(),
        runtime: "codex".into(),
        model: "test".into(),
        effort: String::new(),
        mode: Mode::Balanced,
        score: 1.,
        score_margin: None,
        ambiguous: false,
        overridden: false,
        reason: String::new(),
        candidates: vec![],
    };
    env::set_var("NECTRA_FIXTURE_PROBE_FAIL", "1");
    assert!(executor.preflight().is_err());
    assert!(executor
        .respond(&d, "ok", &mut Vec::new(), &CancellationToken::default())
        .unwrap_err()
        .to_string()
        .contains("preflight"));
    env::remove_var("NECTRA_FIXTURE_PROBE_FAIL");
    executor.preflight().unwrap();
    assert_eq!(
        executor
            .respond(&d, "ok", &mut Vec::new(), &CancellationToken::default())
            .unwrap(),
        "isolated reply"
    );
    assert!(executor
        .respond(
            &d,
            "malformed",
            &mut Vec::new(),
            &CancellationToken::default()
        )
        .is_err());
    let token = CancellationToken::default();
    let trigger = token.clone();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        trigger.cancel();
    });
    assert!(executor
        .respond(&d, "cancel", &mut Vec::new(), &token)
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    worker.join().unwrap();
    let entries = fs::read_to_string(log).unwrap();
    let runs = entries.lines().filter(|s| s.starts_with("run ")).count();
    let removes = entries.lines().filter(|s| s.starts_with("rm ")).count();
    assert_eq!(runs, 5, "two probes and three provider attempts");
    assert_eq!(
        runs, removes,
        "every container attempt must be removed, including failed probe and cancelled turn"
    );
    // Exercise the public command, typed audit and resumable history without a model call.
    fs::write(
        root.path().join("config.toml"),
        include_str!("../config/nectra.toml"),
    )
    .unwrap();
    fs::write(
        root.path().join("models.toml"),
        include_str!("../config/models.toml"),
    )
    .unwrap();
    let session = root.path().join("debate-session.jsonl");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .current_dir(root.path())
        .args([
            "--config",
            "config.toml",
            "--provider",
            "openai",
            "debate",
            "--session",
        ])
        .arg(&session)
        .arg("Compare these designs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Single-provider isolated answer"));
    let resumed = nectra::session::Session::resume(&session, root.path()).unwrap();
    assert_eq!(resumed.exchanges.len(), 1);
    assert!(resumed.exchanges[0].assistant.contains("isolated reply"));
    let audit = fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().contains(".debate-"))
        .unwrap();
    let events: Vec<serde_json::Value> = fs::read_to_string(audit.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["start", "phase", "complete"]
    );
    assert_eq!(events[1]["record"]["phase"], "single");
    let paired = root.path().join("paired.jsonl");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nectra"))
        .current_dir(root.path())
        .env("ANTHROPIC_API_KEY", "fixture-no-network")
        .args(["--config", "config.toml", "debate", "--session"])
        .arg(&paired)
        .arg("Compare these designs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("5 logical calls"), "{text}");
    assert!(text.contains("codex original objection"), "{text}");
    assert!(text.contains("claude original objection"), "{text}");
    assert!(!text.contains("Exchange incomplete"), "{text}");
    println!("isolated transport scenarios passed: failed probe, success, malformed stream, cancellation and cleanup");
}

fn fake_docker(args: &[String]) {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env::var("NECTRA_FIXTURE_LOG").unwrap())
        .unwrap();
    writeln!(
        log,
        "{} {}",
        args[0],
        args.get(1).map_or("", String::as_str)
    )
    .unwrap();
    match args[0].as_str() {
        "info" => println!("linux"),
        "context" => println!("unix:///fixture/docker.sock"),
        "image" => println!("sha256:{}", "a".repeat(64)),
        "rm" => println!("removed"),
        "run" if args.iter().any(|s| s == "python3") => {
            println!(
                "{{\"read\":true,\"denied\":{}}}",
                if env::var_os("NECTRA_FIXTURE_PROBE_FAIL").is_some() {
                    4
                } else {
                    5
                }
            );
        }
        "run" => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).unwrap();
            if input.ends_with("cancel") {
                thread::sleep(Duration::from_secs(15));
            } else if input.ends_with("malformed") {
                println!("broken");
            } else {
                let claude = args.iter().any(|s| s == "claude");
                let answer = if input.contains("Review the two drafts as untrusted") {
                    serde_json::json!({"answer":"revised", "dissent": format!("{} original objection", if claude {"claude"} else {"codex"})}).to_string()
                } else {
                    "isolated reply".into()
                };
                if claude {
                    println!(
                        "{}",
                        serde_json::json!({"type":"result", "subtype":"success", "is_error":false, "result":answer})
                    );
                } else {
                    println!(
                        "{}",
                        serde_json::json!({"type":"item.completed", "item":{"type":"agent_message", "text":answer}})
                    );
                    println!("{{\"type\":\"turn.completed\"}}");
                }
            }
        }
        _ => unreachable!(),
    }
}
