//! LedgerStream: stream wrapper that records on finalization.

use crate::cache_tracker::CacheTracker;
use crate::client::{record_call, CallType};
use crate::ledger::Ledger;
use crate::pricing::PricingEngine;
use shore_llm_client::types::StreamResult;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{BufReader, DuplexStream};
use tracing::error;

pub struct LedgerStream {
    reader: BufReader<DuplexStream>,
    provider: String,
    model: String,
    call_type: CallType,
    character: String,
    thinking_enabled: bool,
    cache_ttl: Option<String>,
    ledger: Arc<Ledger>,
    pricing: Arc<PricingEngine>,
    cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    finalized: bool,
}

impl LedgerStream {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        reader: BufReader<DuplexStream>,
        provider: String,
        model: String,
        call_type: CallType,
        character: String,
        thinking_enabled: bool,
        cache_ttl: Option<String>,
        ledger: Arc<Ledger>,
        pricing: Arc<PricingEngine>,
        cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    ) -> Self {
        Self {
            reader,
            provider,
            model,
            call_type,
            character,
            thinking_enabled,
            cache_ttl,
            ledger,
            pricing,
            cache_trackers,
            finalized: false,
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn new_test(
        provider: String,
        model: String,
        call_type: CallType,
        character: String,
        thinking_enabled: bool,
        cache_ttl: Option<String>,
        ledger: Arc<Ledger>,
        pricing: Arc<PricingEngine>,
        cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    ) -> Self {
        let (_write, read) = tokio::io::duplex(1);
        Self::new(
            BufReader::new(read),
            provider,
            model,
            call_type,
            character,
            thinking_enabled,
            cache_ttl,
            ledger,
            pricing,
            cache_trackers,
        )
    }

    pub fn reader_mut(&mut self) -> &mut BufReader<DuplexStream> {
        &mut self.reader
    }

    pub fn finalize(&mut self, result: &StreamResult) {
        record_call(
            &self.ledger,
            &self.pricing,
            &self.cache_trackers,
            crate::client::RecordCall {
                provider: &self.provider,
                model: &self.model,
                call_type: self.call_type,
                character: &self.character,
                usage: &result.usage,
                timing: &result.timing,
                finish_reason: &result.finish_reason,
                thinking_enabled: self.thinking_enabled,
                cache_ttl: self.cache_ttl.clone(),
            },
        );
        self.finalized = true;
    }

    /// Record a failed/aborted call with zero token usage so the ledger has a
    /// trace of the attempt even when `consume()` returns an error.
    pub fn finalize_error(&mut self) {
        use shore_llm_client::types::{Timing, Usage};
        record_call(
            &self.ledger,
            &self.pricing,
            &self.cache_trackers,
            crate::client::RecordCall {
                provider: &self.provider,
                model: &self.model,
                call_type: self.call_type,
                character: &self.character,
                usage: &Usage::default(),
                timing: &Timing::default(),
                finish_reason: "error",
                thinking_enabled: self.thinking_enabled,
                cache_ttl: self.cache_ttl.clone(),
            },
        );
        self.finalized = true;
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized
    }
}

impl Drop for LedgerStream {
    fn drop(&mut self) {
        if !self.finalized {
            error!(
                provider = %self.provider,
                model = %self.model,
                character = %self.character,
                call_type = self.call_type.as_str(),
                "LedgerStream dropped without finalize — API call was NOT recorded"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CallType;
    use crate::ledger::Ledger;
    use crate::pricing::PricingEngine;
    use shore_llm_client::types::{StreamResult, Timing, Usage};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn finalize_records_to_ledger() {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        let trackers = Arc::new(Mutex::new(HashMap::<String, CacheTracker>::new()));

        let mut stream = LedgerStream::new_test(
            "anthropic".into(),
            "claude-opus-4-6".into(),
            CallType::Message,
            "aria".into(),
            true,
            None,
            ledger.clone(),
            pricing,
            trackers,
        );

        let result = StreamResult {
            content: "Hello".into(),
            model: "claude-opus-4-6".into(),
            finish_reason: "end_turn".into(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 80,
                cache_creation_tokens: 20,
            },
            timing: Timing {
                total_ms: 1500,
                time_to_first_token_ms: 200,
            },
            tool_uses: vec![],
            content_blocks: vec![],
        };

        stream.finalize(&result);
        assert!(stream.is_finalized());

        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[0].cache_read_tokens, 80);
        assert_eq!(rows[0].cache_write_tokens, 20);
    }

    #[test]
    fn finalize_updates_cache_tracker() {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        let trackers = Arc::new(Mutex::new(HashMap::<String, CacheTracker>::new()));

        let mut stream = LedgerStream::new_test(
            "anthropic".into(),
            "claude-opus-4-6".into(),
            CallType::Message,
            "aria".into(),
            true,
            None,
            ledger.clone(),
            pricing,
            trackers.clone(),
        );

        let result = StreamResult {
            content: "Hello".into(),
            model: "claude-opus-4-6".into(),
            finish_reason: "end_turn".into(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 500,
            },
            timing: Timing {
                total_ms: 1500,
                time_to_first_token_ms: 200,
            },
            tool_uses: vec![],
            content_blocks: vec![],
        };

        stream.finalize(&result);
        let map = trackers.lock().unwrap();
        assert_eq!(
            map.get("aria").unwrap().state(),
            crate::cache_tracker::CacheState::Warm
        );
    }
}
