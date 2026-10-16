use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CodeGeneration,
    Debugging,
    Refactoring,
    CodeReview,
    Architecture,
    Research,
    Writing,
    Planning,
    DataAnalysis,
    General,
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_value(self).unwrap().as_str().unwrap()
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Requirements {
    pub coding: f32,
    pub reasoning: f32,
    pub long_context: f32,
    pub writing: f32,
    pub tool_use: f32,
}

pub fn classify(prompt: &str) -> (TaskType, Requirements, String) {
    // Normalise inflection so a keyword list can hold one base form instead of
    // every variant: "fails" and "failing" both reduce to "fail". Matching raw
    // tokens is why "the tests fail" missed a list containing only "failing".
    fn stem(word: &str) -> String {
        // Only the two sides agreeing matters, not linguistic correctness. A
        // bare "es" rule would turn "milestones" into "mileston" while
        // "milestone" stayed whole, so plurals strip a single "s" instead.
        if let Some(base) = word.strip_suffix("ies") {
            if base.len() >= 3 {
                return format!("{base}y");
            }
        }
        for suffix in ["ing", "ed"] {
            if let Some(base) = word.strip_suffix(suffix) {
                if base.len() >= 4 {
                    return base.to_string();
                }
            }
        }
        if !word.ends_with("ss") {
            if let Some(base) = word.strip_suffix('s') {
                if base.len() >= 4 {
                    return base.to_string();
                }
            }
        }
        word.to_string()
    }
    let tokens: Vec<&str> = prompt
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    let stemmed: Vec<String> = lowered.iter().map(|t| stem(t)).collect();
    let p = format!(" {} ", stemmed.join(" "));
    // Both sides are stemmed, so a list entry of "failing" matches a prompt
    // saying "fails". Substring matching is deliberately NOT used: "contest"
    // must not match "test", nor "memoir" match "memo".
    let has = |words: &[&str]| {
        words
            .iter()
            .filter(|phrase| {
                let needle = phrase
                    .split_whitespace()
                    .map(stem)
                    .collect::<Vec<_>>()
                    .join(" ");
                p.contains(&format!(" {needle} "))
            })
            .count() as f32
    };
    let mut code = has(&[
        "implement",
        "code",
        "function",
        "class ",
        "api",
        "test",
        "build",
        "feature",
    ]);
    let debug = has(&[
        "debug",
        "bug",
        "error",
        "failing",
        "exception",
        "trace",
        "race condition",
        "fix",
        "null",
        "crash",
        "broken",
        "panic",
        "stack trace",
        "regression",
    ]);
    let refac = has(&[
        "refactor",
        "rename",
        "cleanup",
        "clean up",
        "tidy",
        "restructure",
        "simplify",
        "technical debt",
    ]);
    let review = has(&[
        "review",
        "audit",
        "vulnerability",
        "security",
        "pr ",
        "pull request",
        "injection",
        "exploit",
        "xss",
        "unsafe",
    ]);
    let arch = has(&[
        "architecture",
        "design",
        "system design",
        "tradeoff",
        "scalability",
        "distributed",
        "split",
        "decompose",
        "modular",
        "boundaries",
        "microservice",
    ]);
    let research = has(&[
        "research",
        "investigate",
        "compare",
        "evidence",
        "sources",
        "latest",
    ]);
    let writing = has(&[
        "write",
        "rewrite",
        "draft",
        "memo",
        "documentation",
        "readme",
        "proposal",
    ]);
    let planning = has(&["plan", "roadmap", "strategy", "break down", "milestone"]);
    let data = has(&[
        "analyze data",
        "dataset",
        "csv",
        "statistics",
        "forecast",
        "plot",
        "chart",
        "histogram",
        "correlation",
    ]);
    // "Build a shed" is physical planning, not software generation.
    let physical = has(&[
        "shed",
        "deck",
        "fence",
        "garage",
        "greenhouse",
        "house",
        "treehouse",
        "patio",
        "cabinet",
        "shelf",
    ]);
    let software = has(&[
        "code", "api", "function", "software", "app", "test", "module",
    ]);
    if physical > 0.0 && software == 0.0 {
        code = 0.0;
    }
    let planning = if physical > 0.0 && software == 0.0 && has(&["build", "construct"]) > 0.0 {
        planning + 2.0
    } else {
        planning
    };
    let scores = [
        code,
        debug * 1.3,
        refac * 1.2,
        review * 1.2,
        arch * 1.3,
        research,
        writing,
        planning,
        data,
    ];
    let (idx, best) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(&a.0)))
        .unwrap();
    let task = if *best == 0.0 {
        TaskType::General
    } else {
        match idx {
            0 => TaskType::CodeGeneration,
            1 => TaskType::Debugging,
            2 => TaskType::Refactoring,
            3 => TaskType::CodeReview,
            4 => TaskType::Architecture,
            5 => TaskType::Research,
            6 => TaskType::Writing,
            7 => TaskType::Planning,
            _ => TaskType::DataAnalysis,
        }
    };
    let r = requirements(task);
    (
        task,
        r,
        format!("local heuristic classified prompt as {task}"),
    )
}

/// Capability weights a task type asks of a model; shared by every classifier.
pub fn requirements(task: TaskType) -> Requirements {
    match task {
        TaskType::CodeGeneration => Requirements {
            coding: 0.95,
            reasoning: 0.72,
            long_context: 0.55,
            writing: 0.20,
            tool_use: 0.90,
        },
        TaskType::Debugging => Requirements {
            coding: 0.95,
            reasoning: 0.92,
            long_context: 0.70,
            writing: 0.10,
            tool_use: 0.95,
        },
        TaskType::Refactoring => Requirements {
            coding: 0.92,
            reasoning: 0.82,
            long_context: 0.78,
            writing: 0.15,
            tool_use: 0.92,
        },
        TaskType::CodeReview => Requirements {
            coding: 0.88,
            reasoning: 0.95,
            long_context: 0.82,
            writing: 0.25,
            tool_use: 0.72,
        },
        TaskType::Architecture => Requirements {
            coding: 0.55,
            reasoning: 1.0,
            long_context: 0.86,
            writing: 0.72,
            tool_use: 0.35,
        },
        TaskType::Research => Requirements {
            coding: 0.18,
            reasoning: 0.88,
            long_context: 0.82,
            writing: 0.75,
            tool_use: 0.65,
        },
        TaskType::Writing => Requirements {
            coding: 0.05,
            reasoning: 0.62,
            long_context: 0.55,
            writing: 1.0,
            tool_use: 0.15,
        },
        TaskType::Planning => Requirements {
            coding: 0.35,
            reasoning: 0.95,
            long_context: 0.70,
            writing: 0.70,
            tool_use: 0.40,
        },
        TaskType::DataAnalysis => Requirements {
            coding: 0.65,
            reasoning: 0.92,
            long_context: 0.72,
            writing: 0.40,
            tool_use: 0.78,
        },
        TaskType::General => Requirements {
            coding: 0.35,
            reasoning: 0.72,
            long_context: 0.50,
            writing: 0.55,
            tool_use: 0.35,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writing_tests_is_coding() {
        assert!(matches!(
            classify("write tests for this module").0,
            TaskType::CodeGeneration
        ));
    }
    #[test]
    fn detects_debugging() {
        let (t, _, _) = classify("debug this failing race condition");
        assert!(matches!(t, TaskType::Debugging));
    }
    #[test]
    fn detects_architecture() {
        let (t, _, _) = classify("design the architecture and scalability tradeoffs");
        assert!(matches!(t, TaskType::Architecture));
    }
}
