//! Ephemeral terminal activity, never part of the assistant answer or session log.
use serde_json::Value;
use std::{
    cell::RefCell,
    io::{self, IsTerminal, Write},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Waiting,
    Thinking,
    Tools,
    Replying,
    Retrying,
    Finishing,
}
impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Waiting => "Waiting for model",
            Self::Thinking => "Thinking",
            Self::Tools => "Using tools",
            Self::Replying => "Receiving reply",
            Self::Retrying => "Retrying connection",
            Self::Finishing => "Finishing",
        }
    }
}
struct State {
    phase: Phase,
    model: String,
    start: Instant,
    last_output: Option<Instant>,
    boundary: bool,
    visible: bool,
    stop: bool,
    tick: usize,
}
impl State {
    fn frame(&mut self, now: Instant, color: &str) -> Option<String> {
        if self
            .last_output
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(700))
        {
            return None;
        }
        // Providers can pause after text without a newline or a progress event.
        // Move below that text before drawing, so it never suppresses activity
        // indefinitely and is never erased by the spinner's line clear.
        let prefix = if self.boundary { "" } else { "\n" };
        self.boundary = true;
        let spinner = ["|", "/", "-", "\\"][self.tick % 4];
        self.tick += 1;
        self.visible = true;
        let text = format!(
            "  {spinner} {} - {} - {}s",
            self.model,
            self.phase.label(),
            now.duration_since(self.start).as_secs()
        );
        Some(if color.is_empty() {
            format!("{prefix}\r\x1b[2K{text}")
        } else {
            format!("{prefix}\r\x1b[2K\x1b[{color}m{text}\x1b[0m")
        })
    }
}
#[derive(Clone)]
pub struct Handle(Arc<(Mutex<State>, Condvar)>);
thread_local! { static CURRENT: RefCell<Option<Handle>> = const { RefCell::new(None) }; }
pub fn current() -> Option<Handle> {
    CURRENT.with(|c| c.borrow().clone())
}
pub fn set_phase(phase: Phase) {
    if let Some(handle) = current() {
        let (lock, wake) = &*handle.0;
        if let Ok(mut state) = lock.lock() {
            // Give a new tool/thinking phase its own line after partial prose.
            if !state.boundary
                && matches!(phase, Phase::Tools | Phase::Thinking)
                && io::stdout().is_terminal()
                && io::stderr().is_terminal()
            {
                let _ = writeln!(io::stderr());
                state.boundary = true;
                state.last_output = None;
            }
            state.phase = phase;
            wake.notify_one();
        }
    }
}

