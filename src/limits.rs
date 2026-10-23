use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub struct RateLimited {
    pub message: String,
    pub provider_wide: bool,
    pub retry_after: Duration,
    pub safe_to_retry: bool,
}
impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RateLimited {}

impl RateLimited {
    /// Read a named field from the event or its nested `error` object.
    fn field<'a>(event: &'a Value, key: &str) -> Option<&'a Value> {
        let nested = event.get("error").and_then(|e| e.get(key));
        event.get(key).or(nested).filter(|v| !v.is_null())
    }

    pub fn from_error(event: &Value, safe_to_retry: bool) -> Option<Self> {
        // Judge only the fields that carry provider meaning. Matching the whole
        // serialized event turns any incidental "429" -- an output_tokens count of
        // 4290, a path, an echoed prompt -- into a spurious rate limit.
        let message = ["message", "detail", "error_description"]
            .iter()
            .find_map(|k| Self::field(event, k).and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        // Providers report the code as a scalar (`type`) or as a list
        // (`errors: ["rate_limit_error"]`); both carry the same meaning.
        let mut codes: Vec<String> = ["type", "code", "reason", "status"]
            .iter()
            .filter_map(|k| Self::field(event, k).and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        for key in ["errors", "error_codes"] {
            if let Some(list) = Self::field(event, key).and_then(Value::as_array) {
                codes.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
        let kind = codes.join(" ").to_lowercase();
        let status = ["status", "status_code", "code"]
            .iter()
            .find_map(|k| Self::field(event, k).and_then(Value::as_u64));

        let lower = message.to_lowercase();
        let limited = status == Some(429)
            || [
                "rate_limit",
                "usage_limit",
                "resource_exhausted",
                "too_many_requests",
                "quota_exceeded",
            ]
            .iter()
            .any(|s| kind.contains(s))
            || [
                "rate limit",
                "usage limit",
                "too many requests",
                "quota exceeded",
                "hit your limit",
                // Google's wording (agy, and the Gemini API behind it).
                "exceeded your current quota",
                "exhausted your capacity",
                "resource has been exhausted",
            ]
            .iter()
            .any(|s| lower.contains(s));
        if !limited {
            return None;
        }
        let message = if message.is_empty() {
            "provider reported a rate limit".to_string()
        } else {
            message
        };
        // Scope comes from the provider's own type and message. A sibling
        // account_id field is metadata, not a statement that every model of
        // this provider is exhausted.
        let says = |s: &&str| kind.contains(s) || lower.contains(s);
        // An exhausted plan or spend cap lasts hours; an organization's
        // per-minute rate limit is provider-wide but clears within a minute.
        let exhausted = [
            "usage_limit",
            "usage limit",
            "spend_limit",
            "hit your limit",
        ]
        .iter()
        .any(says);
        let provider_wide = exhausted || says(&"organization");
        let seconds = event["retry_after"]
            .as_u64()
            .or_else(|| event["error"]["retry_after"].as_u64())
            .or_else(|| {
                let reset = event["resetsAt"]
                    .as_u64()
                    .or_else(|| event["error"]["resetsAt"].as_u64())?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                Some(reset.saturating_sub(now))
            })
            // ponytail: a reset stated as a clock time alone ("try again at
            // 10:53 PM") is within a day; re-probing hourly beats every minute.
            .or_else(|| {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
                reset_in(&message, now.as_secs()).map(|s| s.max(3600))
            })
            .unwrap_or(if exhausted { 3600 } else { 60 })
            .clamp(1, MAX_COOLDOWN);
        Some(Self {
            message,
            provider_wide,
            retry_after: Duration::from_secs(seconds),
            safe_to_retry,
        })
    }
}

/// A weekly plan limit, with a day to spare.
const MAX_COOLDOWN: u64 = 8 * 86400;

/// Seconds until a reset Codex states as a date: "try again at Oct 27th, 2026 3:04 PM".
///
/// ponytail: the time is the user's local time and no time-zone library is
/// here, so this is the earliest instant that wall-clock time occurs anywhere
/// (UTC+14). Up to a day early, never late: an early re-probe just meets the
/// limit and falls back again, while a late one locks out a provider that works.
pub(crate) fn reset_in(message: &str, now: u64) -> Option<u64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = message.to_lowercase();
    let rest = lower.split("try again at ").nth(1)?;
    let mut words = rest
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty());
    let first = words.next()?;
    let month = MONTHS.iter().position(|m| first.starts_with(m))? as i64 + 1;
    let day: i64 = words
        .next()?
        .trim_end_matches(|c: char| c.is_alphabetic() || c == '.')
        .parse()
        .ok()?;
    let (mut year, mut minutes) = (None, 0);
    for word in words.take(3).map(|w| w.trim_end_matches('.')) {
        if let Some((h, m)) = word.split_once(':') {
            minutes = h.parse::<i64>().ok()? % 12 * 60 + m.parse::<i64>().ok()?;
        } else if word == "pm" {
            minutes += 720;
        } else if word.len() == 4 && word.parse::<i64>().is_ok() {
            year = word.parse().ok();
        } else if word != "am" {
            break;
        }
    }
    let now = now as i64;
    let at = |y| days_from_civil(y, month, day) * 86400 + minutes * 60 - 14 * 3600;
    // No year: the next such date, allowing for today's being underway.
    let approx = 1970 + now / 31_556_952;
    let at = match year {
        Some(y) => at(y),
        None => (approx - 1..=approx + 1)
            .map(at)
            .find(|t| t + 2 * 86400 > now)?,
    };
    Some(at.saturating_sub(now).max(0) as u64)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incidental_digits_are_not_rate_limits() {
        // 4290 output tokens contains "429"; matching the serialized event as one
        // blob turns an ordinary successful result into a rate limit.
        assert!(RateLimited::from_error(
            &serde_json::json!({"type": "result", "usage": {"output_tokens": 4290}}),
            true
        )
        .is_none());
    }

    #[test]
    fn incidental_words_do_not_widen_scope_to_the_provider() {
        // A model-scoped limit that merely carries an account id must not cool
        // down every model the provider offers.
        let limit = RateLimited::from_error(
            &serde_json::json!({
                "error": {"type": "rate_limit_error", "message": "Rate limit exceeded for this model"},
                "account_id": "acct_123"
            }),
            true,
        )
        .expect("rate limit should be detected");
        assert!(!limit.provider_wide);
    }

    #[test]
    fn only_an_exhausted_plan_cools_down_for_an_hour() {
        let cooldown = |message: &str| {
            RateLimited::from_error(&serde_json::json!({ "message": message }), true)
                .unwrap()
                .retry_after
                .as_secs()
        };
        assert_eq!(
            cooldown("You've hit your usage limit. Try again at 10:53 PM."),
            3600
        );
        assert_eq!(
            cooldown("Rate limit reached for organization org-1 on tokens per min"),
            60
        );
    }

    #[test]
    fn a_stated_reset_date_is_parsed() {
        // 2026-10-22T12:00:00Z; Codex states the reset in local time, so the
        // estimate is the earliest instant that wall-clock time occurs (UTC+14).
        let now = 1_792_670_400;
        let early = 14 * 3600;
        assert_eq!(
            reset_in("You've hit your usage limit ... try again at Oct 27th", now),
            Some(1_793_059_200 - early - now)
        );
        assert_eq!(
            reset_in(
                "You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro) or try again at Oct 27th, 2026 3:04 PM.",
                now
            ),
            Some(1_793_113_440 - early - now)
        );
        // No year: the next one, across the new year.
        assert_eq!(
            reset_in("try again at Jan 2nd", 1_793_059_200 + 60 * 86400),
            Some(1_798_848_000 - early - (1_793_059_200 + 60 * 86400))
        );
        // A clock time alone keeps the hourly re-probe.
        assert_eq!(reset_in("Try again at 10:53 PM.", now), None);
        let limit = RateLimited::from_error(
            &serde_json::json!({"message": "You've hit your usage limit. Try again at Dec 30th, 2999 1:00 PM."}),
            true,
        )
        .unwrap();
        assert_eq!(limit.retry_after.as_secs(), MAX_COOLDOWN);
    }

    #[test]
    fn display_is_the_provider_message_not_the_serialized_event() {
        let limit = RateLimited::from_error(
            &serde_json::json!({
                "error": {"type": "rate_limit_error", "message": "Rate limit exceeded"}
            }),
            true,
        )
        .expect("rate limit should be detected");
        assert_eq!(limit.to_string(), "Rate limit exceeded");
    }

    #[test]
    fn cooldowns_survive_a_restart() {
        // Held only in memory, a rate limit is forgotten on exit and the next
        // run walks straight back into the same limit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cooldowns.json");
        let limit = RateLimited::from_error(
            &serde_json::json!({"message": "rate limit exceeded", "retry_after": 120}),
            true,
        )
        .unwrap();

        let mut before = Cooldowns::default();
        before.record("opus", "anthropic", &limit);
        before.persist(&path).unwrap();

        let after = Cooldowns::load(&path);
        assert!(after.remaining("opus", "anthropic").is_some());
        assert!(after.remaining("sonnet", "anthropic").is_none());
    }

    #[test]
    fn an_expired_cooldown_is_not_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cooldowns.json");
        let mut limit = RateLimited::from_error(
            &serde_json::json!({"message": "rate limit exceeded", "retry_after": 1}),
            true,
        )
        .unwrap();
        limit.retry_after = Duration::from_secs(0);
        let mut before = Cooldowns::default();
        before.record("opus", "anthropic", &limit);
        before.persist(&path).unwrap();
        assert!(Cooldowns::load(&path)
            .remaining("opus", "anthropic")
            .is_none());
    }

    #[test]
    fn a_missing_or_corrupt_state_file_is_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Cooldowns::load(&dir.path().join("absent.json"))
            .0
            .is_empty());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert!(Cooldowns::load(&bad).0.is_empty());
    }

    #[test]
    fn scope_and_expiry() {
        let mut cooldowns = Cooldowns::default();
        let mut limit = RateLimited::from_error(
            &serde_json::json!({"message":"rate limit exceeded","retry_after":120}),
            true,
        )
        .unwrap();
        cooldowns.record("opus", "anthropic", &limit);
        assert!(cooldowns.remaining("opus", "anthropic").is_some());
        assert!(cooldowns.remaining("sonnet", "anthropic").is_none());
        limit.provider_wide = true;
        cooldowns.record("opus", "anthropic", &limit);
        assert!(cooldowns.remaining("sonnet", "anthropic").is_some());
        assert!(cooldowns.remaining("gpt", "openai").is_none());
        for (_, until) in &mut cooldowns.0 {
            *until = Instant::now() - Duration::from_secs(1);
        }
        assert!(cooldowns.remaining("opus", "anthropic").is_none());
    }
}

