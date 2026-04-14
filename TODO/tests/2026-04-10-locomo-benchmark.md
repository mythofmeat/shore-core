# LoCoMo Retrieval Benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Benchmark Shore's memory researcher/agent retrieval pipeline against the LoCoMo benchmark (ACL 2024) to get a directional F1 score.

**Architecture:** Single integration test file following the `memory_bench.rs` pattern. Loads LoCoMo observation facts directly into an in-memory MemoryDB (bypassing compaction), routes each QA question through the researcher→agent pipeline, scores answers with token F1, and reports per-category + overall results. Supports stratified sampling to keep costs under $1 for a quick run.

**Tech Stack:** Rust, serde_json (parsing), chrono (date parsing), rand (sampling), rusqlite/FTS5 (retrieval), existing InstrumentedLlm + RealAgentLlm (LLM calls)

---

## File Structure

| Action | File | Purpose |
|--------|------|---------|
| Create | `scripts/locomo-setup.sh` | One-liner to download LoCoMo dataset |
| Modify | `.gitignore` | Add `tests/data/` |
| Create | `shore-daemon/tests/data/.gitkeep` | Empty dir for benchmark data |
| Create | `shore-daemon/tests/locomo_bench.rs` | The benchmark — types, scoring, population, harness |

---

### Task 1: Setup — download script and gitignore

**Files:**
- Create: `scripts/locomo-setup.sh`
- Create: `shore-daemon/tests/data/.gitkeep`
- Modify: `.gitignore`

- [ ] **Step 1: Create the download script**

```bash
#!/usr/bin/env bash
# Download LoCoMo benchmark dataset (locomo10.json) from the official repo.
set -euo pipefail

DEST="shore-daemon/tests/data/locomo10.json"

if [ -f "$DEST" ]; then
    echo "Already exists: $DEST"
    exit 0
fi

mkdir -p "$(dirname "$DEST")"
echo "Downloading LoCoMo dataset..."
curl -fSL \
    "https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json" \
    -o "$DEST"

echo "Saved to $DEST ($(wc -c < "$DEST") bytes)"
```

- [ ] **Step 2: Create the data directory with .gitkeep**

```bash
mkdir -p shore-daemon/tests/data
touch shore-daemon/tests/data/.gitkeep
```

- [ ] **Step 3: Add tests/data/ to .gitignore**

Append to `.gitignore`:
```
# Benchmark datasets (large, downloaded on demand)
shore-daemon/tests/data/*.json
```

- [ ] **Step 4: Run the download script**

```bash
chmod +x scripts/locomo-setup.sh
./scripts/locomo-setup.sh
```

Expected: `locomo10.json` downloaded to `shore-daemon/tests/data/locomo10.json`.

- [ ] **Step 5: Verify the file contains 10 conversations**

```bash
python3 -c "import json; d=json.load(open('shore-daemon/tests/data/locomo10.json')); print(f'{len(d)} conversations')"
```

Expected: `10 conversations`

- [ ] **Step 6: Commit**

```bash
git add scripts/locomo-setup.sh .gitignore shore-daemon/tests/data/.gitkeep
git commit -m "chore: add LoCoMo benchmark data setup script"
```

---

### Task 2: Data types, parsing, and scoring functions

**Files:**
- Create: `shore-daemon/tests/locomo_bench.rs`

This task creates the test file with data types, JSON parsing, date parsing, token F1 scoring, and stratified sampling — all with unit tests. No LLM calls yet.

- [ ] **Step 1: Write the serde types and parsing functions**

Create `shore-daemon/tests/locomo_bench.rs`:

```rust
//! LoCoMo retrieval benchmark.
//!
//! Benchmarks Shore's memory researcher → agent pipeline against the
//! LoCoMo benchmark (ACL 2024) for conversational memory retrieval.
//!
//! The benchmark loads LoCoMo observation facts directly into an in-memory
//! MemoryDB (bypassing compaction), then routes each QA question through
//! the researcher → agent pipeline and scores with token F1.
//!
//! Prerequisites:
//!   - Download dataset: ./scripts/locomo-setup.sh
//!   - OPENROUTER_SHORE_TOOL env var set
//!
//! Run (quick, ~50 questions, ~$0.50):
//!   cargo test -p shore-daemon --test locomo_bench -- --ignored --nocapture 2>&1 | tee locomo.log
//!
//! Run (full conversation, ~200 questions, ~$3):
//!   LOCOMO_SAMPLE=0 cargo test -p shore-daemon --test locomo_bench -- --ignored --nocapture

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::NaiveDateTime;
use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// LoCoMo data types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LocomoConversation {
    sample_id: String,
    conversation: Value,
    qa: Vec<QaPair>,
    observation: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct QaPair {
    question: String,
    /// Ground truth answer (categories 1-4).
    #[serde(default)]
    answer: Option<String>,
    /// Evidence turn IDs, e.g. ["D2:8"].
    #[serde(default)]
    evidence: Vec<String>,
    /// 1=multi-hop, 2=temporal, 3=open-domain, 4=single-hop, 5=adversarial.
    category: u8,
}

/// A single observation fact extracted from the LoCoMo dataset.
#[derive(Debug, Clone)]
struct Observation {
    text: String,
    dia_id: String,
    speaker: String,
    session_num: u32,
    session_date: String,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Load the LoCoMo dataset from disk.
fn load_dataset(path: &std::path::Path) -> Vec<LocomoConversation> {
    let data = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}\nRun: ./scripts/locomo-setup.sh", path.display()));
    serde_json::from_str(&data)
        .unwrap_or_else(|e| panic!("Failed to parse LoCoMo JSON: {e}"))
}

/// Extract all observations from a conversation's `observation` field.
fn extract_observations(conv: &LocomoConversation) -> Vec<Observation> {
    let mut results = Vec::new();
    let obs_map = match conv.observation.as_object() {
        Some(m) => m,
        None => return results,
    };
    let conv_map = conv.conversation.as_object().unwrap();

    for (key, value) in obs_map {
        // Keys: "session_1_observation", "session_2_observation", etc.
        let session_num: u32 = match key
            .strip_prefix("session_")
            .and_then(|s| s.strip_suffix("_observation"))
            .and_then(|s| s.parse().ok())
        {
            Some(n) => n,
            None => continue,
        };

        // Get the session date from conversation.session_N_date_time
        let date_key = format!("session_{session_num}_date_time");
        let session_date = conv_map
            .get(&date_key)
            .and_then(|v| v.as_str())
            .map(|s| parse_locomo_date(s))
            .unwrap_or_default();

        // value is { "SpeakerName": [[text, dia_id], ...], ... }
        let speakers = match value.as_object() {
            Some(m) => m,
            None => continue,
        };

        for (speaker, obs_list) in speakers {
            let observations = match obs_list.as_array() {
                Some(a) => a,
                None => continue,
            };
            for obs in observations {
                let arr = match obs.as_array() {
                    Some(a) if a.len() >= 2 => a,
                    _ => continue,
                };
                let text = arr[0].as_str().unwrap_or("").to_string();
                let dia_id = arr[1].as_str().unwrap_or("").to_string();
                if !text.is_empty() {
                    results.push(Observation {
                        text,
                        dia_id,
                        speaker: speaker.clone(),
                        session_num,
                        session_date: session_date.clone(),
                    });
                }
            }
        }
    }

    results
}

/// Parse LoCoMo date strings like "1:56 pm on 8 May, 2023" to RFC3339-ish.
fn parse_locomo_date(s: &str) -> String {
    // Remove comma: "1:56 pm on 8 May 2023"
    let cleaned = s.replace(",", "");
    // Try parsing "H:MM am on D Month YYYY"
    NaiveDateTime::parse_from_str(&cleaned, "%-I:%M %p on %-d %B %Y")
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_else(|_| {
            // Fallback: just keep the original string
            eprintln!("  [warn] could not parse date: {s:?}");
            s.to_string()
        })
}

// ---------------------------------------------------------------------------
// Token F1 scoring
// ---------------------------------------------------------------------------

/// Tokenize a string for F1 scoring: lowercase, split on non-alphanumeric.
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Compute token-level F1 between prediction and ground truth.
fn token_f1(prediction: &str, ground_truth: &str) -> f64 {
    let pred_tokens = tokenize(prediction);
    let gt_tokens = tokenize(ground_truth);

    if gt_tokens.is_empty() && pred_tokens.is_empty() {
        return 1.0;
    }
    if gt_tokens.is_empty() || pred_tokens.is_empty() {
        return 0.0;
    }

    // Count matches (allowing duplicates like standard token F1).
    let mut gt_remaining: HashMap<&str, usize> = HashMap::new();
    for t in &gt_tokens {
        *gt_remaining.entry(t.as_str()).or_default() += 1;
    }

    let mut matches = 0usize;
    for t in &pred_tokens {
        if let Some(count) = gt_remaining.get_mut(t.as_str()) {
            if *count > 0 {
                *count -= 1;
                matches += 1;
            }
        }
    }

    let precision = matches as f64 / pred_tokens.len() as f64;
    let recall = matches as f64 / gt_tokens.len() as f64;

    if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

/// Score a QA pair based on its category.
///
/// - Category 1 (multi-hop): split ground truth by comma, F1 each sub-answer, average.
/// - Category 3 (open-domain): use text before first ";" in ground truth.
/// - Categories 2, 4: standard token F1.
fn score_qa(prediction: &str, qa: &QaPair) -> f64 {
    let answer = match &qa.answer {
        Some(a) => a.as_str(),
        None => return 0.0, // adversarial or missing
    };

    match qa.category {
        1 => {
            // Multi-hop: split ground truth by comma, average F1 across sub-answers.
            let sub_answers: Vec<&str> = answer.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if sub_answers.is_empty() {
                return 0.0;
            }
            let total: f64 = sub_answers.iter().map(|sa| token_f1(prediction, sa)).sum();
            total / sub_answers.len() as f64
        }
        3 => {
            // Open-domain: use text before first semicolon.
            let trimmed = answer.split(';').next().unwrap_or(answer).trim();
            token_f1(prediction, trimmed)
        }
        2 | 4 => token_f1(prediction, answer),
        _ => 0.0, // category 5 (adversarial) — skipped
    }
}

// ---------------------------------------------------------------------------
// Stratified sampling
// ---------------------------------------------------------------------------

/// Sample up to `per_category` QA pairs from each category (1-4).
/// If `per_category` is 0, return all questions for categories 1-4.
fn stratified_sample(qas: &[QaPair], per_category: usize, seed: u64) -> Vec<QaPair> {
    use rand::seq::SliceRandom;
    use rand::SeedableRng;

    // Filter to categories 1-4 only (skip adversarial).
    let mut by_category: HashMap<u8, Vec<&QaPair>> = HashMap::new();
    for qa in qas {
        if qa.category >= 1 && qa.category <= 4 && qa.answer.is_some() {
            by_category.entry(qa.category).or_default().push(qa);
        }
    }

    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let mut sampled = Vec::new();

    for cat in 1..=4u8 {
        if let Some(items) = by_category.get_mut(&cat) {
            items.shuffle(&mut rng);
            if per_category == 0 {
                sampled.extend(items.iter().map(|q| (*q).clone()));
            } else {
                sampled.extend(items.iter().take(per_category).map(|q| (*q).clone()));
            }
        }
    }

    sampled
}

// ---------------------------------------------------------------------------
// Unit tests (run without API keys)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_f1_exact_match() {
        assert!((token_f1("adoption agencies", "Adoption agencies") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_token_f1_partial() {
        let f1 = token_f1("She researched adoption agencies and schools", "adoption agencies");
        // precision = 2/6, recall = 2/2, F1 = 2*(2/6)*(1.0)/(2/6 + 1.0) = 0.5
        assert!(f1 > 0.45 && f1 < 0.55, "F1 was {f1}");
    }

    #[test]
    fn test_token_f1_no_overlap() {
        assert_eq!(token_f1("completely different words", "adoption agencies"), 0.0);
    }

    #[test]
    fn test_token_f1_empty() {
        assert_eq!(token_f1("", ""), 1.0);
        assert_eq!(token_f1("something", ""), 0.0);
        assert_eq!(token_f1("", "something"), 0.0);
    }

    #[test]
    fn test_score_qa_multihop() {
        let qa = QaPair {
            question: "test".into(),
            answer: Some("adoption agencies, local schools".into()),
            evidence: vec![],
            category: 1,
        };
        let f1 = score_qa("She researched adoption agencies and also visited local schools", &qa);
        assert!(f1 > 0.3, "Multi-hop F1 was {f1}");
    }

    #[test]
    fn test_score_qa_open_domain() {
        let qa = QaPair {
            question: "test".into(),
            answer: Some("chocolate cake; it's her favorite dessert".into()),
            evidence: vec![],
            category: 3,
        };
        // Should only score against "chocolate cake"
        let f1 = score_qa("chocolate cake", &qa);
        assert!((f1 - 1.0).abs() < 1e-9, "Open-domain F1 was {f1}");
    }

    #[test]
    fn test_parse_locomo_date() {
        let result = parse_locomo_date("1:56 pm on 8 May, 2023");
        assert_eq!(result, "2023-05-08T13:56:00");
    }

    #[test]
    fn test_parse_locomo_date_morning() {
        let result = parse_locomo_date("9:30 am on 15 June, 2023");
        assert_eq!(result, "2023-06-15T09:30:00");
    }

    #[test]
    fn test_stratified_sample_limits() {
        let qas: Vec<QaPair> = (0..20)
            .map(|i| QaPair {
                question: format!("q{i}"),
                answer: Some(format!("a{i}")),
                evidence: vec![],
                category: (i % 4 + 1) as u8,
            })
            .collect();
        let sampled = stratified_sample(&qas, 3, 42);
        assert_eq!(sampled.len(), 12); // 3 per category × 4 categories
    }

    #[test]
    fn test_stratified_sample_all() {
        let qas: Vec<QaPair> = (0..20)
            .map(|i| QaPair {
                question: format!("q{i}"),
                answer: Some(format!("a{i}")),
                evidence: vec![],
                category: (i % 4 + 1) as u8,
            })
            .collect();
        let sampled = stratified_sample(&qas, 0, 42);
        assert_eq!(sampled.len(), 20); // all questions
    }

    #[test]
    fn test_stratified_sample_skips_cat5() {
        let qas = vec![
            QaPair { question: "q1".into(), answer: Some("a1".into()), evidence: vec![], category: 4 },
            QaPair { question: "q2".into(), answer: None, evidence: vec![], category: 5 },
        ];
        let sampled = stratified_sample(&qas, 0, 42);
        assert_eq!(sampled.len(), 1);
        assert_eq!(sampled[0].category, 4);
    }
}
```

