//! A bounded terminal emulator. Child cursor/erase commands cannot erase Nectra's thread.
use anyhow::{bail, Context, Result};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::{Attribute, SetAttribute},
    terminal::{self, Clear, ClearType},
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    io::{self, Read, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(super) struct Outcome {
    pub diagnostic: String,
    pub success: bool,
    pub pending_input: bool,
}

thread_local! {
    static QUEUED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(super) fn take_queued() -> Vec<String> {
    QUEUED.with(|queue| std::mem::take(&mut *queue.borrow_mut()))
}

struct TerminalGuard {
    bottom: u16,
}
impl TerminalGuard {
    fn enter(bottom: u16) -> Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self { bottom };
        execute!(io::stdout(), EnableBracketedPaste, Hide)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            SetAttribute(Attribute::Reset),
            Show,
            MoveTo(0, self.bottom)
        );
        let _ = terminal::disable_raw_mode();
        let _ = writeln!(io::stdout());
    }
}

struct ChildGuard(
    Box<dyn portable_pty::Child + Send + Sync>,
    #[cfg(windows)] Option<super::windows_job::Job>,
);
impl ChildGuard {
    fn terminate(&mut self) {
        #[cfg(windows)]
        if let Some(job) = &self.1 {
            job.terminate();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn size() -> Result<(u16, u16)> {
    let (cols, rows) = terminal::size()?;
    if cols < 20 || rows < 8 {
        bail!(
            "enlarge the terminal to at least 20 columns and 8 rows, or use --provider-view stream"
        );
    }
    Ok((cols.saturating_sub(1), rows.saturating_sub(3).min(30)))
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn allocate(rows: u16) -> Result<u16> {
    let mut out = io::stdout();
    for _ in 0..rows + 2 {
        write!(out, "\r\n")?;
    }
    out.flush()?;
    let (_, bottom) = crossterm::cursor::position()?;
    Ok(bottom.saturating_sub(rows + 1))
}

pub(super) fn run(
    cmd: CommandBuilder,
    model: &str,
    mut completion: impl FnMut() -> Result<(usize, bool)>,
) -> Result<Outcome> {
    let (mut cols, mut rows) = size()?;
    let pair = native_pty_system().openpty(pty_size(cols, rows))?;
    let mut child = ChildGuard(
        pair.slave
            .spawn_command(cmd)
            .context("starting native provider terminal")?,
        #[cfg(windows)]
        None,
    );
    #[cfg(windows)]
    {
        child.1 = Some(super::windows_job::Job::attach(
            child.0.process_id().context("missing provider PID")?,
        )?);
    }
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    // Bounded queue applies backpressure instead of accumulating unbounded command output.
    let (tx, rx) = mpsc::sync_channel(32);
    let reader_thread = thread::spawn(move || {
        let mut bytes = [0; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ok(bytes[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });
    let mut display_rows = 2;
    let mut origin = allocate(display_rows)?;
    let mut guard = TerminalGuard::enter(origin + display_rows + 1)?;
    let started = Instant::now();
    let mut parser = vt100::Parser::new(rows, cols, 10_000);
    let mut responder = Queries::default();
    let mut previous: Option<Vec<Vec<vt100::Cell>>> = None;
    let mut last_render = Instant::now() - Duration::from_secs(1);
    let mut last_capture = last_render;
    let mut revision = 0;
    let mut ready = false;
    let mut pending_input = false;
    let mut scrollback = 0usize;
    let mut disconnected = false;
    let mut exited = None;
    let mut exit_time = None;
    let mut leave = false;
    let mut dirty = true;
    let mut confirm_cancel = false;
    let mut typed = String::new();
    let mut draft = String::new();
    let mut provider_input = false;
    let mut queued_count = 0usize;
    let result = (|| -> Result<Outcome> {
        loop {
            for _ in 0..32 {
                match rx.try_recv() {
                    Ok(Ok(bytes)) => {
                        #[cfg(debug_assertions)]
                        if let Some(path) = std::env::var_os("NECTRA_TERMINAL_TRACE") {
                            use std::fs::OpenOptions;
                            let mut log =
                                OpenOptions::new().create(true).append(true).open(path)?;
                            log.write_all(&bytes)?;
                        }
                        parser.process(&bytes);
                        responder.process(&bytes, parser.screen(), &mut writer)?;
                        dirty = true;
                    }
                    Ok(Err(e)) => {
                        // Windows ConPTY can signal the closing pipe as an error.
                        if child.0.try_wait()?.is_none() {
                            return Err(e).context("reading provider terminal");
                        }
                        disconnected = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if last_capture.elapsed() >= Duration::from_millis(100) {
                let (new_revision, new_ready) = completion()?;
                if new_revision != revision {
                    revision = new_revision;
                    ready = new_ready;
                    pending_input = !ready;
                    if ready {
                        typed.clear();
                        // Each completed turn belongs back at the routing prompt. Leaving the
                        // provider's prompt active silently bypasses Nectra on the next message.
                        leave = draft.is_empty();
                    }
                    dirty = true;
                }
                last_capture = Instant::now();
            }
            if (dirty || !provider_input) && last_render.elapsed() >= Duration::from_millis(100) {
                let queue_footer = format!(
                    "Queued: {queued_count} | F2: approvals | you> {}",
                    draft.replace(['\n', '\r'], " ")
                );
                if provider_input {
                    render(
                        parser.screen(),
                        previous.as_ref(),
                        origin,
                        model,
                        if confirm_cancel {
                            "Work may be running. Ctrl+] again: stop and return; Esc: stay"
                        } else if provider_input {
                            "PROVIDER INPUT | F2: back to Nectra queue | Ctrl+]: cancel"
                        } else {
                            &queue_footer
                        },
                    )?;
                } else {
                    render_activity(
                        parser.screen(),
                        origin,
                        cols,
                        started.elapsed(),
                        if confirm_cancel {
                            "Ctrl+] again: cancel; Esc: stay"
                        } else {
                            &queue_footer
                        },
                    )?;
                }
                if !provider_input {
                    execute!(io::stdout(), Hide)?;
                }
                previous = Some(
                    (0..rows)
                        .map(|r| {
                            (0..cols)
                                .map(|c| parser.screen().cell(r, c).unwrap().clone())
                                .collect()
                        })
                        .collect(),
                );
                last_render = Instant::now();
                dirty = false;
            }
            if exited.is_none() {
                if let Some(status) = child.0.try_wait()? {
                    exited = Some(status.success());
                    exit_time = Some(Instant::now());
                }
            }
            if leave
                || (exited.is_some()
                    && (disconnected
                        || exit_time.is_some_and(|t| t.elapsed() > Duration::from_millis(500))))
            {
                // Pick up the final hook notification before deciding whether a reply exists.
                let (new_revision, new_ready) = completion()?;
                if new_revision != revision {
                    pending_input = !new_ready;
                }
                return Ok(Outcome {
                    diagnostic: parser.screen().contents(),
                    success: leave || exited == Some(true),
                    pending_input,
                });
            }
            if event::poll(Duration::from_millis(15))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if key.code == KeyCode::F(2) {
                            clear_region(origin, display_rows)?;
                            provider_input = !provider_input;
                            display_rows = if provider_input { rows } else { 2 };
                            origin = allocate(display_rows)?;
                            guard.bottom = origin + display_rows + 1;
                            previous = None;
                            dirty = true;
                            continue;
                        }
                        if key.code == KeyCode::Char(']')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            if (ready && !pending_input) || confirm_cancel {
                                leave = true;
                            } else {
                                confirm_cancel = true;
                                dirty = true;
                            }
                            continue;
                        }
                        if confirm_cancel && key.code == KeyCode::Esc {
                            confirm_cancel = false;
                            dirty = true;
                            continue;
                        }
                        if key.modifiers.contains(KeyModifiers::SHIFT)
                            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
                        {
                            scrollback = if key.code == KeyCode::PageUp {
                                scrollback.saturating_add(rows as usize / 2)
                            } else {
                                scrollback.saturating_sub(rows as usize / 2)
                            };
                            parser.set_scrollback(scrollback);
                            scrollback = parser.screen().scrollback();
                            previous = None;
                            dirty = true;
                            continue;
                        }
                        if scrollback > 0 {
                            scrollback = 0;
                            parser.set_scrollback(0);
                            previous = None;
                            dirty = true;
                        }
                        if !provider_input {
                            match key.code {
                                KeyCode::Enter if !draft.trim().is_empty() => {
                                    QUEUED.with(|queue| {
                                        queue.borrow_mut().push(std::mem::take(&mut draft))
                                    });
                                    queued_count += 1;
                                    if ready {
                                        leave = true;
                                    }
                                }
                                KeyCode::Backspace => {
                                    draft.pop();
                                }
                                KeyCode::Char(c)
                                    if !key
                                        .modifiers
                                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                                {
                                    draft.push(c)
                                }
                                _ => {}
                            }
                            if ready && draft.is_empty() {
                                leave = true;
                            }
                            dirty = true;
                            continue;
                        }
                        if key.code == KeyCode::Enter {
                            if !is_exit_command(&typed) {
                                pending_input = true;
                                ready = false;
                            }
                            typed.clear();
                            dirty = true;
                        } else if let KeyCode::Char(c) = key.code {
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                && typed.len() < 4096
                            {
                                typed.push(c);
                            }
                        } else if key.code == KeyCode::Backspace {
                            typed.pop();
                        } else {
                            // Don't mistake an edited/moved input for a known exit command.
                            typed.push('\0');
                        }
                        if let Some(bytes) = key_bytes(key, parser.screen().application_cursor()) {
                            writer.write_all(&bytes)?;
                            writer.flush()?;
                        }
                    }
                    Event::Paste(text) => {
                        // Sanitize escape/control bytes so paste cannot break out into terminal commands.
                        let text: String = text
                            .chars()
                            .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
                            .collect();
                        if !provider_input {
                            draft.push_str(&text);
                            dirty = true;
                            continue;
                        }
                        if typed.len() + text.len() < 4096 {
                            typed.push_str(&text);
                        } else {
                            typed.push('\0');
                        }
                        if parser.screen().bracketed_paste() {
                            writer.write_all(b"\x1b[200~")?;
                        }
                        writer.write_all(text.as_bytes())?;
                        if parser.screen().bracketed_paste() {
                            writer.write_all(b"\x1b[201~")?;
                        }
                        writer.flush()?;
                    }
                    Event::Resize(_, _) => {
                        let new_size = size()?;
                        (cols, rows) = new_size;
                        // Reallocate below the old pane; never clear the earlier chat on resize.
                        execute!(
                            io::stdout(),
                            MoveTo(0, guard.bottom.min(terminal::size()?.1.saturating_sub(1)))
                        )?;
                        clear_region(origin, display_rows)?;
                        display_rows = if provider_input { rows } else { 2 };
                        origin = allocate(display_rows)?;
                        guard.bottom = origin + display_rows + 1;
                        pair.master.resize(pty_size(cols, rows))?;
                        parser.set_size(rows, cols);
                        previous = None;
                        dirty = true;
                    }
                    _ => {}
                }
            }
        }
    })();
    // Closing ConPTY plus the child closes its reader; stop the queue before joining.
    // Give the provider a chance to cancel its current turn, including work on a
    // shared daemon that must never be terminated as part of this process group.
    if leave {
        let _ = writer.write_all(b"\x03");
        let _ = writer.flush();
        thread::sleep(Duration::from_millis(250));
    }
    child.terminate();
    drop(rx);
    drop(writer);
    drop(pair.master);
    let _ = reader_thread.join();
    let _ = clear_region(origin, display_rows);
    guard.bottom = origin.saturating_sub(1);
    drop(guard);
    result
}

fn color(out: &mut Vec<u8>, value: vt100::Color, foreground: bool) -> io::Result<()> {
    let prefix = if foreground { 38 } else { 48 };
    match value {
        vt100::Color::Default => write!(out, "\x1b[{}m", if foreground { 39 } else { 49 }),
        vt100::Color::Idx(i) => write!(out, "\x1b[{prefix};5;{i}m"),
        vt100::Color::Rgb(r, g, b) => write!(out, "\x1b[{prefix};2;{r};{g};{b}m"),
    }
}

fn is_exit_command(text: &str) -> bool {
    matches!(text.trim(), "/quit" | "/exit")
}

fn clear_region(origin: u16, rows: u16) -> Result<()> {
    let mut out = io::stdout();
    for row in origin..=origin + rows + 1 {
        queue!(
            out,
            MoveTo(0, row),
            SetAttribute(Attribute::Reset),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(out, MoveTo(0, origin))?;
    out.flush()?;
    Ok(())
}

fn activity_line(screen: &vt100::Screen) -> String {
    screen
        .contents()
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim();
            let lower = line.to_lowercase();
            if lower.contains("stop hooks") {
                return Some("Finishing reply…".into());
            }
            // Show native progress, not the provider's answer, prompt, branding or status bars.
            if line.contains('…') && line.starts_with(['*', '·', '✻', '✶', '✢', '✽', '✳'])
                || [
                    "thinking ",
                    "running ",
                    "reading ",
                    "searching ",
                    "writing ",
                    "approve ",
                    "permission ",
                ]
                .iter()
                .any(|prefix| lower.starts_with(prefix))
            {
                Some(line.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "Working… | F2: inspect provider / approvals".into())
}

fn render_activity(
    screen: &vt100::Screen,
    origin: u16,
    cols: u16,
    elapsed: Duration,
    footer: &str,
) -> Result<()> {
    clear_region(origin, 2)?;
    let mut out = io::stdout();
    let frame = ['|', '/', '-', '\\'][(elapsed.as_millis() / 150 % 4) as usize];
    let status = format!("{frame} {}s  {}", elapsed.as_secs(), activity_line(screen));
    for (row, text) in [(origin, status.as_str()), (origin + 2, footer)] {
        queue!(out, MoveTo(0, row))?;
        let accent = crate::terminal::activity_color();
        if !accent.is_empty() {
            write!(out, "\x1b[{accent}m")?;
        }
        write!(
            out,
            "{}",
            text.chars()
                .filter(|c| !c.is_control())
                .take(cols as usize)
                .collect::<String>()
        )?;
        queue!(out, SetAttribute(Attribute::Reset))?;
    }
    out.flush()?;
    Ok(())
}

fn render(
    screen: &vt100::Screen,
    previous: Option<&Vec<Vec<vt100::Cell>>>,
    origin: u16,
    model: &str,
    footer: &str,
) -> Result<()> {
    let (rows, cols) = screen.size();
    let mut bytes = Vec::new();
    queue!(
        bytes,
        Hide,
        SetAttribute(Attribute::Reset),
        MoveTo(0, origin),
        Clear(ClearType::CurrentLine)
    )?;
    let accent = crate::terminal::activity_color();
    if !accent.is_empty() {
        write!(bytes, "\x1b[{accent}m")?;
    }
    write!(
        bytes,
        "{}",
        format!("Nectra | {model}")
            .chars()
            .filter(|c| !c.is_control())
            .take(cols as usize)
            .collect::<String>()
    )?;
    for row in 0..rows {
        for col in 0..cols {
            let cell = screen.cell(row, col).unwrap();
            if cell.is_wide_continuation()
                || previous
                    .and_then(|p| p.get(row as usize).and_then(|r| r.get(col as usize)))
                    .is_some_and(|p| p == cell)
            {
                continue;
            }
            queue!(
                bytes,
                MoveTo(col, origin + row + 1),
                SetAttribute(Attribute::Reset)
            )?;
            color(&mut bytes, cell.fgcolor(), true)?;
            color(&mut bytes, cell.bgcolor(), false)?;
            if cell.bold() {
                write!(bytes, "\x1b[1m")?;
            }
            if cell.italic() {
                write!(bytes, "\x1b[3m")?;
            }
            if cell.underline() {
                write!(bytes, "\x1b[4m")?;
            }
            if cell.inverse() {
                write!(bytes, "\x1b[7m")?;
            }
            if cell.has_contents() {
                bytes.extend_from_slice(cell.contents().as_bytes());
            } else {
                bytes.push(b' ');
            }
        }
    }
    queue!(
        bytes,
        SetAttribute(Attribute::Reset),
        MoveTo(0, origin + rows + 1),
        Clear(ClearType::CurrentLine)
    )?;
    if !accent.is_empty() {
        write!(bytes, "\x1b[{accent}m")?;
    }
    write!(
        bytes,
        "{}",
        footer.chars().take(cols as usize).collect::<String>()
    )?;
    queue!(bytes, SetAttribute(Attribute::Reset))?;
    if !screen.hide_cursor() && screen.scrollback() == 0 {
        let (row, col) = screen.cursor_position();
        queue!(
            bytes,
            MoveTo(col.min(cols - 1), origin + row.min(rows - 1) + 1),
            Show
        )?;
    }
    let mut out = io::stdout();
    out.write_all(&bytes)?;
    out.flush()?;
    Ok(())
}

fn key_bytes(key: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut s = String::new();
            if alt {
                s.push('\x1b');
            }
            if control && (c.is_ascii_alphabetic() || ('@'..='_').contains(&c) || c == ' ') {
                s.push(if c == ' ' {
                    '\0'
                } else {
                    ((c.to_ascii_uppercase() as u8) & 0x1f) as char
                });
            } else {
                s.push(c);
            }
            s.into_bytes()
        }
        KeyCode::Enter => {
            if alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        KeyCode::Backspace => {
            if alt {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Right
        | KeyCode::Left
        | KeyCode::Home
        | KeyCode::End => {
            let suffix = match key.code {
                KeyCode::Up => 'A',
                KeyCode::Down => 'B',
                KeyCode::Right => 'C',
                KeyCode::Left => 'D',
                KeyCode::Home => 'H',
                _ => 'F',
            };
            if modifier > 1 {
                format!("\x1b[1;{modifier}{suffix}").into_bytes()
            } else {
                format!("\x1b{}{suffix}", if application_cursor { 'O' } else { '[' }).into_bytes()
            }
        }
        KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown | KeyCode::F(_) => {
            let code = match key.code {
                KeyCode::Insert => 2,
                KeyCode::Delete => 3,
                KeyCode::PageUp => 5,
                KeyCode::PageDown => 6,
                KeyCode::F(n) => match n {
                    1 => 11,
                    2 => 12,
                    3 => 13,
                    4 => 14,
                    5 => 15,
                    6 => 17,
                    7 => 18,
                    8 => 19,
                    9 => 20,
                    10 => 21,
                    11 => 23,
                    12 => 24,
                    _ => return None,
                },
                _ => unreachable!(),
            };
            if (11..=14).contains(&code) && modifier == 1 {
                format!("\x1bO{}", (b'P' + (code - 11) as u8) as char).into_bytes()
            } else if modifier > 1 {
                format!("\x1b[{code};{modifier}~").into_bytes()
            } else {
                format!("\x1b[{code}~").into_bytes()
            }
        }
        _ => return None,
    };
    Some(bytes)
}

/// Answer common device queries locally, including queries split across pipe reads.
/// Never forward child cursor queries to the outer terminal (coordinates differ).
#[derive(Default)]
struct Queries {
    pending: Vec<u8>,
}
impl Queries {
    fn process(
        &mut self,
        bytes: &[u8],
        screen: &vt100::Screen,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        for &b in bytes {
            if b == 0x1b {
                self.pending.clear();
                self.pending.push(b);
                continue;
            }
            if self.pending.is_empty() {
                continue;
            }
            self.pending.push(b);
            if self.pending.len() == 2 && b != b'[' && b != b']' {
                self.pending.clear();
                continue;
            }
            let done = (self.pending.get(1) == Some(&b'[')
                && self.pending.len() > 2
                && (0x40..=0x7e).contains(&b))
                || b == 7;
            if done {
                match self.pending.as_slice() {
                    b"\x1b[6n" => {
                        let (r, c) = screen.cursor_position();
                        write!(writer, "\x1b[{};{}R", r + 1, c + 1)?;
                    }
                    b"\x1b[5n" => writer.write_all(b"\x1b[0n")?,
                    b"\x1b[c" | b"\x1b[0c" => writer.write_all(b"\x1b[?1;2c")?,
                    b"\x1b[>c" | b"\x1b[>0c" => writer.write_all(b"\x1b[>0;0;0c")?,
                    b"\x1b[?u" => writer.write_all(b"\x1b[?0u")?,
                    b"\x1b]10;?\x07" => writer.write_all(b"\x1b]10;rgb:dddd/dddd/dddd\x07")?,
                    b"\x1b]11;?\x07" => writer.write_all(b"\x1b]11;rgb:1111/1111/1111\x07")?,
                    _ => {}
                }
                self.pending.clear();
            }
            if self.pending.len() > 128 {
                self.pending.clear();
            }
        }
        writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_animation_and_unicode_are_emulated_not_flattened() {
        let mut parser = vt100::Parser::new(8, 40, 100);
        parser.process(b"Writing the doc now.\r\n\x1b[35mThinking |");
        parser.process(b"\r\x1b[2KThinking /");
        assert!(parser.screen().contents().contains("Writing the doc now."));
        assert!(parser.screen().contents().contains("Thinking /"));
        assert!(!parser.screen().contents().contains("Thinking |"));
        parser.process("\r\nSaved: café 👻".as_bytes());
        assert!(parser.screen().contents().contains("café 👻"));
        parser.process(b"\x1b[2J\x1b[HApproval required");
        assert_eq!(parser.screen().contents(), "Approval required");
    }
    #[test]
    fn forwards_control_arrows_and_approval_keys() {
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false
            )
            .unwrap(),
            b"\x03"
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), true).unwrap(),
            b"\x1bOB"
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), false).unwrap(),
            b"\r"
        );
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Char('👻'), KeyModifiers::NONE),
                false
            )
            .unwrap(),
            "👻".as_bytes()
        );
    }
    #[test]
    fn answers_fragmented_terminal_queries() {
        let parser = vt100::Parser::new(8, 40, 0);
        let mut queries = Queries::default();
        let mut bytes = vec![];
        queries
            .process(b"\x1b[", parser.screen(), &mut bytes)
            .unwrap();
        queries
            .process(b"6n\x1b[?u", parser.screen(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, b"\x1b[1;1R\x1b[?0u");
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn compact_view_excludes_provider_chrome_and_answers() {
        let mut parser = vt100::Parser::new(24, 100, 0);
        parser.process("Claude Code v2.1.269\r\nSonnet 5 - Claude Max\r\n\u{276f} which model?\r\n\u{25cf} You're talking to Sonnet 5\u{2026}\r\n".as_bytes());
        assert!(activity_line(parser.screen()).starts_with("Working"));
        parser.process("\u{b7} Waddling\u{2026} (running stop hooks\u{2026} 1/2)\r\n".as_bytes());
        assert_eq!(activity_line(parser.screen()), "Finishing reply\u{2026}");
    }
}
