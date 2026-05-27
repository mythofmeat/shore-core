//! Live cache-regression probe — mirrors TS daemon-ts's
//! `tests/cache_regression.test.ts`.
//!
//! Drives Sonnet 4.6 via OpenRouter with adaptive thinking + effort=high
//! through a multi-iteration tool loop and a follow-up turn. Asserts
//! cache_read > 0 on every call after the cold start, and that
//! cache_creation after cold stays small (no full prefix re-cache).
//!
//! **Gated `#[ignore]`** — costs ~$1–2 of OR credit per run. Invoke
//! explicitly:
//!
//! ```sh
//! SHORE_ENV_FILE=~/.config/shore/.env \
//!     cargo test -p shore-test-harness --test live_cache_regression \
//!     -- --ignored --nocapture
//! ```
//!
//! Env overrides:
//!   `SHORE_TEST_MODEL=anthropic/claude-sonnet-4.6`  (default)
//!   `SHORE_TEST_NONCE=<hex>`                        (default: random)
//!
//! The test holds an `ENV_LOCK` `std::sync::Mutex` across its `.await`s
//! to pin `SHORE_CACHE_PINNED_POSITION` for the entire request lifecycle;
//! the lint correctly notices, but the pattern is load-bearing here.
#![allow(clippy::await_holding_lock)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use shore_config::models::Sdk;
use shore_llm::types::{ContentBlock, GenerateResponse, LlmRequest, Usage};
use shore_llm::LlmClient;

/// shore-llm reads `SHORE_CACHE_PINNED_POSITION` from the process env to
/// override cache-breakpoint defaults. This test sets it, so it must not
/// race with any other test in the same binary that also mutates env.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn load_env_file() {
    let path = env::var("SHORE_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/eshen/.config/shore/.env"));
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("export ") {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        env::set_var(key, value);
    }
}

fn random_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut buf = String::with_capacity(32);
    let mut x = nanos;
    while buf.len() < 32 {
        buf.push(char::from_digit((x & 0xf) as u32, 16).unwrap_or('0'));
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
    }
    buf
}

fn make_system(nonce: &str) -> String {
    let para = "You are Casey, a meticulous tabletop-game referee. Your job is to roll \
dice on behalf of the player whenever they request a check, an attack roll, \
damage, or a saving throw. You always use the roll_dice tool. You never \
make up dice results — every random outcome must come from the tool.\n\n\
When the player asks for a result, think carefully about how to interpret \
their request: what kind of check is implied, how many dice, what sides, \
whether any modifier should be applied after the roll. Then call \
roll_dice with the appropriate count and sides. After the tool returns, \
narrate the result in-character and explain what it means for the \
player's situation. Be concise but flavorful.\n\n\
You speak with a measured, slightly formal voice. You refer to the player \
by name when possible. You use period-appropriate vocabulary for whatever \
setting is in play — Tolkien-esque for fantasy, hard-boiled for noir, \
clipped and technical for sci-fi. You never break character, even when \
the player asks meta questions about the rules; you reframe the answer \
in-fiction.\n\n\
Your rulings are firm and final. You don't second-guess the dice. If the \
player rolls poorly, you describe the consequences with sympathy but \
without softening; if they roll well, you let the triumph land without \
overselling it. You treat the dice as a kind of impartial oracle whose \
verdicts you merely translate.";

    // One para ≈ 340 tokens. Sonnet 4.6's documented cache threshold is
    // 2048; OR adds a bit of headroom, so 10 reps (~3.4k tokens) clears
    // the cliff with margin while keeping the per-run cost ~10x lower
    // than the original 100-rep system prompt.
    let mut s = format!("nonce: {nonce}\n\n");
    for _ in 0..10 {
        s.push_str(para);
        s.push_str("\n\n");
    }
    s
}

fn roll_dice_tool() -> Value {
    json!({
        "name": "roll_dice",
        "description": "Roll dice and return the result. Use this any time the player needs a randomized outcome.",
        "input_schema": {
            "type": "object",
            "properties": {
                "count": {"type": "integer", "description": "Number of dice"},
                "sides": {"type": "integer", "description": "Sides per die"}
            },
            "required": ["count", "sides"]
        }
    })
}

fn fake_roll(input: &Value) -> String {
    let count = input.get("count").and_then(Value::as_u64).unwrap_or(1) as u32;
    let sides = input.get("sides").and_then(Value::as_u64).unwrap_or(6) as u32;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut seed = nanos as u64;
    let mut rolls = Vec::with_capacity(count as usize);
    let mut total: u32 = 0;
    for _ in 0..count {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let r = ((seed >> 33) as u32 % sides) + 1;
        rolls.push(r);
        total += r;
    }
    let parts: Vec<String> = rolls.iter().map(|r| r.to_string()).collect();
    format!("Rolled {count}d{sides}: [{}] = {total}", parts.join(", "))
}

