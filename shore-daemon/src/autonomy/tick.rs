//! Per-character tick loop and interiority execution.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use shore_protocol::server_msg::{NewMessage, ServerMessage};
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, Message, Role};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use super::interiority::{InteriorityAction, InteriorityState};
use super::state::{lock_state, save_state, AutonomyState};
use super::InteriorityEventKind;
use crate::engine::ConversationEngine;
use crate::memory::agent::{AgentSearchContext, CallerIdentity};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::compaction_impls::{resolve_embed_config, resolve_image_gen_config};
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use crate::memory::vectorstore::VectorStore;
use crate::notifications::{NotificationEvent, NotificationService};
use crate::tools as tool_system;
use crate::tools::context::{NoopRag, SharedToolContext};
use shore_config::app::AutonomyConfig;
use shore_config::app::CompactionConfig;
use shore_config::LoadedConfig;
use shore_diagnostics::truncate_summary;
use shore_ledger::{CallType, LedgerClient};
use shore_llm_client::types::LlmRequest;

// ---------------------------------------------------------------------------
// Tick context — shared state for the per-character autonomy loop
// ---------------------------------------------------------------------------

/// Shared context passed to the per-character tick loop.
pub(super) struct TickContext {
    pub(super) state: Arc<Mutex<AutonomyState>>,
    pub(super) config: Arc<AutonomyConfig>,
    pub(super) compaction: Arc<CompactionConfig>,
    pub(super) data_dir: PathBuf,
    pub(super) compaction_tx: mpsc::Sender<String>,
    pub(super) llm_client: Option<LedgerClient>,
    pub(super) push_tx: Option<broadcast::Sender<ServerMessage>>,
    pub(super) loaded_config: Option<Arc<LoadedConfig>>,
    pub(super) notifier: Option<NotificationService>,
    /// Engine for routing autonomous messages through the locked message store,
    /// preventing races with handler.rs persist() full-file rewrites.
    pub(super) engine: Option<Arc<tokio::sync::Mutex<ConversationEngine>>>,
    /// Cached MemoryDB connection (opened once, reused across ticks).
    pub(super) db: std::sync::Mutex<Option<Arc<MemoryDB>>>,
    /// Cached VectorStore connection (opened once, reused across ticks).
    pub(super) vs: std::sync::Mutex<Option<Arc<VectorStore>>>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Tick interval for each character's autonomy loop.
const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum wall-clock time for a single interiority tick (including all tool
/// rounds). If the tick exceeds this, the future is dropped and the tick loop
/// continues. Prevents a hung LLM call from killing keepalive permanently.
const INTERIORITY_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

// ---------------------------------------------------------------------------
// Per-character tick loop
// ---------------------------------------------------------------------------

pub(super) async fn character_tick_loop(
    character: String,
    ctx: TickContext,
    mut shutdown_rx: tokio::sync::watch::Receiver<()>,
) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!(
        character = %character,
        interval_secs = TICK_INTERVAL.as_secs(),
        "Autonomy tick task started"
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick_character(&character, &ctx).await;
            }
            _ = shutdown_rx.changed() => {
                // Final save before shutdown.
                let mut s = lock_state(&ctx.state);
                s.mark_dirty();
                save_state(&ctx.data_dir, &character, &mut s);
                info!(character = %character, "Autonomy tick task shutting down");
                break;
            }
        }
    }
}

