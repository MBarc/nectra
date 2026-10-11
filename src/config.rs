use crate::cli::Mode;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const DEFAULT_CONFIG: &str = include_str!("../config/nectra.toml");

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub command: String,
    #[serde(default)]
    pub extra_args: Vec<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    pub confidence_margin: f32,
    pub default_mode: Mode,
    /// Continue the provider's own session when the same model answers again.
    #[serde(default = "enabled")]
    pub resume_sessions: bool,
}
fn enabled() -> bool {
    true
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierBackend {
    /// Jev when TYPESAFE_API_KEY is set, local keywords otherwise.
    Auto,
    Local,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierConfig {
    pub backend: ClassifierBackend,
    pub timeout_ms: u64,
    /// Jev difficulty (0..1) below this routes to the simple roles.
    pub simple_below: f64,
    /// Jev difficulty (0..1) above this routes to the demanding role.
    pub demanding_above: f64,
}
impl Default for ClassifierConfig {
    fn default() -> Self {
        // ponytail: thresholds eyeballed from a 40-prompt holdout; tune against real chat logs
        Self {
            backend: ClassifierBackend::Auto,
            timeout_ms: 1500,
            simple_below: 0.2,
            demanding_above: 0.6,
        }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub classifier: ClassifierConfig,
    #[serde(default = "default_gemini")]
    pub gemini: RuntimeConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama: Option<crate::ollama::OllamaConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    pub registry: String,
    pub claude: RuntimeConfig,
    pub codex: RuntimeConfig,
    pub routing: RoutingConfig,
}
#[derive(Debug, Clone)]
pub struct Config {
    pub provider_view: crate::executor::native::ProviderView,
    pub skip_permissions: bool,
    pub path: PathBuf,
    pub inner: ConfigFile,
    pub embedded: bool,
}
fn default_gemini() -> RuntimeConfig {
    RuntimeConfig {
        command: "gemini".into(),
        extra_args: vec![],
    }
}

impl Config {
    pub fn ollama_enabled(&self) -> bool {
        self.inner
            .providers
            .as_ref()
            .map_or(self.inner.ollama.is_some(), |p| {
                p.iter().any(|id| id == "ollama")
            })
    }
    pub fn from_defaults() -> Result<Self> {
        let cfg = Self {
            provider_view: Default::default(),
            skip_permissions: false,
            inner: toml::from_str(DEFAULT_CONFIG)?,
            path: PathBuf::from("<embedded>"),
            embedded: true,
        };
        cfg.validate()?;
        Ok(cfg)
    }
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        if let Some(path) = explicit {
            return Self::from_path(path);
        }
        if let Some(path) = dirs::config_dir().map(|p| p.join("nectra/config.toml")) {
            match fs::metadata(&path) {
                Ok(_) => return Self::from_path(&path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("checking user config"),
            }
        }
        // Never execute configuration discovered in an arbitrary working directory.
        let cfg = Self {
            provider_view: Default::default(),
            skip_permissions: false,
            inner: toml::from_str(DEFAULT_CONFIG).context("parsing embedded config")?,
            path: PathBuf::from("<embedded>"),
            embedded: true,
        };
        cfg.validate()?;
        Ok(cfg)
    }
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg = Self {
            provider_view: Default::default(),
            skip_permissions: false,
            inner: toml::from_str(&text).context("parsing config")?,
            path: path.to_path_buf(),
            embedded: false,
        };
        cfg.validate()?;
        Ok(cfg)
    }
    fn validate(&self) -> Result<()> {
        if let Some(ollama) = &self.inner.ollama {
            ollama.validate()?;
        }
        if let Some(providers) = &self.inner.providers {
            for provider in providers {
                if !crate::setup::PROVIDERS.iter().any(|p| p.id == provider) {
                    bail!("unknown provider in configuration: {provider}");
                }
            }
        }
        let c = &self.inner.classifier;
        if !(0.0..=1.0).contains(&c.simple_below)
            || !(0.0..=1.0).contains(&c.demanding_above)
            || c.simple_below > c.demanding_above
        {
            bail!("classifier thresholds must satisfy 0 <= simple_below <= demanding_above <= 1");
        }
        if self.inner.registry.trim().is_empty() {
            bail!("registry path cannot be empty");
        }
        if !self.inner.routing.confidence_margin.is_finite()
            || !(0.0..=1.0).contains(&self.inner.routing.confidence_margin)
        {
            bail!("confidence_margin must be finite and between 0 and 1");
        }
        for runtime in [&self.inner.claude, &self.inner.codex, &self.inner.gemini] {
            if runtime.command.trim().is_empty() {
                bail!("runtime command cannot be empty");
            }
            if runtime.command.contains('\0') || runtime.extra_args.iter().any(|a| a.contains('\0'))
            {
                bail!("runtime arguments cannot contain NUL");
            }
            for arg in &runtime.extra_args {
                let flag = arg.split('=').next().unwrap_or(arg);
                if [
                    "--",
                    "--model",
                    "--config",
                    "--profile",
                    "--oss",
                    "--local-provider",
                    "--effort",
                    "--output-format",
                    "--input-format",
                    "--print",
                    "--prompt",
                    "--prompt-interactive",
                    "--experimental-acp",
                    "--fallback-model",
                    "--json",
                    "--json-schema",
                    "--output-schema",
                    "--output-last-message",
                    "--resume",
                    "--continue",
                    "--session-id",
                    "--fork-session",
                ]
                .contains(&flag)
                    || ["-m", "-c", "-p", "-r", "-o"]
                        .iter()
                        .any(|prefix| arg.starts_with(prefix) && !arg.starts_with("--"))
                {
                    bail!("extra_args cannot override routing or prompt transport: {arg}");
                }
            }
        }
        Ok(())
    }
    pub fn registry_path(&self) -> PathBuf {
        let p = PathBuf::from(&self.inner.registry);
        if p.is_absolute() || self.embedded {
            p
        } else {
            self.path.parent().unwrap_or(Path::new(".")).join(p)
        }
    }
    pub fn pretty_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(&self.inner)?)
    }
}
