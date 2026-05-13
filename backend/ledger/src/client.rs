//! LedgerClient: compiler-enforced wrapper around LlmClient.

use crate::cache_tracker::{Anomaly, CacheState, CacheTracker, Observation};
use crate::ledger::{CallRow, Ledger};
use crate::pricing::PricingEngine;
use crate::stream::LedgerStream;
use crate::sync::lock_or_recover;
use chrono::Utc;
use shore_config::models::ResolvedModel;
use shore_config::providers::ProviderRegistry;
use shore_config::LoadedConfig;
use shore_llm::credentials::{
    classify_credential_failure, read_candidate_env, resolve_key_candidates, CredentialFailureKind,
    KeyCandidate,
};
use shore_llm::types::{GenerateResponse, LlmRequest, Timing, Usage};
use shore_llm::{LlmClient, LlmError};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, instrument, warn};

// ── CallType ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum CallType {
    Message,
    ToolLoop,
    Keepalive,
    Heartbeat,
    Compaction,
    Dreaming,
    MemoryQuery,
}

impl CallType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallType::Message => "message",
            CallType::ToolLoop => "tool_loop",
            CallType::Keepalive => "keepalive",
            CallType::Heartbeat => "heartbeat",
            CallType::Compaction => "compaction",
            CallType::Dreaming => "dreaming",
            CallType::MemoryQuery => "memory_query",
        }
    }

    fn affects_cache_tracker(&self) -> bool {
        !matches!(self, CallType::Dreaming)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialFallbackEvent {
    pub from_key: String,
    pub to_key: Option<String>,
    pub kind: String,
    pub status: Option<u16>,
    pub reason: String,
    pub warn_on_fallback: bool,
}

// ── record_call ─────────────────────────────────────────────────────────────

pub(crate) struct RecordCall<'a> {
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) call_type: CallType,
    pub(crate) character: &'a str,
    pub(crate) usage: &'a Usage,
    pub(crate) timing: &'a Timing,
    pub(crate) finish_reason: &'a str,
    pub(crate) thinking_enabled: bool,
    pub(crate) cache_ttl: Option<String>,
}

