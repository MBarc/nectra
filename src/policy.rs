use crate::{classifier::Requirements, cli::Mode, models::ModelProfile};

pub fn score(r: &Requirements, m: &ModelProfile, mode: Mode) -> f32 {
    let c = &m.capabilities;
    let quality = r.coding * c.coding
        + r.reasoning * c.reasoning
        + r.long_context * c.long_context
        + r.writing * c.writing
        + r.tool_use * c.tool_use;
    let quality_den = r.coding + r.reasoning + r.long_context + r.writing + r.tool_use;
    let q = if quality_den > 0.0 {
        quality / quality_den
    } else {
        0.0
    };
    match mode {
        Mode::Fast => 0.50 * q + 0.40 * c.speed + 0.10 * c.cost_efficiency,
        Mode::Balanced => 0.72 * q + 0.16 * c.speed + 0.12 * c.cost_efficiency,
        Mode::Best => 0.94 * q + 0.04 * c.speed + 0.02 * c.cost_efficiency,
    }
}

/// The routing role a prompt calls for, which models claim via `prefers`.
///
/// Returning a role rather than a model id is what lets any model win a
/// preference. An id table can only ever name models whose ids are known at
/// compile time, which silently excludes generated ids such as `ollama/<name>`.
pub fn preferred_role(
    task: crate::classifier::TaskType,
    prompt: &str,
    mode: Mode,
    tier: Option<crate::jev::Tier>,
) -> &'static str {
    use crate::classifier::TaskType::*;
    use crate::jev::Tier;
    let words = format!(
        " {} ",
        prompt
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let has = |term: &str| words.contains(&format!(" {term} "));
    let demanding = [
        "complex",
        "difficult",
        "hardest",
        "multi step",
        "multistep",
        "end to end",
        "escalate",
        "deep reasoning",
    ]
    .iter()
    .any(|w| has(w))
        || tier == Some(Tier::Demanding);
    // Words about the size of the work, not the kind of it. "summarize" and
    // "extract" name a task; summarising a large document is not cheap work.
    let simple = [
        "quick", "simple", "brief", "briefly", "short", "trivial", "minor",
    ]
    .iter()
    .any(|w| has(w))
        || tier == Some(Tier::Simple)
        // Without Jev, a few words of chat ("thanks, that worked") are the
        // clearest cheap work there is. Coding tasks are excluded: "fix the
        // race condition" is short and hard.
        || (tier.is_none() && task == General && words.split_whitespace().count() <= 4);
    let coding = matches!(
        task,
        CodeGeneration | Debugging | Refactoring | CodeReview | DataAnalysis
    );
    if demanding {
        "demanding"
    } else if matches!(mode, Mode::Fast) || simple {
        if coding {
            "simple-coding"
        } else {
            "simple"
        }
    } else {
        match task {
            CodeGeneration | Debugging | Refactoring | CodeReview | DataAnalysis => "coding",
            Architecture | Planning => "planning",
            Research | Writing | General => "general",
        }
    }
}

/// Legacy id table, used only when no model in the registry declares `prefers`.
///
/// Without this an existing on-disk registry would silently lose its routing
/// preferences on upgrade. Declaring `prefers` on any model opts the whole
/// registry into the declarative path.
fn legacy_preferred_id(role: &str) -> &'static str {
    match role {
        "demanding" => "gpt-6-astra",
        "simple-coding" => "gpt-5.6-luna",
        "simple" => "haiku",
        "coding" => "gpt-5.6-sol",
        "planning" => "opus",
        _ => "sonnet",
    }
}

/// Every routing role, with the plain description Jev chooses between.
pub const ROLES: [(&str, &str); 6] = [
    (
        "simple",
        "Trivial non-coding work: thanks, chit-chat, a quick fact, a short rewrite",
    ),
    (
        "simple-coding",
        "A small, contained code change: rename, add a flag, a short script",
    ),
    (
        "coding",
        "Regular software work: features, debugging, tests, code review, data analysis",
    ),
    ("planning", "Plans, architecture and design decisions"),
    ("general", "Writing, research and explanations"),
    (
        "demanding",
        "Hard, multi-step or high-stakes work that needs the strongest model",
    ),
];

/// The roles at least one enabled model claims: the only answers worth offering Jev.
pub fn claimed_roles(
    models: &[crate::models::ModelProfile],
    declarative: bool,
) -> Vec<(&'static str, &'static str)> {
    ROLES
        .into_iter()
        .filter(|(role, _)| {
            models
                .iter()
                .any(|m| m.enabled && claims(m, role, declarative))
        })
        .collect()
}

/// Whether a model takes the work a routing role describes.
pub fn claims(model: &crate::models::ModelProfile, role: &str, declarative: bool) -> bool {
    if declarative {
        model.prefers.iter().any(|r| r == role)
    } else {
        model.id == legacy_preferred_id(role)
    }
}

/// A difficulty tier only helps when some enabled model claims its role.
/// Otherwise it would cancel the task's own role bonus and leave nobody preferred.
pub fn usable_tier(
    task: crate::classifier::TaskType,
    prompt: &str,
    mode: Mode,
    tier: Option<crate::jev::Tier>,
    models: &[crate::models::ModelProfile],
    declarative: bool,
) -> Option<crate::jev::Tier> {
    let role = preferred_role(task, prompt, mode, tier);
    tier.filter(|_| {
        models
            .iter()
            .any(|m| m.enabled && claims(m, role, declarative))
    })
}

