use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    time::Duration,
};

pub const OUTPUT_TOKEN_LIMIT: usize = 1024;
// Input uses a deliberately conservative byte ceiling rather than a tokenizer estimate.
// Reserve the output allowance before applying that ceiling.
pub const INPUT_RESERVE_BYTES: usize = OUTPUT_TOKEN_LIMIT;
// Extra headroom for converting the shared transcript into provider messages.
pub const TRANSCRIPT_FRAMING_RESERVE_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum LocalPolicy {
    #[default]
    LocalOnly,
    PreferLocal,
    FallbackOnly,
    Mixed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalModel {
    pub name: String,
    pub context_tokens: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OllamaConfig {
    pub base_url: String,
    #[serde(default)]
    pub policy: LocalPolicy,
    pub models: Vec<LocalModel>,
    #[serde(default = "timeout_default")]
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
}
fn timeout_default() -> u64 {
    180
}
pub fn configure(url: &str, names: &[String], policy: LocalPolicy) -> Result<OllamaConfig> {
    let mut config = OllamaConfig {
        base_url: url.trim_end_matches('/').into(),
        policy,
        models: vec![],
        timeout_seconds: 180,
        token_env: None,
    };
    let available = config.discover()?;
    if names.is_empty() {
        bail!("select at least one installed Ollama model");
    }
    for name in names {
        if !available.contains(name) {
            bail!("model '{name}' is not installed on this Ollama server");
        }
        config.models.push(LocalModel {
            name: name.clone(),
            context_tokens: config.inspect(name)?.min(8192),
        });
    }
    config.validate()?;
    Ok(config)
}
pub fn wizard(input: &mut dyn BufRead, output: &mut dyn Write) -> Result<OllamaConfig> {
    fn read(input: &mut dyn BufRead, output: &mut dyn Write, prompt: &str) -> Result<String> {
        write!(output, "{prompt}")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            bail!("Ollama setup needs answers; use nectra ollama --url URL --models NAME --policy local-only for unattended setup");
        }
        Ok(line.trim().into())
    }
    let url = read(
        input,
        output,
        "Ollama server URL [http://localhost:11434]: ",
    )?;
    let url = if url.is_empty() {
        "http://localhost:11434"
    } else {
        &url
    };
    let config = OllamaConfig {
        base_url: url.into(),
        policy: LocalPolicy::LocalOnly,
        models: vec![],
        timeout_seconds: 180,
        token_env: None,
    };
    let models = config.discover()?;
    if models.is_empty() {
        bail!("Ollama has no installed models; install a model on the server, then rerun setup");
    }
    writeln!(output, "Installed models: {}", models.join(", "))?;
    let names = read(input, output, "Model names to enable (comma-separated): ")?
        .split(',')
        .map(|n| n.trim().to_string())
        .collect::<Vec<_>>();
    let policy = read(
        input,
        output,
        "Policy: local-only, prefer-local, fallback-only, mixed [local-only]: ",
    )?;
    let policy = match policy.as_str() {
        "" | "local-only" => LocalPolicy::LocalOnly,
        "prefer-local" => LocalPolicy::PreferLocal,
        "fallback-only" => LocalPolicy::FallbackOnly,
        "mixed" => LocalPolicy::Mixed,
        _ => bail!("unknown local routing policy"),
    };
    configure(url, &names, policy)
}
impl OllamaConfig {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.base_url).context("invalid Ollama server URL")?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            bail!("Ollama URL must be an http(s) server origin without credentials, path, query, or fragment");
        }
        if !(1..=3600).contains(&self.timeout_seconds) {
            bail!("Ollama timeout must be 1..3600 seconds");
        }
        let mut names = std::collections::HashSet::new();
        for model in &self.models {
            if model.name.is_empty()
                || model.name.starts_with('-')
                || model
                    .name
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control())
                || !names.insert(&model.name)
            {
                bail!("invalid or duplicate Ollama model name");
            }
            if !(2048..=131072).contains(&model.context_tokens) {
                bail!("Ollama context_tokens must be 2048..131072");
            }
        }
        if let Some(key) = &self.token_env {
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                bail!("invalid Ollama token environment variable name");
            }
            if url.scheme() != "https"
                && !["localhost", "127.0.0.1", "[::1]"].contains(&url.host_str().unwrap_or(""))
            {
                bail!("remote Ollama bearer tokens require HTTPS");
            }
        }
        Ok(())
    }
    fn client(&self, seconds: u64) -> Result<Client> {
        self.validate()?;
        Ok(Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(seconds))
            .build()?)
    }
    fn request(
        &self,
        client: &Client,
        endpoint: &str,
    ) -> Result<reqwest::blocking::RequestBuilder> {
        let url = format!("{}/api/{endpoint}", self.base_url.trim_end_matches('/'));
        let request = if endpoint == "tags" {
            client.get(url)
        } else {
            client.post(url)
        };
        if let Some(key) = &self.token_env {
            let token =
                std::env::var(key).with_context(|| format!("set {key} for the Ollama server"))?;
            if token.trim().is_empty() {
                bail!("Ollama token environment variable is empty");
            }
            Ok(request.bearer_auth(token))
        } else {
            Ok(request)
        }
    }
    pub fn discover(&self) -> Result<Vec<String>> {
        let client = self.client(5)?;
        let response = self
            .request(&client, "tags")?
            .send()
            .context("Ollama server is unreachable; start Ollama or check its address")?;
        let value = read_json(response)?;
        value["models"]
            .as_array()
            .context("Ollama returned no model list")?
            .iter()
            .map(|m| {
                m["name"]
                    .as_str()
                    .map(str::to_string)
                    .context("Ollama model has no name")
            })
            .collect()
    }
    pub fn inspect(&self, name: &str) -> Result<usize> {
        let client = self.client(5)?;
        let info = read_json(
            self.request(&client, "show")?
                .json(&json!({"model":name}))
                .send()?,
        )?;
        // Every Ollama connection in this release is for locally executed models.
        if ["remote_model", "remote_host"]
            .iter()
            .any(|k| info[*k].as_str().is_some_and(|s| !s.is_empty()))
            || name.ends_with("-cloud")
        {
            bail!("cloud-backed Ollama model '{name}' is not eligible for local routing");
        }
        let metadata = info["model_info"]
            .as_object()
            .context("Ollama model has no local metadata; cannot verify local execution")?;
        let context = metadata
            .iter()
            .filter(|(k, _)| k.ends_with(".context_length"))
            .filter_map(|(_, v)| v.as_u64())
            .min()
            .context("model context capacity is unknown")?;
        if context < 2048 {
            bail!("model context is too small (minimum 2048)");
        }
        if info["capabilities"]
            .as_array()
            .is_some_and(|a| !a.iter().any(|v| v == "completion"))
        {
            bail!("model does not support chat completion");
        }
        Ok(context.min(131072) as usize)
    }
    pub fn check(&self) -> Result<()> {
        let available = self.discover()?;
        if self.models.is_empty() {
            bail!("no Ollama models selected; run nectra provider add ollama");
        }
        for model in &self.models {
            if !available.contains(&model.name) {
                bail!(
                    "Ollama model '{}' is missing; install it on the server or rerun setup",
                    model.name
                );
            }
            if model.context_tokens > self.inspect(&model.name)? {
                bail!("configured context exceeds model capacity");
            }
        }
        Ok(())
    }
}
fn read_json(response: Response) -> Result<Value> {
    if !response.status().is_success() {
        bail!("Ollama metadata request failed: HTTP {}", response.status());
    }
    let mut bytes = vec![];
    response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("Ollama metadata exceeds 1 MiB");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug)]
