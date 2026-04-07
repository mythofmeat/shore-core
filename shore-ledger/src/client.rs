//! LedgerClient: compiler-enforced wrapper around LlmClient.

use crate::cache_tracker::{Anomaly, CacheState, CacheTracker, Observation};
use crate::ledger::{CallRow, Ledger};
use crate::pricing::PricingEngine;
use crate::stream::LedgerStream;
use chrono::Utc;
use shore_config::models::ResolvedModel;
use shore_llm_client::types::{GenerateResponse, LlmRequest, Timing, Usage};
use shore_llm_client::{LlmClient, LlmError};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::error;

// ── CallType ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum CallType {
    Message,
    ToolLoop,
    Keepalive,
    Interiority,
    Compaction,
    MemoryAgent,
    Researcher,
}

impl CallType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallType::Message => "message",
            CallType::ToolLoop => "tool_loop",
            CallType::Keepalive => "keepalive",
            CallType::Interiority => "interiority",
            CallType::Compaction => "compaction",
            CallType::MemoryAgent => "memory_agent",
            CallType::Researcher => "researcher",
        }
    }
}

// ── record_call ─────────────────────────────────────────────────────────────

pub(crate) fn record_call(
    ledger: &Ledger,
    pricing: &PricingEngine,
    cache_trackers: &Mutex<HashMap<String, CacheTracker>>,
    provider: &str,
    model: &str,
    call_type: CallType,
    character: &str,
    usage: &Usage,
    timing: &Timing,
    finish_reason: &str,
    thinking_enabled: bool,
    cache_ttl: Option<String>,
) {
    let ts = Utc::now().to_rfc3339();

    // Cache tracking (Anthropic only)
    let (cache_state, cache_anomaly) = if provider == "anthropic" {
        let obs = Observation {
            ts: ts.clone(),
            model: model.to_string(),
            thinking_enabled,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_creation_tokens,
            call_type: call_type.as_str().to_string(),
        };

        let mut trackers = cache_trackers.lock().unwrap();
        let tracker = trackers
            .entry(character.to_string())
            .or_insert_with(CacheTracker::new);
        let result = tracker.observe(&obs);

        let state_str = match result.state {
            CacheState::Cold => "cold",
            CacheState::Warm => "warm",
        };

        let anomaly_str = result.anomaly.map(|a| match a {
            Anomaly::UnexpectedRead => "unexpected_read",
            Anomaly::UnexpectedWrite => "unexpected_write",
        });

        if let Some(anomaly) = &anomaly_str {
            error!(
                provider,
                model,
                character,
                call_type = call_type.as_str(),
                cache_state = state_str,
                anomaly,
                cache_read_tokens = usage.cache_read_tokens,
                cache_creation_tokens = usage.cache_creation_tokens,
                "Cache anomaly detected"
            );
        }

        (Some(state_str.to_string()), anomaly_str.map(String::from))
    } else {
        (None, None)
    };

    // Cost calculation (sync — cached pricing only, no fetch)
    let cost = pricing
        .calculate_cost(
            provider,
            model,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens,
            cache_ttl.as_deref(),
        )
        .ok()
        .flatten();

    let row = CallRow {
        ts,
        character: character.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        call_type: call_type.as_str().to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_creation_tokens,
        cache_ttl,
        total_ms: timing.total_ms,
        ttft_ms: timing.time_to_first_token_ms,
        finish_reason: finish_reason.to_string(),
        thinking_enabled,
        cache_state,
        cache_anomaly,
        input_cost: cost.as_ref().map(|c| c.input),
        output_cost: cost.as_ref().map(|c| c.output),
        cache_read_cost: cost.as_ref().map(|c| c.cache_read),
        cache_write_cost: cost.as_ref().map(|c| c.cache_write),
        total_cost: cost.as_ref().map(|c| c.total),
    };

    if let Err(e) = ledger.insert(&row) {
        error!(error = %e, "Failed to insert call row into ledger");
    }
}

