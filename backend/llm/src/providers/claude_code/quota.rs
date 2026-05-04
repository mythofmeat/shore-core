//! Classify Claude Code subprocess errors against the credential
//! failure taxonomy used by shore-llm's multi-key fallback.
//!
//! The CLI surfaces quota and rate-limit problems through two
//! channels: a `result` event with `is_error: true` and a body like
//! `"out of extra usage"`, or a `rate_limit_event` with
//! `overageStatus: "exhausted"`. Either way the user is throttled
//! against their Max subscription's 5-hour window. We map these to
//! `LlmError::HttpStatus { status: 429, body }` so the existing
//! credential classifier in `crate::credentials` picks them up as
//! `QuotaExhausted` without special-casing.

use crate::LlmError;

/// Inspect a `result` event's text and `is_error` flag and decide
/// whether the failure is quota-shaped enough to surface as a 429.
///
/// Returns `Some(LlmError::HttpStatus)` when the result text trips
/// any of the quota keyword heuristics; returns `None` otherwise so
/// the caller can fall through to a generic `Provider` error.
pub(super) fn classify_result_error(result_text: &str, is_error: bool) -> Option<LlmError> {
    if !is_error {
        return None;
    }
    let lc = result_text.to_lowercase();
    if mentions_quota(&lc) || mentions_rate_limit_overage(&lc) {
        // Prefix the synthesized body with "quota exhausted" so the
        // shared credential classifier in `crate::credentials` reaches
        // `QuotaExhausted` regardless of which specific CLI wording
        // tripped the heuristic. The original text is preserved for
        // human-readable logs.
        return Some(LlmError::HttpStatus {
            status: 429,
            body: format!("quota exhausted (claude_code): {result_text}"),
        });
    }
    None
}

fn mentions_quota(body_lc: &str) -> bool {
    body_lc.contains("out of extra usage")
        || body_lc.contains("quota")
        || body_lc.contains("usage limit")
        || body_lc.contains("monthly limit")
}

fn mentions_rate_limit_overage(body_lc: &str) -> bool {
    body_lc.contains("rate limit") || body_lc.contains("overage")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_extra_usage_is_429() {
        let err = classify_result_error("out of extra usage", true).unwrap();
        match err {
            LlmError::HttpStatus { status, body } => {
                assert_eq!(status, 429);
                assert!(body.contains("out of extra usage"));
                assert!(body.contains("quota"), "body: {body}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn quota_word_caught_case_insensitively() {
        let err = classify_result_error("MONTHLY QUOTA exceeded for account", true);
        assert!(err.is_some());
    }

    #[test]
    fn usage_limit_phrase_caught() {
        assert!(classify_result_error("you have hit your usage limit", true).is_some());
    }

    #[test]
    fn rate_limit_phrase_caught() {
        assert!(classify_result_error("rate limit reached", true).is_some());
    }

    #[test]
    fn benign_error_is_not_classified() {
        assert!(classify_result_error("tool 'frobnicate' not found", true).is_none());
    }

    #[test]
    fn non_error_result_returns_none_even_with_quota_words() {
        // is_error=false means "not an error" regardless of wording.
        assert!(classify_result_error("you have plenty of quota left", false).is_none());
    }

    #[test]
    fn surfaces_through_credential_classifier_as_quota() {
        // End-to-end check: the LlmError we produce must be
        // classified as QuotaExhausted by the existing credential
        // classifier, so callers get the expected behavior without
        // adding claude_code-specific branches.
        let err = classify_result_error("out of extra usage", true).unwrap();
        let kind = crate::credentials::classify_credential_failure("anthropic", &err);
        assert_eq!(kind, crate::credentials::CredentialFailureKind::QuotaExhausted);
    }
}
