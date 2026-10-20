//! `nectra run --events jsonl` end to end. This executable doubles as a fake
//! Claude Code and Codex (see `fake`), so no real model is called.
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

fn main() {
    if let Ok(log) = std::env::var("NECTRA_FAKE_LOG") {
        fake(Path::new(&log));
        return;
    }
    let scenarios: [(&str, fn()); 17] = [
        ("routed run, then --resume", run_then_resume),
        ("cost guard", cost_guard),
        ("deadline", deadline),
        ("deadline while stdin stays open", deadline_covers_stdin),
        ("rate-limit failover", failover),
        ("typed routing failure", no_model),
        ("--mode/--provider and --no-session", per_task_flags),
        ("--mcp-config", mcp_config),
        ("Codex resume counts only the new turn", codex_resume),
        ("interrupted paid Codex turn", interrupted_codex_turn),
        (
            "persistence failure still ends in one result",
            persistence_failure,
        ),
        (
            "project and plugin MCP servers can't leak into Codex",
            codex_mcp_isolation,
        ),
        (
            "conflicting MCP credentials keep the task off Codex",
            conflicting_credentials,
        ),
        ("credentials are redacted", redaction),
        (
            "Codex without a log still meets the cost guard",
            cost_guard_without_codex_log,
        ),
        (
            "MCP is checked against Codex's own extra arguments",
            codex_mcp_with_extra_args,
        ),
        ("a hung `codex mcp list` meets the deadline", hung_mcp_probe),
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
    if args.iter().any(|a| a == "mcp") {
        return fake_mcp_list(&args);
    }
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
    if prompt.contains("leak") {
        let short = std::env::var("SHORT_API_KEY").unwrap_or_default();
        eprintln!("debug: retrying with key {SECRET} and {short}");
        let error = format!("401 invalid x-api-key {SECRET}");
        if codex {
            // Codex's `error` events are shown as text before the turn fails.
            say(json!({"type":"error","message":format!("stream error: key {SECRET} {short}")}));
            say(json!({"type":"turn.failed","error":{"message":error}}));
        } else {
            say(json!({"type":"result","is_error":true,"errors":[error]}));
        }
        std::process::exit(1);
    }
    if codex {
        return fake_codex(&args, &prompt);
    }
    let start = |id: &str, input: u64, cache_read: u64, cache_write: u64| {
        // With the cache TTL split, as Claude Code 2.1.304 streams it.
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{
            "id":id,"model":"claude-haiku-5-5","usage":{"input_tokens":input,"cache_read_input_tokens":cache_read,
            "cache_creation_input_tokens":cache_write,"output_tokens":1,"cache_creation":{
            "ephemeral_5m_input_tokens":cache_write / 4,"ephemeral_1h_input_tokens":cache_write - cache_write / 4}}}}})
    };
    let delta = |output: u64| {
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_delta",
        "delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":output}}})
    };
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
    // A replayed start must not reset what m1 counted.
    say(start("m1", 2, 1000, 2000));
    say(start("m2", 3, 3000, 500));
    say(text("all "));
    say(text("done"));
    say(delta(20));
    say(
        json!({"type":"result","subtype":"success","is_error":false,"result":"all done",
        "modelUsage":{"claude-haiku-5-5":{"inputTokens":99999}}}),
    );
}

const SECRET: &str = "sk-ant-api03-fakefakefakefakefake";

