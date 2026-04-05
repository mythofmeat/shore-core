//! Model pricing via OpenRouter API with local DB cache.

use crate::ledger::Ledger;
use chrono::Utc;
use rusqlite::params;
use serde_json::Value;
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use tracing::warn;

/// Anthropic 1h cache TTL write price is 4x the 5min price reported by OpenRouter.
const ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER: f64 = 4.0;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_per_token: f64,
    pub output_per_token: f64,
    pub cache_read_per_token: f64,
    pub cache_write_per_token: f64,
}

#[derive(Debug, Clone)]
pub struct CostBreakdown {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

// ── PricingEngine ────────────────────────────────────────────────────────────

pub struct PricingEngine {
    ledger: Arc<Ledger>,
    memory_cache: Mutex<HashMap<String, ModelPricing>>,
}

impl PricingEngine {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        Self {
            ledger,
            memory_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Write pricing to both DB and memory cache.
    pub fn store_pricing(
        &self,
        model_id: &str,
        pricing: &ModelPricing,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.ledger.conn();
        conn.execute(
            r#"INSERT OR REPLACE INTO pricing
                (model_id, input_per_token, output_per_token,
                 cache_read_per_token, cache_write_per_token, fetched_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
            params![
                model_id,
                pricing.input_per_token,
                pricing.output_per_token,
                pricing.cache_read_per_token,
                pricing.cache_write_per_token,
                Utc::now().to_rfc3339(),
            ],
        )?;
        drop(conn);

        self.memory_cache
            .lock()
            .unwrap()
            .insert(model_id.to_string(), pricing.clone());
        Ok(())
    }

    /// Check memory cache first, then DB. On DB hit, populate memory cache.
    pub fn get_cached_pricing(
        &self,
        model_id: &str,
    ) -> Result<Option<ModelPricing>, rusqlite::Error> {
        // Memory cache check
        {
            let cache = self.memory_cache.lock().unwrap();
            if let Some(p) = cache.get(model_id) {
                return Ok(Some(p.clone()));
            }
        }

        // DB check
        let conn = self.ledger.conn();
        let mut stmt = conn.prepare(
            r#"SELECT input_per_token, output_per_token,
                      cache_read_per_token, cache_write_per_token
               FROM pricing WHERE model_id = ?1"#,
        )?;
        let result = stmt
            .query_row(params![model_id], |row| {
                Ok(ModelPricing {
                    input_per_token: row.get(0)?,
                    output_per_token: row.get(1)?,
                    cache_read_per_token: row.get(2)?,
                    cache_write_per_token: row.get(3)?,
                })
            })
            .optional()?;
        drop(stmt);
        drop(conn);

        // Populate memory cache on DB hit
        if let Some(ref p) = result {
            self.memory_cache
                .lock()
                .unwrap()
                .insert(model_id.to_string(), p.clone());
        }

        Ok(result)
    }

    /// HTTP GET to OpenRouter API to fetch per-token pricing.
    /// Applies Anthropic 1h cache write multiplier (4x the 5min price).
    pub async fn fetch_pricing(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Option<ModelPricing>, Box<dyn Error + Send + Sync>> {
        let model_id = to_openrouter_id(provider, model);
        let url = format!("https://openrouter.ai/api/v1/models/{model_id}");

        let resp = reqwest::get(&url).await?;
        if !resp.status().is_success() {
            warn!(
                model_id,
                status = %resp.status(),
                "OpenRouter pricing fetch failed"
            );
            return Ok(None);
        }

        let body: Value = resp.json().await?;
        let pricing_obj = &body["data"]["pricing"];

        let input = parse_price(pricing_obj.get("prompt"));
        let output = parse_price(pricing_obj.get("completion"));
        let cache_read = parse_price(pricing_obj.get("cache_read"));
        let mut cache_write = parse_price(pricing_obj.get("cache_write"));

        // Anthropic 1h cache write is 4x the 5min price OpenRouter reports
        if provider == "anthropic" {
            cache_write *= ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER;
        }

        let pricing = ModelPricing {
            input_per_token: input,
            output_per_token: output,
            cache_read_per_token: cache_read,
            cache_write_per_token: cache_write,
        };

        self.store_pricing(&model_id, &pricing)?;
        Ok(Some(pricing))
    }

    /// Try cached pricing, then fetch from OpenRouter. Returns None if unavailable.
    pub async fn get_or_fetch(
        &self,
        provider: &str,
        model: &str,
    ) -> Option<ModelPricing> {
        let model_id = to_openrouter_id(provider, model);

        match self.get_cached_pricing(&model_id) {
            Ok(Some(p)) => return Some(p),
            Err(e) => {
                warn!(error = %e, "pricing DB read failed");
            }
            Ok(None) => {}
        }

        match self.fetch_pricing(provider, model).await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, model_id, "pricing fetch failed");
                None
            }
        }
    }

    /// Multiply tokens by per-token prices. Returns None if pricing unavailable.
    pub fn calculate_cost(
        &self,
        provider: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
    ) -> Result<Option<CostBreakdown>, rusqlite::Error> {
        let model_id = to_openrouter_id(provider, model);
        let pricing = match self.get_cached_pricing(&model_id)? {
            Some(p) => p,
            None => return Ok(None),
        };

        let input = pricing.input_per_token * input_tokens as f64;
        let output = pricing.output_per_token * output_tokens as f64;
        let cache_read = pricing.cache_read_per_token * cache_read_tokens as f64;
        let cache_write = pricing.cache_write_per_token * cache_write_tokens as f64;

        Ok(Some(CostBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            total: input + output + cache_read + cache_write,
        }))
    }

