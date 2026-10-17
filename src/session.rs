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
/// Transcript budget alongside a handoff note, which already summarises the work.
pub const HANDOFF_BYTES: usize = 16 * 1024;

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
    Header {
        version: u32,
        cwd: PathBuf,
    },
    User {
        text: String,
        model: String,
        /// `/again`: this turn replaces the last answer, which stays in the log
        /// but leaves the context. One record, so a crash can't split the two.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        replaces: bool,
        /// Why the router chose the model, as shown above the answer; for `nectra usage`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        why: String,
        /// The model took over with the previous model's handoff note.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        handoff: bool,
        /// Sent with `/again`, whether or not it replaces an answer.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        again: bool,
    },
    Assistant {
        text: String,
        /// The provider's own session id, so `nectra usage` can read its token log.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_session: Option<String>,
    },
    Failed {
        error: String,
        /// A failed attempt still spent tokens; this lets `nectra usage` find them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_session: Option<String>,
    },
    Clear,
}
/// How a turn was routed, kept in its log record for `nectra usage`.
#[derive(Debug, Default)]
pub struct Route {
    /// Why the router chose the model, as shown above the answer.
    pub why: String,
    /// The model took over with the previous model's handoff note.
    pub handoff: bool,
    /// Sent with `/again`.
    pub again: bool,
}
pub struct Session {
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub exchanges: Vec<Exchange>,
    pub recovered_tail: bool,
    pub interrupted: Option<String>,
    /// The message of a last turn that failed, which `/again` can resend.
    pub last_failed: Option<String>,
    /// (model, provider session id) behind the last saved answer, so a resumed
    /// chat continues that agent's own session instead of replaying the log.
    pub provider_session: Option<(String, String)>,
    /// Every (model, provider session id) this chat has used, for its running cost.
    pub provider_sessions: Vec<(String, String)>,
    file: File,
    pending: Option<(String, String)>,
}
impl Session {
    pub fn create(path: &Path, cwd: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        // Transcripts hold prompts and answers: private to the user from the start.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
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
            last_failed: None,
            provider_session: None,
            provider_sessions: vec![],
            file,
            pending: None,
        };
        session.append(&Event::Header {
            version: 1,
            cwd: session.cwd.clone(),
        })?;
        Ok(session)
    }
    fn default_dir() -> Result<PathBuf> {
        Ok(dirs::data_local_dir()
            .context("no local data directory; use chat --session PATH")?
            .join("nectra/sessions"))
    }
    /// The newest default session started in `cwd`, if any.
    pub fn latest_default(cwd: &Path) -> Result<Option<PathBuf>> {
        Self::latest_in(&Self::default_dir()?, cwd)
    }
    /// Names start with a nanosecond timestamp, so the largest name is the newest.
    pub fn latest_in(dir: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
        let cwd = fs::canonicalize(cwd)?;
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(None);
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| Some(e.ok()?.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        paths.sort_unstable_by(|a, b| b.cmp(a));
        Ok(paths.into_iter().find(|path| {
            let Ok(file) = File::open(path) else {
                return false;
            };
            let mut header = String::new();
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut header);
            matches!(
                serde_json::from_str(&header),
                Ok(Event::Header { cwd: saved, .. })
                    if fs::canonicalize(&saved).is_ok_and(|saved| saved == cwd)
            )
        }))
    }
    pub fn create_default(cwd: &Path) -> Result<Self> {
        let dir = Self::default_dir()?;
        fs::create_dir_all(&dir).context("creating sessions directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
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
            last_failed: None,
            provider_session: None,
            provider_sessions: vec![],
            pending: None,
        };
        for line in lines {
            match serde_json::from_str::<Event>(line).context("invalid complete session record")? {
                Event::User {
                    text,
                    model,
                    replaces,
                    ..
                } => {
                    if session.pending.is_some() {
                        bail!("session contains overlapping turns");
                    }
                    if replaces {
                        session.provider_session = None;
                        session
                            .exchanges
                            .pop()
                            .context("replacing turn without an answer")?;
                    }
                    session.pending = Some((text, model));
                }
                Event::Assistant {
                    text,
                    provider_session,
                } => {
                    let (user, model) = session
                        .pending
                        .take()
                        .context("assistant record without a user turn")?;
                    session.used(&model, provider_session.as_deref());
                    session.provider_session = provider_session.map(|id| (model.clone(), id));
                    session.last_failed = None;
                    session.exchanges.push(Exchange {
                        user,
                        assistant: text,
                        model,
                    });
                }
                Event::Failed {
                    provider_session, ..
                } => {
                    session.provider_session = None;
                    let (text, model) = session
                        .pending
                        .take()
                        .context("failure without a user turn")?;
                    session.used(&model, provider_session.as_deref());
                    session.last_failed = Some(text);
                }
                Event::Clear => {
                    session.provider_session = None;
                    session.exchanges.clear();
                    session.pending = None;
                    session.last_failed = None;
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
            // The interrupted turn may have resumed that session and left unsaved
            // work in it; the next turn starts fresh.
            session.provider_session = None;
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
        self.begin_routed(text, model, false, &Route::default())
    }
    /// `begin` for a turn that replaces the last answer (`/again`).
    pub fn begin_replacing(&mut self, text: &str, model: &str) -> Result<()> {
        let again = Route {
            again: true,
            ..Route::default()
        };
        self.begin_routed(text, model, true, &again)
    }
    /// `begin`, recording how the turn was routed for `nectra usage`.
    pub fn begin_routed(
        &mut self,
        text: &str,
        model: &str,
        replaces: bool,
        route: &Route,
    ) -> Result<()> {
        if self.pending.is_some() {
            bail!("previous turn is still pending");
        }
        if replaces && self.exchanges.is_empty() {
            bail!("no answer to replace");
        }
        self.append(&Event::User {
            text: text.into(),
            model: model.into(),
            replaces,
            why: route.why.clone(),
            handoff: route.handoff,
            again: route.again,
        })?;
        if replaces {
            self.exchanges.pop();
            self.provider_session = None;
        }
        self.pending = Some((text.into(), model.into()));
        Ok(())
    }
    pub fn complete(&mut self, text: &str) -> Result<()> {
        self.complete_in(text, None)
    }
    /// `complete`, noting which provider session produced the answer.
    pub fn complete_in(&mut self, text: &str, provider_session: Option<&str>) -> Result<()> {
        let (user, model) = self.pending.as_ref().context("no pending turn")?.clone();
        self.append(&Event::Assistant {
            text: text.into(),
            provider_session: provider_session.map(String::from),
        })?;
        self.used(&model, provider_session);
        self.pending = None;
        self.last_failed = None;
        self.exchanges.push(Exchange {
            user,
            assistant: text.into(),
            model,
        });
        Ok(())
    }
    pub fn fail(&mut self, error: &str) -> Result<()> {
        self.fail_in(error, None)
    }
    /// `fail`, noting the provider session the failed attempt ran in.
    pub fn fail_in(&mut self, error: &str, provider_session: Option<&str>) -> Result<()> {
        if self.pending.is_none() {
            bail!("no pending turn");
        }
        self.append(&Event::Failed {
            error: error.into(),
            provider_session: provider_session.map(String::from),
        })?;
        let (text, model) = self.pending.take().context("no pending turn")?;
        self.used(&model, provider_session);
        self.last_failed = Some(text);
        Ok(())
    }
    fn used(&mut self, model: &str, provider_session: Option<&str>) {
        if let Some(id) = provider_session {
            let entry = (model.to_string(), id.to_string());
            if !self.provider_sessions.contains(&entry) {
                self.provider_sessions.push(entry);
            }
        }
    }
    pub fn clear(&mut self) -> Result<()> {
        self.append(&Event::Clear)?;
        self.exchanges.clear();
        self.pending = None;
        self.last_failed = None;
        Ok(())
    }
    /// Asked of the outgoing model's resumed session when another model takes over mid-task.
    pub const HANDOFF_REQUEST: &'static str = "Another engineer is taking over this task from you right now. Write them a handoff note of at most 200 words: the goal, files you read or changed, decisions made, the current state, and the next step. Plain text, no preamble. Do not change any files.";
    /// The next message for a resumed provider session, which already holds the history.
    pub fn followup(current: &str) -> Result<String> {
        Ok(format!(
            "Answer current_user_message, the next message in this conversation.\n{}",
            serde_json::to_string(&serde_json::json!({ "current_user_message": current }))?
        ))
    }
    /// Keep the opening exchange when possible and a contiguous suffix of recent exchanges.
    /// Budget is UTF-8 serialized bytes, not a claim about model token limits.
    /// Convert a model's token capacity with [`budget_bytes`] before calling this.
    pub fn prompt(&self, current: &str, budget: usize) -> Result<(String, usize)> {
        self.prompt_with(current, budget, None)
    }
    /// `prompt` for a model taking over mid-task: the previous model's handoff
    /// note carries what the transcript lacks (files touched, state), so fewer
    /// recent exchanges are needed.
    pub fn handoff(&self, current: &str, note: &str) -> Result<(String, usize)> {
        self.prompt_with(current, HANDOFF_BYTES, Some(note))
    }
    fn prompt_with(
        &self,
        current: &str,
        budget: usize,
        note: Option<&str>,
    ) -> Result<(String, usize)> {
        let encode = |history: &[&Exchange]| -> Result<String> {
            let mut body =
                serde_json::json!({"conversation": history, "current_user_message": current});
            if let Some(note) = note {
                body["handoff_note"] = note.into();
            }
            Ok(format!("Continue the conversation below and answer current_user_message directly. Use the transcript silently as context; do not acknowledge receiving context, announce a handoff, or introduce the routing system. Discuss the system or model identity only when relevant to the user's request. Do not repeat completed work. Previous replies may describe tool actions; inspect current files when needed.{}\n{}", if note.is_some() { " handoff_note is the previous model's summary of the work so far; trust it for state the transcript omits." } else { "" }, serde_json::to_string(&body)?))
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
