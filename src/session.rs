use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LOG: u64 = 64 * 1024 * 1024;
pub const CONTEXT_BYTES: usize = 128 * 1024;

/// Bytes held back so the provider's own guard is never the thing that fails.
///
/// Derived from the provider's own constants rather than restated, so the two
/// sides cannot drift: `ollama.rs` rejects above
/// `context_tokens - INPUT_RESERVE_BYTES`, and trimming has to stop below that
/// by at least the framing the transcript adds.
const OLLAMA_RESERVE_BYTES: usize =
    crate::ollama::INPUT_RESERVE_BYTES + crate::ollama::TRANSCRIPT_FRAMING_RESERVE_BYTES;

/// Conservative byte budget for a model advertising `context_tokens`.
///
/// A token is at least one byte, so spending the token count as a *byte*
/// ceiling is a deliberate safe under-estimate, documented in
/// `docs/local-providers.md`. It is not a unit confusion: exact tokenizer
/// counts are not available locally, and over-estimating would hand the
/// provider a request it refuses.
pub fn budget_bytes(context_tokens: usize) -> usize {
    context_tokens.saturating_sub(OLLAMA_RESERVE_BYTES)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exchange {
    pub user: String,
    pub assistant: String,
    pub model: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Header { version: u32, cwd: PathBuf },
    User { text: String, model: String },
    Assistant { text: String },
    Failed { error: String },
    Clear,
}
pub struct Session {
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub exchanges: Vec<Exchange>,
    pub recovered_tail: bool,
    pub interrupted: Option<String>,
    file: File,
    pending: Option<(String, String)>,
}
impl Session {
    pub fn create(path: &Path, cwd: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| {
                format!(
                    "creating new session {} (use --resume for an existing session)",
                    path.display()
                )
            })?;
        file.try_lock().context("locking session")?;
        let mut session = Self {
            path: fs::canonicalize(path)?,
            cwd: fs::canonicalize(cwd)?,
            exchanges: vec![],
            recovered_tail: false,
            interrupted: None,
            file,
            pending: None,
        };
        session.append(&Event::Header {
            version: 1,
            cwd: session.cwd.clone(),
        })?;
        Ok(session)
    }
    pub fn create_default(cwd: &Path) -> Result<Self> {
        let dir = dirs::data_local_dir()
            .context("no local data directory; use chat --session PATH")?
            .join("nectra/sessions");
        fs::create_dir_all(&dir).context("creating sessions directory")?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Self::create(
            &dir.join(format!("{stamp}-{}.jsonl", std::process::id())),
            cwd,
        )
    }
    pub fn resume(path: &Path, cwd: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .context("opening session")?;
        file.try_lock()
            .context("session is already open in another process")?;
        if file.metadata()?.len() > MAX_LOG {
            bail!("session exceeds the 64 MiB log limit");
        }
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        if end == 0 {
            bail!("session has no complete header");
        }
        let recovered_tail = end != bytes.len();
        let text = std::str::from_utf8(&bytes[..end]).context("session is not UTF-8")?;
        let mut lines = text.lines();
        let Event::Header {
            version: 1,
            cwd: saved_cwd,
        } = serde_json::from_str(lines.next().context("missing header")?)?
        else {
            bail!("unsupported session format");
        };
        let cwd = fs::canonicalize(cwd)?;
        if fs::canonicalize(&saved_cwd)? != cwd {
            bail!(
                "session belongs to {}; run Nectra from that directory",
                saved_cwd.display()
            );
        }
        let mut session = Self {
            path: fs::canonicalize(path)?,
            cwd,
            file,
            exchanges: vec![],
            recovered_tail,
            interrupted: None,
            pending: None,
        };
        for line in lines {
            match serde_json::from_str::<Event>(line).context("invalid complete session record")? {
                Event::User { text, model } => {
                    if session.pending.is_some() {
                        bail!("session contains overlapping turns");
                    }
                    session.pending = Some((text, model));
                }
                Event::Assistant { text } => {
                    let (user, model) = session
                        .pending
                        .take()
                        .context("assistant record without a user turn")?;
                    session.exchanges.push(Exchange {
                        user,
                        assistant: text,
                        model,
                    });
                }
                Event::Failed { .. } => {
                    session
                        .pending
                        .take()
                        .context("failure without a user turn")?;
                }
                Event::Clear => {
                    session.exchanges.clear();
                    session.pending = None;
                }
                Event::Header { .. } => bail!("unexpected session header"),
            }
        }
        // Only an incomplete final record is repairable; never discard complete corrupt records.
        if recovered_tail {
            session.file.set_len(end as u64)?;
            session.file.sync_data()?;
        }
        session.file.seek(SeekFrom::End(0))?;
        if let Some((text, _)) = &session.pending {
            session.interrupted = Some(text.clone());
            session.fail("interrupted before a complete answer was saved")?;
        }
        Ok(session)
    }
    fn append(&mut self, event: &Event) -> Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        if self.file.metadata()?.len() + line.len() as u64 > MAX_LOG {
            bail!("session log is full; start a new session");
        }
        self.file.write_all(&line).context("writing session")?;
        self.file.sync_data().context("syncing session to disk")?;
        Ok(())
    }
    pub fn begin(&mut self, text: &str, model: &str) -> Result<()> {
        if self.pending.is_some() {
            bail!("previous turn is still pending");
        }
        self.append(&Event::User {
            text: text.into(),
            model: model.into(),
        })?;
        self.pending = Some((text.into(), model.into()));
        Ok(())
    }
    pub fn complete(&mut self, text: &str) -> Result<()> {
        let (user, model) = self.pending.as_ref().context("no pending turn")?.clone();
        self.append(&Event::Assistant { text: text.into() })?;
        self.pending = None;
        self.exchanges.push(Exchange {
            user,
            assistant: text.into(),
            model,
        });
        Ok(())
    }
    pub fn fail(&mut self, error: &str) -> Result<()> {
        if self.pending.is_none() {
            bail!("no pending turn");
        }
        self.append(&Event::Failed {
            error: error.into(),
        })?;
        self.pending = None;
        Ok(())
    }
    pub fn clear(&mut self) -> Result<()> {
        self.append(&Event::Clear)?;
        self.exchanges.clear();
        self.pending = None;
        Ok(())
    }
    /// Keep the opening exchange when possible and a contiguous suffix of recent exchanges.
    /// Budget is UTF-8 serialized bytes, not a claim about model token limits.
    /// Convert a model's token capacity with [`budget_bytes`] before calling this.
    pub fn prompt(&self, current: &str, budget: usize) -> Result<(String, usize)> {
        let encode = |history: &[&Exchange]| -> Result<String> {
            Ok(format!("Continue the conversation below and answer current_user_message directly. Use the transcript silently as context; do not acknowledge receiving context, announce a handoff, or introduce the routing system. Discuss the system or model identity only when relevant to the user's request. Do not repeat completed work. Previous replies may describe tool actions; inspect current files when needed.\n{}", serde_json::to_string(&serde_json::json!({"conversation": history, "current_user_message": current}))?))
        };
        if encode(&[])?.len() > budget {
            bail!("message exceeds the context budget ({budget} bytes)");
        }
        let mut selected = vec![];
        let mut first_pinned = false;
        if let Some(first) = self.exchanges.first() {
            if encode(&[first])?.len() <= budget {
                selected.push(first);
                first_pinned = true;
            }
        }
        for (index, exchange) in self.exchanges.iter().enumerate().rev() {
            if index == 0 && first_pinned {
                break;
            }
            let position = usize::from(first_pinned);
            selected.insert(position, exchange);
            if encode(&selected)?.len() > budget {
                selected.remove(position);
                break;
            }
        }
        let omitted = self.exchanges.len() - selected.len();
        Ok((encode(&selected)?, omitted))
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    /// The provider's hard guard in `ollama.rs`: it bails above this size.
    fn provider_limit(context_tokens: usize) -> usize {
        context_tokens.saturating_sub(crate::ollama::INPUT_RESERVE_BYTES)
    }

    #[test]
    fn the_trim_budget_stays_under_the_provider_hard_limit() {
        // Trimming must stop before the provider refuses, otherwise chat
        // assembles a history that ollama.rs rejects outright.
        for tokens in [2048, 8192, 32768, 131_072] {
            assert!(
                budget_bytes(tokens) < provider_limit(tokens),
                "budget {} must stay under the provider limit {} for {tokens} tokens",
                budget_bytes(tokens),
                provider_limit(tokens)
            );
        }
    }

    #[test]
    fn a_larger_context_earns_a_larger_budget() {
        assert!(budget_bytes(8192) > budget_bytes(2048));
    }

    #[test]
    fn a_context_smaller_than_the_reserve_does_not_underflow() {
        assert_eq!(budget_bytes(256), 0);
    }
}
