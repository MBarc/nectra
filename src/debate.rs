//! Choosing the two participants for a debate turn.
//!
//! Selection is deliberately separate from running a debate: it is pure,
//! testable, and unaffected by the open questions in the design spec (cost,
//! Gemini's status, how an unenforceable read-only provider is labelled).
//!
//! Router candidates carry only model, provider and score. Everything else a
//! debate needs -- runtime, and whether the provider can be constrained --
//! comes from the validated registry.

pub mod plain;

use crate::{
    models::{ModelProfile, Registry, Runtime},
    router::Candidate,
};

pub mod command;
pub mod runner;

/// Why a debate could not be held with two participants.
#[derive(Debug, PartialEq, Eq)]
pub enum Degraded {
    /// The user pinned a model or provider; honouring that leaves one choice.
    UserConstrained,
    /// Only one model survived availability, cooldown and capability filters.
    NoEligibleOpponent,
}

#[derive(Debug)]
pub struct Selection<'a> {
    pub primary: &'a ModelProfile,
    pub opponent: Option<&'a ModelProfile>,
    pub degraded: Option<Degraded>,
}

impl Selection<'_> {
    pub fn is_debate(&self) -> bool {
        self.opponent.is_some()
    }
}

/// Everything selection needs to know about the world, injected so tests never
/// touch real CLIs, cooldown state or the network.
pub struct Eligibility<'a> {
    /// Is this runtime's CLI actually usable right now?
    pub available: &'a dyn Fn(Runtime) -> bool,
    /// Is this model or its provider cooling down after a rate limit?
    pub cooling_down: &'a dyn Fn(&str, &str) -> bool,
    /// Can this runtime be launched so it cannot mutate the repository?
    pub read_only_capable: &'a dyn Fn(Runtime) -> bool,
    /// True when the user pinned a model or provider for this turn.
    pub user_constrained: bool,
}

fn eligible<'a>(
    candidates: &[Candidate],
    registry: &'a Registry,
    rules: &Eligibility,
) -> Vec<&'a ModelProfile> {
    candidates
        .iter()
        .filter_map(|c| registry.models.iter().find(|m| m.id == c.model))
        .filter(|m| m.enabled)
        .filter(|m| (rules.available)(m.runtime))
        .filter(|m| (rules.read_only_capable)(m.runtime))
        .filter(|m| !(rules.cooling_down)(&m.id, m.provider.as_str()))
        .collect()
}

/// Pick the debaters from the router's ranked candidates.
///
/// Selects two different providers for the requested cross-provider comparison.
/// It never discards a user pin to manufacture that diversity: an
/// explicit constraint outranks the shape of the mode.
pub fn select<'a>(
    candidates: &[Candidate],
    registry: &'a Registry,
    rules: &Eligibility,
) -> Option<Selection<'a>> {
    let eligible = eligible(candidates, registry, rules);
    let primary = *eligible.first()?;

    if rules.user_constrained {
        return Some(Selection {
            primary,
            opponent: None,
            degraded: Some(Degraded::UserConstrained),
        });
    }

    let opponent = eligible
        .iter()
        .find(|m| m.provider != primary.provider)
        .copied();

    Some(Selection {
        degraded: opponent.is_none().then_some(Degraded::NoEligibleOpponent),
        primary,
        opponent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Capabilities, Provider};

    fn profile(id: &str, provider: Provider, runtime: Runtime) -> ModelProfile {
        ModelProfile {
            id: id.into(),
            provider,
            runtime,
            enabled: true,
            effort: String::new(),
            prefers: vec![],
            capabilities: Capabilities {
                coding: 0.8,
                reasoning: 0.8,
                long_context: 0.8,
                writing: 0.8,
                tool_use: 0.8,
                speed: 0.8,
                cost_efficiency: 0.8,
            },
        }
    }

    fn registry() -> Registry {
        Registry {
            models: vec![
                profile("sol", Provider::Openai, Runtime::Codex),
                profile("sonnet", Provider::Anthropic, Runtime::Claude),
                profile("luna", Provider::Openai, Runtime::Codex),
            ],
        }
    }

    fn ranked(ids: &[&str]) -> Vec<Candidate> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| Candidate {
                model: (*id).into(),
                provider: String::new(),
                score: 1.0 - i as f32 / 10.0,
            })
            .collect()
    }

    fn rules<'a>() -> Eligibility<'a> {
        Eligibility {
            available: &|_| true,
            cooling_down: &|_, _| false,
            read_only_capable: &|_| true,
            user_constrained: false,
        }
    }

    #[test]
    fn picks_the_top_candidate_and_the_best_from_another_provider() {
        let reg = registry();
        // "luna" outranks "sonnet" but shares a provider with the leader.
        let s = select(&ranked(&["sol", "luna", "sonnet"]), &reg, &rules()).unwrap();
        assert_eq!(s.primary.id, "sol");
        assert_eq!(s.opponent.unwrap().id, "sonnet");
        assert!(s.degraded.is_none());
    }

    #[test]
    fn an_explicit_user_pin_is_never_overridden_for_diversity() {
        let reg = registry();
        let mut r = rules();
        r.user_constrained = true;
        let s = select(&ranked(&["sol", "sonnet"]), &reg, &r).unwrap();
        assert!(!s.is_debate());
        assert_eq!(s.degraded, Some(Degraded::UserConstrained));
    }

    #[test]
    fn a_cooling_down_model_is_not_selected() {
        let reg = registry();
        let mut r = rules();
        r.cooling_down = &|id, _| id == "sonnet";
        let s = select(&ranked(&["sol", "luna", "sonnet"]), &reg, &r).unwrap();
        // Only OpenAI models remain, so there is no cross-provider opponent.
        assert_eq!(s.primary.id, "sol");
        assert_eq!(s.degraded, Some(Degraded::NoEligibleOpponent));
    }

    #[test]
    fn a_runtime_that_cannot_be_constrained_is_excluded() {
        let reg = registry();
        let mut r = rules();
        r.read_only_capable = &|runtime| runtime != Runtime::Claude;
        let s = select(&ranked(&["sol", "sonnet"]), &reg, &r).unwrap();
        assert!(!s.is_debate());
        assert_eq!(s.degraded, Some(Degraded::NoEligibleOpponent));
    }

    #[test]
    fn an_unavailable_runtime_is_skipped_rather_than_chosen() {
        let reg = registry();
        let mut r = rules();
        r.available = &|runtime| runtime != Runtime::Codex;
        let s = select(&ranked(&["sol", "luna", "sonnet"]), &reg, &r).unwrap();
        assert_eq!(s.primary.id, "sonnet");
        assert_eq!(s.degraded, Some(Degraded::NoEligibleOpponent));
    }

    #[test]
    fn no_eligible_model_selects_nothing() {
        let reg = registry();
        let mut r = rules();
        r.available = &|_| false;
        assert!(select(&ranked(&["sol", "sonnet"]), &reg, &r).is_none());
    }
}