pub struct Unavailable {
    pub message: String,
    pub safe_to_retry: bool,
    pub provider_wide: bool,
}
impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Unavailable {}

pub fn conversation(
    model_id: &str,
    prompt: &str,
    cfg: &crate::config::Config,
    output: &mut dyn Write,
) -> Result<String> {
    let ollama = cfg
        .inner
        .ollama
        .as_ref()
        .context("Ollama is not configured")?;
    let name = model_id
        .strip_prefix("ollama/")
        .context("invalid Ollama model ID")?;
    let model = ollama
        .models
        .iter()
        .find(|m| m.name == name)
        .context("Ollama model is not selected")?;
    // Check locality again before sending any conversation, in case a model was replaced.
    let capacity = ollama.inspect(name).map_err(|e| Unavailable {
        message: format!("Ollama model check failed: {e:#}"),
        safe_to_retry: true,
        provider_wide: false,
    })?;
    if model.context_tokens > capacity {
        bail!("configured context exceeds model capacity; rerun Ollama setup");
    }
    let messages = messages(prompt)?;
    let size = serde_json::to_vec(&messages)?.len();
    if size > model.context_tokens.saturating_sub(INPUT_RESERVE_BYTES) {
        bail!("message exceeds conservative Ollama context budget; shorten the message");
    }
    let client = ollama.client(ollama.timeout_seconds)?;
    crate::activity::set_phase(crate::activity::Phase::Waiting);
    let response = ollama.request(&client, "chat")?.json(&json!({"model":name,"messages":messages,"stream":true,"options":{"num_ctx":model.context_tokens,"num_predict":OUTPUT_TOKEN_LIMIT}})).send()
        .map_err(|e| Unavailable { message: format!("Ollama request failed: {e}"), safe_to_retry: true, provider_wide: true })?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        if [404, 429, 500, 502, 503, 504].contains(&status) {
            return Err(Unavailable {
                message: format!("Ollama HTTP {status}"),
                safe_to_retry: true,
                provider_wide: status != 404,
            }
            .into());
        }
        bail!("Ollama request rejected: HTTP {status}");
    }
    stream(BufReader::new(response), output)
}