/// Balanced mode tries the cheap coder first. In eval/chat-savings the
/// `simple-coding` model passed every check on all three workloads (including
/// an optimal-assignment algorithm) at about 4% of the strong coders' cost.
/// Demanding work still escalates and best mode keeps the strong coder; a
/// registry where nobody claims `simple-coding` keeps `coding`.
pub fn cheap_first<'a>(
    role: &'a str,
    mode: Mode,
    models: &[crate::models::ModelProfile],
    declarative: bool,
) -> &'a str {
    if mode == Mode::Balanced
        && role == "coding"
        && models
            .iter()
            .any(|m| m.enabled && claims(m, "simple-coding", declarative))
    {
        "simple-coding"
    } else {
        role
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        classifier::TaskType,
        models::{Capabilities, ModelProfile, Provider, Runtime},
    };

    pub(super) fn local(id: &str, prefers: &[&str]) -> ModelProfile {
        ModelProfile {
            id: id.into(),
            provider: Provider::Ollama,
            runtime: Runtime::Ollama,
            enabled: true,
            effort: String::new(),
            prefers: prefers.iter().map(|s| s.to_string()).collect(),
            capabilities: Capabilities {
                coding: 0.65,
                reasoning: 0.65,
                long_context: 0.5,
                writing: 0.7,
                tool_use: 0.0,
                speed: 0.5,
                cost_efficiency: 1.0,
            },
        }
    }

    #[test]
    fn a_local_model_can_win_a_task_preference() {
        // ollama/* and gemini-* ids are generated, so an id-literal preference
        // table can never reach them and the +0.30 always lands on a cloud model.
        let model = local("ollama/llama3", &["simple"]);
        let role = preferred_role(TaskType::General, "quick summary", Mode::Fast, None);
        assert!(claims(&model, role, true));
    }

    #[test]
    fn a_model_that_claims_no_role_gets_no_bonus() {
        let model = local("ollama/llama3", &[]);
        let role = preferred_role(TaskType::General, "quick summary", Mode::Fast, None);
        assert!(!claims(&model, role, true));
    }

    #[test]
    fn a_registry_without_declarations_keeps_the_legacy_preferences() {
        let mut haiku = local("haiku", &[]);
        haiku.provider = Provider::Anthropic;
        haiku.runtime = Runtime::Claude;
        let role = preferred_role(TaskType::General, "quick summary", Mode::Fast, None);
        assert!(claims(&haiku, role, false));
    }

    #[test]
    fn balanced_coding_tries_the_cheap_coder_first() {
        let models = [local("luna", &["simple-coding"]), local("sol", &["coding"])];
        assert_eq!(
            cheap_first("coding", Mode::Balanced, &models, true),
            "simple-coding"
        );
        assert_eq!(cheap_first("coding", Mode::Best, &models, true), "coding");
        assert_eq!(
            cheap_first("demanding", Mode::Balanced, &models, true),
            "demanding"
        );
        assert_eq!(
            cheap_first("planning", Mode::Balanced, &models, true),
            "planning"
        );
        // Nobody claims the cheap role: keep the strong coder's preference.
        assert_eq!(
            cheap_first("coding", Mode::Balanced, &models[1..], true),
            "coding"
        );
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;
    use crate::classifier::TaskType;

    #[test]
    fn summarising_is_not_by_itself_a_simplicity_signal() {
        // "summarize" describes the task, not its size. Summarising a large
        // document should not be routed as cheap work.
        assert_eq!(
            preferred_role(
                TaskType::Research,
                "summarize this 400 page report",
                Mode::Balanced,
                None
            ),
            "general"
        );
    }

    #[test]
    fn an_explicit_simplicity_word_still_routes_cheap() {
        assert_eq!(
            preferred_role(TaskType::General, "quick question", Mode::Balanced, None),
            "simple"
        );
    }

    #[test]
    fn jev_difficulty_picks_cheap_or_strong_roles() {
        use crate::jev::Tier;
        let role =
            |t, tier| preferred_role(t, "port this resizer to rust", Mode::Balanced, Some(tier));
        assert_eq!(
            role(TaskType::CodeGeneration, Tier::Simple),
            "simple-coding"
        );
        assert_eq!(role(TaskType::CodeGeneration, Tier::Normal), "coding");
        assert_eq!(role(TaskType::CodeGeneration, Tier::Demanding), "demanding");
        assert_eq!(role(TaskType::Writing, Tier::Simple), "simple");
    }

    #[test]
    fn jev_is_only_offered_roles_some_model_claims() {
        let models = [
            super::tests::local("gpt-5.6-sol", &["coding"]),
            super::tests::local("gpt-6-astra", &["demanding"]),
        ];
        let names: Vec<_> = claimed_roles(&models, true).iter().map(|r| r.0).collect();
        assert_eq!(names, ["coding", "demanding"]);
        // A legacy registry claims through the built-in id table.
        let legacy = [
            super::tests::local("opus", &[]),
            super::tests::local("sonnet", &[]),
        ];
        let names: Vec<_> = claimed_roles(&legacy, false).iter().map(|r| r.0).collect();
        assert_eq!(names, ["planning", "general"]);
    }

    #[test]
    fn an_unclaimed_tier_keeps_the_task_role() {
        use crate::jev::Tier;
        let sol = super::tests::local("gpt-5.6-sol", &["coding"]);
        let models = [sol];
        let p = "debug this race";
        assert_eq!(
            usable_tier(
                TaskType::Debugging,
                p,
                Mode::Balanced,
                Some(Tier::Demanding),
                &models,
                true
            ),
            None
        );
        let astra = super::tests::local("gpt-6-astra", &["demanding"]);
        let models = [models[0].clone(), astra];
        assert_eq!(
            usable_tier(
                TaskType::Debugging,
                p,
                Mode::Balanced,
                Some(Tier::Demanding),
                &models,
                true
            ),
            Some(Tier::Demanding)
        );
    }
}
