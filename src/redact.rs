//! Credentials out of what Nectra passes on: provider diagnostics and error text.
use std::sync::{Mutex, OnceLock};

static ALSO: Mutex<Vec<String>> = Mutex::new(vec![]);
const MASK: &str = "[redacted]";
/// Prefixes of the token formats providers and forges hand out.
const PREFIXES: [&str; 10] = [
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "AIza",
];

/// Whether variable `name`'s `value` is masked: from 4 characters when the
/// name says it holds a credential (except a few setting words, which would
/// mask ordinary text), from 8 for any other value Nectra hands a child.
fn secret(name: &str, value: &str, any_name: bool) -> bool {
    let name = name.to_ascii_uppercase();
    let credential = ["KEY", "TOKEN", "SECRET", "PASSW", "CREDENTIAL", "AUTH"]
        .iter()
        .any(|w| name.contains(w));
    let setting = ["true", "false", "none", "null", "auto"]
        .iter()
        .any(|w| value.eq_ignore_ascii_case(w));
    (credential && value.len() >= 4 && !setting) || (any_name && value.len() >= 8)
}

/// Mask these variables' values too: what Nectra hands a child, such as an MCP
/// server's environment.
pub fn also(vars: impl IntoIterator<Item = (String, String)>) {
    let mut also = ALSO.lock().unwrap_or_else(|e| e.into_inner());
    also.extend(
        vars.into_iter()
            .filter(|(name, value)| secret(name, value, true))
            .map(|(_, value)| value),
    );
}

/// Values of Nectra's own variables whose names say they hold a credential.
fn environment() -> &'static [String] {
    static SECRETS: OnceLock<Vec<String>> = OnceLock::new();
    SECRETS.get_or_init(|| {
        std::env::vars()
            .filter(|(name, value)| secret(name, value, false))
            .map(|(_, value)| value)
            .collect()
    })
}

/// Every known credential value, longest first, so a secret that contains
/// another is masked whole.
fn secrets() -> Vec<String> {
    let mut secrets: Vec<String> = ALSO.lock().unwrap_or_else(|e| e.into_inner()).clone();
    secrets.extend(environment().iter().cloned());
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets
}

/// `text` with known credential values and token-shaped words masked.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for secret in &secrets() {
        out = out.replace(secret.as_str(), MASK);
    }
    let word = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
    let mut masked = String::with_capacity(out.len());
    let mut rest = out.as_str();
    let mut bearer = false;
    while let Some(start) = rest.find(word) {
        masked.push_str(&rest[..start]);
        let len = rest[start..]
            .find(|c| !word(c))
            .unwrap_or(rest.len() - start);
        let w = &rest[start..start + len];
        let token = (PREFIXES.iter().any(|p| w.starts_with(p)) && w.len() >= 20)
            || (bearer && w.len() >= 16)
            || (w.starts_with("eyJ") && w.len() >= 30 && w.matches('.').count() == 2);
        masked.push_str(if token { MASK } else { w });
        bearer = w.eq_ignore_ascii_case("bearer");
        rest = &rest[start + len..];
    }
    masked.push_str(rest);
    masked
}

/// Where [`ready`] prefers to cut: at line ends (diagnostics), or at any
/// space (a reply, shown as it streams).
pub const LINE: fn(u8) -> bool = |c| c == b'\n';
pub const SPACE: fn(u8) -> bool = |c| c.is_ascii_whitespace();

/// How much of a stream's `pending` bytes can be masked and passed on now
/// without cutting a credential in two: through the last `boundary`; past
/// 64 KiB without one, through the last space, or else all but the last 4 KiB
/// (at least the longest known credential less one byte). Never inside a known
/// credential, or what may yet become one, nor inside a character. Everything
/// at the end of the stream.
pub fn ready(pending: &[u8], eof: bool, boundary: fn(u8) -> bool) -> usize {
    if eof {
        return pending.len();
    }
    cut(pending, boundary, &secrets())
}

fn cut(pending: &[u8], boundary: fn(u8) -> bool, secrets: &[String]) -> usize {
    let after = |b: &dyn Fn(u8) -> bool| pending.iter().rposition(|&c| b(c)).map(|i| i + 1);
    let keep = secrets.first().map_or(0, |s| s.len().saturating_sub(1));
    // A forced cut backs off to a character's start; bytes that aren't UTF-8
    // at all are left to the lossy decoding.
    let forced = |mut cut: usize| {
        while cut > 0 && pending[cut] & 0xC0 == 0x80 {
            cut -= 1;
        }
        cut
    };
    let mut cut = match after(&boundary) {
        Some(end) => end,
        None if pending.len() <= 65536 => 0,
        None => {
            after(&SPACE).unwrap_or_else(|| forced(pending.len().saturating_sub(keep.max(4096))))
        }
    };
    // Back to the start of a credential the cut would split, or of what the
    // pending bytes end in that may yet be one, and to before a trailing
    // "Bearer" so its token is masked with it; until neither moves the cut.
    loop {
        let word = |c: u8| c.is_ascii_alphanumeric() || b"-_.".contains(&c);
        let before = pending[..cut].trim_ascii_end();
        let bearer = before.len() >= 6
            && before[before.len() - 6..].eq_ignore_ascii_case(b"bearer")
            && (before.len() == 6 || !word(before[before.len() - 7]));
        if bearer {
            cut = before.len() - 6;
            continue;
        }
        let split = secrets.iter().map(String::as_bytes).find_map(|s| {
            (cut.saturating_sub(s.len() - 1)..cut).find(|&start| {
                let rest = &pending[start..];
                let n = rest.len().min(s.len());
                start + s.len() > cut && rest[..n] == s[..n]
            })
        });
        match split {
            Some(start) => cut = start,
            None => return cut,
        }
    }
}

