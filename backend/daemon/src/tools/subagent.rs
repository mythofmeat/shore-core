//! Sub-agent delegation runtime.
//!
//! A `[subagents.<name>]` config entry surfaces to the primary model as a
//! single `ask_<name>(query)` tool. Invoking it runs a *nested* tool loop on a
//! (typically cheaper) model over a subset of the in-process tools, then
//! returns only the agent's final text. The bulky intermediate tool results
//! never enter the primary model's context, and the primary model's tool
//! surface stays small — that's the cost/compression win (see issue #35).
//!
//! Nesting is hard-capped at one level: the nested loop runs against
//! [`SubagentGuardContext`], whose `run_subagent` falls back to the trait
//! default (`NotImplemented`), so a sub-agent can never invoke another. The
//! offered tool subset also never contains `ask_*`, so a well-behaved model
//! has no `ask_*` affordance in the first place — the guard only defends
//! against a hallucinated call.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use shore_config::app::SubagentConfig;
use shore_config::models::ResolvedModel;
use shore_config::LoadedConfig;
use shore_diagnostics::Diagnostics;
use shore_ledger::{CallType, LedgerClient};
use shore_llm::stream::StreamConsumer;
use shore_llm::types::LlmRequest;
use shore_protocol::server_msg::ServerMessage;
use tokio::sync::mpsc;

use super::context::SharedToolContext;
use super::{ToolContext, ToolError};
use crate::engine::tools::{run_tool_loop, ToolLoopRetry};

/// Per-turn dependencies a [`SharedToolContext`] needs to run a sub-agent.
///
/// Only the chat generation path populates this; background contexts
/// (heartbeat, compaction, dreaming) leave it `None`, so calling `ask_*`
/// there returns `NotImplemented` rather than panicking.
pub(crate) struct SubagentRuntime {
    /// Ledger-wrapped client so sub-agent spend is recorded and attributable
    /// (initial stream tagged [`CallType::Subagent`]).
    pub(crate) ledger_client: LedgerClient,
    /// Diagnostics sink threaded into the nested tool loop.
    pub(crate) diagnostics: Arc<Mutex<Diagnostics>>,
    /// The effective config for this character — sub-agent specs, the model
    /// catalog/providers for resolution, and tool-loop knobs.
    pub(crate) config: Arc<LoadedConfig>,
    /// The live client channel for this turn. The sub-agent's nested tool-loop
    /// frames are forwarded here, tagged with the sub-agent name, so the UI can
    /// show the nested loop instead of appearing frozen. Intermediate tool
    /// *results* still never enter the primary model's context — this is a
    /// client-side view only.
    pub(crate) direct_tx: mpsc::Sender<ServerMessage>,
}

/// Run sub-agent `name` with `query`, returning its final text.
pub(crate) async fn run(
    ctx: &SharedToolContext,
    runtime: &SubagentRuntime,
    name: &str,
    query: &str,
) -> Result<Value, ToolError> {
    let config = &runtime.config;
    let (spec, resolved) = resolve_spec_and_model(config, name)?;
    let mut request = build_request(&resolved, config, spec, ctx, query)?;

    let thinking = thinking_enabled(&request);
    let char_name = ctx.character_name();

    // Forward the sub-agent's nested tool-loop frames to the client, each
    // tagged with the sub-agent name, so the UI renders the nested loop instead
    // of freezing on the `ask_<name>` call. The bulky tool *results* still never
    // enter the primary model's context — only the returned summary does; this
    // is purely a client-side view.
    let (tx, rx) = mpsc::channel::<ServerMessage>(64);
    let forward = spawn_forwarder(rx, runtime.direct_tx.clone(), name.to_owned());

    let consumer = StreamConsumer::new(tx.clone(), request.rid.clone());
    let mut ledger_stream = runtime
        .ledger_client
        .stream_raw(&request, CallType::Subagent, char_name, thinking)
        .await
        .map_err(|e| ToolError::Http(e.to_string()))?;
    let first = match consumer.consume(ledger_stream.reader_mut(), false).await {
        Ok(r) => {
            ledger_stream.finalize(&r);
            r
        }
        Err(e) => {
            ledger_stream.finalize_error(&e);
            forward.abort();
            return Err(ToolError::Http(e.to_string()));
        }
    };

    let retry = ToolLoopRetry {
        max_retries: config
            .app
            .advanced
            .max_retries
            .unwrap_or(shore_llm::retry::RetryPolicy::default().max_retries),
        backoff_base_ms: config
            .app
            .advanced
            .retry_backoff
            .map_or(500, |d| d.as_millis()),
    };

    // The nested loop runs against the guard context — never `ctx` — so a
    // hallucinated `ask_*` cannot recurse.
    let guard = SubagentGuardContext { inner: ctx };
    let loop_result = run_tool_loop(
        &runtime.ledger_client,
        &tx,
        &mut request,
        first,
        &guard,
        spec.max_iterations.or(resolved.max_tool_iterations),
        &config.app.tools,
        &runtime.diagnostics,
        char_name,
        thinking,
        retry,
    )
    .await
    .map_err(|e| ToolError::Http(e.to_string()));

    // Close the channel (drop both senders) so the forwarder drains any
    // buffered frames, then join it so the client sees the whole nested loop
    // before we return the summary.
    drop(consumer);
    drop(tx);
    let _ignored = forward.await;
    Ok(Value::String(loop_result?.result.content))
}

