use crate::{
    classifier,
    cli::{Mode, ProviderArg},
    config::Config,
    models::{Provider, Registry},
    policy,
};
use anyhow::{bail, Result};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub model: String,
    pub provider: String,
    pub score: f32,
}
#[derive(Debug, Serialize)]
pub struct Decision {
    pub task: String,
    pub provider: String,
    pub runtime: String,
    pub model: String,
    pub effort: String,
    pub mode: Mode,
    pub score: f32,
    pub score_margin: Option<f32>,
    pub ambiguous: bool,
    pub overridden: bool,
    pub reason: String,
    pub candidates: Vec<Candidate>,
}
pub fn route(
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
) -> Result<Decision> {
    if prompt.trim().is_empty() {
        bail!("prompt cannot be empty");
    }
    registry.validate()?;
    let (task, req, why) = classifier::classify(prompt);
    let wanted = provider.map(|p| match p {
        ProviderArg::Anthropic => Provider::Anthropic,
        ProviderArg::Openai => Provider::Openai,
        ProviderArg::Ollama => Provider::Ollama,
        ProviderArg::Google => Provider::Google,
    });
    let mut candidates: Vec<_> = registry
        .models
        .iter()
        .filter(|m| {
            m.enabled
                && !(cfg.ollama_enabled()
                    && cfg
                        .inner
                        .ollama
                        .as_ref()
                        .is_some_and(|o| o.policy == crate::ollama::LocalPolicy::LocalOnly)
                    && m.provider != Provider::Ollama)
                && wanted.is_none_or(|p| p == m.provider)
                && model.as_ref().is_none_or(|id| id == &m.id)
        })
        .map(|m| {
            (
                m,
                policy::score(&req, m, mode)
                    + policy::responsibility_bonus(task, prompt, &m.id, mode),
            )
        })
        .collect();
    if wanted.is_none() && model.is_none() && cfg.ollama_enabled() {
        if let Some(ollama) = &cfg.inner.ollama {
            let prefer_local = ollama.policy == crate::ollama::LocalPolicy::PreferLocal;
            let prefer_cloud = ollama.policy == crate::ollama::LocalPolicy::FallbackOnly;
            if (prefer_local || prefer_cloud)
                && candidates
                    .iter()
                    .any(|(m, _)| (m.provider == Provider::Ollama) == prefer_local)
            {
                candidates.retain(|(m, _)| (m.provider == Provider::Ollama) == prefer_local);
            }
        }
    }
    if candidates.is_empty() {
        bail!("no enabled model matches the requested provider/model; run nectra models");
    }
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
    let (winner, score) = candidates[0];
    let margin = candidates.get(1).map(|c| (score - c.1).max(0.0));
    let ambiguous = margin.is_some_and(|m| m < cfg.inner.routing.confidence_margin);
    let overridden = model.is_some() || provider.is_some();
    let reason = format!(
        "{why}; {}{}",
        if overridden {
            "explicit constraints applied; "
        } else {
            ""
        },
        if ambiguous {
            "close heuristic scores; use --model to choose explicitly"
        } else {
            "highest eligible score including task responsibility and complexity"
        }
    );
    Ok(Decision {
        task: task.to_string(),
        provider: winner.provider.as_str().into(),
        runtime: winner.runtime.as_str().into(),
        model: winner.id.clone(),
        effort: winner.effort.clone(),
        mode,
        score,
        score_margin: margin,
        ambiguous,
        overridden,
        reason,
        candidates: candidates
            .iter()
            .map(|(m, score)| Candidate {
                model: m.id.clone(),
                provider: m.provider.as_str().into(),
                score: *score,
            })
            .collect(),
    })
}