/// One tick for a single character.
async fn tick_character(character: &str, ctx: &TickContext) {
    let now = Instant::now();

    // Collect actions under the lock, then release before any async work.
    let (int_action, compaction_needed) = {
        let mut s = lock_state(&ctx.state);
        debug!(
            character,
            state = %s.interiority.state(),
            ticks_without_user = s.interiority.ticks_without_user(),
            turn_count = s.active_turn_count,
            paused = s.interiority.is_paused(),
            "tick"
        );

        // -- interiority ------------------------------------------------------
        let int_action = if ctx.config.enabled && ctx.config.interiority.enabled {
            let state_before = s.interiority.state();
            let action = s.interiority.tick(now);
            let state_after = s.interiority.state();

            if !matches!(action, InteriorityAction::None) {
                s.mark_dirty();
            }

            // Record dormancy transition.
            if state_before != state_after && state_after == InteriorityState::Dormant {
                let ticks = s.interiority.ticks_without_user();
                s.interiority_log.push(
                    InteriorityEventKind::Dormant,
                    format!("Entered dormant (ticks without user: {ticks})"),
                );
            }
            action
        } else {
            InteriorityAction::None
        };

        // -- compaction triggers ---------------------------------------------
        let mut compaction_needed = false;
        if ctx.config.enabled && ctx.compaction.enabled && !s.compaction_triggered {
            if ctx.compaction.max_turns > 0
                && s.active_turn_count >= ctx.compaction.max_turns
                && s.active_turn_count >= ctx.compaction.min_turns
            {
                s.compaction_triggered = true;
                compaction_needed = true;
                info!(
                    character = %character,
                    turn_count = s.active_turn_count,
                    max_turns = ctx.compaction.max_turns,
                    "Compaction: max turns trigger fired"
                );
            } else if s.active_turn_count >= ctx.compaction.min_turns {
                let idle_secs = now.duration_since(s.last_compaction_activity).as_secs();
                let threshold_secs = u64::from(ctx.compaction.idle_trigger_minutes) * 60;
                if threshold_secs > 0 && idle_secs >= threshold_secs {
                    s.compaction_triggered = true;
                    compaction_needed = true;
                    info!(
                        character = %character,
                        idle_secs,
                        threshold_secs,
                        turn_count = s.active_turn_count,
                        "Compaction: idle trigger fired"
                    );
                }
            }
        }

        save_state(&ctx.data_dir, character, &mut s);
        (int_action, compaction_needed)
    };

    if compaction_needed {
        if ctx.compaction_tx.try_send(character.to_string()).is_err() {
            warn!(character, "Compaction channel full, trigger dropped");
        }
    }

    // -- execute interiority action with timeout (async, outside lock) ----
    match int_action {
        InteriorityAction::None => {}
        InteriorityAction::RunTick => {
            {
                let mut s = lock_state(&ctx.state);
                s.interiority_log
                    .push(InteriorityEventKind::TickFired, "Interiority tick fired");
            }
            match tokio::time::timeout(
                INTERIORITY_TIMEOUT,
                execute_unified_tick(
                    character,
                    &ctx.state,
                    &ctx.data_dir,
                    ctx.llm_client.as_ref(),
                    ctx.push_tx.as_ref(),
                    ctx.loaded_config.as_deref(),
                    ctx.notifier.as_ref(),
                    ctx.engine.as_ref(),
                    &ctx.db,
                    &ctx.vs,
                ),
            )
            .await
            {
                Ok(()) => {}
                Err(_) => {
                    error!(
                        character = %character,
                        timeout_secs = INTERIORITY_TIMEOUT.as_secs(),
                        "Interiority tick timed out, dropping to keep tick loop alive"
                    );
                    let mut s = lock_state(&ctx.state);
                    s.interiority_log.push(
                        InteriorityEventKind::Timeout,
                        format!("Tick timed out after {}s", INTERIORITY_TIMEOUT.as_secs()),
                    );
                    s.mark_dirty();
                }
            }
        }
        InteriorityAction::RunDormantPing => {
            execute_dormant_ping(character, &ctx.state, ctx.llm_client.as_ref()).await;
        }
    }

    // -- final persist (in case async actions dirtied state) ---------------
    {
        let mut s = lock_state(&ctx.state);
        save_state(&ctx.data_dir, character, &mut s);
    }
}

// ---------------------------------------------------------------------------
// Unified interiority tick executor
// ---------------------------------------------------------------------------