/// Codex 0.162.1's shape: its log (rollout) gets the thread's running total
/// after every model call, and `turn.completed` repeats the thread's total,
/// history included.
fn fake_codex(args: &[String], prompt: &str) {
    let say = |v: Value| println!("{v}");
    // `exec resume <opts> -- <thread> -`
    let thread = match args.iter().any(|a| a == "resume") {
        true => args[args.iter().position(|a| a == "--").unwrap() + 1].clone(),
        false => "019a0000-0000-7000-8000-000000000001".to_string(),
    };
    let rollout = (!args.iter().any(|a| a == "--ephemeral")).then(|| {
        let dir = Path::new(&std::env::var("CODEX_HOME").unwrap()).join("sessions/2026/10/20");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-20T08-00-00-{thread}.jsonl"));
        if !path.exists() {
            let meta = json!({"type":"session_meta","payload":{"id":thread}});
            fs::write(&path, format!("{meta}\n")).unwrap();
        }
        path
    });
    // The thread's running total so far: the last one logged.
    let mut total = rollout
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| {
            t.lines()
                .last()
                .map(|l| serde_json::from_str::<Value>(l).unwrap())
        })
        .map(|e| e["payload"]["info"]["total_token_usage"].clone())
        .filter(|t| !t.is_null())
        .unwrap_or(json!({"input_tokens":0,"cached_input_tokens":0,"output_tokens":0}));
    let mut call = |input: u64, cached: u64, output: u64| {
        for (k, v) in [
            ("input_tokens", input),
            ("cached_input_tokens", cached),
            ("output_tokens", output),
        ] {
            total[k] = json!(total[k].as_u64().unwrap() + v);
        }
        if let Some(path) = &rollout {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            let context =
                json!({"type":"turn_context","payload":{"turn_id":"t","model":"gpt-5.6-luna"}});
            let count = json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":total}}});
            writeln!(f, "{context}\n{count}").unwrap();
        }
        total.clone()
    };
    say(json!({"type":"thread.started","thread_id":thread}));
    say(json!({"type":"turn.started"}));
    say(json!({"type":"item.started","item":{"id":"i0","type":"command_execution"}}));
    call(600, 500, 30);
    say(json!({"type":"item.completed","item":{"id":"i0","type":"command_execution"}}));
    if prompt.contains("die") {
        // Killed mid-turn: the next call is never logged, no turn.completed.
        std::process::exit(1);
    }
    let total = call(400, 300, 20);
    say(
        json!({"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"done by codex"}}),
    );
    say(json!({"type":"turn.completed","usage":total}));
}

/// `codex mcp list --json`: the servers in NECTRA_FAKE_CODEX_MCP (from any
/// config layer, or a plugin), with `--config` overrides applied as Codex does.
fn fake_mcp_list(args: &[String]) {
    if std::env::var("NECTRA_FAKE_CODEX_MCP_HANG").is_ok() {
        std::thread::sleep(Duration::from_secs(60));
    }
    let mut servers: Vec<Value> =
        serde_json::from_str(&std::env::var("NECTRA_FAKE_CODEX_MCP").unwrap_or("[]".into()))
            .unwrap();
    // As if `--enable plugins` were applied after every --config.
    if !args
        .windows(2)
        .any(|w| w[0] == "--enable" && w[1] == "plugins")
    {
        servers
            .retain(|s| s["plugin"] != true || !args.iter().any(|a| a == "features.plugins=false"));
    }
    for pair in args.windows(2).filter(|w| w[0] == "--config") {
        let setting = pair[1].as_str();
        if setting.starts_with("features.") {
        } else if let Some(name) = setting
            .strip_prefix("mcp_servers.")
            .and_then(|s| s.strip_suffix(".enabled=false"))
        {
            let name = name.trim_matches('"');
            match servers.iter_mut().find(|s| s["name"] == name) {
                // A server that ignores the override, which Nectra must notice.
                Some(s) if s["sticky"] != true => s["enabled"] = json!(false),
                Some(_) => {}
                None => {
                    eprintln!("Error: invalid transport in `mcp_servers.{name}`");
                    std::process::exit(1);
                }
            }
        } else if let Some((name, _)) = setting
            .strip_prefix("mcp_servers.")
            .and_then(|s| s.split_once('='))
        {
            servers.push(json!({ "name": name }));
        }
    }
    let listed: Vec<Value> = servers
        .iter()
        .map(|s| json!({"name": s["name"], "enabled": s["enabled"] != false}))
        .collect();
    println!("{}", Value::from(listed));
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
    fn command(&self) -> Command {
        let root = self.dir.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_nectra"));
        command
            .arg("--config")
            .arg(root.join("config.toml"))
            .current_dir(root.join("work"))
            .env("NECTRA_FAKE_LOG", root.join("log"))
            .env("CODEX_HOME", root.join("codex"))
            .env_remove("NECTRA_FAKE_CODEX_MCP")
            .env_remove("TYPESAFE_API_KEY")
            .env_remove("PONS_MCP_TOKEN");
        command
    }
    fn run(&self, args: &[&str]) -> (Output, Vec<Value>) {
        self.run_with(args, &[])
    }
    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> (Output, Vec<Value>) {
        let output = self
            .command()
            .args(["run", "--events", "jsonl", "--state-dir"])
            .arg(self.dir.path().join("state"))
            .args(args)
            .envs(env.iter().copied())
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
    let usage =
        json!({"input":5,"cache_read":4000,"cache_write":2500,"cache_write_5m":625,"output":120});
    assert_eq!(r["usage"], usage);
    assert_eq!(r["usage_complete"], true);
    assert_eq!(r["warnings"], json!([]));
    assert_eq!(r["models"][0]["model"], "claude-haiku-5-5");
    assert_eq!(r["models"][0]["routed_as"], "haiku");
    assert_eq!(r["models"][0]["usage"], usage);
    // haiku: 0.10 input, 0.01 cache read, 0.20 1-hour and 0.125 5-minute
    // cache write, 0.50 output per Mtok.
    let cost = (5.0 * 0.10 + 4000.0 * 0.01 + 1875.0 * 0.20 + 625.0 * 0.125 + 120.0 * 0.50) / 1e6;
    assert!((r["cost_usd"].as_f64().unwrap() - cost).abs() < 1e-12);
    assert_eq!(r["route"]["model"], "haiku");
    assert_eq!(r["exit_code"], 0);
    let session = r["session_id"].as_str().unwrap().to_string();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let runs = env.dir.path().join("state/runs");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&runs), 0o700, "session records are private");
        assert_eq!(mode(&runs.join(format!("{session}.json"))), 0o600);
    }
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

