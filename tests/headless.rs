//! `nectra run --events jsonl` end to end. This executable doubles as a fake
//! Claude Code and Codex (see `fake`), so no real model is called.
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

fn main() {
    if let Ok(log) = std::env::var("NECTRA_FAKE_LOG") {
        fake(Path::new(&log));
        return;
    }
    let scenarios: [(&str, fn()); 7] = [
        ("routed run, then --resume", run_then_resume),
        ("cost guard", cost_guard),
        ("deadline", deadline),
        ("rate-limit failover", failover),
        ("typed routing failure", no_model),
        ("--mode/--provider and --no-session", per_task_flags),
        ("--mcp-config", mcp_config),
    ];
    for (name, scenario) in scenarios {
        scenario();
        println!("ok: {name}");
    }
    #[cfg(unix)]
    {
        sigterm();
        println!("ok: SIGTERM");
    }
}

/// Prints `lines` as provider events; behaviour is chosen by words in the prompt.
fn fake(log: &Path) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt).unwrap();
    let codex = args.first().is_some_and(|a| a == "exec");
    let n = fs::read_dir(log).unwrap().count();
    fs::write(
        log.join(format!("launch-{n}.json")),
        json!({"args": args, "prompt": prompt, "env": std::env::var("PONS_MCP_TOKEN").ok()})
            .to_string(),
    )
    .unwrap();
    let say = |v: Value| println!("{v}");
    if prompt.contains("limit") && n == 0 {
        // Rejected before any work: safe to send elsewhere.
        if codex {
            say(json!({"type":"turn.failed","error":{"message":"usage limit reached"}}));
        } else {
            say(json!({"type":"result","is_error":true,"errors":["rate_limit_error"]}));
        }
        std::process::exit(1);
    }
    if codex {
        say(json!({"type":"thread.started","thread_id":"thread-1"}));
        say(json!({"type":"item.started","item":{"id":"i0","type":"command_execution"}}));
        say(json!({"type":"item.completed","item":{"id":"i0","type":"command_execution"}}));
        say(
            json!({"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"done by codex"}}),
        );
        say(
            json!({"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}),
        );
        return;
    }
    let start = |id: &str, input: u64, cache_read: u64, cache_write: u64| {
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{
            "id":id,"model":"claude-haiku-5-5","usage":{"input_tokens":input,"cache_read_input_tokens":cache_read,
            "cache_creation_input_tokens":cache_write,"output_tokens":1}}}})
    };
    let delta = |output: u64| json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_delta","usage":{"output_tokens":output}}});
    let text = |t: &str| json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":t}}});
    say(json!({"type":"system","subtype":"init"}));
    if prompt.contains("expensive") {
        say(start("m0", 10, 0, 1_000_000));
        std::thread::sleep(Duration::from_secs(30));
    }
    if prompt.contains("slow") {
        std::thread::sleep(Duration::from_secs(30));
    }
    say(start("m1", 2, 1000, 2000));
    say(
        json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash"}]}}),
    );
    say(delta(100));
    say(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}));
    say(start("m2", 3, 3000, 500));
    say(text("all "));
    say(text("done"));
    say(delta(20));
    say(
        json!({"type":"result","subtype":"success","is_error":false,"result":"all done",
        "modelUsage":{"claude-haiku-5-5":{"inputTokens":99999}}}),
    );
}