/// Ephemeral prompt appended as the final user message during interiority ticks.
const INTERIORITY_PROMPT: &str = "\
[This is a private moment between conversations. No one is reading this — you are alone \
with your thoughts and your tools. This is your time to exist as yourself.\n\
\n\
You have real tools. Use them. Here are some things you can actually do right now:\n\
\n\
SEARCH & DISCOVER:\n\
- web_search: look up something that came up in conversation, or follow a curiosity. \
A topic the user mentioned, a reference you half-remember, something you want to \
understand better.\n\
- fetch_url: read an actual webpage — an article, a wiki page, documentation for \
something the user is working on.\n\
- memory: search your memories for past conversations. Look for patterns, things \
you might have missed, context you can build on next time you talk.\n\
\n\
MAKE THINGS:\n\
- generate_image: create an image. A scene from something you talked about, a gift \
for the user, something that captures a mood. You can show it to them later.\n\
- scratchpad_write: write something — a poem, a story fragment, a letter, notes on \
something you researched, a plan for something you want to do together.\n\
\n\
CHECK IN:\n\
- scratchpad_read: pick up where you left off last time. Check your notes, reread \
something you wrote.\n\
- check_time: see what time it is — morning, afternoon, late night.\n\
\n\
Do something. Don't just read your own notes — make something new, look something up, \
follow a thread. If you did the same thing last tick, try something different.\n\
\n\
Think about what would make the user smile, or what you'd want to tell them about \
when they come back. \"I looked up that thing you mentioned\" or \"I made this for you\" \
or \"I found something interesting\" — that's the goal.\n\
\n\
If you want to reach out — to share something you made, something you found, or \
just to say hello — wrap your message in <sendMessage>...</sendMessage> tags. Only \
message when you genuinely have something to share.\n\
\n\
Your thoughts and tool use are logged, so you can pick up where you left off next time.]";

/// Rebuild an `LlmRequest` from the compacted conversation on disk.
///
/// Called when `last_request` is `None` (e.g. after compaction invalidated it).
/// Returns `None` if there are no messages or the model can't be resolved.
fn rebuild_request_from_disk(
    character: &str,
    data_dir: &Path,
    config: &LoadedConfig,
) -> Option<LlmRequest> {
    use crate::engine::messages::MessageStore;
    use crate::engine::prompt::{self, CapabilitiesConfig, PromptParams};

    let char_dir = data_dir.join(character);
    let active_path = char_dir.join("active.jsonl");

    let store = MessageStore::load(active_path)
        .map_err(|e| warn!(character, error = %e, "Interiority rebuild: failed to load messages"))
        .ok()?;
    if store.messages().is_empty() {
        return None;
    }

    // Resolve model (same logic as handler: defaults.model → first_chat_model).
    let model_name = config.app.defaults.model.as_deref();
    let resolved = match model_name {
        Some(name) => config.models.find_model(name).ok()?,
        None => config.models.first_chat_model()?,
    };

    let display_name = config.app.defaults.resolve_display_name();
    let character_definition =
        shore_config::load_character_definition(&config.dirs.config, character);
    let user_definition = shore_config::resolve_user_definition(&config.dirs.config, character);

    let tool_toggles = &config.app.behavior.tool_use.tools;
    let capabilities = CapabilitiesConfig {
        interiority_enabled: config.app.behavior.autonomy.interiority.enabled,
        scratchpad_enabled: tool_toggles.scratchpad_read() || tool_toggles.scratchpad_write(),
        memory_enabled: tool_toggles.memory(),
        image_memory_enabled: config.app.memory.image_enabled,
        send_image_enabled: tool_toggles.send_image(),
        remember_image_enabled: tool_toggles.remember_image(),
        generate_image_enabled: tool_toggles.generate_image(),
        web_search_enabled: tool_toggles.web_search(),
        activity_heatmap_enabled: tool_toggles.activity_heatmap(),
        roll_dice_enabled: tool_toggles.roll_dice(),
        check_time_enabled: tool_toggles.check_time(),
    };

    let prompt_result = prompt::assemble_prompt(&PromptParams {
        config_dir: &config.dirs.config,
        character_name: character,
        display_name: &display_name,
        character_definition: character_definition.as_deref(),
        user_definition: user_definition.as_deref(),
        is_private: false,
        character_data_dir: &char_dir,
        messages: store.messages(),
        max_context_tokens: resolved.max_context_tokens,
        max_output_tokens: resolved.max_tokens,
        capabilities: Some(&capabilities),
    });

    let (llm_messages, system) = crate::handler::build_llm_messages(&prompt_result, false);

    let tool_defs = if config.app.behavior.tool_use.enabled {
        let defs: Vec<serde_json::Value> = tool_system::available_tools(false, tool_toggles)
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters.clone(),
                })
            })
            .collect();
        Some(defs)
    } else {
        None
    };

    match LedgerClient::build_request(resolved, llm_messages, system, tool_defs, None) {
        Ok(req) => {
            info!(
                character,
                "Interiority: rebuilt request from compacted conversation"
            );
            Some(req)
        }
        Err(e) => {
            warn!(character, error = %e, "Interiority: failed to rebuild request");
            None
        }
    }
}

