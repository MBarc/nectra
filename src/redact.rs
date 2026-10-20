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

/// Mask these values too: credentials Nectra hands a child, such as an MCP
/// server's environment.
pub fn also(values: impl IntoIterator<Item = String>) {
    let mut also = ALSO.lock().unwrap_or_else(|e| e.into_inner());
    also.extend(values.into_iter().filter(|v| v.len() >= 8));
}

/// Values of Nectra's own variables whose names say they hold a credential.
fn environment() -> &'static [String] {
    static SECRETS: OnceLock<Vec<String>> = OnceLock::new();
    SECRETS.get_or_init(|| {
        std::env::vars()
            .filter(|(name, value)| {
                let name = name.to_ascii_uppercase();
                value.len() >= 8
                    && ["KEY", "TOKEN", "SECRET", "PASSW", "CREDENTIAL", "AUTH"]
                        .iter()
                        .any(|w| name.contains(w))
            })
            .map(|(_, value)| value)
            .collect()
    })
}

/// `text` with known credential values and token-shaped words masked.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    let also = ALSO.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for secret in environment().iter().chain(&also) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_masked_and_prose_is_not() {
        also(["pons-secret-value".to_string()]);
        let text = "401: key sk-ant-api03-abcdefghijklmnop rejected; \
                    Authorization: Bearer abcdefghijklmnopqrstu; token pons-secret-value; \
                    see https://example.com/docs and skip-permissions";
        let out = redact(text);
        assert!(!out.contains("sk-ant"), "{out}");
        assert!(!out.contains("abcdefghijklmnopqrstu"), "{out}");
        assert!(!out.contains("pons-secret-value"), "{out}");
        assert!(out.contains("https://example.com/docs and skip-permissions"));
        assert!(out.starts_with("401: key [redacted] rejected"));
    }
}