fn cost_guard_without_codex_log() {
    let env = Env::new();
    // The turn's whole cost, $0.000116, is only known from turn.completed.
    let (out, events) = env.run(&[
        "--model",
        "gpt-5.6-luna",
        "--no-session",
        "--max-cost-usd",
        "0.0001",
        "fix it",
    ]);
    let r = result(&events);
    assert_eq!(r["error_class"], "cost_limit", "{r}");
    assert_eq!(out.status.code(), Some(4));
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0001);
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
        json!({"input":200,"cache_read":800,"cache_write":0,"cache_write_5m":0,"output":50})
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
    let file = mcp_file(
        &env,
        json!({"pons":{"command":"pons-mcp","args":["--stdio"],"env":{"PONS_MCP_TOKEN":"t0ken"}}}),
    );
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

fn deadline_covers_stdin() {
    let env = Env::new();
    let started = Instant::now();
    let mut child = env
        .command()
        .args([
            "run",
            "--events",
            "jsonl",
            "--no-session",
            "--model",
            "haiku",
        ])
        .args(["--stdin", "--deadline", "2s"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // The prompt's producer never closes stdin.
    let _stdin = child.stdin.take();
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = child.wait().unwrap();
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(status.code(), Some(4));
    let last: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(last["error_class"], "deadline");
    assert!(env.launches().is_empty());
}

fn codex_resume() {
    let env = Env::new();
    let (out, events) = env.run(&["--model", "gpt-5.6-luna", "fix it"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = result(&events);
    let turn = json!({"input":200,"cache_read":800,"cache_write":0,"cache_write_5m":0,"output":50});
    assert_eq!(r["usage"], turn);
    assert_eq!(r["usage_complete"], true);
    assert_eq!(r["models"][0]["model"], "gpt-5.6-luna");
    let session = r["session_id"].as_str().unwrap().to_string();
    let (out, events) = env.run(&["--resume", &session, "and the tests"]);
    assert_eq!(out.status.code(), Some(0));
    let r = result(&events);
    // Codex's turn.completed said 400 input / 1600 cached / 100 output: the
    // thread's total. Only this turn's two calls count.
    assert_eq!(
        r["usage"],
        turn,
        "a resumed Codex turn counts only its own calls {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(r["usage_complete"], true);
    // luna: 0.20 input, 0.02 cache read, 1.20 output per Mtok.
    let cost = (200.0 * 0.20 + 800.0 * 0.02 + 50.0 * 1.20) / 1e6;
    assert!((r["cost_usd"].as_f64().unwrap() - cost).abs() < 1e-12);
    assert!(args(&env.launches()[1]).contains(&"resume".to_string()));
}

fn interrupted_codex_turn() {
    let env = Env::new();
    let (out, events) = env.run(&["--model", "gpt-5.6-luna", "die halfway"]);
    assert_eq!(out.status.code(), Some(3));
    let r = result(&events);
    assert_eq!(r["error_class"], "agent_error");
    // The call Codex logged before it died is real spend...
    assert_eq!(r["usage"]["cache_read"], 500);
    assert_eq!(r["usage"]["output"], 30);
    assert!(r["cost_usd"].as_f64().unwrap() > 0.0);
    // ...and the call in flight was never reported.
    assert_eq!(r["usage_complete"], false);
    assert_eq!(r["models"][0]["usage_complete"], false);
}

fn persistence_failure() {
    let env = Env::new();
    // The state directory can't be created: something else is in the way.
    fs::write(env.dir.path().join("state"), "not a directory").unwrap();
    let (out, events) = env.run(&["--model", "haiku", "fix the typo"]);
    assert_eq!(out.status.code(), Some(0));
    let r = result(&events);
    assert_eq!(r["is_error"], false, "the task itself succeeded");
    assert_eq!(r["text"], "all done");
    assert_eq!(r["session_id"], Value::Null);
    assert!(
        r["warnings"][0]
            .as_str()
            .unwrap()
            .contains("session not saved"),
        "{r}"
    );
    assert_eq!(r["usage"]["output"], 120, "the usage is still reported");
}

fn mcp_file(env: &Env, servers: Value) -> String {
    let file = env.dir.path().join("mcp.json");
    fs::write(&file, json!({ "mcpServers": servers }).to_string()).unwrap();
    file.to_string_lossy().into_owned()
}

fn codex_mcp_isolation() {
    let env = Env::new();
    let file = mcp_file(
        &env,
        json!({"pons":{"command":"pons-mcp","args":["--stdio"]}}),
    );
    let codex = [
        "--model",
        "gpt-5.6-luna",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ];
    // A project-level server and a plugin's server, as `codex mcp list` shows them.
    let layers = r#"[{"name":"leaky"},{"name":"cua","plugin":true}]"#;
    let (out, _) = env.run_with(&codex, &[("NECTRA_FAKE_CODEX_MCP", layers)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = args(&env.launches()[0]).join(" ");
    assert!(text.contains("features.plugins=false"), "{text}");
    assert!(
        text.contains(r#"mcp_servers."leaky".enabled=false"#),
        "{text}"
    );
    assert!(text.contains("mcp_servers.pons="), "{text}");

    // A server Codex keeps loading despite the override: Codex can't be held
    // to the file, so the task doesn't run there.
    let stuck = r#"[{"name":"stuck","sticky":true}]"#;
    let (out, events) = env.run_with(&codex, &[("NECTRA_FAKE_CODEX_MCP", stuck)]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(result(&events)["error_class"], "no_model");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Codex models skipped") && stderr.contains("stuck"),
        "{stderr}"
    );
    assert_eq!(env.launches().len(), 1);
    // Claude's --strict-mcp-config holds whatever Codex has.
    let claude = [
        "--model",
        "haiku",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ];
    let (out, _) = env.run_with(&claude, &[("NECTRA_FAKE_CODEX_MCP", stuck)]);
    assert_eq!(out.status.code(), Some(0));
}

fn codex_mcp_with_extra_args() {
    let set_extras = |env: &Env, extras: &str| {
        let config = env.dir.path().join("config.toml");
        let text = fs::read_to_string(&config).unwrap();
        fs::write(&config, format!("{text}extra_args = {extras}\n")).unwrap();
    };
    let env = Env::new();
    let file = mcp_file(&env, json!({"pons":{"command":"pons-mcp"}}));
    let codex = [
        "--model",
        "gpt-5.6-luna",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ];
    // Codex's extra arguments turn plugins back on: the check must see that.
    set_extras(&env, r#"["--enable", "plugins"]"#);
    let plugin = r#"[{"name":"cua","plugin":true,"sticky":true}]"#;
    let (out, events) = env.run_with(&codex, &[("NECTRA_FAKE_CODEX_MCP", plugin)]);
    assert_eq!(result(&events)["error_class"], "no_model");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cua"), "{stderr}");
    // Another working directory can't be checked here, so Codex isn't used.
    let env = Env::new();
    let file = mcp_file(&env, json!({"pons":{"command":"pons-mcp"}}));
    let codex = [
        "--model",
        "gpt-5.6-luna",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ];
    set_extras(&env, r#"["--cd", "elsewhere"]"#);
    let (out, events) = env.run(&codex);
    assert_eq!(result(&events)["error_class"], "no_model");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--cd"), "{stderr}");
    assert!(env.launches().is_empty());
}

fn hung_mcp_probe() {
    let env = Env::new();
    let file = mcp_file(&env, json!({"pons":{"command":"pons-mcp"}}));
    let started = Instant::now();
    let (out, events) = env.run_with(
        &[
            "--no-session",
            "--deadline",
            "2s",
            "--mcp-config",
            &file,
            "x",
        ],
        &[("NECTRA_FAKE_CODEX_MCP_HANG", "1")],
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(result(&events)["error_class"], "deadline");
    assert_eq!(out.status.code(), Some(4));
}

fn conflicting_credentials() {
    let env = Env::new();
    let file = mcp_file(
        &env,
        json!({
            "a":{"command":"a-mcp","env":{"SHARED_TOKEN":"value-for-server-a"}},
            "b":{"command":"b-mcp","env":{"SHARED_TOKEN":"value-for-server-b"}},
        }),
    );
    let (out, events) = env.run(&[
        "--model",
        "gpt-5.6-luna",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(result(&events)["error_class"], "no_model");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SHARED_TOKEN"), "{stderr}");
    assert!(
        !stderr.contains("value-for-server"),
        "values are never printed"
    );
    assert!(env.launches().is_empty());
    // Claude starts each server with its own env from the file.
    let (out, _) = env.run(&[
        "--model",
        "haiku",
        "--no-session",
        "--mcp-config",
        &file,
        "x",
    ]);
    assert_eq!(out.status.code(), Some(0));
}

fn redaction() {
    let env = Env::new();
    for model in ["haiku", "gpt-5.6-luna"] {
        // A short credential is masked too.
        let (out, events) = env.run_with(
            &["--model", model, "--no-session", "leak it"],
            &[("SHORT_API_KEY", "k9x2z")],
        );
        assert_eq!(out.status.code(), Some(3));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        for secret in [SECRET, "k9x2z"] {
            assert!(
                !stdout.contains(secret) && !stderr.contains(secret),
                "{stdout}\n{stderr}"
            );
        }
        assert!(
            stderr.contains("debug: retrying with key [redacted] and [redacted]"),
            "{stderr}"
        );
        if model != "haiku" {
            assert!(stdout.contains("stream error: key [redacted]"), "{stdout}");
        }
        if model != "haiku" {
            // Codex's own error message is passed on, masked.
            let error = result(&events)["error"].as_str().unwrap().to_string();
            assert!(error.contains("x-api-key [redacted]"), "{error}");
        }
    }
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