/// Resolve the subagent config entry and effective model.
fn resolve_spec_and_model<'conf>(
    config: &'conf LoadedConfig,
    name: &str,
) -> Result<(&'conf SubagentConfig, ResolvedModel), ToolError> {
    let spec = config
        .app
        .subagents
        .get(name)
        .ok_or_else(|| ToolError::NotImplemented(format!("ask_{name}")))?;

    // Model resolution: spec → defaults.subagent_model → defaults.model. Stop
    // there rather than chaining to the (expensive) active chat model — the
    // whole point is to land on something cheap.
    let model_name = spec
        .model
        .as_deref()
        .or(config.app.defaults.subagent_model.as_deref())
        .or(config.app.defaults.model.as_deref())
        .ok_or_else(|| {
            ToolError::InvalidArgs(format!(
                "subagent '{name}' has no model; set subagents.{name}.model or defaults.subagent_model"
            ))
        })?;
    let resolved = crate::effective_catalog::find_effective_model(
        config,
        &config.dirs.cache,
        model_name,
        true,
    )
    .map_err(|e| ToolError::InvalidArgs(format!("subagent '{name}' model '{model_name}': {e}")))?;

    Ok((spec, resolved))
}

/// Build the LLM request for the sub-agent, assembling system prompt and tools.
fn build_request(
    resolved: &ResolvedModel,
    config: &LoadedConfig,
    spec: &SubagentConfig,
    ctx: &SharedToolContext,
    query: &str,
) -> Result<LlmRequest, ToolError> {
    let display_name = config.app.defaults.resolve_display_name();
    let vars = template_vars(ctx.character_name(), &display_name);
    let system_text = crate::engine::prompt::render_template(&spec.prompt, &vars);
    let tools = subagent_tool_subset(&spec.tools, &vars);

    // Mirror the dreaming/compaction shape: Anthropic-cache SDKs take the
    // system prompt as an inline `role:"system"` entry (kept byte-stable
    // across iterations); everyone else takes it top-level.
    let uses_anthropic_cache = resolved.sdk.uses_anthropic_prompt_cache();
    let system_arg = if uses_anthropic_cache {
        None
    } else {
        Some(json!(system_text))
    };
    let mut request = LedgerClient::build_request_with_provider_keys(
        resolved,
        &config.providers,
        vec![json!({ "role": "user", "content": query })],
        system_arg,
        Some(tools),
        None,
    )
    .map_err(|e| ToolError::Http(e.to_string()))?;
    if uses_anthropic_cache {
        request.push_inline_system(system_text);
    }

    Ok(request)
}

/// Spawn the task that tags each frame from the sub-agent's nested loop with
/// `name` and relays it to the client channel. It ends when every sender on
/// `rx` is dropped (or the client channel closes), so the caller drops its
/// senders and awaits the handle to flush.
fn spawn_forwarder(
    mut rx: mpsc::Receiver<ServerMessage>,
    client_tx: mpsc::Sender<ServerMessage>,
    name: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(mut msg) = rx.recv().await {
            msg.set_subagent(&name);
            if client_tx.send(msg).await.is_err() {
                break;
            }
        }
    })
}

/// Build the `{{char}}` / `{{user}}` substitution table.
fn template_vars(char_name: &str, display_name: &str) -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = HashMap::new();
    let _ = vars.insert("char".into(), char_name.to_owned());
    let _ = vars.insert("character_name".into(), char_name.to_owned());
    let _ = vars.insert("user".into(), display_name.to_owned());
    vars
}

/// Render the sub-agent's allowed tool subset to the outbound `tools` array.
///
/// Only registered static tools are eligible; unknown names are skipped (the
/// config layer can't see the daemon tool registry, so the filter lands here)
/// and `ask_*` can never appear because sub-agent tools are not in the static
/// registry — both the offering guard and the recursion cap fall out of this.
fn subagent_tool_subset(allowed: &[String], vars: &HashMap<String, String>) -> Vec<Value> {
    let registry = super::all_tools();
    allowed
        .iter()
        .filter_map(|name| {
            let def = registry.iter().find(|t| t.name == name).or_else(|| {
                tracing::warn!(tool = %name, "subagent references unknown tool; skipping");
                None
            })?;
            Some(json!({
                "name": def.name,
                "description": crate::engine::prompt::render_template(def.description, vars),
                "input_schema": def.parameters.clone(),
            }))
        })
        .collect()
}

