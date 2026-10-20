//! Sharing MCP servers between the provider CLIs.
//!
//! Each CLI keeps its own servers (Claude in `~/.claude.json`, Codex in
//! `~/.codex/config.toml`, Gemini in `~/.gemini/settings.json`), so a task routed
//! from Claude to Codex loses Claude's tools. Claude and Codex take extra servers
//! at launch, which Nectra adds without writing their config. Gemini 0.64 skips
//! a settings file unless an administrator owns it, so it has no per-launch
//! channel; `nectra mcp --sync` adds the servers to each CLI's own config instead.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// A local (stdio) server. HTTP servers aren't shared: each CLI needs its own
/// sign-in or headers for them, and Codex can't send custom headers.
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct Servers {
    /// Every server name each CLI has, shareable or not.
    pub names: BTreeMap<&'static str, Vec<String>>,
    /// Shareable servers by name, with the CLI that has them (Claude's first).
    pub shared: BTreeMap<String, (&'static str, Server)>,
}

/// The Codex desktop app installs and runs this one for itself, over a private pipe.
const CLI_OWNED: [&str; 1] = ["node_repl"];

impl Servers {
    /// Servers other CLIs have under a name `cli` doesn't use.
    pub fn missing(&self, cli: &str) -> Vec<(&str, &Server)> {
        let names = self.names.get(cli);
        self.shared
            .iter()
            .filter(|(name, (from, _))| *from != cli && !names.is_some_and(|n| n.contains(name)))
            .map(|(name, (_, server))| (name.as_str(), server))
            .collect()
    }
}

fn config_file(home: &Path, var: &str, default: &str, file: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(default))
        .join(file)
}

/// The user-scope servers of every CLI. Project servers stay with their project.
pub fn scan(home: &Path) -> Result<Servers> {
    let mut servers = Servers::default();
    let claude = config_file(home, "CLAUDE_CONFIG_DIR", "", ".claude.json");
    let codex = config_file(home, "CODEX_HOME", ".codex", "config.toml");
    let gemini = config_file(home, "GEMINI_CLI_HOME", "", ".gemini/settings.json");
    // Both expand $VAR in a server's settings, which another CLI might not.
    let json_servers = |path: &Path, gemini: bool| -> Result<Vec<(String, Option<Value>)>> {
        let Some(text) = read(path)? else {
            return Ok(vec![]);
        };
        // Gemini allows comments in its settings; Claude's file has none.
        let value: Value = serde_json::from_str(&strip_comments(&text))
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(value["mcpServers"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), expand_server(v, gemini)))
                    .collect()
            })
            .unwrap_or_default())
    };
    let codex_servers = || -> Result<Vec<(String, Option<Value>)>> {
        let Some(text) = read(&codex)? else {
            return Ok(vec![]);
        };
        let value: toml::Value =
            toml::from_str(&text).with_context(|| format!("parsing {}", codex.display()))?;
        let Some(table) = value.get("mcp_servers").and_then(|t| t.as_table()) else {
            return Ok(vec![]);
        };
        table
            .iter()
            .map(|(k, v)| Ok((k.clone(), Some(serde_json::to_value(v)?))))
            .collect()
    };
    for (cli, entries) in [
        ("claude", json_servers(&claude, false)?),
        ("codex", codex_servers()?),
        ("gemini", json_servers(&gemini, true)?),
    ] {
        for (name, value) in entries {
            servers.names.entry(cli).or_default().push(name.clone());
            if let Some(server) = value
                .as_ref()
                .and_then(local)
                .filter(|_| !CLI_OWNED.contains(&name.as_str()))
            {
                servers.shared.entry(name).or_insert((cli, server));
            }
        }
    }
    Ok(servers)
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Settings that limit a server's tools. Claude has no equivalent, so a server
/// using one is not shared rather than shared without its limits.
const RESTRICTIONS: [&str; 4] = [
    "includeTools",
    "excludeTools",
    "enabled_tools",
    "disabled_tools",
];

