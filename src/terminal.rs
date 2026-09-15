use std::{
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicU8, Ordering},
};

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}
static MODE: AtomicU8 = AtomicU8::new(0);
static THEME: AtomicU8 = AtomicU8::new(0);

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Default,
    Hacker,
    IceBlue,
}
impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Hacker => "hacker",
            Self::IceBlue => "ice-blue",
        }
    }
}
pub fn set_theme(theme: Theme) {
    THEME.store(theme as u8, Ordering::Relaxed);
}
pub fn theme() -> Theme {
    match THEME.load(Ordering::Relaxed) {
        1 => Theme::Hacker,
        2 => Theme::IceBlue,
        _ => Theme::Default,
    }
}

pub fn theme_path(config: Option<&std::path::Path>) -> anyhow::Result<std::path::PathBuf> {
    if let Some(config) = config {
        return Ok(
            config.with_file_name(if config.file_name().is_some_and(|n| n == "ui.toml") {
                "nectra-ui.toml"
            } else {
                "ui.toml"
            }),
        );
    }
    Ok(dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot locate theme preferences; pass --config"))?
        .join("nectra/ui.toml"))
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    theme: Theme,
}
pub fn load_theme(path: &std::path::Path) -> anyhow::Result<Theme> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Theme::Default),
        Err(e) => return Err(e.into()),
    };
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096 {
        anyhow::bail!("theme preferences exceed 4 KiB");
    }
    Ok(toml::from_str::<Preferences>(&text)?.theme)
}
pub fn save_theme(path: &std::path::Path, theme: Theme) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = parent.join(format!(".nectra-theme-{}-{nonce}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(toml::to_string(&Preferences { theme })?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub fn activity_color() -> &'static str {
    if enabled() {
        palette("1;96", theme())
    } else {
        ""
    }
}
thread_local! { static PLAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub struct PlainGuard(bool);
pub fn plain() -> PlainGuard {
    PlainGuard(PLAIN.with(|p| p.replace(true)))
}
impl Drop for PlainGuard {
    fn drop(&mut self) {
        PLAIN.with(|p| p.set(self.0));
    }
}
pub fn set_mode(mode: ColorMode) {
    MODE.store(
        match mode {
            ColorMode::Auto => 0,
            ColorMode::Always => 1,
            ColorMode::Never => 2,
        },
        Ordering::Relaxed,
    );
}
fn enabled() -> bool {
    match MODE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").ok().as_deref() != Some("dumb")
        }
    }
}

pub fn stdout() -> impl Write {
    let color = enabled();
    UiWriter::new(
        anstream::AutoStream::new(
            io::stdout(),
            if color {
                anstream::ColorChoice::Always
            } else {
                anstream::ColorChoice::Never
            },
        ),
        color,
    )
}
pub fn line(text: &str) -> io::Result<()> {
    writeln!(stdout(), "{text}")
}
pub fn error(text: &str) {
    let color = match MODE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    let mut output = anstream::AutoStream::new(
        io::stderr(),
        if color {
            anstream::ColorChoice::Always
        } else {
            anstream::ColorChoice::Never
        },
    );
    let _ = if color {
        writeln!(output, "\x1b[1;91m{text}\x1b[0m")
    } else {
        writeln!(output, "{text}")
    };
}
/// Style human UI text only. Streaming model content uses the raw stdout writer.
pub struct UiWriter<W: Write> {
    inner: W,
    pending: Vec<u8>,
    color: bool,
}
impl<W: Write> UiWriter<W> {
    pub fn new(inner: W, color: bool) -> Self {
        Self {
            inner,
            pending: vec![],
            color,
        }
    }
    fn emit(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.pending);
        let style = palette(style(&text), theme());
        let color = self.color && !PLAIN.with(|p| p.get());
        if color && !style.is_empty() {
            write!(self.inner, "\x1b[{style}m")?;
        }
        self.inner.write_all(&self.pending)?;
        if color && !style.is_empty() {
            self.inner.write_all(b"\x1b[0m")?;
        }
        self.pending.clear();
        Ok(())
    }
}
fn palette(base: &'static str, theme: Theme) -> &'static str {
    match theme {
        Theme::Default => base,
        Theme::Hacker => match base {
            "1;91" => "1;91",
            "93" => "93",
            "90" => "32",
            "1;96" => "1;92",
            "" => "32",
            _ => "92",
        },
        Theme::IceBlue => match base {
            "1;91" => "1;91",
            "93" => "93",
            "90" => "38;5;110",
            "1;96" => "1;38;5;117",
            "95" => "38;5;147",
            "92" => "38;5;159",
            "94" => "38;5;75",
            "96" => "38;5;123",
            _ => "38;5;153",
        },
    }
}
fn style(text: &str) -> &'static str {
    let t = text.trim_start();
    let lower = t.to_lowercase();
    if lower.contains("failed") || lower.contains("[missing") || lower.starts_with("error") {
        "1;91"
    } else if lower.contains("cooldown")
        || lower.contains("cooling down")
        || lower.contains("unavailable")
        || lower.contains("permission bypass")
        || lower.contains("not yet implemented")
    {
        "93"
    } else if t.starts_with("[OK]") || t.starts_with("Saved ") || t.starts_with("Session saved:") {
        "92"
    } else if t.contains("(anthropic)") || t.starts_with("anthropic ") {
        "95"
    } else if t.contains("(openai)") || t.starts_with("openai ") {
        "92"
    } else if t.contains("(google)") || t.starts_with("google ") {
        "94"
    } else if t.contains("(ollama)") || t.starts_with("ollama ") {
        "96"
    } else if t.starts_with("you>")
        || t.starts_with('/')
        || t.starts_with('|')
        || t.starts_with("_   _")
        || t.starts_with("Which providers")
    {
        "1;96"
    } else if t.starts_with("Session:") || t.starts_with("Resumed ") || t.starts_with("Context ") {
        "90"
    } else {
        ""
    }
}
impl<W: Write> Write for UiWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for byte in bytes {
            self.pending.push(*byte);
            if *byte == b'\n' || self.pending.len() >= 65536 {
                self.emit()?;
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.emit()?;
        self.inner.flush()
    }
}
impl<W: Write> Drop for UiWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_and_plain_output_preserve_text() {
        for color in [false, true] {
            let mut bytes = vec![];
            {
                let mut writer = UiWriter::new(&mut bytes, color);
                write!(writer, "[OK] connected\nyou> ").unwrap();
                writer.flush().unwrap();
            }
            let text = String::from_utf8(bytes).unwrap();
            if color {
                assert!(text.contains("\x1b[92m"));
                assert!(text.contains("\x1b[1;96m"));
            } else {
                assert_eq!(text, "[OK] connected\nyou> ");
            }
        }
    }
}