- [ ] **Step 2: Run unit tests to verify parsing and scoring**

Run: `cargo test -p shore-daemon --test locomo_bench -- --nocapture`
Expected: All unit tests pass (these don't need API keys or the dataset file).

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/tests/locomo_bench.rs
git commit -m "feat: add LoCoMo benchmark — data types, scoring, sampling"
```

---

### Task 3: DB population — observations to memory entries

**Files:**
- Modify: `shore-daemon/tests/locomo_bench.rs`

Add the function that converts LoCoMo observations into MemoryDB entries and a unit test that verifies it with actual dataset data.

- [ ] **Step 1: Add the DB population function**

Add after the `stratified_sample` function, before the `#[cfg(test)]` module:

```rust
use shore_daemon::memory::db::{Entry, MemoryDB};

// ---------------------------------------------------------------------------
// DB population
// ---------------------------------------------------------------------------

/// Populate an in-memory MemoryDB with observation facts from a conversation.
fn populate_db(db: &MemoryDB, conv: &LocomoConversation) -> usize {
    let observations = extract_observations(conv);
    let now = chrono::Local::now().to_rfc3339();

    for (i, obs) in observations.iter().enumerate() {
        let entry = Entry {
            id: format!("{}_s{}_{}", conv.sample_id, obs.session_num, i),
            memory_type: "semantic".to_string(),
            source: format!("locomo:{}", conv.sample_id),
            reason: "benchmark observation".to_string(),
            status: "active".to_string(),
            confidence: 1.0,
            summary_text: obs.text.clone(),
            topic_tags: obs.speaker.clone(),
            topic_key: format!("session_{}", obs.session_num),
            start_timestamp: obs.session_date.clone(),
            end_timestamp: obs.session_date.clone(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        };
        db.create_entry(&entry).expect("Failed to insert entry");
    }

    observations.len()
}
```

- [ ] **Step 2: Add unit test that loads real dataset and populates DB**

Add to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn test_populate_db_with_real_data() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/locomo10.json");
        if !path.exists() {
            eprintln!("Skipping: dataset not found at {}", path.display());
            return;
        }

        let dataset = load_dataset(&path);
        let conv = dataset.iter().find(|c| c.sample_id == "conv-26").unwrap();

        let observations = extract_observations(conv);
        assert!(!observations.is_empty(), "No observations extracted");
        eprintln!("conv-26: {} observations", observations.len());

        // Check we got observations for both speakers
        let speakers: std::collections::HashSet<&str> =
            observations.iter().map(|o| o.speaker.as_str()).collect();
        assert!(speakers.len() >= 2, "Expected at least 2 speakers, got: {speakers:?}");

        // Populate DB
        let db = MemoryDB::open_in_memory().unwrap();
        let count = populate_db(&db, conv);
        assert_eq!(count, observations.len());

        // Verify FTS works on populated data
        let hits = db.search_entries_fts("support group", Some("active"), 10).unwrap();
        eprintln!("FTS 'support group': {} hits", hits.len());
        // conv-26 is Caroline & Melanie — Caroline attends a support group
        assert!(!hits.is_empty(), "FTS should find 'support group' in conv-26 observations");

        // Print category distribution for conv-26
        let mut cat_counts: HashMap<u8, usize> = HashMap::new();
        for qa in &conv.qa {
            *cat_counts.entry(qa.category).or_default() += 1;
        }
        eprintln!("conv-26 QA distribution: {cat_counts:?}");
    }
