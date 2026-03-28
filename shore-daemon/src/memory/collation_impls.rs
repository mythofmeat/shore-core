//! Production implementation of the `CollationLlm` trait.
//!
//! `RealCollationLlm` — sends prompts to shore-llm via `LlmClient`, parses
//! JSON responses into the typed collation structs.

use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;
use serde_json::json;

use shore_config::models::ResolvedModel;
use shore_llm_client::types::ContentBlock;
use shore_llm_client::LlmClient;

use super::collation::{
    CollateMerge, CollationError, CollationLlm, EntityNormalization, TidySplit,
};

// ---------------------------------------------------------------------------
// JSON response wrappers (top-level envelope only)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TidyResponse {
    splits: Vec<TidySplit>,
}

#[derive(Deserialize)]
struct CollateResponse {
    merges: Vec<CollateMerge>,
}

#[derive(Deserialize)]
struct NormalizeResponse {
    normalizations: Vec<EntityNormalization>,
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

// ---------------------------------------------------------------------------
// RealCollationLlm
// ---------------------------------------------------------------------------

/// Production `CollationLlm` backed by `LlmClient` (Unix socket to shore-llm).
///
/// Sends prompts, receives raw text, parses JSON into typed collation structs.
pub struct RealCollationLlm {
    client: LlmClient,
    model: ResolvedModel,
}

impl RealCollationLlm {
    pub fn new(client: LlmClient, model: ResolvedModel) -> Self {
        Self { client, model }
    }

    /// Send a prompt to the LLM and return the raw text response.
    async fn generate(&self, prompt: &str) -> Result<String, CollationError> {
        let messages = vec![json!({"role": "user", "content": prompt})];

        let request = LlmClient::build_request(&self.model, messages, None, None, None)
            .map_err(|e| CollationError::Llm(e.to_string()))?;

        let resp = self
            .client
            .generate(&request, None)
            .await
            .map_err(|e| CollationError::Llm(e.to_string()))?;

        // Extract text from content blocks, falling back to content field.
        let text = if resp.content_blocks.is_empty() {
            resp.content.clone()
        } else {
            resp.content_blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        };

        Ok(text)
    }
}

impl CollationLlm for RealCollationLlm {
    fn tidy(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TidySplit>, CollationError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let raw = self.generate(&prompt).await?;
            let json_str = extract_json(&raw);
            let resp: TidyResponse = serde_json::from_str(json_str)
                .map_err(|e| CollationError::Llm(format!("failed to parse tidy JSON: {e}")))?;
            Ok(resp.splits)
        })
    }

    fn collate(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CollateMerge>, CollationError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let raw = self.generate(&prompt).await?;
            let json_str = extract_json(&raw);
            let resp: CollateResponse = serde_json::from_str(json_str)
                .map_err(|e| CollationError::Llm(format!("failed to parse collate JSON: {e}")))?;
            Ok(resp.merges)
        })
    }

    fn normalize_entities(
        &self,
        prompt: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<EntityNormalization>, CollationError>> + Send + '_>,
    > {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let raw = self.generate(&prompt).await?;
            let json_str = extract_json(&raw);
            let resp: NormalizeResponse = serde_json::from_str(json_str).map_err(|e| {
                CollationError::Llm(format!("failed to parse normalize JSON: {e}"))
            })?;
            Ok(resp.normalizations)
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
        let input = r#"{"splits":[]}"#;
        assert_eq!(extract_json(input), r#"{"splits":[]}"#);
    }

    #[test]
    fn extract_json_fenced() {
        let input = "```json\n{\"splits\":[]}\n```";
        assert_eq!(extract_json(input), r#"{"splits":[]}"#);
    }

    #[test]
    fn extract_json_fenced_no_lang() {
        let input = "```\n{\"merges\":[]}\n```";
        assert_eq!(extract_json(input), r#"{"merges":[]}"#);
    }

    #[test]
    fn extract_json_with_whitespace() {
        let input = "  \n  {\"splits\":[]}  \n  ";
        assert_eq!(extract_json(input), r#"{"splits":[]}"#);
    }

    #[test]
    fn parse_tidy_response() {
        let json = r#"{"splits":[{"original_entry_id":"e1","replacements":[{"summary_text":"fact","topic_tags":"tag","topic_key":"key","confidence":0.9}]}]}"#;
        let resp: TidyResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.splits.len(), 1);
        assert_eq!(resp.splits[0].original_entry_id, "e1");
        assert_eq!(resp.splits[0].replacements.len(), 1);
    }

    #[test]
    fn parse_collate_response() {
        let json = r#"{"merges":[{"source_entry_ids":["e1","e2"],"merged_summary":"combined","merged_topic_tags":"t","merged_topic_key":"k","merged_confidence":0.85}]}"#;
        let resp: CollateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.merges.len(), 1);
        assert_eq!(resp.merges[0].source_entry_ids, vec!["e1", "e2"]);
    }

    #[test]
    fn parse_normalize_response() {
        let json = r#"{"normalizations":[{"canonical_name":"Bob","duplicate_names":["Bobby","Robert"]}]}"#;
        let resp: NormalizeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.normalizations.len(), 1);
        assert_eq!(resp.normalizations[0].duplicate_names.len(), 2);
    }

    #[test]
    fn parse_empty_responses() {
        let _: TidyResponse = serde_json::from_str(r#"{"splits":[]}"#).unwrap();
        let _: CollateResponse = serde_json::from_str(r#"{"merges":[]}"#).unwrap();
        let _: NormalizeResponse = serde_json::from_str(r#"{"normalizations":[]}"#).unwrap();
    }
}
