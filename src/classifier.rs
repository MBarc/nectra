use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
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
    let p = format!(
        " {} ",
        prompt
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let has = |words: &[&str]| {
        words
            .iter()
            .filter(|w| p.contains(&format!(" {} ", w.trim())))
            .count() as f32
    };
    let mut code = has(&[
        "implement",
        "code",
        "function",
        "class ",
        "api",
        "test",
        "tests",
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
    ]);
    let refac = has(&[
        "refactor",
        "cleanup",
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
    ]);
    let arch = has(&[
        "architecture",
        "design",
        "system design",
        "tradeoff",
        "scalability",
        "distributed",
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
        "regression",
        "forecast",
    ]);
    // "Build a shed" is physical planning, not software generation.
    let physical = has(&["shed", "deck", "fence", "garage", "greenhouse", "house"]);
    let software = has(&[
        "code", "api", "function", "software", "app", "tests", "module",
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
    let r = match task {
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
    };
    (
        task,
        r,
        format!("local heuristic classified prompt as {task}"),
    )
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