```

- [ ] **Step 3: Verify the `search_fts` method name is correct**

Check `shore-daemon/src/memory/db.rs` for the FTS search method signature. If the method is named differently (e.g., `search_fts5`, `fts_search`), update the test accordingly.

Run: `grep "pub fn.*fts\|pub fn.*search" shore-daemon/src/memory/db.rs`

Adjust the method call in the test if needed.

- [ ] **Step 4: Run the test**

Run: `cargo test -p shore-daemon --test locomo_bench test_populate_db -- --nocapture`
Expected: PASS — observations extracted, DB populated, FTS finds results.

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/tests/locomo_bench.rs
git commit -m "feat(locomo): DB population from LoCoMo observations + integration test"
```

---

### Task 4: Benchmark harness — researcher pipeline + reporting

**Files:**
- Modify: `shore-daemon/tests/locomo_bench.rs`

Add the InstrumentedLlm wrapper (same pattern as `memory_bench.rs`), model definitions, the main benchmark test function, and result reporting.

- [ ] **Step 1: Add InstrumentedLlm and model definitions**

Add after the `populate_db` function, before the `#[cfg(test)]` module. This is the same `InstrumentedLlm` from `memory_bench.rs` (lines 35-148), plus model definitions:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use shore_config::models::{ResolvedModel, Sdk};
use shore_daemon::memory::agent::{CallerIdentity, MemoryAgent};
use shore_daemon::memory::agent_llm::{AgentLlm, AgentLlmError, AgentLlmResponse};
use shore_daemon::memory::researcher::MemoryResearcher;
use shore_ledger::{CallType, LedgerClient};
use shore_llm_client::LlmClient;

