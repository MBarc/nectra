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

/// Product routing preferences, not model benchmark measurements.
pub fn responsibility_bonus(
    task: crate::classifier::TaskType,
    prompt: &str,
    id: &str,
    mode: Mode,
) -> f32 {
    use crate::classifier::TaskType::*;
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
    .any(|w| has(w));
    let simple = ["quick", "simple", "brief", "short", "summarize", "extract"]
        .iter()
        .any(|w| has(w));
    let coding = matches!(
        task,
        CodeGeneration | Debugging | Refactoring | CodeReview | DataAnalysis
    );
    let preferred = if demanding {
        "gpt-6-astra"
    } else if matches!(mode, Mode::Fast) || simple {
        if coding {
            "gpt-5.6-luna"
        } else {
            "haiku"
        }
    } else {
        match task {
            CodeGeneration | Debugging | Refactoring | CodeReview | DataAnalysis => "gpt-5.6-sol",
            Architecture | Planning => "opus",
            Research | Writing | General => "sonnet",
        }
    };
    if id == preferred {
        0.30
    } else {
        0.0
    }
}