#[derive(Default)]
pub struct Cooldowns(Vec<(String, Instant)>);

impl Cooldowns {
    /// Where cooldowns are remembered between runs.
    pub fn state_path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|p| p.join("nectra/cooldowns.json"))
    }

    /// Read remembered cooldowns, dropping any that already expired.
    ///
    /// Best effort by design: a missing, unreadable or corrupt state file means
    /// "nothing is cooling down", never a failed launch.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(saved) = serde_json::from_str::<BTreeMap<String, u64>>(&text) else {
            return Self::default();
        };
        let now = Self::unix_now();
        let mut cooldowns = Self::default();
        for (key, expires_at) in saved {
            if let Some(remaining) = expires_at.checked_sub(now).filter(|s| *s > 0) {
                cooldowns
                    .0
                    .push((key, Instant::now() + Duration::from_secs(remaining)));
            }
        }
        cooldowns
    }

    /// Write live cooldowns as absolute wall-clock expiries.
    ///
    /// `Instant` is monotonic and meaningless across processes, so the on-disk
    /// form is seconds since the epoch.
    pub fn persist(&self, path: &Path) -> std::io::Result<()> {
        let now = Instant::now();
        let unix_now = Self::unix_now();
        let live: BTreeMap<&str, u64> = self
            .0
            .iter()
            .filter_map(|(key, until)| {
                let remaining = until.checked_duration_since(now)?.as_secs();
                (remaining > 0).then(|| (key.as_str(), unix_now + remaining))
            })
            .collect();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec(&live)?)
    }

    fn unix_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    pub fn remaining(&self, model: &str, provider: &str) -> Option<Duration> {
        let now = Instant::now();
        self.0
            .iter()
            .filter(|(key, _)| {
                key == &format!("model:{model}") || key == &format!("provider:{provider}")
            })
            .filter_map(|(_, until)| until.checked_duration_since(now))
            .max()
    }
    pub fn record(&mut self, model: &str, provider: &str, limit: &RateLimited) {
        let key = if limit.provider_wide {
            format!("provider:{provider}")
        } else {
            format!("model:{model}")
        };
        self.0
            .retain(|(k, until)| k != &key && *until > Instant::now());
        self.0.push((key, Instant::now() + limit.retry_after));
    }
}