// ---------------------------------------------------------------------------
// Instrumented LLM wrapper — counts calls for cost tracking
// ---------------------------------------------------------------------------

struct InstrumentedLlm {
    inner: shore_daemon::memory::agent_llm::RealAgentLlm,
    call_count: AtomicUsize,
    label: String,
}

impl InstrumentedLlm {
    fn new(client: LedgerClient, label: &str, call_type: CallType) -> Self {
        Self {
            inner: shore_daemon::memory::agent_llm::RealAgentLlm::new(
                client,
                "locomo-bench".to_string(),
                call_type,
            ),
            call_count: AtomicUsize::new(0),
            label: label.to_string(),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

impl AgentLlm for InstrumentedLlm {
    fn generate<'a>(
        &'a self,
        messages: Vec<Value>,
        system: Option<Value>,
        tools: Option<Vec<Value>>,
        model: &'a ResolvedModel,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentLlmResponse, AgentLlmError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let n = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!("  [{} call #{}] → {}", self.label, n, model.model_id);

            let start = Instant::now();
            let result = self.inner.generate(messages, system, tools, model).await;
            let elapsed = start.elapsed().as_millis();

            match &result {
                Ok(resp) => {
                    eprintln!(
                        "  [{} call #{} done] {}ms | {}",
                        self.label, n, elapsed, resp.finish_reason
                    );
                }
                Err(e) => {
                    eprintln!("  [{} call #{} ERROR] {}ms | {}", self.label, n, elapsed, e);
                }
            }

            result
        })
    }
}