#[instrument(skip(ledger, pricing, cache_trackers, record), fields(call_type = record.call_type.as_str()))]
pub(crate) fn record_call(
    ledger: &Ledger,
    pricing: &PricingEngine,
    cache_trackers: &Mutex<HashMap<String, CacheTracker>>,
    record: RecordCall<'_>,
) {
    let RecordCall {
        provider,
        model,
        call_type,
        character,
        usage,
        timing,
        finish_reason,
        thinking_enabled,
        cache_ttl,
    } = record;
    let ts = Utc::now().to_rfc3339();

    // Cache tracking: run for any call that reports cache metrics (not just
    // provider == "anthropic", which misses OpenRouter-routed Anthropic calls).
    let has_cache_metrics = usage.cache_read_tokens > 0 || usage.cache_creation_tokens > 0;
    let (cache_state, cache_anomaly) =
        if call_type.affects_cache_tracker() && (has_cache_metrics || provider == "anthropic") {
            let obs = Observation {
                ts: ts.clone(),
                model: model.to_string(),
                thinking_enabled,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_creation_tokens,
                call_type: call_type.as_str().to_string(),
            };

            let mut trackers = lock_or_recover("ledger cache tracker map", cache_trackers);
            let tracker = trackers.entry(character.to_string()).or_default();
            let result = tracker.observe(&obs);

            let state_str = match result.state {
                CacheState::Cold => "cold",
                CacheState::Warm => "warm",
            };

            let anomaly_str = result.anomaly.map(|a| match a {
                Anomaly::UnexpectedWrite => "unexpected_write",
                Anomaly::KeepaliveMiss => "keepalive_miss",
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
                shore_llm::cache_forensics::notify_anomaly(
                    character,
                    anomaly,
                    call_type.as_str(),
                    usage.cache_read_tokens,
                    usage.cache_creation_tokens,
                );
            }

            (Some(state_str.to_string()), anomaly_str.map(String::from))
        } else {
            (None, None)
        };

    // Cost calculation (sync — cached pricing only, no fetch)
    let priced_cost = pricing
        .calculate_cost(crate::pricing::CostRequest {
            provider,
            model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_creation_tokens,
            cache_ttl: cache_ttl.as_deref(),
        })
        .ok()
        .flatten();
    let total_cost_override = usage.total_cost_usd;

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
        input_cost: if total_cost_override.is_some() {
            None
        } else {
            priced_cost.as_ref().map(|c| c.input)
        },
        output_cost: if total_cost_override.is_some() {
            None
        } else {
            priced_cost.as_ref().map(|c| c.output)
        },
        cache_read_cost: if total_cost_override.is_some() {
            None
        } else {
            priced_cost.as_ref().map(|c| c.cache_read)
        },
        cache_write_cost: if total_cost_override.is_some() {
            None
        } else {
            priced_cost.as_ref().map(|c| c.cache_write)
        },
        total_cost: total_cost_override.or_else(|| priced_cost.as_ref().map(|c| c.total)),
    };

    // Cache forensics: log response-side data for ALL cache events.
    // Uses call_id=0 since we don't have the request-side correlation ID
    // here — but character + call_type + timestamp provide enough context.
    if has_cache_metrics && shore_llm::cache_forensics::is_enabled() {
        shore_llm::cache_forensics::log_response(shore_llm::cache_forensics::ResponseLog {
            call_id: 0, // no request-side correlation for streaming path
            model,
            character,
            call_type: call_type.as_str(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
        });
    }

    info!(
        provider,
        model,
        character,
        call_type = call_type.as_str(),
        input_tokens = usage.input_tokens,
        output_tokens = usage.output_tokens,
        total_cost = total_cost_override.or_else(|| priced_cost.as_ref().map(|c| c.total)),
        "LLM call recorded"
    );
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
    pub fn new(client: LlmClient, db_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
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
    ///
    /// Honors only the per-model `api_key_env`. Callers that have a
    /// `ProviderRegistry` available should prefer
    /// [`Self::build_request_with_provider_keys`] so users with
    /// `[providers.<name>].keys` (and no per-model `api_key_env`) don't
    /// hit `MissingApiKey` on non-streaming paths.
    pub fn build_request(
        model: &ResolvedModel,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        provider_options: Option<serde_json::Value>,
    ) -> Result<LlmRequest, LlmError> {
        LlmClient::build_request(model, messages, system, tools, provider_options)
    }

    /// Passthrough to `LlmClient::build_request_with_provider_keys`.
    pub fn build_request_with_provider_keys(
        model: &ResolvedModel,
        registry: &shore_config::providers::ProviderRegistry,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        provider_options: Option<serde_json::Value>,
    ) -> Result<LlmRequest, LlmError> {
        LlmClient::build_request_with_provider_keys(
            model,
            registry,
            messages,
            system,
            tools,
            provider_options,
        )
    }

    /// Send a non-streaming request, then record the call to the ledger.
    ///
    /// Calls `pricing.get_or_fetch()` first for lazy pricing resolution.
    #[instrument(skip(self, request, call_type), fields(model = %request.model, call_type = call_type.as_str()))]
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
        debug!(
            model = request.model,
            call_type = call_type.as_str(),
            character,
            "generate: sending request"
        );
        self.pricing
            .get_or_fetch(provider_key, &request.model)
            .await;

        let resp = match self.inner.generate(request).await {
            Ok(r) => r,
            Err(e) => {
                // Log the failure to the forensic log so keepalive and other
                // errors are diagnosable from disk, not just journald.
                shore_llm::cache_forensics::log_error(
                    0,
                    &request.model,
                    character,
                    call_type.as_str(),
                    &e.to_string(),
                );
                return Err(e);
            }
        };
        debug!(
            model = request.model,
            call_type = call_type.as_str(),
            finish_reason = resp.finish_reason,
            "generate: response received"
        );

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
            RecordCall {
                provider: provider_key,
                model: &request.model,
                call_type,
                character,
                usage: &resp.usage,
                timing: &resp.timing,
                finish_reason: &resp.finish_reason,
                thinking_enabled,
                cache_ttl,
            },
        );

        Ok(resp)
    }

    /// Send a non-streaming request with the same ordered provider-key
    /// fallback policy used by chat streaming.
    ///
    /// Every call starts at the first enabled key. Credential-shaped failures
    /// such as missing keys, rejected keys, exhausted quota, or exhausted
    /// budget rotate to the next configured key. Transient/provider failures
    /// still return normally so callers can apply their own retry/backoff
    /// policy.
    pub async fn generate_with_credential_fallback(
        &self,
        request: &mut LlmRequest,
        resolved: &ResolvedModel,
        providers: &ProviderRegistry,
        call_type: CallType,
        character: &str,
        thinking_enabled: bool,
    ) -> Result<(GenerateResponse, Vec<CredentialFallbackEvent>), LlmError> {
        if matches!(resolved.sdk, shore_config::models::Sdk::ClaudeCode) {
            request.api_key.clear();
            return self
                .generate(request, call_type, character, thinking_enabled)
                .await
                .map(|resp| (resp, Vec::new()));
        }

        let candidates = resolve_key_candidates(&resolved.provider_key, providers, resolved);
        if candidates.is_empty() {
            return Err(LlmError::MissingApiKey {
                var: format!("provider '{}' has no enabled keys", resolved.provider_key),
            });
        }

        debug!(
            provider = %resolved.provider_key,
            model = %resolved.qualified_name,
            call_type = call_type.as_str(),
            candidates = candidates.len(),
            "generate_with_credential_fallback starting"
        );

        let total = candidates.len();
        let mut events = Vec::new();
        let mut last_err: Option<LlmError> = None;

        for (i, cand) in candidates.iter().enumerate() {
            let next_cand = candidates.get(i + 1);

            let api_key = match read_candidate_env(cand) {
                Some(value) => value,
                None => {
                    let kind = CredentialFailureKind::MissingKey;
                    let reason = format!("env {:?} unset or empty", cand.env);
                    events.push(record_generate_fallback_event(
                        request, resolved, call_type, character, cand, next_cand, kind, None,
                        &reason,
                    ));
                    last_err = Some(LlmError::MissingApiKey {
                        var: cand.env.clone(),
                    });
                    if next_cand.is_some() {
                        continue;
                    }
                    break;
                }
            };

            request.api_key = api_key;

            match self
                .generate(request, call_type, character, thinking_enabled)
                .await
            {
                Ok(resp) => return Ok((resp, events)),
                Err(e) => {
                    let kind = classify_credential_failure(&resolved.provider_key, &e);
                    if !kind.should_rotate() {
                        return Err(e);
                    }

                    let status = match &e {
                        LlmError::HttpStatus { status, .. } => Some(*status),
                        _ => None,
                    };
                    let reason = sanitize_fallback_reason(&e);
                    events.push(record_generate_fallback_event(
                        request, resolved, call_type, character, cand, next_cand, kind, status,
                        &reason,
                    ));
                    last_err = Some(e);
                    if next_cand.is_none() {
                        break;
                    }
                }
            }
        }

        let final_err = last_err.unwrap_or_else(|| LlmError::MissingApiKey {
            var: format!("all keys for provider '{}' failed", resolved.provider_key),
        });
        error!(
            provider = %resolved.provider_key,
            model = %resolved.qualified_name,
            call_type = call_type.as_str(),
            character,
            candidates = total,
            error = %final_err,
            "generate_with_credential_fallback exhausted all keys"
        );
        Err(final_err)
    }

    /// Resolve the request's model from a loaded config, then apply
    /// non-streaming provider-key fallback. If the request is from an older
    /// persisted state and cannot be matched to the current catalog, fall back
    /// to the existing single-key `generate` behavior.
    pub async fn generate_with_config_fallback(
        &self,
        request: &mut LlmRequest,
        config: &LoadedConfig,
        call_type: CallType,
        character: &str,
        thinking_enabled: bool,
    ) -> Result<(GenerateResponse, Vec<CredentialFallbackEvent>), LlmError> {
        let resolved = resolve_model_for_request(request, config).cloned();
        match resolved {
            Some(resolved) => {
                self.generate_with_credential_fallback(
                    request,
                    &resolved,
                    &config.providers,
                    call_type,
                    character,
                    thinking_enabled,
                )
                .await
            }
            None => {
                debug!(
                    provider = request.provider_key.as_deref().unwrap_or(request.sdk.as_str()),
                    model = %request.model,
                    call_type = call_type.as_str(),
                    character,
                    "generate_with_config_fallback could not resolve model; using single-key request"
                );
                self.generate(request, call_type, character, thinking_enabled)
                    .await
                    .map(|resp| (resp, Vec::new()))
            }
        }
    }

    /// Send a streaming request, returning a LedgerStream that must be finalized.
    ///
    /// Calls `pricing.get_or_fetch()` first for lazy pricing resolution.
    /// The caller MUST call `finalize()` on the returned stream after consumption,
    /// otherwise the API call will not be recorded (and a tracing::error is emitted on drop).
    #[instrument(skip(self, request, call_type), fields(model = %request.model, call_type = call_type.as_str()))]
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
        debug!(
            model = request.model,
            call_type = call_type.as_str(),
            character,
            "stream_raw: opening stream"
        );
        self.pricing
            .get_or_fetch(provider_key, &request.model)
            .await;

        let reader = match self.inner.stream_raw(request).await {
            Ok(r) => r,
            Err(e) => {
                shore_llm::cache_forensics::log_error(
                    0,
                    &request.model,
                    character,
                    call_type.as_str(),
                    &e.to_string(),
                );
                return Err(e);
            }
        };

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

fn resolve_model_for_request<'a>(
    request: &LlmRequest,
    config: &'a LoadedConfig,
) -> Option<&'a ResolvedModel> {
    let provider = request.provider_key.as_deref();
    config
        .models
        .chat
        .values()
        .chain(config.models.tools.values())
        .find(|model| {
            model.model_id == request.model
                && model.sdk == request.sdk
                && provider.map_or(true, |p| p == model.provider_key)
        })
}

