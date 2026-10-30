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
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// A participant that has not finished by now is abandoned.
const TURN_DEADLINE: Duration = Duration::from_secs(300);

/// Runtimes that can genuinely expose no tools.
///
/// Codex is absent because it is *unverified*, not because it is impossible.
/// Codex found that 0.154.0 accepts `--disable shell_tool`, which disproves the
/// earlier reasoning from `exec --help` alone. It stays excluded until patch,
/// MCP, apps and delegated tool surfaces are all accounted for.
pub fn supports(runtime: Runtime) -> bool {
    matches!(runtime, Runtime::Claude | Runtime::Ollama)
}

/// Launch arguments for a participant, with the tool surface emptied, and the
/// `[knowledge] index` block as a system prompt when there is one.
pub fn provider_args(decision: &Decision, knowledge: Option<&str>) -> Result<Vec<String>> {
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
            "--setting-sources",
            "",
            "--settings",
            r#"{"disableAllHooks":true}"#,
            "--disable-slash-commands",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([decision.model.clone()])
        .chain(
            knowledge
                .into_iter()
                .flat_map(|k| ["--append-system-prompt".to_string(), k.to_string()]),
        )
        .collect()),
        "codex" => bail!(
            "Codex CLI cannot join a debate: it has no tool-less mode, and its \
             read-only sandbox permits deletion. Choose another provider."
        ),
        "gemini" | "agy" => {
            bail!("Google's CLI is not yet verified as tool-less and cannot join a debate.")
        }
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
        let args = provider_args(decision, self.cfg.knowledge.as_deref())?;
        let deadline = std::time::Instant::now() + TURN_DEADLINE;
        let command = &self.cfg.inner.claude.command;
        let mut command_line = Command::new(command);
        let neutral = tempfile::tempdir().context("creating debate working directory")?;
        command_line
            .args(&args)
            .current_dir(neutral.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::executor::process::OwnedChild::spawn(&mut command_line)
            .with_context(|| format!("launching {command} for debate"))?;

        // The prompt goes down stdin, not the command line: a debate prompt
        // carries the shared transcript and would exceed the Windows
        // command-line limit.
        let stdin = child.child.stdin.take().context("provider stdin")?;
        let prompt = match self.cfg.knowledge {
            Some(_) => crate::knowledge::neutralise(prompt).into_owned(),
            None => prompt.to_owned(),
        };
        let (written, written_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            let result = std::io::Write::write_all(&mut stdin, prompt.as_bytes());
            drop(stdin);
            let _ = written.send(result);
        });

        // Both pipes must be drained while the child runs. Waiting for exit
        // first and reading afterwards deadlocks as soon as either pipe fills,
        // because the child then blocks on write and can never exit.
        let mut out = child.child.stdout.take().context("provider stdout")?;
        let mut err = child.child.stderr.take().context("provider stderr")?;
        let (stdout_tx, stdout_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = out.read_to_end(&mut buffer);
            let _ = stdout_tx.send(buffer);
        });
        let (stderr_tx, stderr_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = err.read_to_end(&mut buffer);
            let _ = stderr_tx.send(buffer);
        });

        let status = loop {
            if let Some(status) = child.child.try_wait()? {
                break Some(status);
            }
            if cancel.is_cancelled() || std::time::Instant::now() >= deadline {
                child.stop();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        if status.is_some() {
            let write_result = written_rx
                .recv_timeout(Duration::from_secs(2))
                .context("provider stdin writer did not finish")?;
            write_result.context("writing debate prompt")?;
        }
        // Close the owned process scope before collecting output. This also
        // closes descendants' inherited pipes after a normal provider exit.
        child.stop();

        let stdout = stdout_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default();
        let stderr = stderr_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default();
        let Some(status) = status else {
            if cancel.is_cancelled() {
                bail!("Debate cancelled; no completed answer was saved.");
            }
            bail!(
                "debate participant {} exceeded the {}s turn deadline",
                decision.model,
                TURN_DEADLINE.as_secs()
            );
        };
        if !status.success() {
            bail!(
                "debate participant {} failed: {}",
                decision.model,
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        final_result(&String::from_utf8_lossy(&stdout))
    }
}

/// Turn a stream-json transcript into a completed answer, failing closed.
///
/// Every check here previously failed open: a transcript with no init event, a
/// `tools` field of the wrong type, or a result missing `is_error` was all
/// accepted. Absence of evidence is not evidence of an empty tool surface.
fn final_result(stdout: &str) -> Result<String> {
    let mut verified = false;
    let mut answer = None;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let event: serde_json::Value = serde_json::from_str(line)
            .context("provider emitted a line that is not valid stream-json")?;

        if event["type"] == "system" && event["subtype"] == "init" {
            let tools = event["tools"]
                .as_array()
                .context("init event has no tools array; cannot verify the tool surface")?;
            let mcp = event["mcp_servers"]
                .as_array()
                .context("init event has no mcp_servers array; cannot verify the tool surface")?;
            if !tools.is_empty() {
                bail!("debate participant exposed tools: {tools:?}");
            }
            if !mcp.is_empty() {
                bail!("debate participant exposed MCP servers: {mcp:?}");
            }
            verified = true;
        }
        // A tool call cannot happen with an empty inventory; if one appears,
        // the inventory was not the whole story.
        if event["type"] == "assistant" {
            if let Some(content) = event["message"]["content"].as_array() {
                if content.iter().any(|c| c["type"] == "tool_use") {
                    bail!("debate participant attempted a tool call");
                }
            }
        }
        if event["type"] == "result" {
            match event["is_error"].as_bool() {
                Some(false) => {}
                Some(true) => bail!("debate participant reported an error result"),
                None => bail!("result event has no is_error field; completion is unverifiable"),
            }
            answer = event["result"].as_str().map(str::to_string);
        }
    }
    if !verified {
        bail!("provider emitted no init event; the tool surface was never verified");
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
        let args = provider_args(&decision("claude", "sonnet"), None).unwrap();
        let tools = args.iter().position(|a| a == "--tools").expect("--tools");
        assert_eq!(args[tools + 1], "", "the tool set must be empty");
        assert!(args.contains(&"--strict-mcp-config".to_string()));
    }

    #[test]
    fn no_bypass_or_mutating_flag_is_ever_passed() {
        let args = provider_args(&decision("claude", "sonnet"), None).unwrap();
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
    fn a_claude_participant_gets_the_knowledge_index() {
        let args = provider_args(&decision("claude", "sonnet"), Some("<block>")).unwrap();
        let at = args.iter().position(|a| a == "--append-system-prompt");
        assert_eq!(args[at.expect("--append-system-prompt") + 1], "<block>");
        let tools = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
    }

    #[test]
    fn codex_is_refused_with_the_reason() {
        let error = provider_args(&decision("codex", "gpt-5.6-sol"), None).unwrap_err();
        assert!(format!("{error}").contains("no tool-less mode"));
    }

    #[test]
    fn a_participant_that_exposes_tools_is_rejected() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","tools":["Bash"],"mcp_servers":[]}"#,
            "\n",
            r#"{"type":"result","is_error":false,"result":"done"}"#,
        );
        let error = final_result(stream).unwrap_err();
        assert!(format!("{error}").contains("exposed tools"));
    }

    #[test]
    fn the_completed_answer_is_taken_from_the_result_event() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[]}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text"}]}}"#,
            "\n",
            r#"{"type":"result","is_error":false,"result":"the answer"}"#,
        );
        assert_eq!(final_result(stream).unwrap(), "the answer");
    }

    const INIT_OK: &str = r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[]}"#;

    #[test]
    fn a_transcript_with_no_init_event_is_rejected() {
        // Absence of evidence is not evidence of an empty tool surface.
        let stream = r#"{"type":"result","is_error":false,"result":"answer"}"#;
        let error = final_result(stream).unwrap_err();
        assert!(format!("{error}").contains("never verified"));
    }

    #[test]
    fn an_init_event_without_typed_arrays_is_rejected() {
        let stream = format!(
            "{}
{}",
            r#"{"type":"system","subtype":"init","tools":"none","mcp_servers":[]}"#,
            r#"{"type":"result","is_error":false,"result":"a"}"#
        );
        assert!(final_result(&stream).is_err());
    }

    #[test]
    fn exposed_mcp_servers_are_rejected_like_tools() {
        let stream = format!(
            "{}
{}",
            r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":["fs"]}"#,
            r#"{"type":"result","is_error":false,"result":"a"}"#
        );
        let error = final_result(&stream).unwrap_err();
        assert!(format!("{error}").contains("MCP servers"));
    }

    #[test]
    fn a_result_without_is_error_is_not_treated_as_success() {
        let stream = format!(
            "{INIT_OK}
{}",
            r#"{"type":"result","result":"a"}"#
        );
        let error = final_result(&stream).unwrap_err();
        assert!(format!("{error}").contains("unverifiable"));
    }

    #[test]
    fn an_attempted_tool_call_is_rejected() {
        let stream = format!(
            "{INIT_OK}
{}
{}",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"result","is_error":false,"result":"a"}"#
        );
        let error = final_result(&stream).unwrap_err();
        assert!(format!("{error}").contains("attempted a tool call"));
    }

    #[test]
    fn only_tool_less_runtimes_may_participate() {
        assert!(supports(Runtime::Claude));
        assert!(supports(Runtime::Ollama));
        assert!(!supports(Runtime::Codex));
        assert!(!supports(Runtime::Gemini));
        assert!(!supports(Runtime::Agy));
    }
}