// ---------------------------------------------------------------------------
// Model definition
// ---------------------------------------------------------------------------

fn bench_model() -> ResolvedModel {
    // Default: cheapest model for quick benchmarks. Edit to test others.
    ResolvedModel {
        name: "gemini-flash-lite".into(),
        qualified_name: "tools.openrouter.gemini-flash-lite".into(),
        category: "tools".into(),
        provider_key: "openrouter".into(),
        sdk: Sdk::Openai,
        model_id: "google/gemini-2.5-flash-lite".into(),
        api_key_env: Some("OPENROUTER_SHORE_TOOL".into()),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        max_context_tokens: None,
        max_tokens: Some(4096),
        temperature: Some(0.3),  // low temp for consistent benchmark answers
        top_p: None,
        reasoning_effort: None,
        budget_tokens: None,
        cache_ttl: None,
        keepalive_enabled: None,
        keepalive_ttl: None,
        keepalive_max_pings: None,
        openrouter_provider: None,
        vertex_project: None,
        vertex_location: None,
        gemini_generation: None,
        gemini_web_search: None,
        zai_clear_thinking: None,
        zai_subscription: None,
    }
}
```

- [ ] **Step 2: Add per-question runner**

Add after the model definition:

```rust
// ---------------------------------------------------------------------------
// Per-question benchmark runner
// ---------------------------------------------------------------------------

struct QuestionResult {
    question: String,
    ground_truth: String,
    prediction: String,
    category: u8,
    f1: f64,
    researcher_calls: usize,
    agent_calls: usize,
    elapsed_ms: u128,
}

/// Run a single question through the researcher → agent pipeline.
async fn run_question(
    qa: &QaPair,
    researcher: &MemoryResearcher,
    researcher_llm: &InstrumentedLlm,
    researcher_model: &ResolvedModel,
    agent: &MemoryAgent,
    agent_llm: &InstrumentedLlm,
    agent_model: &ResolvedModel,
    db: &MemoryDB,
) -> QuestionResult {
    let r_before = researcher_llm.call_count();
    let a_before = agent_llm.call_count();
    let start = Instant::now();

    let prediction = match researcher
        .research(
            &qa.question,
            researcher_llm,
            researcher_model,
            agent,
            agent_llm,
            agent_model,
            db,
            None,
            None,
        )
        .await
    {
        Ok(text) => text,
        Err(e) => {
            eprintln!("  [ERROR] {e}");
            String::new()
        }
    };

    let elapsed_ms = start.elapsed().as_millis();
    let researcher_calls = researcher_llm.call_count() - r_before;
    let agent_calls = agent_llm.call_count() - a_before;

    let ground_truth = qa.answer.clone().unwrap_or_default();
    let f1 = score_qa(&prediction, qa);

    QuestionResult {
        question: qa.question.clone(),
        ground_truth,
        prediction,
        category: qa.category,
        f1,
        researcher_calls,
        agent_calls,
        elapsed_ms,
    }
}
```

- [ ] **Step 3: Add the main benchmark test**

Add after `run_question`:

```rust
// ---------------------------------------------------------------------------
// Main benchmark
// ---------------------------------------------------------------------------

/// The default conversation to benchmark (smallest, fastest).
const DEFAULT_CONV: &str = "conv-26";

/// Default questions per category for a quick run.
const DEFAULT_SAMPLE: usize = 12;

