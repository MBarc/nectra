use crate::{
    classifier::TaskType,
    cli::{Mode, ProviderArg},
    config::Config,
    models::{Provider, Registry, Runtime},
    policy,
    table::{self, Steer},
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
    /// Set when the task x tier table chose the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ladder: Option<crate::table::Selection>,
    /// Escalations the task had taken when this was chosen; 0 for a first pick.
    #[serde(skip_serializing_if = "is_zero")]
    pub escalation: u32,
}
fn is_zero(n: &u32) -> bool {
    *n == 0
}
pub fn route(
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
) -> Result<Decision> {
    route_traced(prompt, cfg, registry, mode, provider, model).map(|(d, _)| d)
}

/// `route`, also returning the classification behind the decision, for the
/// decision log.
pub fn route_traced(
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
) -> Result<(Decision, crate::jev::Classification)> {
    route_traced_steered(
        prompt,
        cfg,
        registry,
        mode,
        provider,
        model,
        &Steer::default(),
    )
}

/// `route_traced` for a retry after a failure: `steer` says which model failed.
pub fn route_traced_steered(
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
    steer: &Steer,
) -> Result<(Decision, crate::jev::Classification)> {
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
    route_summarized_steered(
        classification.clone(),
        prompt,
        cfg,
        registry,
        mode,
        provider,
        model,
        steer,
    )
    .map(|(d, _)| (d, classification))
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
    route_summarized_steered(
        classification,
        prompt,
        cfg,
        registry,
        mode,
        provider,
        model,
        &Steer::default(),
    )
}

/// `route_summarized` for a retry after a failure.
#[allow(clippy::too_many_arguments)]
pub fn route_summarized_steered(
    classification: crate::jev::Classification,
    prompt: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    provider: Option<ProviderArg>,
    model: Option<String>,
    steer: &Steer,
) -> Result<(Decision, String)> {
    let crate::jev::Classification {
        task,
        req,
        tier,
        why,
        role: jev_role,
        classifier,
        confidence,
        difficulty,
        ..
    } = classification;
    let table_in = table_inputs(task, prompt, classifier, tier, confidence, difficulty, cfg);
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
    } else if role == "planning" {
        // The strong planner may take demanding design work.
        role
    } else {
        // Hard code goes to the demanding model, not the coder: Laya files
        // most hard specs under "coding" (eval/laya/long-spec-tuning.json).
        // A hard task filed under "general" or a cheap role needs it too.
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
    // The measured table decides when it has an entry for this kind and tier;
    // a pinned model is the user's word and skips it.
    let ladder = match (&table_in, model.is_none()) {
        (Some(inputs), true) => table::select(
            &registry.table,
            &table::Inputs {
                kind: &task.to_string(),
                ..*inputs
            },
            mode,
            &cfg.inner.routing,
            &|id| candidates.iter().any(|(m, _)| m.id == id),
            steer,
        )?,
        _ => None,
    };
    if let Some(sel) = &ladder {
        if let Some(at) = candidates.iter().position(|(m, _)| m.id == sel.model) {
            let picked = candidates.remove(at);
            candidates.insert(0, picked);
        }
    }
    let (winner, score) = candidates[0];
    let margin = candidates.get(1).map(|c| (score - c.1).max(0.0));
    let ambiguous =
        ladder.is_none() && margin.is_some_and(|m| m < cfg.inner.routing.confidence_margin);
    let overridden = model.is_some() || provider.is_some();
    let reason = format!(
        "{why}; {}{}",
        if overridden {
            "explicit constraints applied; "
        } else {
            ""
        },
        if let Some(sel) = &ladder {
            format!(
                "table {} x {}: rung {}/{}{} ({}){}",
                sel.kind,
                sel.tier,
                sel.rung + 1,
                sel.ladder_len,
                if sel.hedged {
                    ", hedged up for an uncertain tier"
                } else {
                    ""
                },
                sel.evidence,
                if steer.escalations > 0 {
                    format!(
                        ", escalation {} after {}",
                        steer.escalations,
                        steer.above.as_deref().unwrap_or("a failure")
                    )
                } else {
                    String::new()
                }
            )
        } else if ambiguous {
            "close heuristic scores; use --model to choose explicitly".to_string()
        } else {
            "highest eligible score including task responsibility and complexity".to_string()
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
    } else if let Some(sel) = &ladder {
        format!(
            "measured pick, rung {}/{}{}",
            sel.rung + 1,
            sel.ladder_len,
            if steer.escalations > 0 {
                ", escalated"
            } else {
                ""
            }
        )
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
            escalation: if ladder.is_some() {
                steer.escalations
            } else {
                0
            },
            ladder,
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

/// The tier, with how sure it is, for the table lookup; None without a tier.
///
/// Laya's tier follows its difficulty against the configured thresholds, read
/// here so `simple_below` and `demanding_above` steer the table. Prompt
/// keywords ("complex", "quick") only decide the tier on the keywords
/// backend, which has no difficulty to go by.
fn table_inputs<'a>(
    task: TaskType,
    prompt: &str,
    classifier: &str,
    tier: Option<crate::jev::Tier>,
    confidence: Option<f64>,
    difficulty: Option<f64>,
    cfg: &Config,
) -> Option<table::Inputs<'a>> {
    use crate::jev::Tier;
    let c = &cfg.inner.classifier;
    match (classifier, difficulty) {
        (k, Some(d)) if k != "keywords" => {
            let tier = if d < c.simple_below {
                Tier::Simple
            } else if d > c.demanding_above {
                Tier::Demanding
            } else {
                Tier::Normal
            };
            let (to_simple, to_demanding) =
                ((d - c.simple_below).abs(), (d - c.demanding_above).abs());
            let (margin, neighbour) = if to_simple <= to_demanding {
                (
                    to_simple,
                    if tier == Tier::Simple {
                        Tier::Normal
                    } else {
                        Tier::Simple
                    },
                )
            } else {
                (
                    to_demanding,
                    if tier == Tier::Demanding {
                        Tier::Normal
                    } else {
                        Tier::Demanding
                    },
                )
            };
            Some(table::Inputs {
                kind: "",
                tier,
                confidence,
                margin: Some(margin),
                neighbour: Some(neighbour),
            })
        }
        (k, None) if k != "keywords" => tier.map(|tier| table::Inputs {
            kind: "",
            tier,
            confidence,
            margin: None,
            neighbour: None,
        }),
        _ => {
            let tier = match policy::preferred_role(task, prompt, Mode::Balanced, None) {
                "demanding" => Tier::Demanding,
                "simple" | "simple-coding" => Tier::Simple,
                _ => Tier::Normal,
            };
            Some(table::Inputs {
                kind: "",
                tier,
                confidence: None,
                margin: None,
                neighbour: None,
            })
        }
    }
}