/// A server another CLI can run as is: stdio, enabled, unrestricted, and not
/// tied to a working directory (Claude has no setting for one).
fn local(v: &Value) -> Option<Server> {
    let stdio = v["type"].as_str().is_none_or(|t| t == "stdio");
    if !stdio
        || v["enabled"] == json!(false)
        || !v["cwd"].is_null()
        || RESTRICTIONS.iter().any(|r| !v[r].is_null())
    {
        return None;
    }
    let strings = |v: &Value| v.as_str().map(String::from);
    Some(Server {
        command: strings(&v["command"])?,
        args: v["args"]
            .as_array()
            .map(|a| a.iter().filter_map(strings).collect())
            .unwrap_or_default(),
        env: v["env"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), strings(v)?)))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Arguments and environment that give `runtime` the servers only the other CLIs
/// have. Values go in the environment, never on the command line, which any user
/// can read on Linux. Sharing is a convenience, so an unreadable config skips it.
pub fn launch(runtime: &str, home: &Path) -> (Vec<OsString>, Vec<(String, String)>) {
    let Ok(servers) = scan(home) else {
        return (vec![], vec![]);
    };
    let (mut args, mut env) = (vec![], vec![]);
    match runtime {
        "claude" => {
            let mut config = serde_json::Map::new();
            for (name, server) in servers.missing("claude") {
                let mut vars = serde_json::Map::new();
                for (key, value) in &server.env {
                    // Claude expands ${VAR} in --mcp-config (seen on 2.1.302).
                    let var = env_name(name, key);
                    vars.insert(key.clone(), json!(format!("${{{var}}}")));
                    env.push((var, value.clone()));
                }
                config.insert(
                    name.into(),
                    json!({"command": server.command, "args": server.args, "env": vars}),
                );
            }
            if !config.is_empty() {
                // `=`: the flag takes several values and would swallow what follows.
                args.push(format!("--mcp-config={}", json!({ "mcpServers": config })).into());
            }
        }
        "codex" => {
            for (name, server) in servers.missing("codex") {
                // Codex forwards variables by name only (env_vars), so a value
                // that clashes with the user's own variable can't be passed.
                // ponytail: such a server is skipped; inline `env` would put the
                // value on the command line.
                let clash = server.env.iter().any(|(k, v)| {
                    std::env::var(k).is_ok_and(|own| &own != v)
                        || env
                            .iter()
                            .any(|(e, ev): &(String, String)| e == k && ev != v)
                });
                if clash
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
                {
                    continue;
                }
                args.extend(["--config".into(), codex_server(name, server)]);
                env.extend(server.env.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        _ => {}
    }
    (args, env)
}

/// [`launch`] for this decision: nothing for a read-only launch, whose tools
/// are what keeps it read-only, or when no home was given. An explicit
/// `--mcp-config` replaces sharing.
pub fn for_launch(
    runtime: &str,
    cfg: &crate::config::Config,
) -> (Vec<OsString>, Vec<(String, String)>) {
    if cfg.read_only {
        return (vec![], vec![]);
    }
    if let Some(explicit) = &cfg.mcp_config {
        return explicit.launch(runtime);
    }
    match &cfg.mcp_home {
        Some(home) => launch(runtime, home),
        _ => (vec![], vec![]),
    }
}

/// Codex takes a server at launch as `--config mcp_servers.NAME={...}`, with
/// env values passed by name only (`env_vars`) so they stay off the command line.
fn codex_server(name: &str, server: &Server) -> OsString {
    let strings =
        |v: Vec<String>| toml::Value::Array(v.into_iter().map(toml::Value::String).collect());
    format!(
        "mcp_servers.{name}={{command={},args={},env_vars={}}}",
        toml::Value::String(server.command.clone()),
        strings(server.args.clone()),
        strings(server.env.keys().cloned().collect()),
    )
    .into()
}

/// A launch's extra arguments and environment.
type Launch = (Vec<OsString>, Vec<(String, String)>);

/// `--mcp-config FILE`: exactly these servers (standard `mcpServers` JSON), for
/// whichever runtime the task routes to.
#[derive(Debug, Clone)]
pub struct Explicit {
    path: PathBuf,
    /// Codex's launch arguments and environment, or why Codex can't be held to FILE.
    codex: std::result::Result<Launch, String>,
}
impl Explicit {
    /// `codex` is the Codex command, asked which servers it would load here.
    pub fn load(path: &Path, codex: Option<&str>) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let value: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let servers = value["mcpServers"]
            .as_object()
            .with_context(|| format!("{} has no mcpServers object", path.display()))?;
        crate::redact::also(
            servers
                .values()
                .filter_map(|v| local(&expand_server(v, false)?))
                .flat_map(|s| s.env.into_iter()),
        );
        Ok(Self {
            path: std::path::absolute(path)?,
            codex: match codex {
                Some(command) => codex_explicit(servers, command),
                None => Err("no Codex model is enabled".into()),
            },
        })
    }
    /// Why Codex can't take FILE exactly, if it can't.
    pub fn codex_excluded(&self) -> Option<&str> {
        self.codex.as_ref().err().map(String::as_str)
    }
    /// Whether a launch on `runtime` gets exactly FILE's servers. Gemini has no
    /// launch-time server option, and Ollama runs no tools.
    pub fn supports(&self, runtime: &str) -> bool {
        match runtime {
            "claude" => true,
            "codex" => self.codex.is_ok(),
            _ => false,
        }
    }
    fn launch(&self, runtime: &str) -> (Vec<OsString>, Vec<(String, String)>) {
        match runtime {
            "claude" => {
                let mut flag = OsString::from("--mcp-config=");
                flag.push(&self.path);
                (vec![flag, "--strict-mcp-config".into()], vec![])
            }
            "codex" => self.codex.clone().unwrap_or_default(),
            _ => (vec![], vec![]),
        }
    }
}

/// Codex has no strict mode and loads servers from its user, project and plugin
/// config layers. So plugins and apps go off, every other server Codex reports
/// goes off by name, and `codex mcp list` must then show exactly FILE's servers.
fn codex_explicit(
    servers: &serde_json::Map<String, Value>,
    command: &str,
) -> std::result::Result<Launch, String> {
    let mut args: Vec<OsString> = vec![];
    for setting in ["features.plugins=false", "features.apps=false"] {
        args.extend(["--config".into(), setting.into()]);
    }
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for (name, v) in servers {
        let server = expand_server(v, false)
            .as_ref()
            .and_then(local)
            .ok_or(format!("server {name} isn't a local (stdio) server"))?;
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
        {
            return Err(format!("Codex can't take the server name {name}"));
        }
        // Codex forwards variables by name from its own environment, so one
        // name with two values would give a server the other's credential.
        for (key, value) in &server.env {
            if std::env::var(key).is_ok_and(|own| &own != value)
                || env.get(key).is_some_and(|other| other != value)
            {
                return Err(format!(
                    "{key} has a different value for server {name}, and Codex passes one value to every server"
                ));
            }
            env.insert(key.clone(), value.clone());
        }
        args.extend(["--config".into(), codex_server(name, &server)]);
    }
    let list = |args: &[OsString]| -> std::result::Result<Vec<(String, bool)>, String> {
        let out = crate::executor::launch(command)
            .map_err(|e| format!("{e:#}"))?
            .args(["mcp", "list", "--json"])
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("running codex mcp list: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "codex mcp list failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listed: Vec<Value> = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("reading codex mcp list: {e}"))?;
        Ok(listed
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap_or("").to_string(),
                    s["enabled"] == true,
                )
            })
            .collect())
    };
    for (name, enabled) in list(&args)? {
        if enabled && !servers.contains_key(&name) {
            let key = toml::Value::String(name);
            args.extend([
                "--config".into(),
                format!("mcp_servers.{key}.enabled=false").into(),
            ]);
        }
    }
    let mut on: Vec<String> = list(&args)?
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect();
    on.sort();
    if !on.iter().eq(servers.keys()) {
        return Err(format!("Codex would still load {}", on.join(", ")));
    }
    Ok((args, env.into_iter().collect()))
}