// ── LedgerClient ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LedgerClient {
    inner: LlmClient,
    ledger: Arc<Ledger>,
    cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    pricing: Arc<PricingEngine>,
}

impl LedgerClient {
    /// Create a new LedgerClient backed by a file database at `db_path`.
    pub fn new(
        client: LlmClient,
        db_path: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ledger = Arc::new(Ledger::open(db_path)?);
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        Ok(Self {
            inner: client,
            ledger,
            cache_trackers: Arc::new(Mutex::new(HashMap::new())),
            pricing,
        })
    }

    /// Create a LedgerClient with an in-memory database (tests only).
    #[cfg(test)]
    pub fn new_in_memory(client: LlmClient) -> Self {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        Self {
            inner: client,
            ledger,
            cache_trackers: Arc::new(Mutex::new(HashMap::new())),
            pricing,
        }
    }

    /// Passthrough to `LlmClient::build_request`.
    pub fn build_request(
        model: &ResolvedModel,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        provider_options: Option<serde_json::Value>,
    ) -> Result<LlmRequest, LlmError> {
        LlmClient::build_request(model, messages, system, tools, provider_options)
    }

    /// Send a non-streaming request, then record the call to the ledger.
    ///
    /// Calls `pricing.get_or_fetch()` first for lazy pricing resolution.
    pub async fn generate(
        &self,
        request: &LlmRequest,
        call_type: CallType,
        character: &str,
        thinking_enabled: bool,
    ) -> Result<GenerateResponse, LlmError> {
        // Lazy pricing fetch (best-effort, don't block on failure)
        let provider_key = request
            .provider_key
            .as_deref()
            .unwrap_or(request.sdk.as_str());
        self.pricing
            .get_or_fetch(provider_key, &request.model)
            .await;

        let resp = self.inner.generate(request, None).await?;

        let cache_ttl = request
            .provider_options
            .as_ref()
            .and_then(|opts| opts.get("cache_ttl"))
            .and_then(|v| v.as_str())
            .map(String::from);

        record_call(
            &self.ledger,
            &self.pricing,
            &self.cache_trackers,
            provider_key,
            &request.model,
            call_type,
            character,
            &resp.usage,
            &resp.timing,
            &resp.finish_reason,
            thinking_enabled,
            cache_ttl,
        );

        Ok(resp)
    }

    /// Send a streaming request, returning a LedgerStream that must be finalized.
    ///
    /// Calls `pricing.get_or_fetch()` first for lazy pricing resolution.
    /// The caller MUST call `finalize()` on the returned stream after consumption,
    /// otherwise the API call will not be recorded (and a tracing::error is emitted on drop).
    pub async fn stream_raw(
        &self,
        request: &LlmRequest,
        call_type: CallType,
        character: &str,
        thinking_enabled: bool,
    ) -> Result<LedgerStream, LlmError> {
        let provider_key = request
            .provider_key
            .as_deref()
            .unwrap_or(request.sdk.as_str());
        self.pricing
            .get_or_fetch(provider_key, &request.model)
            .await;

        let reader = self.inner.stream_raw(request, None).await?;

        let cache_ttl = request
            .provider_options
            .as_ref()
            .and_then(|opts| opts.get("cache_ttl"))
            .and_then(|v| v.as_str())
            .map(String::from);

        Ok(LedgerStream::new(
            reader,
            provider_key.to_string(),
            request.model.clone(),
            call_type,
            character.to_string(),
            thinking_enabled,
            cache_ttl,
            self.ledger.clone(),
            self.pricing.clone(),
            self.cache_trackers.clone(),
        ))
    }

    /// Access the inner LlmClient (for embed/image_generate passthrough).
    pub fn inner(&self) -> &LlmClient {
        &self.inner
    }

    /// Access the ledger (for CLI queries).
    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// Access the pricing engine (for CLI refresh/recalculate).
    pub fn pricing(&self) -> &Arc<PricingEngine> {
        &self.pricing
    }

