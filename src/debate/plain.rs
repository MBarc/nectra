//! Tool-less debate backend.
//!
//! Debate participants get no tools at all, so there is nothing to sandbox: a
//! model that cannot call a write tool cannot write. Nectra supplies the
//! evidence in the prompt instead, being a trusted local program rather than a
//! model. That also keeps the debate fair -- participants that each explored the
//! repository separately would be disagreeing about different inputs.
//!
//! Verified against Claude Code 2.1.273: `--tools ""` reports an empty tool
//! inventory and an empty MCP server list, and the call still completes.

use crate::{config::Config, executor::CancellationToken, models::Runtime, router::Decision};
use anyhow::{bail, Context, Result};
use std::{
    process::{Command, Stdio},
    time::Duration,
};

/// Runtimes that can genuinely expose no tools.
///
/// Codex CLI is absent deliberately. Its modes are read-only, workspace-write
/// and danger-full-access, every one of which describes executing
/// model-generated shell commands, and its read-only sandbox was verified to
/// permit deletion. There is no tool-less mode to ask for.
pub fn supports(runtime: Runtime) -> bool {
    matches!(runtime, Runtime::Claude | Runtime::Ollama)
}

/// Launch arguments for a participant, with the tool surface emptied.
pub fn provider_args(decision: &Decision, prompt: &str) -> Result<Vec<String>> {
    if decision.model.is_empty() || decision.model.starts_with('-') {
        bail!("invalid debate model");
    }
    match decision.runtime.as_str() {
        "claude" => Ok([
            "-p",
            "--restricted",
            "--safe-mode",
            // The whole design rests on this being empty.
            "--tools",
            "",
            "--strict-mcp-config",
            "--permission-prompts",
            "none",
            "--permission-mode",
            "manual",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([decision.model.clone(), prompt.to_string()])
        .collect()),
        "codex" => bail!(
            "Codex CLI cannot join a debate: it has no tool-less mode, and its \
             read-only sandbox permits deletion. Choose another provider."
        ),
        "gemini" => bail!("Gemini is not yet verified as tool-less and cannot join a debate."),
        other => bail!("unsupported debate runtime {other}"),
    }
}

pub struct PlainBackend<'a> {
    cfg: &'a Config,
}

impl<'a> PlainBackend<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        Self { cfg }
    }

    fn run_cli(
        &self,
        decision: &Decision,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let args = provider_args(decision, prompt)?;
        let command = &self.cfg.inner.claude.command;
        let mut child = Command::new(command)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("launching {command} for debate"))?;

        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Debate cancelled; no completed answer was saved.");
            }
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "debate participant {} failed: {}",
                decision.model,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        final_result(&String::from_utf8_lossy(&out.stdout))
    }
}

/// Pull the completed answer out of a stream-json transcript.
fn final_result(stdout: &str) -> Result<String> {
    let mut answer = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        // A participant that somehow acquired a tool must not pass silently.
        if event["type"] == "system" && event["subtype"] == "init" {
            if let Some(tools) = event["tools"].as_array() {
                if !tools.is_empty() {
                    bail!("debate participant exposed tools: {tools:?}");
                }
            }
        }
        if event["type"] == "result" {
            if event["is_error"].as_bool().unwrap_or(false) {
                bail!("debate participant reported an error result");
            }
            answer = event["result"].as_str().map(str::to_string);
        }
    }
    answer.context("debate participant produced no completed answer")
}

impl crate::debate::runner::DebateBackend for PlainBackend<'_> {
    fn respond(
        &self,
        decision: &Decision,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<String> {
        if decision.runtime == "ollama" {
            // The Ollama adapter executes no tools by construction.
            let mut sink = std::io::sink();
            return crate::ollama::conversation(&decision.model, prompt, self.cfg, &mut sink);
        }
        self.run_cli(decision, prompt, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(runtime: &str, model: &str) -> Decision {
        Decision {
            task: "general".into(),
            provider: "anthropic".into(),
            runtime: runtime.into(),
            model: model.into(),
            effort: String::new(),
            mode: crate::cli::Mode::Balanced,
            score: 1.0,
            score_margin: None,
            ambiguous: false,
            overridden: false,
            reason: String::new(),
            candidates: vec![],
        }
    }

    #[test]
    fn a_claude_participant_is_launched_with_an_empty_tool_set() {
        let args = provider_args(&decision("claude", "sonnet"), "question").unwrap();
        let tools = args.iter().position(|a| a == "--tools").expect("--tools");
        assert_eq!(args[tools + 1], "", "the tool set must be empty");
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert_eq!(args.last().unwrap(), "question");
    }

    #[test]
    fn no_bypass_or_mutating_flag_is_ever_passed() {
        let args = provider_args(&decision("claude", "sonnet"), "q").unwrap();
        for forbidden in [
            "--dangerously-skip-permissions",
            "--add-dir",
            "acceptEdits",
            "bypassPermissions",
            "Write",
            "Edit",
            "Bash",
        ] {
            assert!(
                !args.iter().any(|a| a.contains(forbidden)),
                "{forbidden} must never reach a debate participant"
            );
        }
    }

    #[test]
    fn codex_is_refused_with_the_reason() {
        let error = provider_args(&decision("codex", "gpt-5.6-sol"), "q").unwrap_err();
        assert!(format!("{error}").contains("no tool-less mode"));
    }

    #[test]
    fn a_participant_that_exposes_tools_is_rejected() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","tools":["Bash"]}"#,
            "\n",
            r#"{"type":"result","is_error":false,"result":"done"}"#,
        );
        let error = final_result(stream).unwrap_err();
        assert!(format!("{error}").contains("exposed tools"));
    }

    #[test]
    fn the_completed_answer_is_taken_from_the_result_event() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","tools":[]}"#,
            "\n",
            r#"{"type":"assistant","message":"ignored"}"#,
            "\n",
            r#"{"type":"result","is_error":false,"result":"the answer"}"#,
        );
        assert_eq!(final_result(stream).unwrap(), "the answer");
    }

    #[test]
    fn only_tool_less_runtimes_may_participate() {
        assert!(supports(Runtime::Claude));
        assert!(supports(Runtime::Ollama));
        assert!(!supports(Runtime::Codex));
        assert!(!supports(Runtime::Gemini));
    }
}