struct Env {
    dir: tempfile::TempDir,
}
impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = std::env::current_exe().unwrap();
        let fake = toml::Value::String(fake.to_string_lossy().into());
        fs::write(
            dir.path().join("config.toml"),
            format!(
                "registry = \"models.toml\"\nproviders = [\"anthropic\", \"openai\"]\n\
                 [claude]\ncommand = {fake}\n[codex]\ncommand = {fake}\n"
            ),
        )
        .unwrap();
        let model = |id: &str, provider: &str, runtime: &str| {
            format!(
                "[[model]]\nid = \"{id}\"\nprovider = \"{provider}\"\nruntime = \"{runtime}\"\nenabled = true\n\
                 effort = \"\"\nprefers = [\"general\"]\n[model.capabilities]\ncoding=0.9\nreasoning=0.9\n\
                 long_context=0.9\nwriting=0.9\ntool_use=0.9\nspeed=0.9\ncost_efficiency=0.9\n\n"
            )
        };
        fs::write(
            dir.path().join("models.toml"),
            model("haiku", "anthropic", "claude") + &model("gpt-5.6-luna", "openai", "codex"),
        )
        .unwrap();
        fs::create_dir(dir.path().join("log")).unwrap();
        fs::create_dir(dir.path().join("work")).unwrap();
        Self { dir }
    }
    fn run(&self, args: &[&str]) -> (Output, Vec<Value>) {
        let root = self.dir.path();
        let output = Command::new(env!("CARGO_BIN_EXE_nectra"))
            .arg("--config")
            .arg(root.join("config.toml"))
            .args(["run", "--events", "jsonl", "--state-dir"])
            .arg(root.join("state"))
            .args(args)
            .current_dir(root.join("work"))
            .env("NECTRA_FAKE_LOG", root.join("log"))
            .env_remove("TYPESAFE_API_KEY")
            .env_remove("PONS_MCP_TOKEN")
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        let events = stdout
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|_| panic!("not JSON: {l}")))
            .collect::<Vec<_>>();
        for e in &events {
            assert_eq!(e["v"], 1, "every event carries the schema version");
        }
        let results = events.iter().filter(|e| e["type"] == "result").count();
        assert_eq!(results, 1, "exactly one result event: {stdout}");
        assert_eq!(events.last().unwrap()["type"], "result");
        (output, events)
    }
    fn launches(&self) -> Vec<Value> {
        let log = self.dir.path().join("log");
        (0..fs::read_dir(&log).unwrap().count())
            .map(|n| {
                serde_json::from_str(
                    &fs::read_to_string(log.join(format!("launch-{n}.json"))).unwrap(),
                )
                .unwrap()
            })
            .collect()
    }
}
fn result(events: &[Value]) -> &Value {
    events.last().unwrap()
}
fn args(launch: &Value) -> Vec<String> {
    serde_json::from_value(launch["args"].clone()).unwrap()
}