#[tokio::test]
#[ignore = "Requires OPENROUTER_SHORE_TOOL and locomo10.json"]
async fn locomo_retrieval_benchmark() {
    // ── Configuration ──────────────────────────────────────────────────
    let conv_id = std::env::var("LOCOMO_CONV").unwrap_or_else(|_| DEFAULT_CONV.to_string());
    let sample_size: usize = std::env::var("LOCOMO_SAMPLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SAMPLE);

    let data_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/locomo10.json");
    let model = bench_model();

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("LOCOMO RETRIEVAL BENCHMARK");
    eprintln!("  Conversation: {conv_id}");
    eprintln!("  Sample size:  {} per category (0=all)", sample_size);
    eprintln!("  Model:        {}", model.model_id);
    eprintln!("{}\n", "=".repeat(80));

    // ── Load dataset ───────────────────────────────────────────────────
    let dataset = load_dataset(&data_path);
    let conv = dataset
        .iter()
        .find(|c| c.sample_id == conv_id)
        .unwrap_or_else(|| panic!("Conversation {conv_id} not found in dataset"));

    // ── Populate memory DB ─────────────────────────────────────────────
    let db = MemoryDB::open_in_memory().unwrap();
    let entry_count = populate_db(&db, conv);
    eprintln!("Populated DB with {entry_count} observation entries\n");

    // ── Sample questions ───────────────────────────────────────────────
    let questions = stratified_sample(&conv.qa, sample_size, 42);
    eprintln!("Selected {} questions (categories 1-4)\n", questions.len());

    // ── Setup LLM pipeline ─────────────────────────────────────────────
    let tmp = tempfile::tempdir().unwrap();
    let ledger = LedgerClient::new(LlmClient::new(), &tmp.path().join("ledger.db")).unwrap();
    let researcher_llm = InstrumentedLlm::new(ledger.clone(), "researcher", CallType::Researcher);
    let agent_llm = InstrumentedLlm::new(ledger, "agent", CallType::MemoryAgent);

    let researcher = MemoryResearcher::new(String::new(), String::new());
    let agent = MemoryAgent::one_shot(CallerIdentity::Char, "bench", "user");

    // ── Run benchmark ──────────────────────────────────────────────────
    let mut results: Vec<QuestionResult> = Vec::new();
    let total_start = Instant::now();

    for (i, qa) in questions.iter().enumerate() {
        eprintln!("\n─── Question {}/{} (cat {}) ───", i + 1, questions.len(), qa.category);
        eprintln!("  Q: {}", qa.question);
        eprintln!("  A: {}", qa.answer.as_deref().unwrap_or("N/A"));

        let result = run_question(
            qa,
            &researcher,
            &researcher_llm,
            &model,
            &agent,
            &agent_llm,
            &model,
            &db,
        )
        .await;

        // Truncate prediction for display.
        let pred_preview: String = result.prediction.chars().take(200).collect();
        eprintln!("  P: {pred_preview}");
        eprintln!(
            "  F1: {:.3} | R:{} A:{} | {}ms",
            result.f1, result.researcher_calls, result.agent_calls, result.elapsed_ms
        );

        results.push(result);
    }

    let total_ms = total_start.elapsed().as_millis();

    // ── Report ─────────────────────────────────────────────────────────
    eprintln!("\n\n{}", "=".repeat(80));
    eprintln!("RESULTS — {} (model: {})", conv_id, model.model_id);
    eprintln!("{}", "=".repeat(80));

    let category_names = HashMap::from([
        (1u8, "Multi-hop"),
        (2, "Temporal"),
        (3, "Open-domain"),
        (4, "Single-hop"),
    ]);

    let mut overall_f1_sum = 0.0;
    let mut overall_count = 0;
    let mut total_r_calls = 0;
    let mut total_a_calls = 0;

    for cat in 1..=4u8 {
        let cat_results: Vec<&QuestionResult> = results.iter().filter(|r| r.category == cat).collect();
        if cat_results.is_empty() {
            continue;
        }

        let cat_f1: f64 = cat_results.iter().map(|r| r.f1).sum::<f64>() / cat_results.len() as f64;
        let cat_name = category_names.get(&cat).unwrap_or(&"Unknown");

        eprintln!(
            "  Cat {} ({:<12}): F1 = {:.3}  ({} questions)",
            cat, cat_name, cat_f1, cat_results.len()
        );

        overall_f1_sum += cat_results.iter().map(|r| r.f1).sum::<f64>();
        overall_count += cat_results.len();
        total_r_calls += cat_results.iter().map(|r| r.researcher_calls).sum::<usize>();
        total_a_calls += cat_results.iter().map(|r| r.agent_calls).sum::<usize>();
    }

    let overall_f1 = if overall_count > 0 {
        overall_f1_sum / overall_count as f64
    } else {
        0.0
    };

    eprintln!("{}", "-".repeat(80));
    eprintln!("  OVERALL F1:    {:.3}  ({} questions)", overall_f1, overall_count);
    eprintln!("  Total calls:   {} (researcher: {}, agent: {})", total_r_calls + total_a_calls, total_r_calls, total_a_calls);
    eprintln!("  Total time:    {}ms ({:.1}s)", total_ms, total_ms as f64 / 1000.0);
    eprintln!(
        "  Avg per Q:     {:.0}ms, {:.1} calls",
        total_ms as f64 / overall_count.max(1) as f64,
        (total_r_calls + total_a_calls) as f64 / overall_count.max(1) as f64
    );
    eprintln!("{}", "=".repeat(80));

    // ── Per-question detail table ──────────────────────────────────────
    eprintln!("\n{}", "=".repeat(100));
    eprintln!("PER-QUESTION DETAIL");
    eprintln!("{}", "=".repeat(100));
    eprintln!(
        "{:<4} {:<50} {:>5} {:>5} {:>5} {:>8}",
        "Cat", "Question", "F1", "R", "A", "ms"
    );
    eprintln!("{}", "-".repeat(100));

    for r in &results {
        let q_short: String = r.question.chars().take(48).collect();
        eprintln!(
            "{:<4} {:<50} {:>5.3} {:>5} {:>5} {:>8}",
            r.category, q_short, r.f1, r.researcher_calls, r.agent_calls, r.elapsed_ms
        );
    }
    eprintln!("{}", "=".repeat(100));
}
```

- [ ] **Step 4: Move all use statements to the top of the file**

Consolidate all `use` statements at the top of `locomo_bench.rs`. The file should have a single `use` block at the top:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use chrono::NaiveDateTime;
use serde::Deserialize;
use serde_json::Value;

use shore_config::models::{ResolvedModel, Sdk};
use shore_daemon::memory::agent::{CallerIdentity, MemoryAgent};
use shore_daemon::memory::agent_llm::{AgentLlm, AgentLlmError, AgentLlmResponse};
use shore_daemon::memory::db::{Entry, MemoryDB};
use shore_daemon::memory::researcher::MemoryResearcher;
use shore_ledger::{CallType, LedgerClient};
use shore_llm_client::LlmClient;
```

Remove any inline `use` statements from the body.

- [ ] **Step 5: Verify compilation**

Run: `cargo test -p shore-daemon --test locomo_bench --no-run`
Expected: Compiles without errors.

- [ ] **Step 6: Run unit tests (no API keys needed)**

Run: `cargo test -p shore-daemon --test locomo_bench -- --nocapture`
Expected: All unit tests pass. The `#[ignore]` benchmark is skipped.

- [ ] **Step 7: Commit**

```bash
git add shore-daemon/tests/locomo_bench.rs
git commit -m "feat(locomo): benchmark harness with researcher pipeline + F1 reporting"
```

---

### Task 5: Verification — run the actual benchmark

**Files:** None (execution only)

- [ ] **Step 1: Ensure dataset is downloaded**

```bash
./scripts/locomo-setup.sh
```

- [ ] **Step 2: Run the benchmark (quick mode, ~50 questions)**

```bash
OPENROUTER_SHORE_TOOL=<key> cargo test -p shore-daemon --test locomo_bench locomo_retrieval_benchmark -- --ignored --nocapture 2>&1 | tee locomo.log
```

Expected: Benchmark runs, prints per-category F1 scores and overall F1. Should complete in 5-15 minutes depending on model latency.

- [ ] **Step 3: Review results and fix any issues**

Check `locomo.log` for:
- Errors (API failures, parse errors)
- Suspiciously low F1 on specific categories
- Questions where the researcher returned "No relevant memories found" (indicates FTS isn't finding the observations — may need to adjust topic_tags or add more searchable metadata)

If FTS retrieval is failing, consider adding the full conversation turn text to the observation's `summary_text` rather than just the observation summary.

- [ ] **Step 4: Document results in DECISIONS.md**

Record the benchmark configuration, model used, and F1 results.
