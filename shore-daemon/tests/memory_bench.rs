//! Memory agent/researcher model benchmark.
//!
//! Tests the full researcher → agent pipeline with different models to compare:
//! - Synthesis quality (subjective — printed for human review)
//! - Number of LLM calls (researcher + agent iterations)
//! - Total latency
//! - Token usage
//!
//! Prerequisites:
//!   - shore-llm running at /run/user/$UID/shore/llm.sock
//!   - OPENROUTER_SHORE_TOOL env var set (for tool-tier models)
//!   - OPENROUTER_SHORE_PRIMARY env var set (for kimi-k2.5)
//!   - qifei memory DB at ~/.local/share/shore/qifei/memory/memory.db
//!
//! Run:
//!   cargo test -p shore-daemon --test memory_bench -- --ignored --nocapture 2>&1 | tee bench.log

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use shore_config::models::{ResolvedModel, Sdk};
use shore_daemon::memory::agent::{CallerIdentity, MemoryAgent};
use shore_daemon::memory::agent_llm::{AgentLlm, AgentLlmError, AgentLlmResponse};
use shore_daemon::memory::db::MemoryDB;
use shore_daemon::memory::researcher::MemoryResearcher;
use shore_llm_client::LlmClient;

// ---------------------------------------------------------------------------
// Instrumented LLM wrapper — counts calls and tokens
// ---------------------------------------------------------------------------

struct InstrumentedLlm {
    inner: shore_daemon::memory::agent_llm::RealAgentLlm,
    call_count: AtomicUsize,
    label: String,
    /// (model_id, input snippet, output snippet, finish_reason)
    calls: Mutex<Vec<CallRecord>>,
}

#[derive(Debug, Clone)]
struct CallRecord {
    model_id: String,
    finish_reason: String,
    tool_calls: Vec<String>,
    elapsed_ms: u128,
}

