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
    Gemini,
}
impl Runtime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Ollama => "ollama",
            Self::Gemini => "gemini",
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
        // Upgrade the original bundled roster in memory, preserving customized entries.
        if ["opus", "sonnet", "gpt-5.6-sol", "gpt-5.6-luna"]
            .iter()
            .all(|id| file.model.iter().any(|m| m.id == *id))
        {
            let defaults: RegistryFile = toml::from_str(DEFAULT_REGISTRY)?;
            for model in defaults
                .model
                .into_iter()
                .filter(|m| matches!(m.id.as_str(), "haiku" | "gpt-6-astra"))
            {
                if !file.model.iter().any(|m| m.id == model.id) {
                    file.model.push(model);
                }
            }
        }
        if cfg
            .inner
            .providers
            .as_ref()
            .is_some_and(|p| p.iter().any(|p| p == "google"))
            && !file.model.iter().any(|m| m.provider == Provider::Google)
        {
            for (id, quality, speed) in [
                ("gemini-2.5-pro", 0.9, 0.65),
                ("gemini-2.5-flash", 0.8, 0.9),
            ] {
                file.model.push(ModelProfile {
                    id: id.into(),
                    provider: Provider::Google,
                    runtime: Runtime::Gemini,
                    enabled: true,
                    effort: String::new(),
                    capabilities: Capabilities {
                        coding: quality,
                        reasoning: quality,
                        long_context: 0.95,
                        writing: quality,
                        tool_use: quality,
                        speed,
                        cost_efficiency: speed,
                    },
                });
            }
        }
        if let Some(ollama) = &cfg.inner.ollama {
            for model in &ollama.models {
                file.model.push(ModelProfile {
                    id: format!("ollama/{}", model.name),
                    provider: Provider::Ollama,
                    runtime: Runtime::Ollama,
                    enabled: true,
                    effort: String::new(),
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
        Ok(Self { models: file.model })
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
                (Provider::Google, Runtime::Gemini) => {}
                (Provider::Anthropic, Runtime::Claude)
                | (Provider::Openai, Runtime::Codex)
                | (Provider::Ollama, Runtime::Ollama) => {}
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
    fn embedded_registry_only_has_allowed_providers() {
        let file: RegistryFile = toml::from_str(DEFAULT_REGISTRY).unwrap();
        let r = Registry { models: file.model };
        r.validate().unwrap();
    }
}