pub struct Activity {
    handle: Handle,
    worker: Option<thread::JoinHandle<()>>,
    previous: Option<Handle>,
}
impl Activity {
    pub fn start(model: &str) -> Self {
        let handle = Handle(Arc::new((
            Mutex::new(State {
                phase: Phase::Starting,
                model: model.chars().filter(|c| !c.is_control()).take(32).collect(),
                start: Instant::now(),
                last_output: None,
                boundary: true,
                visible: false,
                stop: false,
                tick: 0,
            }),
            Condvar::new(),
        )));
        let previous = CURRENT.with(|c| c.replace(Some(handle.clone())));
        let enabled = io::stdout().is_terminal()
            && io::stderr().is_terminal()
            && std::env::var("TERM").ok().as_deref() != Some("dumb");
        let worker = enabled.then(|| {
            let handle = handle.clone();
            thread::spawn(move || {
                let mut output =
                    anstream::AutoStream::new(io::stderr(), anstream::ColorChoice::Always);
                let (lock, wake) = &*handle.0;
                let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
                while !state.stop {
                    if let Some(frame) =
                        state.frame(Instant::now(), crate::terminal::activity_color())
                    {
                        if output
                            .write_all(frame.as_bytes())
                            .and_then(|_| output.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                    state = wake
                        .wait_timeout(state, Duration::from_millis(120))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                if state.visible {
                    let _ = output.write_all(b"\r\x1b[2K");
                    let _ = output.flush();
                    state.visible = false;
                }
            })
        });
        Self {
            handle,
            worker,
            previous,
        }
    }
    pub fn writer<'a>(&self, output: &'a mut dyn Write) -> ProgressWriter<'a> {
        ProgressWriter {
            output,
            handle: self.handle.clone(),
        }
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        let (lock, wake) = &*self.handle.0;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.stop = true;
            wake.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        CURRENT.with(|c| {
            c.replace(self.previous.take());
        });
    }
}
impl Handle {
    fn write(&self, output: &mut dyn Write, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0 .0.lock().unwrap_or_else(|e| e.into_inner());
        if state.visible {
            let mut stderr = io::stderr();
            stderr.write_all(b"\r\x1b[2K")?;
            stderr.flush()?;
            state.visible = false;
        }
        let count = output.write(bytes)?;
        if count > 0 {
            state.boundary = bytes[count - 1] == b'\n';
            state.last_output = Some(Instant::now());
        }
        Ok(count)
    }
}
pub struct ProgressWriter<'a> {
    output: &'a mut dyn Write,
    handle: Handle,
}
impl Write for ProgressWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.handle.write(self.output, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
pub fn diagnostic(handle: &Option<Handle>, bytes: &[u8]) -> io::Result<()> {
    let mut stderr = io::stderr();
    if let Some(handle) = handle {
        let mut writer = ProgressWriter {
            output: &mut stderr,
            handle: handle.clone(),
        };
        writer.write_all(bytes)?;
        writer.flush()
    } else {
        stderr.write_all(bytes)?;
        stderr.flush()
    }
}

pub fn reply(handle: &Option<Handle>, bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout();
    if let Some(handle) = handle {
        {
            let mut state = handle.0 .0.lock().unwrap_or_else(|e| e.into_inner());
            state.phase = Phase::Replying;
        }
        let mut writer = ProgressWriter {
            output: &mut stdout,
            handle: handle.clone(),
        };
        writer.write_all(bytes)?;
        writer.flush()
    } else {
        stdout.write_all(bytes)?;
        stdout.flush()
    }
}

/// Interpret event kinds only; never display raw reasoning or tool arguments.
pub fn event_phase(runtime: &str, event: &Value) -> Option<Phase> {
    let kind = event["type"].as_str().unwrap_or("");
    match runtime {
        "claude" => match kind {
            "system" => Some(Phase::Waiting),
            "assistant" => event["message"]["content"].as_array().map(|blocks| {
                if blocks.iter().any(|b| b["type"] == "tool_use") {
                    Phase::Tools
                } else if blocks.iter().any(|b| b["type"] == "thinking") {
                    Phase::Thinking
                } else {
                    Phase::Waiting
                }
            }),
            "user" => Some(Phase::Waiting),
            "tool_progress" => Some(Phase::Tools),
            "stream_event" => {
                let event = &event["event"];
                match event["delta"]["type"]
                    .as_str()
                    .or_else(|| event["content_block"]["type"].as_str())
                {
                    Some("thinking_delta" | "thinking" | "signature_delta") => {
                        Some(Phase::Thinking)
                    }
                    Some("tool_use" | "input_json_delta") => Some(Phase::Tools),
                    Some("text_delta" | "text") => Some(Phase::Replying),
                    _ => None,
                }
            }
            "result" => Some(Phase::Finishing),
            _ => None,
        },
        "codex" => match kind {
            "thread.started" | "turn.started" => Some(Phase::Waiting),
            "item.started" | "item.updated" | "item.completed" => {
                match event["item"]["type"].as_str() {
                    Some("reasoning") => Some(Phase::Thinking),
                    Some("agent_message") => Some(Phase::Replying),
                    Some("command_execution" | "file_change" | "mcp_tool_call" | "web_search") => {
                        Some(Phase::Tools)
                    }
                    _ => None,
                }
            }
            "error" => Some(Phase::Retrying),
            "turn.completed" => Some(Phase::Finishing),
            _ => None,
        },
        "gemini" => match kind {
            "init" => Some(Phase::Waiting),
            "tool_use" => Some(Phase::Tools),
            "tool_result" => Some(Phase::Waiting),
            "message" if event["role"] == "assistant" => Some(Phase::Replying),
            "result" => Some(Phase::Finishing),
            _ => None,
        },
        "ollama" => {
            if event["done"] == true {
                Some(Phase::Finishing)
            } else if event["message"]["thinking"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                Some(Phase::Thinking)
            } else if event["message"]["content"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                Some(Phase::Replying)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn lifecycle_restores_context_and_keeps_reply_bytes_clean() {
        assert!(current().is_none());
        let mut bytes = vec![];
        {
            let activity = Activity::start("Preview model");
            set_phase(Phase::Thinking);
            std::thread::sleep(Duration::from_millis(300));
            let mut writer = activity.writer(&mut bytes);
            writer.write_all(b"Answer only\n").unwrap();
            writer.flush().unwrap();
        }
        assert!(current().is_none());
        assert_eq!(bytes, b"Answer only\n");
    }
    #[test]
    fn recognizes_activity_without_copying_provider_text() {
        assert_eq!(
            event_phase(
                "claude",
                &json!({"type":"stream_event","event":{"delta":{"type":"thinking_delta","thinking":"private text"}}})
            ),
            Some(Phase::Thinking)
        );
        assert_eq!(
            event_phase(
                "codex",
                &json!({"type":"item.started","item":{"type":"command_execution","command":"secret"}})
            ),
            Some(Phase::Tools)
        );
        assert_eq!(
            event_phase("gemini", &json!({"type":"tool_use"})),
            Some(Phase::Tools)
        );
        assert_eq!(
            event_phase("ollama", &json!({"message":{"thinking":"private text"}})),
            Some(Phase::Thinking)
        );
    }
    #[test]
    fn clock_keeps_moving_without_events_and_preserves_partial_lines() {
        let start = Instant::now();
        let mut state = State {
            phase: Phase::Waiting,
            model: "model".into(),
            start,
            last_output: None,
            boundary: true,
            visible: false,
            stop: false,
            tick: 0,
        };
        let first = state.frame(start, "").unwrap();
        let later = state.frame(start + Duration::from_secs(4), "").unwrap();
        assert_ne!(first, later);
        assert!(later.contains("4s"));
        state.boundary = false;
        state.last_output = Some(start + Duration::from_secs(4));
        assert!(state
            .frame(start + Duration::from_millis(4500), "")
            .is_none());
        let resumed = state.frame(start + Duration::from_secs(5), "").unwrap();
        assert!(resumed.starts_with("\n\r\x1b[2K"));
        assert!(!state
            .frame(start + Duration::from_secs(6), "")
            .unwrap()
            .starts_with('\n'));
        state.boundary = true;
        state.last_output = Some(start + Duration::from_secs(5));
        assert!(state.frame(start + Duration::from_secs(5), "").is_none());
    }
}