impl InstrumentedLlm {
    fn new(client: LlmClient, label: &str) -> Self {
        Self {
            inner: shore_daemon::memory::agent_llm::RealAgentLlm::new(client),
            call_count: AtomicUsize::new(0),
            label: label.to_string(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }

    fn summary(&self) -> String {
        let calls = self.calls.lock().unwrap();
        let mut lines = Vec::new();
        for (i, c) in calls.iter().enumerate() {
            let tools_str = if c.tool_calls.is_empty() {
                "(no tools)".to_string()
            } else {
                c.tool_calls.join(", ")
            };
            lines.push(format!(
                "    #{:>2} [{:>5}ms] {} | {}",
                i + 1,
                c.elapsed_ms,
                c.finish_reason,
                tools_str
            ));
        }
        lines.join("\n")
    }
}

impl AgentLlm for InstrumentedLlm {
    fn generate<'a>(
        &'a self,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        model: &'a ResolvedModel,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentLlmResponse, AgentLlmError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let n = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
            let n_tools = tools.as_ref().map(|t| t.len()).unwrap_or(0);
            eprintln!(
                "  [{} call #{}, {} tools, {} msgs] → {}",
                self.label, n, n_tools, messages.len(), model.model_id
            );

            let start = Instant::now();
            let result = self.inner.generate(messages, system, tools, model).await;
            let elapsed = start.elapsed().as_millis();

            match &result {
                Ok(resp) => {
                    let tool_calls: Vec<String> = resp
                        .content_blocks
                        .iter()
                        .filter_map(|b| match b {
                            shore_llm_client::types::ContentBlock::ToolUse {
                                name, ..
                            } => Some(name.clone()),
                            _ => None,
                        })
                        .collect();

                    eprintln!(
                        "  [{} call #{} done] {}ms | {} | tools: {:?} | text: {}b",
                        self.label,
                        n,
                        elapsed,
                        resp.finish_reason,
                        tool_calls,
                        resp.text.len()
                    );

                    self.calls.lock().unwrap().push(CallRecord {
                        model_id: model.model_id.clone(),
                        finish_reason: resp.finish_reason.clone(),
                        tool_calls,
                        elapsed_ms: elapsed,
                    });
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
// Model definitions
// ---------------------------------------------------------------------------

fn openrouter_tool_model(name: &str, model_id: &str) -> ResolvedModel {
    ResolvedModel {
        name: name.into(),
        qualified_name: format!("tools.openrouter.{name}"),
        category: "tools".into(),
        provider_key: "openrouter".into(),
        sdk: Sdk::Openai,
        model_id: model_id.into(),
        api_key_env: Some("OPENROUTER_SHORE_TOOL".into()),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        max_context_tokens: None,
        max_tokens: Some(4096),
        temperature: Some(1.0),
        top_p: None,
        reasoning_effort: None,
        budget_tokens: None,
        cache_ttl: None,
        keepalive_enabled: None,
        keepalive_ttl_minutes: None,
        keepalive_max_pings: None,
        openrouter_provider: None,
        vertex_project: None,
        vertex_location: None,
        gemini_generation: None,
        gemini_web_search: None,
    }
}

fn openrouter_primary_model(name: &str, model_id: &str) -> ResolvedModel {
    let mut m = openrouter_tool_model(name, model_id);
    m.api_key_env = Some("OPENROUTER_SHORE_PRIMARY".into());
    m.category = "chat".into();
    m.qualified_name = format!("chat.openrouter.{name}");
    m.temperature = Some(1.0);
    m.top_p = Some(0.95);
    m
}

fn all_models() -> Vec<(&'static str, ResolvedModel)> {
    vec![
        (
            "mistral-small (baseline)",
            openrouter_tool_model("mistral-small", "mistralai/mistral-small-2603"),
        ),
        (
            "qwen3.5-flash",
            openrouter_tool_model("qwen3.5-flash", "qwen/qwen3.5-flash-02-23"),
        ),
        (
            "qwen3-235b-moe",
            openrouter_tool_model("qwen3-235b", "qwen/qwen3-235b-a22b-2507"),
        ),
        (
            "gemini-2.5-flash-lite",
            openrouter_tool_model("gemini-flash-lite", "google/gemini-2.5-flash-lite"),
        ),
        (
            "gemini-3.1-flash-lite",
            openrouter_tool_model("gemini-3.1-flash-lite", "google/gemini-3.1-flash-lite-preview"),
        ),
        (
            "deepseek-v3.2",
            openrouter_tool_model("deepseek-v3.2", "deepseek/deepseek-v3.2"),
        ),
        (
            "kimi-k2.5",
            openrouter_primary_model("kimi-k2.5", "moonshotai/kimi-k2.5"),
        ),
        (
            "nemotron-3-super",
            openrouter_tool_model("nemotron-3-super", "nvidia/nemotron-3-super-120b-a12b"),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Test queries — broad questions that need decomposition
// ---------------------------------------------------------------------------

fn test_queries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "broad_recent_activity",
            "What has ren been up to recently? I want to know about TV shows, \
             music, gaming, and anything else interesting from March 2026.",
        ),
        (
            "relationship_context",
            "What's the current state of ren and vivian's relationship? \
             Any recent events or changes?",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn socket_path() -> PathBuf {
    let uid: u32 = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(1000))
        .unwrap_or(1000);
    PathBuf::from(format!("/run/user/{uid}/shore/llm.sock"))
}

fn memory_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/home/eshen/.local/share"))
        .join("shore/qifei/memory/memory.db")
}

fn backup_db_path() -> PathBuf {
    memory_db_path().with_extension("db.benchmark-backup")
}

/// Restore DB from backup (in case writes happened).
fn restore_db() {
    let backup = backup_db_path();
    let db = memory_db_path();
    if backup.exists() {
        std::fs::copy(&backup, &db).expect("Failed to restore DB from backup");
        eprintln!("  (DB restored from backup)");
    }
}

fn char_definition() -> String {
    // Read a trimmed version — just enough for researcher context.
    let path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/home/eshen/.config"))
        .join("shore/characters/qifei/character.md");
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            // Take first 2000 chars to keep context reasonable.
            s.chars().take(2000).collect()
        }
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// The benchmark
// ---------------------------------------------------------------------------

/// Run a single model through the researcher → agent pipeline (same model both tiers).
async fn run_benchmark(
    _label: &str,
    model: &ResolvedModel,
    query: &str,
    db: &MemoryDB,
    char_def: &str,
) -> (String, usize, usize, u128) {
    run_benchmark_mixed(_label, model, model, query, db, char_def).await
}

/// Run with potentially different models for researcher and agent tiers.
async fn run_benchmark_mixed(
    _label: &str,
    researcher_model: &ResolvedModel,
    agent_model: &ResolvedModel,
    query: &str,
    db: &MemoryDB,
    char_def: &str,
) -> (String, usize, usize, u128) {
    let sock = socket_path();
    let researcher_llm = InstrumentedLlm::new(LlmClient::new(), "researcher");
    let agent_llm = InstrumentedLlm::new(LlmClient::new(), "agent");

    let researcher = MemoryResearcher::new(char_def.to_string(), String::new());
    let agent = MemoryAgent::one_shot(CallerIdentity::Char, "qifei", "ren");

    let start = Instant::now();

    let result = researcher
        .research(
            query,
            &researcher_llm,
            researcher_model,
            &agent,
            &agent_llm,
            agent_model,
            db,
            None,
        )
        .await;

    let total_ms = start.elapsed().as_millis();
    let researcher_calls = researcher_llm.call_count();
    let agent_calls = agent_llm.call_count();

    let output = match result {
        Ok(text) => text,
        Err(e) => format!("[ERROR: {e}]"),
    };

    // Print call trace
    eprintln!("\n  --- Researcher calls ---");
    eprintln!("{}", researcher_llm.summary());
    eprintln!("  --- Agent calls ---");
    eprintln!("{}", agent_llm.summary());

    (output, researcher_calls, agent_calls, total_ms)
}

#[tokio::test]
#[ignore = "Requires running shore-llm and OPENROUTER keys"]
async fn memory_model_benchmark() {
    let db_path = memory_db_path();
    assert!(
        db_path.exists(),
        "Memory DB not found at {}",
        db_path.display()
    );
    assert!(
        backup_db_path().exists(),
        "Backup DB not found — run: cp {} {}",
        db_path.display(),
        backup_db_path().display()
    );

    let char_def = char_definition();
    let queries = test_queries();
    let models = all_models();

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("MEMORY AGENT/RESEARCHER MODEL BENCHMARK");
    eprintln!("Models: {}", models.len());
    eprintln!("Queries: {}", queries.len());
    eprintln!("{}\n", "=".repeat(80));

    let mut results: Vec<(String, String, String, usize, usize, u128)> = Vec::new();

    for (query_label, query_text) in &queries {
        eprintln!("\n{}", "─".repeat(80));
        eprintln!("QUERY: {query_label}");
        eprintln!("  \"{query_text}\"");
        eprintln!("{}", "─".repeat(80));

        for (model_label, model) in &models {
            eprintln!("\n>>> {model_label} ({}) <<<", model.model_id);

            let db = MemoryDB::open(&db_path).expect("Failed to open memory DB");

            let (output, researcher_calls, agent_calls, total_ms) =
                run_benchmark(model_label, model, query_text, &db, &char_def).await;

            eprintln!("\n  RESULT ({model_label}):");
            eprintln!("  Researcher calls: {researcher_calls}");
            eprintln!("  Agent calls:      {agent_calls}");
            eprintln!("  Total calls:      {}", researcher_calls + agent_calls);
            eprintln!("  Total time:       {total_ms}ms");
            eprintln!("  Output length:    {} chars", output.len());
            eprintln!("  ---");
            // Print first 1500 chars of output for human review.
            let preview: String = output.chars().take(1500).collect();
            eprintln!("{preview}");
            if output.len() > 1500 {
                eprintln!("  ... ({} more chars)", output.len() - 1500);
            }

            results.push((
                query_label.to_string(),
                model_label.to_string(),
                output,
                researcher_calls,
                agent_calls,
                total_ms,
            ));

            // Restore DB after each run in case writes happened.
            restore_db();
        }
    }

    // Print summary table.
    eprintln!("\n\n{}", "=".repeat(100));
    eprintln!("SUMMARY");
    eprintln!("{}", "=".repeat(100));
    eprintln!(
        "{:<25} {:<25} {:>6} {:>6} {:>6} {:>8} {:>6}",
        "Query", "Model", "R.call", "A.call", "Total", "Time ms", "OutLen"
    );
    eprintln!("{}", "-".repeat(100));

    for (q, m, output, rc, ac, ms) in &results {
        eprintln!(
            "{:<25} {:<25} {:>6} {:>6} {:>6} {:>8} {:>6}",
            q,
            m,
            rc,
            ac,
            rc + ac,
            ms,
            output.len()
        );
    }

    eprintln!("\n{}", "=".repeat(100));
}

// ---------------------------------------------------------------------------
// Mixed-model benchmark: different researcher × agent combos
// ---------------------------------------------------------------------------

fn mixed_model_pairs() -> Vec<(&'static str, ResolvedModel, ResolvedModel)> {
    let kimi = openrouter_primary_model("kimi-k2.5", "moonshotai/kimi-k2.5");
    let qwen_flash = openrouter_tool_model("qwen3.5-flash", "qwen/qwen3.5-flash-02-23");
    let gemini = openrouter_tool_model("gemini-3.1-flash-lite", "google/gemini-3.1-flash-lite-preview");
    let qwen_moe = openrouter_tool_model("qwen3-235b", "qwen/qwen3-235b-a22b-2507");

    vec![
        // Smart orchestrators with cheap workers
        ("kimi→qwen-flash", kimi.clone(), qwen_flash.clone()),
        ("kimi→qwen-moe", kimi.clone(), qwen_moe.clone()),
        ("kimi→gemini", kimi.clone(), gemini.clone()),
        // Qwen MoE orchestrating cheaper workers
        ("qwen-moe→qwen-flash", qwen_moe.clone(), qwen_flash.clone()),
        ("qwen-moe→gemini", qwen_moe.clone(), gemini.clone()),
        // Qwen flash orchestrating the bigger model
        ("qwen-flash→qwen-moe", qwen_flash.clone(), qwen_moe.clone()),
        // Gemini orchestrating others
        ("gemini→qwen-flash", gemini.clone(), qwen_flash.clone()),
        ("gemini→qwen-moe", gemini.clone(), qwen_moe.clone()),
    ]
}

#[tokio::test]
#[ignore = "Requires running shore-llm and OPENROUTER keys"]
async fn memory_mixed_model_benchmark() {
    let db_path = memory_db_path();
    assert!(db_path.exists(), "Memory DB not found at {}", db_path.display());
    assert!(backup_db_path().exists(), "Backup not found");

    let char_def = char_definition();
    let pairs = mixed_model_pairs();

    // Use the broad query — it's the harder one that exposes orchestration quality.
    let query = "What has ren been up to recently? I want to know about TV shows, \
                 music, gaming, and anything else interesting from March 2026.";

    eprintln!("\n{}", "=".repeat(90));
    eprintln!("MIXED-MODEL BENCHMARK (researcher → agent)");
    eprintln!("Pairs: {}", pairs.len());
    eprintln!("Query: broad_recent_activity");
    eprintln!("{}\n", "=".repeat(90));

    let mut results: Vec<(&str, String, usize, usize, u128)> = Vec::new();

    for (label, researcher_model, agent_model) in &pairs {
        eprintln!("\n>>> {} <<<", label);
        eprintln!("  researcher: {}", researcher_model.model_id);
        eprintln!("  agent:      {}", agent_model.model_id);

        let db = MemoryDB::open(&db_path).expect("Failed to open memory DB");

        let (output, rc, ac, ms) =
            run_benchmark_mixed(label, researcher_model, agent_model, query, &db, &char_def).await;

        eprintln!("\n  RESULT ({label}):");
        eprintln!("  Researcher calls: {rc}");
        eprintln!("  Agent calls:      {ac}");
        eprintln!("  Total calls:      {}", rc + ac);
        eprintln!("  Total time:       {ms}ms");
        eprintln!("  Output length:    {} chars", output.len());
        eprintln!("  ---");
        let preview: String = output.chars().take(1500).collect();
        eprintln!("{preview}");
        if output.len() > 1500 {
            eprintln!("  ... ({} more chars)", output.len() - 1500);
        }

        results.push((label, output, rc, ac, ms));
        restore_db();
    }

    // Summary table
    eprintln!("\n\n{}", "=".repeat(90));
    eprintln!("MIXED-MODEL SUMMARY (broad_recent_activity)");
    eprintln!("{}", "=".repeat(90));
    eprintln!(
        "{:<25} {:>6} {:>6} {:>6} {:>8} {:>6}",
        "Pair (R→A)", "R.call", "A.call", "Total", "Time ms", "OutLen"
    );
    eprintln!("{}", "-".repeat(90));

    for (label, output, rc, ac, ms) in &results {
        eprintln!(
            "{:<25} {:>6} {:>6} {:>6} {:>8} {:>6}",
            label,
            rc,
            ac,
            rc + ac,
            ms,
            output.len()
        );
    }

    eprintln!("\n{}", "=".repeat(90));
}

/// Quick single-model test for iteration during development.
#[tokio::test]
#[ignore = "Requires running shore-llm and OPENROUTER keys"]
async fn memory_bench_single() {
    let db_path = memory_db_path();
    let db = MemoryDB::open(&db_path).expect("Failed to open memory DB");
    let char_def = char_definition();

    // Change this to test a specific model.
    let model = openrouter_tool_model("qwen3.5-flash", "qwen/qwen3.5-flash-02-23");

    let query = "What has ren been up to recently? I want to know about TV shows, \
                 music, gaming, and anything else interesting from March 2026.";

    eprintln!("\n>>> Single model test: {} <<<", model.model_id);

    let (output, rc, ac, ms) = run_benchmark("test", &model, query, &db, &char_def).await;

    eprintln!("\nRESULT:");
    eprintln!("  Researcher: {rc} calls | Agent: {ac} calls | Total: {}ms", ms);
    eprintln!("  Output ({} chars):", output.len());
    eprintln!("{output}");

    restore_db();
}