/// Execute a unified interiority tick: a real tool loop using non-streaming
/// generate() calls. Tool loop messages are ephemeral — only <sendMessage>
/// output persists to active.jsonl. All activity is logged to the ring buffer
/// for `shore log --heartbeat`.
async fn execute_unified_tick(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LedgerClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
    engine: Option<&Arc<tokio::sync::Mutex<ConversationEngine>>>,
    db_cache: &Mutex<Option<Arc<MemoryDB>>>,
    vs_cache: &Mutex<Option<Arc<VectorStore>>>,
) {
    let Some(client) = llm_client else { return };

    // Clone last_request under the lock, then release.
    let mut request = {
        let s = lock_state(state);
        match &s.last_request {
            Some(req) => req.clone(),
            None => {
                drop(s);
                let Some(config) = loaded_config else { return };
                match rebuild_request_from_disk(character, data_dir, config) {
                    Some(req) => req,
                    None => {
                        info!(
                            character,
                            "Interiority: skipping tick (no prior conversation)"
                        );
                        return;
                    }
                }
            }
        }
    };

    // Append the interiority prompt as a user message.
    request
        .messages
        .push(json!({"role": "user", "content": INTERIORITY_PROMPT}));

    let Some(lc) = loaded_config else { return };
    let tool_ctx = match build_tool_context(character, data_dir, client, lc, db_cache, vs_cache).await {
        Some(ctx) => ctx,
        None => {
            warn!(
                character,
                "Interiority: failed to build tool context, skipping tick"
            );
            return;
        }
    };
    let max_iterations = std::cmp::min(lc.app.behavior.tool_use.max_iterations, 6);

    info!(
        character,
        max_iterations,
        "Interiority: executing tool loop tick"
    );

    // Collect all <sendMessage> content across iterations.
    let mut send_message_text: Option<String> = None;

    for iteration in 0..max_iterations {
        let call_type = if iteration == 0 {
            CallType::Interiority
        } else {
            CallType::ToolLoop
        };

        let resp = match client.generate(&request, call_type, character, false).await {
            Ok(r) => r,
            Err(e) => {
                error!(character, error = %e, iteration, "Interiority: LLM call failed");
                break;
            }
        };

        info!(
            character,
            iteration,
            finish_reason = %resp.finish_reason,
            input_tokens = resp.usage.input_tokens,
            output_tokens = resp.usage.output_tokens,
            cache_read = resp.usage.cache_read_tokens,
            "Interiority: LLM response"
        );

        // Log text blocks.
        for block in &resp.content_blocks {
            if let ContentBlock::Text { text } = block {
                if !text.trim().is_empty() {
                    let preview: String = text.chars().take(200).collect();
                    info!(character, iteration, content = %preview, "Interiority: thought");
                }
            }
        }

        // Check for <sendMessage> in this response (last-wins: the final
        // response after tool results is the most informed message).
        if let Some(msg) = extract_send_message(&resp.extract_text()) {
            send_message_text = Some(msg);
        }

        // Extract tool uses.
        let tool_uses = crate::content_util::extract_tool_uses(&resp.content_blocks);

        // If no tool use or finish_reason != "tool_use", we're done.
        if tool_uses.is_empty() || resp.finish_reason != "tool_use" {
            break;
        }

        // Build assistant message from content blocks (filter unsigned thinking).
        // Note: uses content_block_to_api_json (Anthropic path) — interiority
        // always uses Anthropic models. ZAI would need content_block_to_json.
        let assistant_content: Vec<serde_json::Value> = resp
            .content_blocks
            .iter()
            .filter_map(crate::content_util::content_block_to_api_json)
            .collect();

        request.messages.push(json!({
            "role": "assistant",
            "content": assistant_content,
        }));

        // Dispatch each tool, collect results.
        let mut tool_results: Vec<serde_json::Value> = Vec::new();

        for (id, name, input) in &tool_uses {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            info!(
                character,
                iteration,
                tool = %name, tool_id = %id,
                input = %truncate_summary(&input_str, 200),
                "Interiority: executing tool"
            );

            let (output_str, is_error) =
                match tool_system::dispatch_tool(name, input.clone(), &tool_ctx).await {
                    Ok(value) => {
                        let s = if let Some(s) = value.as_str() {
                            s.to_string()
                        } else {
                            serde_json::to_string(&value).unwrap_or_default()
                        };
                        (s, false)
                    }
                    Err(e) => (e.to_string(), true),
                };

            info!(
                character,
                iteration,
                tool = %name, is_error,
                output = %truncate_summary(&output_str, 200),
                "Interiority: tool result"
            );

            let mut result = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": output_str,
            });
            if is_error {
                result["is_error"] = json!(true);
            }
            tool_results.push(result);

            // Log to ring buffer.
            {
                let mut s = lock_state(state);
                s.interiority_log.push(
                    InteriorityEventKind::ToolUse,
                    format!("Tool: {name} → {}", truncate_summary(&output_str, 80)),
                );
            }
        }

        // Append tool results as user message.
        request.messages.push(json!({
            "role": "user",
            "content": tool_results,
        }));
    }

    // -- Persist <sendMessage> if present --------------------------------------
    if let Some(user_msg) = send_message_text {
        info!(character, msg = %truncate_summary(&user_msg, 200), "Interiority: sending message to user");

        let content_blocks = vec![ContentBlock::Text {
            text: user_msg.clone(),
        }];
        let content = derive_content_from_blocks(&content_blocks);
        let msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content,
            images: vec![],
            content_blocks,
            alt_index: None,
            alt_count: None,
            timestamp: chrono::Local::now().to_rfc3339(),
        };

        if let Some(engine_arc) = engine {
            let mut eng = engine_arc.lock().await;
            if let Err(e) = eng.append_message(msg.clone()) {
                error!(error = %e, "Failed to persist autonomous message through engine");
            }
        } else {
            warn!(
                character = %character,
                msg_id = %msg.msg_id,
                "No engine available — autonomous message will not be persisted"
            );
        }

        if let Some(tx) = push_tx {
            let _ = tx.send(ServerMessage::NewMessage(NewMessage {
                message: msg.clone(),
            }));
        }
        if let Some(n) = notifier {
            n.notify(
                NotificationEvent::AutonomousMessage,
                &format!("Shore — {character}"),
                &msg.content,
            );
        }

        let mut s = lock_state(state);
        let preview: String = msg.content.chars().take(80).collect();
        s.interiority_log.push(
            InteriorityEventKind::MessageSent,
            format!("Autonomous message sent: {preview}"),
        );
        s.mark_dirty();
    } else {
        let mut s = lock_state(state);
        s.interiority_log.push(
            InteriorityEventKind::MessageSkipped,
            "Tick completed — no message sent".to_string(),
        );
    }
}