/// Text passed on in pieces (a reply's deltas), masked as a whole: the end of
/// what came so far waits for the next piece when it may be part of a
/// credential or a token-shaped word.
#[derive(Default)]
pub struct Stream(String);
impl Stream {
    /// What can go out now of everything held so far and `text`, masked.
    pub fn push(&mut self, text: &str) -> String {
        self.0.push_str(text);
        let end = ready(self.0.as_bytes(), false, SPACE);
        redact(&self.0.drain(..end).collect::<String>())
    }
    /// Everything still held, masked.
    pub fn flush(&mut self) -> String {
        redact(&std::mem::take(&mut self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_is_cut_at_line_ends_then_spaces() {
        assert_eq!(ready(b"one\ntwo", false, LINE), 4);
        assert_eq!(ready(b"two", false, LINE), 0);
        assert_eq!(ready(b"two", true, LINE), 3);
        let mut long = vec![b'a'; 70_000];
        assert_eq!(ready(&long, false, LINE), 70_000 - 4096);
        long[60_000] = b' ';
        assert_eq!(ready(&long, false, LINE), 60_001);
    }

    #[test]
    fn a_credential_is_never_cut_in_two() {
        also([("FORCED_PASSWORD".into(), "48213".into())]);
        // A diagnostic with no line end or space, the credential across the
        // forced cut at 73,728 - 4,096.
        let mut long = vec![b'a'; 73_728];
        long[69_630..69_635].copy_from_slice(b"48213");
        let cut = ready(&long, false, LINE);
        let pieces = [&long[..cut], &long[cut..]].map(|p| redact(&String::from_utf8_lossy(p)));
        assert!(!pieces.iter().any(|p| p.contains("482")), "{cut}");
        // A cut at a boundary waits for what may yet complete one.
        also([("SPACED_PASSWORD".into(), "open sesame".into())]);
        assert_eq!(ready(b"say open ", false, SPACE), 4);
        // A forced cut keeps whole characters.
        let wide = "é".repeat(40_000);
        let cut = ready(wide.as_bytes(), false, LINE);
        assert!(wide.is_char_boundary(cut));
    }

    #[test]
    fn bytes_that_arent_utf_8_never_break_the_cut() {
        assert_eq!(ready(&[0x80, b'a'], false, LINE), 0);
        assert_eq!(ready(&[0x80; 70_000], false, LINE), 0);
        assert_eq!(ready(&[0x80, b'\n', 0x80], false, LINE), 2);
    }

    #[test]
    fn a_credential_longer_than_the_stream_never_breaks_the_cut() {
        let long = ["x".repeat(100_000)];
        assert_eq!(cut(&[b'a'; 70_000], LINE, &long), 0);
    }

    #[test]
    fn a_bearer_token_is_masked_however_it_streams() {
        for deltas in [
            vec!["auth Bearer A7b8C9d0E1f2G3h4I5j6", " ok"],
            vec!["auth Bearer ", "A7b8C9d0E1f2G3h4I5j6 ok"],
            vec!["auth Bearer", " A7b8C9d0E1f2G3h4I5j6 ok"],
        ] {
            let mut stream = Stream::default();
            let mut shown: String = deltas.iter().map(|d| stream.push(d)).collect();
            shown += &stream.flush();
            assert_eq!(shown, "auth Bearer [redacted] ok", "{deltas:?}");
        }
    }

    #[test]
    fn a_reply_in_pieces_is_masked_whole() {
        also([("STREAM_API_KEY".into(), "k9x2z".into())]);
        let mut stream = Stream::default();
        let mut shown = String::new();
        for delta in ["the key is k9", "x2", "z, done"] {
            shown += &stream.push(delta);
        }
        shown += &stream.flush();
        assert_eq!(shown, "the key is [redacted], done");
    }

    #[test]
    fn credentials_are_masked_and_prose_is_not() {
        also([("PONS_TOKEN".to_string(), "pons-secret-value".to_string())]);
        also([
            ("PONS_PASSWORD".into(), "48213".into()),
            ("API_KEY".into(), "auto".into()),
        ]);
        also([("MODE".into(), "fast".into())]);
        let text = "401: key sk-ant-api03-abcdefghijklmnop rejected; \
                    Authorization: Bearer abcdefghijklmnopqrstu; token pons-secret-value; \
                    see https://example.com/docs and skip-permissions";
        let out = redact(text);
        assert!(!out.contains("sk-ant"), "{out}");
        assert!(!out.contains("abcdefghijklmnopqrstu"), "{out}");
        assert!(!out.contains("pons-secret-value"), "{out}");
        assert!(out.contains("https://example.com/docs and skip-permissions"));
        assert!(out.starts_with("401: key [redacted] rejected"));
        // Short values are masked under a credential's name, not otherwise.
        assert_eq!(
            redact("pin 48213, mode fast, auto"),
            "pin [redacted], mode fast, auto"
        );
    }
}
