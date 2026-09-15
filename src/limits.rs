use serde_json::Value;
use std::{
    fmt,
    time::{Duration, Instant},
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
    pub fn from_error(event: &Value, safe_to_retry: bool) -> Option<Self> {
        let message = event.to_string();
        let lower = message.to_lowercase();
        if ![
            "rate_limit",
            "rate limit",
            "usage_limit",
            "usage limit",
            "too many requests",
            "resource_exhausted",
            "quota exceeded",
            "hit your limit",
            "429",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            return None;
        }
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
            .unwrap_or(60)
            .clamp(1, 86400);
        Some(Self {
            message,
            provider_wide: [
                "usage_limit",
                "usage limit",
                "organization",
                "account",
                "hit your limit",
                "spend_limit",
            ]
            .iter()
            .any(|s| lower.contains(s)),
            retry_after: Duration::from_secs(seconds),
            safe_to_retry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