/// Hex keeps every (server, key) pair distinct: `foo-bar` and `foo_bar` differ.
fn env_name(server: &str, key: &str) -> String {
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02X}")).collect::<String>();
    format!("NECTRA_MCP_{}_{}", hex(server), hex(key))
}

/// `${VAR}` and `${VAR:-default}` from Nectra's environment, plus `$VAR` when
/// `bare`. An unset variable without a default stays as written.
fn expand(s: &str, bare: bool) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let (name, default, len) = if let Some(body) = rest.strip_prefix('{') {
            match body.find('}') {
                Some(end) => {
                    let inner = &body[..end];
                    let (name, default) = match inner.split_once(":-") {
                        Some((n, d)) => (n, Some(d)),
                        None => (inner, None),
                    };
                    (name, default, end + 2)
                }
                None => ("", None, 0),
            }
        } else if bare {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (&rest[..end], None, end)
        } else {
            ("", None, 0)
        };
        match std::env::var(name)
            .ok()
            .filter(|_| !name.is_empty())
            .or(default.map(String::from))
        {
            Some(value) => out.push_str(&value),
            None => out.push_str(&format!("${}", &rest[..len])),
        }
        rest = &rest[len..];
    }
    out + rest
}

/// A server's settings as its own CLI sees them, or None when that can't be
/// worked out. Claude expands only `${VAR}` (code.claude.com/docs/en/mcp).
/// Gemini 0.64 expands `$VAR` too across its settings, then expands env values
/// again with dotenv rules (and `%VAR%` on Windows) when starting the server; a
/// value still holding `$` or `%` after the first pass isn't guessed at.
fn expand_server(v: &Value, gemini: bool) -> Option<Value> {
    fn walk(v: &Value, f: &dyn Fn(&str) -> String) -> Value {
        match v {
            Value::String(s) => Value::String(f(s)),
            Value::Array(a) => Value::Array(a.iter().map(|v| walk(v, f)).collect()),
            Value::Object(m) => {
                Value::Object(m.iter().map(|(k, v)| (k.clone(), walk(v, f))).collect())
            }
            other => other.clone(),
        }
    }
    let v = walk(v, &|s| expand(s, gemini));
    let unsure = |s: &str| s.contains('$') || (cfg!(windows) && s.contains('%'));
    let env = v["env"].as_object().into_iter().flat_map(|m| m.values());
    if gemini && env.filter_map(Value::as_str).any(unsure) {
        return None;
    }
    Some(v)
}