fn run_then_resume() {
    let env = Env::new();
    let (out, events) = env.run(&["--model", "haiku", "fix the typo"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(events[0]["type"], "route");
    assert_eq!(events[0]["model"], "haiku");
    let kinds: Vec<_> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["route", "tool", "tool", "text", "text", "result"]);
    assert_eq!(events[1]["phase"], "start");
    assert_eq!(events[2]["name"], "Bash");
    let r = result(&events);
    assert_eq!(r["is_error"], false);
    assert_eq!(r["error_class"], Value::Null);
    assert_eq!(r["text"], "all done");
    assert_eq!(r["model"], "haiku");
    // Summed per message, never the provider's cumulative modelUsage.
    let usage = json!({"input":5,"cache_read":4000,"cache_write":2500,"output":120});
    assert_eq!(r["usage"], usage);
    assert_eq!(r["models"][0]["model"], "claude-haiku-5-5");
    assert_eq!(r["models"][0]["routed_as"], "haiku");
    assert_eq!(r["models"][0]["usage"], usage);
    // haiku: 0.10 input, 0.01 cache read, 0.20 cache write, 0.50 output per Mtok.
    let cost = (5.0 * 0.10 + 4000.0 * 0.01 + 2500.0 * 0.20 + 120.0 * 0.50) / 1e6;
    assert!((r["cost_usd"].as_f64().unwrap() - cost).abs() < 1e-12);
    assert_eq!(r["route"]["model"], "haiku");
    assert_eq!(r["exit_code"], 0);
    let session = r["session_id"].as_str().unwrap().to_string();
    let first = args(&env.launches()[0]);
    let i = first.iter().position(|a| a == "--session-id").unwrap();
    let provider_session = first[i + 1].clone();
    assert!(!first.contains(&"--no-session-persistence".to_string()));

    let (out, events) = env.run(&["--resume", &session, "now add a test"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(events[0]["resumed"], true);
    let r = result(&events);
    assert_eq!(r["session_id"], session.as_str());
    assert_eq!(r["usage"], usage, "a follow-up counts only its own calls");
    let second = &env.launches()[1];
    let resumed = args(second);
    let i = resumed.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(resumed[i + 1], provider_session);
    assert_eq!(
        second["prompt"], "now add a test",
        "only the new message is sent"
    );

    let (out, events) = env.run(&["--resume", "../../escape", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(result(&events)["error_class"], "harness_error");
}

fn cost_guard() {
    let env = Env::new();
    let started = Instant::now();
    let (out, events) = env.run(&[
        "--model",
        "haiku",
        "--max-cost-usd",
        "0.01",
        "an expensive task",
    ]);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "stopped promptly"
    );
    let r = result(&events);
    assert_eq!(r["error_class"], "cost_limit");
    assert_eq!(r["is_error"], true);
    assert_eq!(out.status.code(), Some(4));
    // The call that crossed the limit is in the bill.
    assert_eq!(r["usage"]["cache_write"], 1_000_000);
    assert!(r["cost_usd"].as_f64().unwrap() > 0.01);
    // A stopped task can still be continued.
    assert!(r["session_id"].is_string());
}

fn deadline() {
    let env = Env::new();
    let started = Instant::now();
    let (out, events) = env.run(&["--model", "haiku", "--deadline", "2s", "a slow task"]);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(result(&events)["error_class"], "deadline");
    assert_eq!(out.status.code(), Some(4));
}

fn failover() {
    let env = Env::new();
    let (out, events) = env.run(&["hit the limit"]);
    assert_eq!(out.status.code(), Some(0), "{events:?}");
    let routes: Vec<_> = events.iter().filter(|e| e["type"] == "route").collect();
    assert_eq!(routes.len(), 2);
    assert_ne!(routes[0]["model"], routes[1]["model"]);
    let r = result(&events);
    assert_eq!(r["attempts"][0]["error_class"], "rate_limited");
    assert_eq!(r["attempts"][1]["error_class"], Value::Null);
    assert_eq!(r["model"], routes[1]["model"]);
    assert!(env.dir.path().join("state/cooldowns.json").is_file());
}

fn no_model() {
    let env = Env::new();
    let (out, events) = env.run(&["--model", "nonexistent", "hello"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(result(&events)["error_class"], "no_model");
    assert!(env.launches().is_empty());
}

fn per_task_flags() {
    let env = Env::new();
    let (out, events) = env.run(&[
        "--no-session",
        "--mode",
        "fast",
        "--provider",
        "openai",
        "fix it",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(events[0]["provider"], "openai");
    assert_eq!(events[0]["mode"], "fast");
    let r = result(&events);
    assert_eq!(r["text"], "done by codex");
    assert_eq!(
        r["usage"],
        json!({"input":200,"cache_read":800,"cache_write":0,"output":50})
    );
    assert_eq!(r["session_id"], Value::Null);
    assert!(args(&env.launches()[0]).contains(&"--ephemeral".to_string()));
    assert!(
        !env.dir.path().join("state").exists(),
        "--no-session writes nothing"
    );
}

fn mcp_config() {
    let env = Env::new();
    let file = env.dir.path().join("mcp.json");
    fs::write(
        &file,
        json!({"mcpServers":{"pons":{"command":"pons-mcp","args":["--stdio"],"env":{"PONS_MCP_TOKEN":"t0ken"}}}})
            .to_string(),
    )
    .unwrap();
    let file = file.to_string_lossy().into_owned();
    let (out, _) = env.run(&[
        "--model",
        "haiku",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let claude = args(&env.launches()[0]);
    assert!(claude
        .iter()
        .any(|a| a.starts_with("--mcp-config=") && a.ends_with("mcp.json")));
    assert!(claude.contains(&"--strict-mcp-config".to_string()));
    let (out, _) = env.run(&[
        "--model",
        "gpt-5.6-luna",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let codex = &env.launches()[1];
    let text = args(codex).join(" ");
    assert!(
        text.contains(
            r#"mcp_servers.pons={command="pons-mcp",args=["--stdio"],env_vars=["PONS_MCP_TOKEN"]}"#
        ),
        "{text}"
    );
    assert!(!text.contains("t0ken"), "secrets stay off the command line");
    assert_eq!(
        codex["env"], "t0ken",
        "and reach the agent through its environment"
    );
}

#[cfg(unix)]
fn sigterm() {
    let env = Env::new();
    let root = env.dir.path();
    let child = Command::new(env!("CARGO_BIN_EXE_nectra"))
        .arg("--config")
        .arg(root.join("config.toml"))
        .args([
            "run",
            "--events",
            "jsonl",
            "--no-session",
            "--model",
            "haiku",
            "a slow task",
        ])
        .current_dir(root.join("work"))
        .env("NECTRA_FAKE_LOG", root.join("log"))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    while env.launches().is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let started = Instant::now();
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let out = child.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(out.status.code(), Some(130));
    let last: Value = serde_json::from_str(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["error_class"], "cancelled");
}
