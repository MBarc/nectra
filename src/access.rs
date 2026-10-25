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
}
