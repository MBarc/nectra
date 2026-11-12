//! Model access is learned from actual provider responses, never inferred from CLI installation.
#[derive(Debug)]
pub struct ModelUnavailable {
    pub safe_to_retry: bool,
}
impl std::fmt::Display for ModelUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "provider rejected the requested model (access, unsupported model, or model not found)"
        )
    }
}
impl std::error::Error for ModelUnavailable {}
impl ModelUnavailable {
    /// Judge a provider event by the fields that carry provider meaning, the way
    /// `RateLimited::from_error` does. Serialising the whole event let any
    /// incidental text (a "tool not supported" notice, an echoed prompt, a path)
    /// exclude the model for the session.
    pub fn from_event(event: &serde_json::Value, safe_to_retry: bool) -> Option<Self> {
        let field = |event: &'_ serde_json::Value, key: &str| {
            event
                .get(key)
                .or_else(|| event.get("error").and_then(|e| e.get(key)))
                .filter(|v| !v.is_null())
                .cloned()
        };
        let mut parts: Vec<String> = [
            "message",
            "detail",
            "error_description",
            "short_error",
            // Claude's failed `result` event carries the notice as its text.
            "result",
        ]
        .iter()
        .chain(["type", "code", "reason", "status"].iter())
        .filter_map(|k| field(event, k))
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
        // `error` is a bare string for some runtimes, an object for others.
        if let Some(e) = event.get("error").and_then(|e| e.as_str()) {
            parts.push(e.to_string());
        }
        // A tool or feature the model lacks is not a rejection of the model.
        let text = parts.join(" ");
        let lower = text.to_lowercase();
        if lower.contains("tool") && !lower.contains("model_not_found") {
            return None;
        }
        Self::detect(&text, safe_to_retry)
    }
    pub fn detect(message: &str, safe_to_retry: bool) -> Option<Self> {
        let text = message.to_lowercase();
        let rejected = [
            "model_not_found",
            "unsupported_model",
            "model not found",
            "unknown model",
            "invalid model",
            "model_not_available",
            // agy: `model nope-9 is not recognized as a known model`
            "is not recognized as a known model",
        ]
        .iter()
        .any(|s| text.contains(s))
            || (text.contains("model")
                && [
                    "do not have access",
                    "don't have access",
                    "does not have access",
                    "not supported",
                    "not available for",
                    "not available with",
                    "not available on",
                    "not authorized",
                    "not permitted",
                    "not entitled",
                ]
                .iter()
                .any(|s| text.contains(s)));
        rejected.then_some(Self { safe_to_retry })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn access_errors_do_not_confuse_tool_permissions_with_model_access() {
        assert!(
            ModelUnavailable::detect("You do not have access to model gpt-6-astra", true)
                .unwrap()
                .safe_to_retry
        );
        assert!(
            !ModelUnavailable::detect("unsupported_model", false)
                .unwrap()
                .safe_to_retry
        );
        assert!(ModelUnavailable::detect("Permission denied reading design.md", true).is_none());
    }
    #[test]
    fn a_tool_not_supported_event_does_not_exclude_the_model() {
        use serde_json::json;
        // The word "model" and "not supported" appear, but only in a tool notice.
        let tool = json!({"type":"error","message":"Tool web_search is not supported","model":"gpt-6-astra"});
        assert!(ModelUnavailable::from_event(&tool, true).is_none());
        // An incidental field (echoed prompt, usage blob) is never judged.
        let echoed =
            json!({"type":"item.completed","text":"the model is not available for x","usage":{}});
        assert!(ModelUnavailable::from_event(&echoed, true).is_none());
        let real = json!({"type":"error","error":{"message":"You do not have access to model gpt-6-astra"}});
        assert!(ModelUnavailable::from_event(&real, true).is_some());
        let code = json!({"error":{"code":"model_not_found"}});
        assert!(ModelUnavailable::from_event(&code, false).is_some());
    }
}