/// JSON with `//` and `/* */` comments outside strings removed.
fn strip_comments(text: &str) -> String {
    let (mut out, mut chars) = (String::new(), text.chars().peekable());
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
        } else if c == '/' && chars.peek() == Some(&'/') {
            while chars.next_if(|&c| c != '\n').is_some() {}
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.next_if_eq(&'/').is_some() {
                    break;
                }
            }
        } else {
            in_string = c == '"';
            out.push(c);
        }
    }
    out
}

/// Adds every shareable server to each installed CLI that lacks it. Nectra writes
/// the CLI's config itself rather than running its `mcp add`, which takes
/// environment values as arguments that other users can read in the process list.
pub fn sync(home: &Path, cfg: &crate::config::Config) -> Result<Vec<String>> {
    let servers = scan(home)?;
    let mut done = vec![];
    for (cli, command) in [
        ("claude", &cfg.inner.claude.command),
        ("codex", &cfg.inner.codex.command),
        ("gemini", &cfg.inner.gemini.command),
    ] {
        if crate::executor::launch(command).is_err() {
            continue; // not installed
        }
        for (name, server) in servers.missing(cli) {
            add(home, cli, name, server)?;
            done.push(format!("{name} -> {cli}"));
        }
    }
    Ok(done)
}

/// Adds one server to a CLI's user config, as its own `mcp add` would.
fn add(home: &Path, cli: &str, name: &str, server: &Server) -> Result<()> {
    if cli == "codex" {
        let path = config_file(home, "CODEX_HOME", ".codex", "config.toml");
        let before = read(&path)?;
        let mut text = before.clone().unwrap_or_default();
        let table = json!({"mcp_servers": {name: {
            "command": server.command, "args": server.args, "env": server.env,
        }}});
        // Appended, so the user's comments and layout stay as they are.
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text += &format!("\n{}", toml::to_string(&table)?);
        // An inline `mcp_servers = {...}` can't take an appended table.
        toml::from_str::<toml::Value>(&text).with_context(|| {
            format!(
                "can't append {name} to {}; add it with `codex mcp add`",
                path.display()
            )
        })?;
        return replace(&path, before.as_deref(), &text);
    }
    let path = if cli == "claude" {
        config_file(home, "CLAUDE_CONFIG_DIR", "", ".claude.json")
    } else {
        config_file(home, "GEMINI_CLI_HOME", "", ".gemini/settings.json")
    };
    let before = read(&path)?;
    let text = before.clone().unwrap_or_else(|| "{}".into());
    if strip_comments(&text) != text {
        anyhow::bail!(
            "{} has comments, which rewriting it would lose; add {name} with `{cli} mcp add`",
            path.display()
        );
    }
    let mut config: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut entry = json!({"command": server.command, "args": server.args, "env": server.env});
    if cli == "claude" {
        entry["type"] = "stdio".into();
    }
    config
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("mcpServers is not an object")?
        .insert(name.into(), entry);
    // A Claude session that rewrites ~/.claude.json from memory can drop the
    // server again, as it can after `claude mcp add`; rerun --sync if so.
    replace(
        &path,
        before.as_deref(),
        &serde_json::to_string_pretty(&config)?,
    )
}