    /// Delete all rows from pricing table and clear memory cache.
    pub fn clear_cache(&self) -> Result<(), rusqlite::Error> {
        let conn = self.ledger.conn();
        conn.execute("DELETE FROM pricing", [])?;
        drop(conn);
        self.memory_cache.lock().unwrap().clear();
        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Map our (provider, model) pair to OpenRouter's model ID format.
/// For most providers: `"{provider}/{model}"`.
/// For openrouter: the model_id is already in OpenRouter format (e.g. `google/gemini-3.1-flash-lite-preview`).
pub fn to_openrouter_id(provider: &str, model: &str) -> String {
    if provider == "openrouter" {
        // OpenRouter model IDs already include the provider prefix
        model.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

/// Parse a price value from OpenRouter JSON. Prices can be string or number.
/// Returns 0.0 if missing or unparseable.
fn parse_price(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

// Bring optional() into scope for rusqlite queries
use rusqlite::OptionalExtension;

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_engine() -> PricingEngine {
        let ledger = Arc::new(crate::ledger::Ledger::open_in_memory().unwrap());
        PricingEngine::new(ledger)
    }

    #[test]
    fn calculate_cost_with_known_pricing() {
        let engine = test_engine();
        engine
            .store_pricing(
                "anthropic/claude-opus-4-6",
                &ModelPricing {
                    input_per_token: 0.000015,
                    output_per_token: 0.000075,
                    cache_read_per_token: 0.0000015,
                    cache_write_per_token: 0.00001875,
                },
            )
            .unwrap();
        let cost = engine
            .calculate_cost("anthropic", "claude-opus-4-6", 100, 50, 80, 20)
            .unwrap();
        assert!(cost.is_some());
        let c = cost.unwrap();
        assert!((c.input - 0.0015).abs() < 1e-10);
        assert!((c.output - 0.00375).abs() < 1e-10);
        assert!((c.cache_read - 0.00012).abs() < 1e-10);
        assert!((c.cache_write - 0.000375).abs() < 1e-10);
    }

    #[test]
    fn returns_none_for_unknown_model() {
        let engine = test_engine();
        let cost = engine
            .calculate_cost("unknown", "model", 100, 50, 0, 0)
            .unwrap();
        assert!(cost.is_none());
    }

    #[test]
    fn model_id_mapping() {
        assert_eq!(
            to_openrouter_id("anthropic", "claude-opus-4-6"),
            "anthropic/claude-opus-4-6"
        );
        assert_eq!(to_openrouter_id("openai", "gpt-4o"), "openai/gpt-4o");
        // OpenRouter model IDs already have provider prefix — don't double-prefix
        assert_eq!(
            to_openrouter_id("openrouter", "google/gemini-3.1-flash-lite-preview"),
            "google/gemini-3.1-flash-lite-preview"
        );
    }

    #[test]
    fn store_and_retrieve_pricing() {
        let engine = test_engine();
        engine
            .store_pricing(
                "anthropic/claude-opus-4-6",
                &ModelPricing {
                    input_per_token: 0.000015,
                    output_per_token: 0.000075,
                    cache_read_per_token: 0.0000015,
                    cache_write_per_token: 0.00001875,
                },
            )
            .unwrap();
        let pricing = engine
            .get_cached_pricing("anthropic/claude-opus-4-6")
            .unwrap();
        assert!(pricing.is_some());
        assert!((pricing.unwrap().input_per_token - 0.000015).abs() < 1e-10);
    }

    #[test]
    fn clear_cache_removes_all() {
        let engine = test_engine();
        engine
            .store_pricing(
                "test/model",
                &ModelPricing {
                    input_per_token: 0.001,
                    output_per_token: 0.002,
                    cache_read_per_token: 0.0,
                    cache_write_per_token: 0.0,
                },
            )
            .unwrap();
        engine.clear_cache().unwrap();
        let pricing = engine.get_cached_pricing("test/model").unwrap();
        assert!(pricing.is_none());
    }

    #[test]
    fn parse_price_handles_string_and_number() {
        let s = Value::String("0.000015".into());
        assert!((parse_price(Some(&s)) - 0.000015).abs() < 1e-10);

        let n = serde_json::json!(0.000075);
        assert!((parse_price(Some(&n)) - 0.000075).abs() < 1e-10);

        assert!((parse_price(None)).abs() < 1e-10);

        let null = Value::Null;
        assert!((parse_price(Some(&null))).abs() < 1e-10);
    }

    #[test]
    fn db_fallback_populates_memory_cache() {
        let engine = test_engine();
        let pricing = ModelPricing {
            input_per_token: 0.000015,
            output_per_token: 0.000075,
            cache_read_per_token: 0.0000015,
            cache_write_per_token: 0.00001875,
        };
        engine.store_pricing("test/model", &pricing).unwrap();

        // Clear memory cache only, leaving DB intact
        engine.memory_cache.lock().unwrap().clear();

        // Should read from DB and re-populate memory
        let result = engine.get_cached_pricing("test/model").unwrap();
        assert!(result.is_some());
        assert!((result.unwrap().input_per_token - 0.000015).abs() < 1e-10);

        // Verify memory cache was repopulated
        let cache = engine.memory_cache.lock().unwrap();
        assert!(cache.contains_key("test/model"));
    }
}
