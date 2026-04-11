//! Production implementation of the `CollationLlm` trait.
//!
//! `RealCollationLlm` — sends prompts to shore-llm via `LlmClient`, parses
//! JSON responses into the typed collation structs.

use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;
use serde_json::json;

use shore_config::models::ResolvedModel;
use shore_ledger::{CallType, LedgerClient};

use super::collation::{CollationError, CollationLlm, RefineAction};

// ---------------------------------------------------------------------------
// JSON response wrapper
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RefineResponse {
    actions: Vec<RefineAction>,
}

// ---------------------------------------------------------------------------
// JSON extraction helper
// ---------------------------------------------------------------------------

/// Strip markdown fencing (```json ... ```) and leading/trailing whitespace
/// so that `serde_json::from_str` can parse the payload.
fn extract_json(raw: &str) -> &str {
    let trimmed = raw.trim();

    // Strip ```json ... ``` or ``` ... ```
    let body = if trimmed.starts_with("```") {
        let start = trimmed.find('\n').map(|i| i + 1).unwrap_or(3);
        let end = trimmed.rfind("```").unwrap_or(trimmed.len());
        &trimmed[start..end]
    } else {
        trimmed
    };

    body.trim()
}

/// Truncate raw LLM output for error messages.
fn truncate_raw(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

// ---------------------------------------------------------------------------
// RealCollationLlm
// ---------------------------------------------------------------------------

/// Production `CollationLlm` backed by `LedgerClient` (ledger-tracked LLM calls).
///
/// Sends prompts, receives raw text, parses JSON into typed collation structs.
pub struct RealCollationLlm {
    client: LedgerClient,
    model: ResolvedModel,
    character: String,
}

impl RealCollationLlm {
    pub fn new(client: LedgerClient, model: ResolvedModel, character: String) -> Self {
        Self { client, model, character }
    }

    /// Send a prompt to the LLM and return the raw text response.
    async fn generate(&self, prompt: &str) -> Result<String, CollationError> {
        let messages = vec![json!({"role": "user", "content": prompt})];

        let request = LedgerClient::build_request(&self.model, messages, None, None, None)
            .map_err(|e| CollationError::Llm(e.to_string()))?;

        let resp = self
            .client
            .generate(&request, CallType::Collation, &self.character, false)
            .await
            .map_err(|e| CollationError::Llm(e.to_string()))?;

        Ok(resp.extract_text())
    }
}

impl CollationLlm for RealCollationLlm {
    fn refine(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RefineAction>, CollationError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let raw = self.generate(&prompt).await?;
            let json_str = extract_json(&raw);
            if json_str.is_empty() {
                return Ok(vec![]);
            }
            let resp: RefineResponse = serde_json::from_str(json_str).map_err(|e| {
                CollationError::Llm(format!(
                    "failed to parse refine JSON: {e}\nraw response: {}",
                    truncate_raw(&raw, 500),
                ))
            })?;
            Ok(resp.actions)
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_plain() {
        let input = r#"{"actions":[]}"#;
        assert_eq!(extract_json(input), r#"{"actions":[]}"#);
    }

    #[test]
    fn extract_json_fenced() {
        let input = "```json\n{\"actions\":[]}\n```";
        assert_eq!(extract_json(input), r#"{"actions":[]}"#);
    }

    #[test]
    fn extract_json_fenced_no_lang() {
        let input = "```\n{\"actions\":[]}\n```";
        assert_eq!(extract_json(input), r#"{"actions":[]}"#);
    }

    #[test]
    fn extract_json_with_whitespace() {
        let input = "  \n  {\"actions\":[]}  \n  ";
        assert_eq!(extract_json(input), r#"{"actions":[]}"#);
    }

    #[test]
    fn parse_refine_response_merge() {
        let json = r#"{"actions":[{"action":"merge","source_entry_ids":["e1","e2"],"result":{"summary_text":"combined","topic_tags":"t","topic_key":"k","confidence":0.85},"reason":"dup"}]}"#;
        let resp: RefineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.actions.len(), 1);
        match &resp.actions[0] {
            RefineAction::Merge {
                source_entry_ids,
                result,
                reason,
            } => {
                assert_eq!(source_entry_ids, &vec!["e1", "e2"]);
                assert_eq!(result.summary_text, "combined");
                assert_eq!(reason, "dup");
            }
            _ => panic!("expected merge"),
        }
    }

    #[test]
    fn parse_refine_response_split() {
        let json = r#"{"actions":[{"action":"split","source_entry_id":"e1","results":[{"summary_text":"a","topic_tags":"t","topic_key":"k","confidence":0.9},{"summary_text":"b","topic_tags":"t","topic_key":"k","confidence":0.8}],"reason":"broad"}]}"#;
        let resp: RefineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.actions.len(), 1);
        match &resp.actions[0] {
            RefineAction::Split {
                source_entry_id,
                results,
                ..
            } => {
                assert_eq!(source_entry_id, "e1");
                assert_eq!(results.len(), 2);
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn parse_refine_response_update() {
        let json = r#"{"actions":[{"action":"update","entry_id":"e1","result":{"summary_text":"better","topic_tags":"t","topic_key":"k","confidence":0.9},"reason":"clarity"}]}"#;
        let resp: RefineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.actions.len(), 1);
        match &resp.actions[0] {
            RefineAction::Update {
                entry_id,
                result,
                reason,
            } => {
                assert_eq!(entry_id, "e1");
                assert_eq!(result.summary_text, "better");
                assert_eq!(reason, "clarity");
            }
            _ => panic!("expected update"),
        }
    }

    #[test]
    fn parse_refine_response_mixed() {
        let json = r#"{"actions":[
            {"action":"merge","source_entry_ids":["e1","e2"],"result":{"summary_text":"m","topic_tags":"t","topic_key":"k","confidence":0.9},"reason":"r"},
            {"action":"split","source_entry_id":"e3","results":[{"summary_text":"a","topic_tags":"t","topic_key":"k","confidence":0.9}],"reason":"r"},
            {"action":"update","entry_id":"e4","result":{"summary_text":"u","topic_tags":"t","topic_key":"k","confidence":0.9},"reason":"r"}
        ]}"#;
        let resp: RefineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.actions.len(), 3);
        assert!(matches!(&resp.actions[0], RefineAction::Merge { .. }));
        assert!(matches!(&resp.actions[1], RefineAction::Split { .. }));
        assert!(matches!(&resp.actions[2], RefineAction::Update { .. }));
    }

    #[test]
    fn parse_refine_response_empty() {
        let resp: RefineResponse = serde_json::from_str(r#"{"actions":[]}"#).unwrap();
        assert!(resp.actions.is_empty());
    }
}