/// Writes through a private, uniquely named temporary file, so a failed or
/// concurrent write can't truncate the config. The file keeps its permissions,
/// and is left alone if something else changed it since `before` was read.
fn replace(path: &Path, before: Option<&str>, text: &str) -> Result<()> {
    let dir = path.parent().context("config path has no folder")?;
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, text.as_bytes())?;
    if let Ok(meta) = fs::metadata(path) {
        fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    // ponytail: a writer between this check and the rename can still lose its
    // change; that window is microseconds, against the CLI's own `mcp add`'s same race.
    if read(path)?.as_deref() != before {
        anyhow::bail!("{} changed while syncing; run it again", path.display());
    }
    tmp.persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn render(servers: &Servers) -> String {
    let mut out = format!(
        "{:<24} {:<7} {:<7} {:<7}\n",
        "SERVER", "CLAUDE", "CODEX", "GEMINI"
    );
    let all: std::collections::BTreeSet<&String> = servers.names.values().flatten().collect();
    for name in all {
        let cell = |cli: &str| {
            if servers.names.get(cli).is_some_and(|n| n.contains(name)) {
                "yes"
            } else if servers
                .shared
                .get(name)
                .is_some_and(|(from, _)| *from != cli)
                && cli != "gemini"
            {
                "shared"
            } else {
                "-"
            }
        };
        out += &format!(
            "{name:<24} {:<7} {:<7} {:<7}\n",
            cell("claude"),
            cell("codex"),
            cell("gemini")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_servers_reach_the_clis_that_lack_them_without_values_on_the_command_line() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::write(
            home.join(".claude.json"),
            json!({"mcpServers": {
                "roblox": {"type": "stdio", "command": "cmd", "args": ["/c", r"%LOCALAPPDATA%\mcp.bat"]},
                "github": {"command": "gh-mcp", "env": {"NECTRA_TEST_GH_TOKEN": "s3cret"}},
                "canva": {"type": "http", "url": "https://mcp.canva.com/mcp"},
                "both": {"command": "claude-one"}
            }})
            .to_string(),
        )
        .unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            "[mcp_servers.both]\ncommand = \"codex-one\"\n\
             [mcp_servers.node_repl]\ncommand = \"node_repl.exe\"\n\
             [mcp_servers.off]\ncommand = \"x\"\nenabled = false\n",
        )
        .unwrap();
        let servers = scan(home).unwrap();
        let names = |cli| {
            servers
                .missing(cli)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        };
        // HTTP, disabled and CLI-owned servers stay put; a name both have is never shadowed.
        assert_eq!(names("codex"), ["github", "roblox"]);
        assert!(names("claude").is_empty());
        assert_eq!(names("gemini"), ["both", "github", "roblox"]);

        let (args, env) = launch("codex", home);
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args[1],
            r#"mcp_servers.github={command="gh-mcp",args=[],env_vars=["NECTRA_TEST_GH_TOKEN"]}"#
        );
        assert_eq!(
            args[3],
            r#"mcp_servers.roblox={command="cmd",args=["/c", '%LOCALAPPDATA%\mcp.bat'],env_vars=[]}"#
        );
        assert!(!args.iter().any(|a| a.contains("s3cret")));
        assert_eq!(
            env,
            [("NECTRA_TEST_GH_TOKEN".to_string(), "s3cret".to_string())]
        );

        // Codex's own servers reach Claude; the secret is a ${VAR} reference.
        fs::write(
            home.join(".codex/config.toml"),
            "[mcp_servers.db]\ncommand = \"db-mcp\"\nenv = { DB_PASS = \"pw\" }\n",
        )
        .unwrap();
        let (args, env) = launch("claude", home);
        let config = args[0].to_string_lossy();
        let config: Value =
            serde_json::from_str(config.strip_prefix("--mcp-config=").unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["db"]["env"]["DB_PASS"],
            "${NECTRA_MCP_6462_44425F50415353}"
        );
        assert_eq!(
            env,
            [(
                "NECTRA_MCP_6462_44425F50415353".to_string(),
                "pw".to_string()
            )]
        );
    }

    #[test]
    fn gemini_settings_are_read_as_gemini_reads_them() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".gemini")).unwrap();
        std::env::set_var("NECTRA_TEST_MCP_KEY", "k1");
        fs::write(
            home.join(".gemini/settings.json"),
            r#"{
              // comments are allowed here
              "mcpServers": {
                "api": {"command": "api-mcp", "args": ["--url=http://x//y"],
                        "env": {"KEY": "$NECTRA_TEST_MCP_KEY", "B": "${NECTRA_TEST_UNSET:-dflt}"}},
                "unsure": {"command": "u-mcp", "env": {"C": "abc$NECTRA_TEST_UNSET"}},
                /* a server limited to some tools isn't shared without its limits */
                "db": {"command": "db-mcp", "excludeTools": ["drop_table"]}
              }
            }"#,
        )
        .unwrap();
        let servers = scan(home).unwrap();
        let shared = servers.missing("codex");
        assert_eq!(shared.len(), 1);
        let (name, api) = shared[0];
        assert_eq!(name, "api");
        assert_eq!(api.args, ["--url=http://x//y"]);
        assert_eq!(api.env["KEY"], "k1");
        assert_eq!(api.env["B"], "dflt");

        // Claude expands only the braced form: `abc$X` is a literal secret.
        fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers": {"c": {"command": "c-mcp",
                "env": {"LIT": "abc$NECTRA_TEST_MCP_KEY", "REF": "${NECTRA_TEST_MCP_KEY}", "U": "${NECTRA_TEST_UNSET}"}}}}"#,
        )
        .unwrap();
        let servers = scan(home).unwrap();
        let c = &servers.shared["c"].1;
        assert_eq!(c.env["LIT"], "abc$NECTRA_TEST_MCP_KEY");
        assert_eq!(c.env["REF"], "k1");
        assert_eq!(c.env["U"], "${NECTRA_TEST_UNSET}");
        assert_ne!(env_name("foo-bar", "T"), env_name("foo_bar", "T"));
    }

    #[test]
    fn sync_writes_each_config_so_the_cli_reads_the_server_back() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(home.join(".codex/config.toml"), "# mine\nmodel = \"x\"").unwrap();
        fs::write(home.join(".claude.json"), r#"{"numStartups": 3}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let private = fs::Permissions::from_mode(0o600);
            fs::set_permissions(home.join(".claude.json"), private).unwrap();
        }
        let server = Server {
            command: r"C:\mcp\gh.exe".into(),
            args: vec!["--a=\"q\"".into()],
            env: [("TOKEN".to_string(), "s3cret".to_string())].into(),
        };
        for cli in ["claude", "codex", "gemini"] {
            add(home, cli, "gh", &server).unwrap();
        }
        let codex = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(codex.starts_with("# mine\nmodel = \"x\"\n"));
        let claude: Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(claude["numStartups"], 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join(".claude.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "a private config stays private");
        }
        let servers = scan(home).unwrap();
        for cli in ["claude", "codex", "gemini"] {
            assert!(servers.names[cli].contains(&"gh".to_string()), "{cli}");
        }
        assert_eq!(servers.shared["gh"].1, server);

        fs::write(home.join(".gemini/settings.json"), "{ // mine\n}").unwrap();
        assert!(add(home, "gemini", "gh", &server).is_err());
    }
}