pub fn messages(prompt: &str) -> Result<Vec<Value>> {
    let mut messages = vec![
        json!({"role":"system","content":"Continue the shared Nectra conversation. Earlier assistants may be different models. Answer the latest user. You are a chat-only model: you cannot inspect files, edit files, or run commands. Do not claim to have performed those actions."}),
    ];
    if let Some((prefix, data)) = prompt.split_once('\n').filter(|(prefix, _)| {
        prefix.starts_with("Continue this shared Nectra conversation.")
            || prefix.starts_with(
                "Continue the conversation below and answer current_user_message directly.",
            )
    }) {
        let _ = prefix;
        let context: Value = serde_json::from_str(data)?;
        for exchange in context["conversation"]
            .as_array()
            .context("invalid conversation history")?
        {
            messages.push(json!({"role":"user","content":exchange["user"].as_str().context("missing user message")?}));
            messages.push(json!({"role":"assistant","content":exchange["assistant"].as_str().context("missing assistant message")?}));
        }
        messages.push(json!({"role":"user","content":context["current_user_message"].as_str().context("missing current message")?}));
    } else {
        messages.push(json!({"role":"user","content":prompt}));
    }
    Ok(messages)
}

pub fn stream(mut reader: impl BufRead, output: &mut dyn Write) -> Result<String> {
    let mut reply = String::new();
    let mut activity = false;
    loop {
        let mut line = vec![];
        let count = (&mut reader)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| Unavailable {
                message: format!("Ollama stream interrupted: {e}"),
                safe_to_retry: !activity,
                provider_wide: true,
            })?;
        if count == 0 {
            return Err(Unavailable {
                message: "Ollama stream ended without completion".into(),
                safe_to_retry: !activity,
                provider_wide: true,
            }
            .into());
        }
        if count > 1024 * 1024 {
            bail!("Ollama event exceeds 1 MiB");
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event: Value = serde_json::from_slice(&line).context("invalid Ollama stream event")?;
        if let Some(phase) = crate::activity::event_phase("ollama", &event) {
            crate::activity::set_phase(phase);
        }
        if let Some(error) = event["error"].as_str() {
            return Err(Unavailable {
                message: format!("Ollama: {error}"),
                safe_to_retry: !activity,
                provider_wide: false,
            }
            .into());
        }
        if event["message"]["tool_calls"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            bail!("Ollama requested tools; this connection supports chat only");
        }
        if event["message"]["thinking"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        {
            activity = true;
        }
        if let Some(text) = event["message"]["content"].as_str() {
            if !text.is_empty() {
                activity = true;
            }
            if reply.len() + text.len() > 8 * 1024 * 1024 {
                bail!("Ollama answer exceeds 8 MiB");
            }
            reply.push_str(text);
            write!(output, "{text}")?;
            output.flush()?;
        }
        if event["done"] == true {
            if event["done_reason"] == "length" {
                bail!("Ollama reached its output limit; the partial answer was not saved as a completed turn. Ask for a shorter answer or an explicit continuation.");
            }
            if reply.trim().is_empty() {
                bail!("Ollama returned an empty answer");
            }
            writeln!(output)?;
            return Ok(reply);
        }
    }
}