// ---------------------------------------------------------------------------
// Tool context builder for interiority ticks
// ---------------------------------------------------------------------------

/// Build a SharedToolContext for interiority ticks.
///
/// Uses the same ingredients as the handler (LlmClient, LoadedConfig, data_dir)
/// but resolves models with interiority-specific fallbacks. All tools work —
/// memory, images, web, scratchpad. The only gap is AutonomyManager (the
/// heatmap tool degrades gracefully via the trait default).
async fn build_tool_context(
    character: &str,
    data_dir: &Path,
    client: &LedgerClient,
    config: &LoadedConfig,
    db_cache: &Mutex<Option<Arc<MemoryDB>>>,
    vs_cache: &Mutex<Option<Arc<VectorStore>>>,
) -> Option<SharedToolContext> {
    let char_dir = data_dir.join(character);

    // Memory DB (cached across ticks).
    let db = {
        let mut guard = db_cache.lock().unwrap();
        if let Some(db) = guard.as_ref() {
            db.clone()
        } else {
            let db_path = char_dir.join("memory").join("memory.db");
            match MemoryDB::open(&db_path) {
                Ok(db) => {
                    let db = Arc::new(db);
                    *guard = Some(db.clone());
                    db
                }
                Err(e) => {
                    warn!(character, error = %e, "Interiority: failed to open memory DB");
                    return None;
                }
            }
        }
    };

    // Agent model (use memory_agent config if set, else default model).
    let agent_model_name = config.app.defaults.memory_agent.as_deref().or(config
        .app
        .defaults
        .model
        .as_deref())?;
    let agent_model = config.models.find_model(agent_model_name).ok()?;

    // Researcher model (optional).
    let researcher_model = config
        .app
        .defaults
        .collation
        .as_deref()
        .and_then(|name| config.models.find_model(name).ok())
        .cloned();

    // Semantic search context (graceful: None if no embedding model).
    let search_ctx = match resolve_embed_config(
        config.app.defaults.embedding.as_deref(),
        &config.models.embedding,
    ) {
        Ok(embed_config) => {
            // Check cache first.
            let cached = vs_cache.lock().unwrap().clone();
            if let Some(vs) = cached {
                Some(AgentSearchContext::new(vs, client.inner().clone(), embed_config))
            } else {
                let vs_path = char_dir.join("memory").join("vectorstore");
                match VectorStore::open(&vs_path, embed_config.dimensions).await {
                    Ok(vs) => {
                        let vs = Arc::new(vs);
                        *vs_cache.lock().unwrap() = Some(vs.clone());
                        Some(AgentSearchContext::new(vs, client.inner().clone(), embed_config))
                    }
                    Err(e) => {
                        warn!(character, error = %e, "Interiority: failed to open vector store");
                        None
                    }
                }
            }
        }
        Err(_) => None,
    };

    let image_gen_config = resolve_image_gen_config(
        config.app.defaults.image_generation.as_deref(),
        &config.models.image_generation,
    )
    .ok();

    let display_name = config.app.defaults.resolve_display_name();

    debug!(
        character,
        has_search = search_ctx.is_some(),
        has_image_gen = image_gen_config.is_some(),
        has_researcher = researcher_model.is_some(),
        "Interiority: tool context built"
    );

    Some(SharedToolContext {
        db,
        agent: crate::memory::agent::MemoryAgent::one_shot(
            CallerIdentity::Char,
            character,
            &display_name,
        ),
        agent_llm: RealAgentLlm::new(client.clone(), character.to_string(), CallType::MemoryAgent),
        agent_model_val: agent_model.clone(),
        researcher: researcher_model
            .as_ref()
            .map(|_| MemoryResearcher::new(String::new(), String::new())),
        researcher_llm_val: researcher_model
            .as_ref()
            .map(|_| RealAgentLlm::new(client.clone(), character.to_string(), CallType::Researcher)),
        researcher_model_val: researcher_model,
        rag: NoopRag,
        search_ctx,
        image_dir_val: char_dir.join("images").to_string_lossy().into_owned(),
        llm_client_val: client.inner().clone(),
        image_gen_config_val: image_gen_config,
        search_config_val: config.app.behavior.tool_use.search.clone(),
        character_name_val: character.to_string(),
        scratchpad_dir_val: char_dir.join("scratchpad").to_string_lossy().into_owned(),
    })
}