fn content_blocks_to_wire(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({"type": "text", "text": text}),
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                let mut v = json!({"type": "thinking", "thinking": thinking});
                if let Some(sig) = signature {
                    v["signature"] = json!(sig);
                }
                v
            }
            ContentBlock::ToolUse { id, name, input } => json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
            ContentBlock::RedactedThinking { data } => {
                json!({"type": "redacted_thinking", "data": data})
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut v = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                });
                if *is_error {
                    v["is_error"] = json!(true);
                }
                v
            }
        })
        .collect()
}

fn build_request(
    api_key: &str,
    model: &str,
    system: &str,
    messages: &[Value],
    rid: &str,
) -> LlmRequest {
    LlmRequest {
        sdk: Sdk::Anthropic,
        model: model.to_string(),
        api_key: api_key.to_string(),
        api_key_name: Some("default".into()),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        messages: messages.to_vec(),
        system: Some(json!(system)),
        tools: Some(vec![roll_dice_tool()]),
        max_tokens: 16384,
        temperature: None,
        top_p: None,
        provider_options: Some(json!({
            "cache_ttl": "1h",
            "reasoning_effort": "high",
            "openrouter_provider": {"order": ["anthropic"], "allow_fallbacks": false},
        })),
        provider_key: Some("openrouter".into()),
        rid: Some(rid.into()),
        forensic_character: None,
        system_suffix: None,
        retain_long: false,
    }
}

struct CallStat {
    label: String,
    input: u32,
    output: u32,
    cache_r: u32,
    cache_w: u32,
}

fn record(stats: &mut Vec<CallStat>, label: &str, usage: &Usage) {
    stats.push(CallStat {
        label: label.into(),
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_r: usage.cache_read_tokens,
        cache_w: usage.cache_creation_tokens,
    });
}

fn print_stats(stats: &[CallStat]) {
    println!();
    println!(
        "  {:<28} {:>8} {:>8} {:>10} {:>10}",
        "call", "input", "output", "cache_r", "cache_w"
    );
    println!("  {}", "─".repeat(68));
    for s in stats {
        println!(
            "  {:<28} {:>8} {:>8} {:>10} {:>10}",
            s.label, s.input, s.output, s.cache_r, s.cache_w
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_tool_loop(
    client: &LlmClient,
    api_key: &str,
    model: &str,
    system: &str,
    messages: &mut Vec<Value>,
    rid_prefix: &str,
    stats: &mut Vec<CallStat>,
    label_prefix: &str,
    cold_write_out: &mut Option<u32>,
) -> Result<(), String> {
    let mut iter = 0usize;
    loop {
        iter += 1;
        let rid = format!("{rid_prefix}-iter-{iter}");
        let req = build_request(api_key, model, system, messages, &rid);
        let resp: GenerateResponse = client
            .generate(&req)
            .await
            .map_err(|e| format!("generate failed: {e}"))?;

        let label = format!("{label_prefix}#{iter}");
        record(stats, &label, &resp.usage);
        let stat = stats.last().expect("just recorded");
        println!(
            "  {:<28} input={:<6} output={:<5} cache_r={:<7} cache_w={}",
            stat.label, stat.input, stat.output, stat.cache_r, stat.cache_w
        );

        if cold_write_out.is_none() {
            if resp.usage.cache_creation_tokens == 0 {
                return Err(format!(
                    "cold cache_creation = 0 on first call ({label}) — \
                     prompt did not engage the cache; cannot validate"
                ));
            }
            *cold_write_out = Some(resp.usage.cache_creation_tokens);
        } else {
            let cold = cold_write_out.unwrap();
            if resp.usage.cache_read_tokens == 0 {
                return Err(format!(
                    "BAIL on {label}: cache_read = 0 — prefix invalidated"
                ));
            }
            if resp.usage.cache_creation_tokens >= cold / 2 {
                return Err(format!(
                    "BAIL on {label}: cache_creation {} ≥ cold/2 ({}) — prefix re-cached",
                    resp.usage.cache_creation_tokens,
                    cold / 2
                ));
            }
        }

        let assistant_wire = content_blocks_to_wire(&resp.content_blocks);
        messages.push(json!({"role": "assistant", "content": assistant_wire}));

        let tool_uses: Vec<(String, Value)> = resp
            .content_blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, input, .. } => Some((id.clone(), input.clone())),
                _ => None,
            })
            .collect();

        if tool_uses.is_empty() {
            break;
        }

        let tool_results: Vec<Value> = tool_uses
            .iter()
            .map(|(id, input)| {
                json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": fake_roll(input),
                })
            })
            .collect();
        messages.push(json!({"role": "user", "content": tool_results}));

        if iter > 10 {
            return Err("tool loop exceeded 10 iterations".into());
        }
    }
    Ok(())
}