#[allow(clippy::too_many_arguments)]
fn record_generate_fallback_event(
    request: &LlmRequest,
    resolved: &ResolvedModel,
    call_type: CallType,
    character: &str,
    from: &KeyCandidate,
    to: Option<&KeyCandidate>,
    kind: CredentialFailureKind,
    status: Option<u16>,
    reason: &str,
) -> CredentialFallbackEvent {
    let to_key = to.map(|candidate| candidate.name.clone());
    warn!(
        provider = %resolved.provider_key,
        model = %resolved.qualified_name,
        call_type = call_type.as_str(),
        character,
        from_key = %from.name,
        to_key = to_key.as_deref().unwrap_or("-"),
        kind = kind.as_str(),
        status = ?status,
        rid = request.rid.as_deref().unwrap_or("-"),
        reason = %reason,
        "rotating provider key after non-streaming credential failure"
    );

    CredentialFallbackEvent {
        from_key: from.name.clone(),
        to_key,
        kind: kind.as_str().to_string(),
        status,
        reason: reason.to_string(),
        warn_on_fallback: from.warn_on_fallback,
    }
}

fn sanitize_fallback_reason(err: &LlmError) -> String {
    match err {
        LlmError::HttpStatus { status, .. } => format!("HTTP {status}"),
        LlmError::MissingApiKey { var } => format!("env {var:?} not set"),
        LlmError::Provider { message } => {
            let truncated = if message.len() > 200 {
                let end = message.floor_char_boundary(200);
                format!("{}...", &message[..end])
            } else {
                message.clone()
            };
            format!("provider error: {truncated}")
        }
        LlmError::Refusal => "model refusal".into(),
        LlmError::IncompleteStream => "stream ended without done event".into(),
        LlmError::Request(_) => "transport error".into(),
        LlmError::Serialize(_) => "request serialization failed".into(),
        LlmError::Deserialize(_) => "response deserialization failed".into(),
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

    type TestParts = (
        Arc<Ledger>,
        Arc<PricingEngine>,
        Arc<Mutex<HashMap<String, CacheTracker>>>,
    );

    fn test_parts() -> TestParts {
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
            RecordCall {
                provider: "anthropic",
                model: "claude-opus-4-6",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: false,
                cache_ttl: None,
            },
        );
        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].character, "aria");
        assert_eq!(rows[0].call_type, "message");
    }

    #[test]
    fn record_uses_provider_total_cost_override() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            RecordCall {
                provider: "claude_code",
                model: "claude-sonnet-4-5",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    total_cost_usd: Some(0.0042),
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: false,
                cache_ttl: None,
            },
        );
        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows[0].total_cost, Some(0.0042));
        assert!(rows[0].input_cost.is_none());
        assert!(rows[0].output_cost.is_none());
        assert!(rows[0].cache_read_cost.is_none());
        assert!(rows[0].cache_write_cost.is_none());
    }

    #[test]
    fn record_updates_cache_tracker() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            RecordCall {
                provider: "anthropic",
                model: "claude-opus-4-6",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 500,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: true,
                cache_ttl: None,
            },
        );
        let map = trackers.lock().unwrap();
        let tracker = map.get("aria").unwrap();
        assert_eq!(tracker.state(), crate::cache_tracker::CacheState::Warm);
    }

    #[test]
    fn dreaming_call_does_not_touch_cache_tracker() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            RecordCall {
                provider: "anthropic",
                model: "claude-opus-4-6",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 400,
                    cache_creation_tokens: 0,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: false,
                cache_ttl: None,
            },
        );
        record_call(
            &ledger,
            &pricing,
            &trackers,
            RecordCall {
                provider: "anthropic",
                model: "claude-opus-4-6",
                call_type: CallType::Dreaming,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: false,
                cache_ttl: None,
            },
        );

        let map = trackers.lock().unwrap();
        let tracker = map.get("aria").unwrap();
        assert_eq!(tracker.state(), crate::cache_tracker::CacheState::Warm);
        assert_eq!(tracker.last_cache_read(), 400);
        let rows = ledger.recent(2).unwrap();
        assert_eq!(rows[0].call_type, "dreaming");
        assert!(rows[0].cache_state.is_none());
        assert!(rows[0].cache_anomaly.is_none());
    }

    #[test]
    fn non_anthropic_skips_cache_tracker() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            RecordCall {
                provider: "openai",
                model: "gpt-4o",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "stop",
                thinking_enabled: false,
                cache_ttl: None,
            },
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
        assert_eq!(CallType::Heartbeat.as_str(), "heartbeat");
        assert_eq!(CallType::Compaction.as_str(), "compaction");
        assert_eq!(CallType::Dreaming.as_str(), "dreaming");
        assert_eq!(CallType::MemoryQuery.as_str(), "memory_query");
    }

    #[test]
    fn record_maps_cache_creation_to_cache_write() {
        let (ledger, pricing, trackers) = test_parts();
        record_call(
            &ledger,
            &pricing,
            &trackers,
            RecordCall {
                provider: "anthropic",
                model: "claude-opus-4-6",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 80,
                    cache_creation_tokens: 200,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: true,
                cache_ttl: None,
            },
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
            RecordCall {
                provider: "anthropic",
                model: "claude-opus-4-6",
                call_type: CallType::Message,
                character: "aria",
                usage: &Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    ..Default::default()
                },
                timing: &Timing {
                    total_ms: 1500,
                    time_to_first_token_ms: 0,
                },
                finish_reason: "end_turn",
                thinking_enabled: false,
                cache_ttl: Some("5m".to_string()),
            },
        );
        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cache_ttl, Some("5m".to_string()));
    }
}