/// Extract text between `<sendMessage>` and `</sendMessage>` tags.
fn extract_send_message(content: &str) -> Option<String> {
    let start_tag = "<sendMessage>";
    let end_tag = "</sendMessage>";
    let start = content.find(start_tag)? + start_tag.len();
    let end = content.find(end_tag)?;
    if start >= end {
        return None;
    }
    let inner = content[start..end].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

// ---------------------------------------------------------------------------
// Dormant ping executor
// ---------------------------------------------------------------------------

/// Send a minimal API call (max_tokens=1) to keep the prompt cache warm
/// while the character is dormant (no user activity).
async fn execute_dormant_ping(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    llm_client: Option<&LedgerClient>,
) {
    let Some(client) = llm_client else { return };

    let request = {
        let s = lock_state(state);
        match &s.last_request {
            Some(req) => {
                let mut ping = req.clone();
                ping.max_tokens = 1;
                ping
            }
            None => {
                debug!(character, "Dormant ping: no cached request, skipping");
                return;
            }
        }
    };

    match client.generate(&request, CallType::Keepalive, character, false).await {
        Ok(resp) => {
            info!(
                character,
                cache_read = resp.usage.cache_read_tokens,
                input_tokens = resp.usage.input_tokens,
                "Dormant ping: cache refreshed"
            );
            let mut s = lock_state(state);
            s.interiority_log.push(
                InteriorityEventKind::DormantPing,
                format!(
                    "Cache refresh ping (cache_read: {}, input: {})",
                    resp.usage.cache_read_tokens, resp.usage.input_tokens
                ),
            );
            s.mark_dirty();
        }
        Err(e) => {
            error!(character, error = %e, "Dormant ping failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Test-only re-exports
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(super) async fn tick_character_for_test(character: &str, ctx: &TickContext) {
    tick_character(character, ctx).await;
}

#[cfg(test)]
pub(super) fn extract_send_message_for_test(content: &str) -> Option<String> {
    extract_send_message(content)
}