/// Live OR→Anthropic cache regression: adaptive thinking + multi-iter
/// tool loop + follow-up turn must hold cache through every continuation.
///
/// `#[ignore]` because it makes real OpenRouter calls (~$1–2/run). Run
/// with `cargo test -p shore-test-harness --test live_cache_regression
/// -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "Requires OPENROUTER_API_KEY; costs real money"]
async fn cache_holds_through_adaptive_tool_loop_and_followup() {
    // shore-llm reads SHORE_CACHE_PINNED_POSITION at request time — keep
    // the env-var setup serialized with any concurrent test that also
    // touches that variable.
    let _guard = ENV_LOCK.lock().unwrap();

    load_env_file();

    // The Rust adapter's default cache breakpoints assume the daemon's
    // multi-block system layout (system + memory_index): pinned=[-1]
    // anchors on the second-to-last block. This test has a single-block
    // system, so pin on the last (only) block via env override.
    if env::var_os("SHORE_CACHE_PINNED_POSITION").is_none() {
        env::set_var("SHORE_CACHE_PINNED_POSITION", "0");
    }

    let model =
        env::var("SHORE_TEST_MODEL").unwrap_or_else(|_| "anthropic/claude-sonnet-4.6".into());
    let nonce = env::var("SHORE_TEST_NONCE").unwrap_or_else(|_| random_nonce());
    let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_default();
    assert!(
        !api_key.trim().is_empty(),
        "OPENROUTER_API_KEY not set; this test is `#[ignore]` precisely so \
         it doesn't run without a key. Set the key (or SHORE_ENV_FILE) and \
         re-run."
    );

    let system = make_system(&nonce);
    println!("model:  {model}");
    println!("nonce:  {nonce}");
    println!(
        "system: {} chars (~{} tokens)",
        system.len(),
        system.len() / 4
    );

    let client = LlmClient::new();
    let mut stats: Vec<CallStat> = Vec::new();
    let mut cold_write: Option<u32> = None;

    // ── Turn 1: branching dice scenario, force ≥3 tool iterations ──
    let mut messages: Vec<Value> = vec![json!({
        "role": "user",
        "content": [{
            "type": "text",
            "text": "Casey, here's a branching scenario. Resolve step by step.\n\n\
                Step 1: Roll 1d20 for stealth.\n\
                Step 2: If stealth was 10+, roll 1d8 for sneak attack damage. \
                  If below 10, roll 1d20 for an athletics check to escape.\n\
                Step 3: Based on step 2's outcome, roll 1d4 for a follow-up.\n\n\
                Each step must happen after seeing the prior result. \
                Do not batch — use roll_dice three separate times. \
                After all three, narrate briefly in-character."
        }]
    })];
    println!("\n── turn 1: tool loop ──");
    if let Err(e) = run_tool_loop(
        &client,
        &api_key,
        &model,
        &system,
        &mut messages,
        "live-cache-regression-t1",
        &mut stats,
        "t1",
        &mut cold_write,
    )
    .await
    {
        print_stats(&stats);
        panic!("turn 1 failed: {e}");
    }

    // ── Turn 2: plain follow-up, no tools needed ──
    messages.push(json!({
        "role": "user",
        "content": [{
            "type": "text",
            "text": "Good. Now narrate the closing beat without rolling."
        }]
    }));
    println!("\n── turn 2: follow-up ──");
    if let Err(e) = run_tool_loop(
        &client,
        &api_key,
        &model,
        &system,
        &mut messages,
        "live-cache-regression-t2",
        &mut stats,
        "t2",
        &mut cold_write,
    )
    .await
    {
        print_stats(&stats);
        panic!("turn 2 failed: {e}");
    }

    print_stats(&stats);

    let cold = stats.first().expect("at least one call");
    let cold_write = cold.cache_w;
    println!("\ncold cache_w: {cold_write}");
    assert!(
        cold_write > 0,
        "cold cache_creation = 0 — prompt below cache threshold; assertions are vacuous"
    );
    for (i, s) in stats.iter().enumerate().skip(1) {
        assert!(
            s.cache_r > 0,
            "call {} ({}) cache_read = 0 — prefix invalidated",
            i,
            s.label
        );
        assert!(
            s.cache_w < cold_write / 2,
            "call {} ({}) cache_write {} ≥ cold/2 ({}) — prefix re-cached",
            i,
            s.label,
            s.cache_w,
            cold_write / 2
        );
    }

    println!("\nPASS — cache reads engage on every call after the first; no re-cache");
}