    /// Reconstruct cache tracker state from the last Anthropic call in the DB.
    pub fn reconstruct_cache_state(&self, character: &str, ttl_secs: u64) {
        match self.ledger.last_anthropic_call(character) {
            Ok(Some(row)) => {
                let tracker = CacheTracker::reconstruct(
                    &row.ts,
                    &row.model,
                    row.thinking_enabled,
                    row.cache_read_tokens,
                    ttl_secs,
                );
                self.cache_trackers
                    .lock()
                    .unwrap()
                    .insert(character.to_string(), tracker);
            }
            Ok(None) => {} // No prior call — start cold
            Err(e) => {
                error!(
                    error = %e,
                    character,
                    "Failed to read last Anthropic call for cache reconstruction"
                );
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_tracker::CacheTracker;
    use crate::ledger::Ledger;
    use crate::pricing::PricingEngine;
    use std::sync::Arc;

    fn test_parts() -> (
        Arc<Ledger>,
        Arc<PricingEngine>,
        Arc<Mutex<HashMap<String, CacheTracker>>>,
    ) {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        let trackers = Arc::new(Mutex::new(HashMap::new()));
        (ledger, pricing, trackers)
    }

    #[test]
    fn record_inserts_row() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            "anthropic",
            "claude-opus-4-6",
            CallType::Message,
            "aria",
            &Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
            &Timing {
                total_ms: 1500,
                time_to_first_token_ms: 0,
            },
            "end_turn",
            false,
            None,
        );
        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].character, "aria");
        assert_eq!(rows[0].call_type, "message");
    }

    #[test]
    fn record_updates_cache_tracker() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            "anthropic",
            "claude-opus-4-6",
            CallType::Message,
            "aria",
            &Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 500,
            },
            &Timing {
                total_ms: 1500,
                time_to_first_token_ms: 0,
            },
            "end_turn",
            true,
            None,
        );
        let map = trackers.lock().unwrap();
        let tracker = map.get("aria").unwrap();
        assert_eq!(tracker.state(), crate::cache_tracker::CacheState::Warm);
    }

    #[test]
    fn non_anthropic_skips_cache_tracker() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            "openai",
            "gpt-4o",
            CallType::Message,
            "aria",
            &Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
            &Timing {
                total_ms: 500,
                time_to_first_token_ms: 0,
            },
            "stop",
            false,
            None,
        );
        let rows = ledger.recent(1).unwrap();
        assert!(rows[0].cache_state.is_none());
        assert!(!trackers.lock().unwrap().contains_key("aria"));
    }

    #[test]
    fn call_type_as_str() {
        assert_eq!(CallType::Message.as_str(), "message");
        assert_eq!(CallType::ToolLoop.as_str(), "tool_loop");
        assert_eq!(CallType::Keepalive.as_str(), "keepalive");
        assert_eq!(CallType::Interiority.as_str(), "interiority");
        assert_eq!(CallType::Compaction.as_str(), "compaction");
        assert_eq!(CallType::MemoryAgent.as_str(), "memory_agent");
        assert_eq!(CallType::Researcher.as_str(), "researcher");
    }

    #[test]
    fn record_maps_cache_creation_to_cache_write() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            "anthropic",
            "claude-opus-4-6",
            CallType::Message,
            "aria",
            &Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 80,
                cache_creation_tokens: 200,
            },
            &Timing {
                total_ms: 1500,
                time_to_first_token_ms: 0,
            },
            "end_turn",
            true,
            None,
        );
        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows[0].cache_write_tokens, 200);
        assert_eq!(rows[0].cache_read_tokens, 80);
    }

    #[test]
    fn record_stores_cache_ttl() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            "anthropic",
            "claude-opus-4-6",
            CallType::Message,
            "aria",
            &Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
            &Timing {
                total_ms: 1500,
                time_to_first_token_ms: 0,
            },
            "end_turn",
            false,
            Some("5m".to_string()),
        );
        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cache_ttl, Some("5m".to_string()));
    }
}
