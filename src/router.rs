use crate::{
    classifier::TaskType,
    cli::{Mode, ProviderArg},
    config::Config,
    models::{Provider, Registry, Runtime},
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
    // A pinned model makes the classification informational; don't pay Jev for it.
    let classification = if model.is_some() {
        crate::jev::classify_local(prompt, None)
    } else {
        let roles = policy::claimed_roles(&registry.models, registry.uses_declared_preferences());
        crate::jev::classify_turn(prompt, None, cfg, &roles)
    };
    route_with(classification, prompt, cfg, registry, mode, provider, model)
}

/// `route` with a classification made earlier, e.g. once per chat message
/// and reused across rate-limit retries.
pub fn route_with(
    classification: crate::jev::Classification,
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
) -> Result<Decision> {
    route_summarized(classification, prompt, cfg, registry, mode, provider, model).map(|(d, _)| d)
}

/// `route_with`, plus the one-line why chat shows above each answer, such as
/// "quick question · preferred for this".
pub fn route_summarized(
    classification: crate::jev::Classification,
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
) -> Result<(Decision, String)> {
    let crate::jev::Classification {
        task,
        req,
        tier,
        why,
        role: jev_role,
        ..
    } = classification;
    let wanted = provider.map(|p| match p {
        ProviderArg::Anthropic => Provider::Anthropic,
        ProviderArg::Openai => Provider::Openai,
        ProviderArg::Ollama => Provider::Ollama,
        ProviderArg::Google => Provider::Google,
    });
    let declarative = registry.uses_declared_preferences();
    // Demanding intent, from Jev's difficulty or the prompt's own words, even
    // when no model claims the demanding role and the tier is dropped below.
    let demanding = tier == Some(crate::jev::Tier::Demanding)
        || policy::preferred_role(task, prompt, mode, None) == "demanding";
    let tier = policy::usable_tier(task, prompt, mode, tier, &registry.models, declarative);
    let mut eligible: Vec<_> = registry
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
        .collect();
    // Ollama is chat-only: it can describe a code change but cannot read, edit
    // or run anything. Work on code goes to a model that can act, whenever one
    // is eligible, before any local preference is applied.
    let acts = matches!(
        task,
        TaskType::CodeGeneration
            | TaskType::Debugging
            | TaskType::Refactoring
            | TaskType::CodeReview
    );
    if acts && eligible.iter().any(|m| m.runtime != Runtime::Ollama) {
        eligible.retain(|m| m.runtime != Runtime::Ollama);
    }
    // Jev's role is chosen among roles your models claim; otherwise the local
    // heuristic picks one from task type and wording. Both then go cheap-first
    // among the models actually eligible. These are product routing
    // preferences, not model benchmark measurements.
    let role = jev_role
        .as_deref()
        .unwrap_or_else(|| policy::preferred_role(task, prompt, mode, tier));
    let role = if !demanding {
        policy::cheap_first(role, mode, &eligible, declarative)
    } else if matches!(role, "coding" | "planning") {
        // The strong coder or planner may take demanding work in its field.
        role
    } else {
        // A hard task Jev files under "general" or a cheap role still needs
        // the strongest model, not the writer.
        "demanding"
    };
    // Demanding work that no model claims is scored for capability alone, so a
    // registry without a demanding model sends it to its strongest one (Opus
    // when only Claude is set up), never the cheap coder or the balanced pick.
    // An ordinary role (Jev's "general", say) must not hand its bonus back to
    // the balanced pick.
    let unclaimed = demanding
        && !eligible
            .iter()
            .any(|m| policy::claims(m, "demanding", declarative));
    let scoring = if unclaimed { Mode::Best } else { mode };
    let mut candidates: Vec<_> = eligible
        .into_iter()
        .map(|m| {
            let bonus = if !unclaimed && policy::claims(m, role, declarative) {
                0.30
            } else {
                0.0
            };
            (m, policy::score(&req, m, scoring) + bonus)
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
    ) + if acts && winner.runtime == Runtime::Ollama {
        "; no eligible model can edit files, so this Ollama model can only describe the change"
    } else {
        ""
    };
    let kind = match role {
        "simple" => "quick question",
        "simple-coding" => "small code change",
        "coding" => "coding",
        "planning" => "planning",
        "general" => "writing or explaining",
        other => other,
    };
    let how = if overridden {
        "your choice".to_string()
    } else if unclaimed {
        "strongest available".to_string()
    } else if ambiguous {
        format!("close call with {}", candidates[1].0.id)
    } else if policy::claims(winner, role, declarative) {
        "preferred for this".to_string()
    } else {
        "best fit available".to_string()
    };
    let summary = format!("{kind} · {how}");
    Ok((
        Decision {
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
        },
        summary,
    ))
}
