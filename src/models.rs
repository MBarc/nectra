use crate::config::Config;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

const DEFAULT_REGISTRY: &str = include_str!("../config/models.toml");

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Openai,
    Ollama,
    Google,
}
impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Ollama => "ollama",
            Self::Google => "google",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Claude,
    Codex,
    Ollama,
    /// The retired Gemini CLI, kept as an API-key fallback (docs/reference.md).
    Gemini,
    /// Antigravity CLI: Google's runtime now that Gemini CLI rejects personal sign-in.
    Agy,
}
impl Runtime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Ollama => "ollama",
            Self::Gemini => "gemini",
            Self::Agy => "agy",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub coding: f32,
    pub reasoning: f32,
    pub long_context: f32,
    pub writing: f32,
    pub tool_use: f32,
    pub speed: f32,
    pub cost_efficiency: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub id: String,
    pub provider: Provider,
    pub runtime: Runtime,
    pub enabled: bool,
    pub capabilities: Capabilities,
    #[serde(default)]
    pub effort: String,
    /// Routing roles this model claims, e.g. "demanding" or "simple-coding".
    /// Any model may declare these, including generated ollama/* ids.
    #[serde(default)]
    pub prefers: Vec<String>,
}

/// Warn when a registry predates declared routing roles.
///
/// An installed `models.toml` shadows the bundled one, so an older copy keeps
/// routing on the legacy id table indefinitely. Nothing in the CLI revealed
/// that, which made routing look inexplicable rather than merely out of date.
pub fn legacy_registry_notice(registry: &Registry) -> Option<String> {
    if registry.uses_declared_preferences() {
        return None;
    }
    let bundled: RegistryFile = toml::from_str(DEFAULT_REGISTRY).ok()?;
    if !bundled.model.iter().any(|m| !m.prefers.is_empty()) {
        return None;
    }
    Some(
        concat!(
            "This registry predates the `prefers` field, so routing preferences ",
            "use the legacy built-in table. Add `prefers` entries, or refresh the ",
            "registry from this release, to declare roles per model."
        )
        .to_string(),
    )
}

/// One `nectra models` row.
///
/// Access is deliberately absent: it is learned from real provider responses,
/// so printing a constant "unverified" on every row told the user nothing.
/// The declared routing role is state they can actually act on.
pub fn roster_line(m: &ModelProfile) -> String {
    format!(
        "{:<10} {:<18} {:<8} {:<8} {:<7} {}",
        m.provider.as_str(),
        m.id,
        m.runtime.as_str(),
        if m.enabled { "enabled" } else { "disabled" },
        if m.effort.is_empty() { "-" } else { &m.effort },
        if m.prefers.is_empty() {
            "-".to_string()
        } else {
            m.prefers.join(",")
        }
    )
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    model: Vec<ModelProfile>,
}
#[derive(Debug)]
pub struct Registry {
    pub models: Vec<ModelProfile>,
}

impl Registry {
    pub fn load(cfg: &Config) -> Result<Self> {
        let path = cfg.registry_path();
        let text = if cfg.embedded {
            DEFAULT_REGISTRY.to_string()
        } else if path.exists() {
            fs::read_to_string(&path)
                .with_context(|| format!("reading model registry {}", path.display()))?
        } else {
            return Err(anyhow::anyhow!(
                "model registry not found: {}",
                path.display()
            ));
        };
        let mut file: RegistryFile = toml::from_str(&text).context("parsing model registry")?;
        if let Some(ollama) = &cfg.inner.ollama {
            for model in &ollama.models {
                file.model.push(ModelProfile {
                    id: format!("ollama/{}", model.name),
                    provider: Provider::Ollama,
                    runtime: Runtime::Ollama,
                    enabled: true,
                    effort: String::new(),
                    prefers: vec![],
                    capabilities: Capabilities {
                        coding: 0.65,
                        reasoning: 0.65,
                        long_context: 0.5,
                        writing: 0.7,
                        tool_use: 0.0,
                        speed: 0.5,
                        cost_efficiency: 1.0,
                    },
                });
            }
        }
        if let Some(providers) = &cfg.inner.providers {
            for model in &mut file.model {
                model.enabled &= providers.iter().any(|p| p == model.provider.as_str());
            }
        }
        // Google is declared in the registry like every other model, but stays
        // opt-in: a configuration that never selected google must not route to it.
        let google_selected = cfg
            .inner
            .providers
            .as_ref()
            .is_some_and(|p| p.iter().any(|id| id == "google"));
        if !google_selected {
            for model in &mut file.model {
                if model.provider == Provider::Google {
                    model.enabled = false;
                }
            }
        }
        let registry = Self { models: file.model };
        registry.validate()?;
        Ok(registry)
    }
    /// True once any model declares `prefers`, which switches routing
    /// preferences from the legacy id table to registry declarations.
    pub fn uses_declared_preferences(&self) -> bool {
        self.models.iter().any(|m| !m.prefers.is_empty())
    }
    pub fn validate(&self) -> Result<()> {
        if self.models.is_empty() {
            bail!("model registry is empty")
        }
        let mut ids = std::collections::HashSet::new();
        for m in &self.models {
            if m.id.trim().is_empty()
                || m.id.starts_with('-')
                || m.id.chars().any(char::is_whitespace)
                || m.id.contains('\0')
            {
                bail!("invalid model id: {:?}", m.id);
            }
            if !ids.insert(&m.id) {
                bail!("duplicate model id: {}", m.id);
            }
            if ![
                "", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .contains(&m.effort.as_str())
            {
                bail!("invalid effort for {}", m.id);
            }
            match (m.provider, m.runtime) {
                (Provider::Anthropic, Runtime::Claude)
                | (Provider::Openai, Runtime::Codex)
                | (Provider::Ollama, Runtime::Ollama)
                | (Provider::Google, Runtime::Gemini | Runtime::Agy) => {}
                _ => bail!("fail-closed provider/runtime violation for model {}", m.id),
            }
            let c = &m.capabilities;
            for v in [
                c.coding,
                c.reasoning,
                c.long_context,
                c.writing,
                c.tool_use,
                c.speed,
                c.cost_efficiency,
            ] {
                if !(0.0..=1.0).contains(&v) {
                    bail!("capability values must be 0..1 for {}", m.id)
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_roster_line_shows_the_declared_routing_role() {
        // "access=unverified (checked on use)" was printed on every row as a
        // constant. The role a model claims is real, inspectable routing state.
        let file: RegistryFile = toml::from_str(DEFAULT_REGISTRY).unwrap();
        let opus = file.model.iter().find(|m| m.id == "opus").unwrap();
        let line = roster_line(opus);
        assert!(
            line.contains("planning"),
            "expected the declared role in: {line}"
        );
        assert!(
            !line.contains("unverified"),
            "constant noise on every row: {line}"
        );
    }

    #[test]
    fn embedded_registry_only_has_allowed_providers() {
        let file: RegistryFile = toml::from_str(DEFAULT_REGISTRY).unwrap();
        let r = Registry { models: file.model };
        r.validate().unwrap();
    }
}