/// True when the request enables reasoning via either provider knob. Mirrors
/// `handler::generation::thinking_enabled_from_request` (kept local to avoid a
/// cross-module `pub` widening).
fn thinking_enabled(request: &LlmRequest) -> bool {
    let Some(opts) = request.provider_options.as_ref() else {
        return false;
    };
    if opts.get("thinking_enabled") == Some(&Value::Bool(false)) {
        return false;
    }
    let budget_on = opts
        .get("budget_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|b| b > 0);
    let effort_on = opts.get("reasoning_effort").is_some_and(|v| !v.is_null());
    budget_on || effort_on
}

/// Tool context for a sub-agent's nested loop: delegates everything to the
/// parent [`SharedToolContext`] except `run_subagent`, which falls back to the
/// trait default (`NotImplemented`) — hard-capping nesting at one level.
struct SubagentGuardContext<'ctx> {
    inner: &'ctx SharedToolContext,
}

impl ToolContext for SubagentGuardContext<'_> {
    fn image_dir(&self) -> &str {
        self.inner.image_dir()
    }
    fn llm_client(&self) -> Option<&shore_llm::LlmClient> {
        self.inner.llm_client()
    }
    fn image_gen_config(&self) -> Option<&crate::memory::compaction_impls::ImageGenConfig> {
        self.inner.image_gen_config()
    }
    fn search_config(&self) -> &shore_config::app::SearchConfig {
        self.inner.search_config()
    }
    fn character_name(&self) -> &str {
        self.inner.character_name()
    }
    fn workspace_dir(&self) -> &str {
        self.inner.workspace_dir()
    }
    fn character_data_dir(&self) -> &str {
        self.inner.character_data_dir()
    }
    fn markdown_store(&self) -> Option<&crate::memory::markdown_store::MarkdownMemoryStore> {
        self.inner.markdown_store()
    }
    fn memory_retrieval_config(&self) -> &shore_config::app::RetrievalConfig {
        self.inner.memory_retrieval_config()
    }
    fn embedder(&self) -> Option<&dyn shore_llm::embed::Embedder> {
        self.inner.embedder()
    }
    fn memory_index_path(&self) -> Option<&std::path::Path> {
        self.inner.memory_index_path()
    }
    fn config_dir(&self) -> &str {
        self.inner.config_dir()
    }
    fn defer_edit(&self, path: &str) {
        self.inner.defer_edit(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_subset_keeps_known_skips_unknown() {
        let vars = template_vars("qifei", "ren");
        let allowed = vec![
            "read".to_owned(),
            "not_a_real_tool".to_owned(),
            "search".to_owned(),
        ];
        let defs = subagent_tool_subset(&allowed, &vars);
        let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["read", "search"]);
    }

    #[test]
    fn tool_subset_cannot_offer_ask_tools() {
        // `ask_*` are not in the static registry, so even if a config names
        // one it is silently dropped — the recursion cap is structural.
        let vars = template_vars("qifei", "ren");
        let allowed = vec!["ask_music".to_owned(), "read".to_owned()];
        let defs = subagent_tool_subset(&allowed, &vars);
        let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["read"]);
    }

    #[tokio::test]
    async fn forwarder_tags_frames_with_subagent_name() {
        use shore_protocol::server_msg::{ToolCall, ToolResult};

        let (inner_tx, inner_rx) = mpsc::channel::<ServerMessage>(8);
        let (client_tx, mut client_rx) = mpsc::channel::<ServerMessage>(8);
        let handle = spawn_forwarder(inner_rx, client_tx, "research".to_owned());

        inner_tx
            .send(ServerMessage::ToolCall(ToolCall {
                rid: None,
                tool_id: "t1".into(),
                tool_name: "search".into(),
                input: json!({}),
                subagent: None,
            }))
            .await
            .unwrap();
        inner_tx
            .send(ServerMessage::ToolResult(ToolResult {
                rid: None,
                tool_id: "t1".into(),
                tool_name: "search".into(),
                output: "hits".into(),
                is_error: false,
                subagent: None,
            }))
            .await
            .unwrap();
        drop(inner_tx);

        // Both frames arrive tagged with the sub-agent name.
        let call = client_rx.recv().await.unwrap();
        assert_eq!(call.subagent(), Some("research"));
        let result = client_rx.recv().await.unwrap();
        assert_eq!(result.subagent(), Some("research"));
        // Channel closes once all senders drop, so the task ends.
        assert!(client_rx.recv().await.is_none());
        handle.await.unwrap();
    }
}
