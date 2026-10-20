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

/// `text` with known credential values and token-shaped words masked.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    let mut secrets: Vec<String> = ALSO.lock().unwrap_or_else(|e| e.into_inner()).clone();
    secrets.extend(environment().iter().cloned());
    // Longest first, so a secret that contains another is masked whole.
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in &secrets {
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

/// How much of a stream's `pending` bytes can be masked and passed on now
/// without cutting a credential in two: through the last line end; for a line
/// over 64 KiB, through its last space (or all but its last 4 KiB without one);
/// everything at the end of the stream.
pub fn ready(pending: &[u8], eof: bool) -> usize {
    let after = |b: &dyn Fn(u8) -> bool| pending.iter().rposition(|&c| b(c)).map(|i| i + 1);
    match after(&|c| c == b'\n') {
        _ if eof => pending.len(),
        Some(end) => end,
        None if pending.len() <= 65536 => 0,
        None => after(&|c| c.is_ascii_whitespace()).unwrap_or(pending.len() - 4096),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_is_only_cut_between_words() {
        assert_eq!(ready(b"one\ntwo", false), 4);
        assert_eq!(ready(b"two", false), 0);
        assert_eq!(ready(b"two", true), 3);
        let mut long = vec![b'a'; 70_000];
        assert_eq!(ready(&long, false), 70_000 - 4096);
        long[60_000] = b' ';
        assert_eq!(ready(&long, false), 60_001);
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
