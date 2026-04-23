//! Stream retry and tool-phase execution for the generation pipeline.

use tracing::{debug, error, instrument, warn};

/// True when the request has extended thinking / reasoning enabled via
/// either provider knob: Anthropic-style `budget_tokens > 0`, or
/// OpenAI/Anthropic-style `reasoning_effort` set to any non-null value.
/// Used by both the primary stream call and the tool-loop re-entry to
/// tag ledger rows and SSE metadata consistently.
pub(super) fn thinking_enabled_from_request(request: &shore_llm_client::types::LlmRequest) -> bool {
    thinking_enabled_from_provider_options(request.provider_options.as_ref())
}

fn thinking_enabled_from_provider_options(opts: Option<&serde_json::Value>) -> bool {
    let Some(opts) = opts else {
        return false;
    };
    let budget_on = opts
        .get("budget_tokens")
        .and_then(|v| v.as_u64())
        .is_some_and(|b| b > 0);
    let effort_on = opts.get("reasoning_effort").is_some_and(|v| !v.is_null());
    budget_on || effort_on
}

#[cfg(test)]
mod tests {
    use super::thinking_enabled_from_provider_options;
    use serde_json::json;

    #[test]
    fn thinking_enabled_none_provider_options() {
        assert!(!thinking_enabled_from_provider_options(None));
    }

    #[test]
    fn thinking_enabled_empty_object() {
        let v = json!({});
        assert!(!thinking_enabled_from_provider_options(Some(&v)));
    }

    #[test]
    fn thinking_enabled_budget_zero() {
        let v = json!({ "budget_tokens": 0 });
        assert!(!thinking_enabled_from_provider_options(Some(&v)));
    }

    #[test]
    fn thinking_enabled_budget_positive() {
        let v = json!({ "budget_tokens": 4096 });
        assert!(thinking_enabled_from_provider_options(Some(&v)));
    }

    #[test]
    fn thinking_enabled_reasoning_effort_string() {
        let v = json!({ "reasoning_effort": "high" });
        assert!(thinking_enabled_from_provider_options(Some(&v)));
    }

    #[test]
    fn thinking_enabled_reasoning_effort_null_ignored() {
        let v = json!({ "reasoning_effort": null });
        assert!(!thinking_enabled_from_provider_options(Some(&v)));
    }

    #[test]
    fn thinking_enabled_both_knobs_set() {
        let v = json!({ "budget_tokens": 2048, "reasoning_effort": "medium" });
        assert!(thinking_enabled_from_provider_options(Some(&v)));
    }

    #[test]
    fn thinking_enabled_unrelated_keys_only() {
        let v = json!({ "cache_ttl": "1h", "vertex_project": "x" });
        assert!(!thinking_enabled_from_provider_options(Some(&v)));
    }
}

use crate::engine::tools;
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::compaction_impls::resolve_image_gen_config;
use crate::memory::markdown_store::MarkdownMemoryStore;
use crate::tools::context::SharedToolContext;
use shore_config::LoadedConfig;
use shore_ledger::CallType;
use shore_llm_client::retry::{self, RetryDecision, RetryPolicy};
use shore_llm_client::stream::StreamConsumer;

use super::{GenContext, HandlerToolContext};

/// Phase 10: Stream the LLM response with exponential backoff retry.
#[instrument(skip(ctx, request, effective_config), fields(char = char_name, model = %resolved.qualified_name))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn stream_with_retry(
    ctx: &GenContext,
    request: &shore_llm_client::types::LlmRequest,
    resolved: &shore_config::models::ResolvedModel,
    effective_config: &LoadedConfig,
    regen: bool,
    char_name: &str,
    thinking_enabled: bool,
) -> Result<shore_llm_client::types::StreamResult, Box<dyn std::error::Error + Send + Sync>> {
    let retry_policy = RetryPolicy {
        max_retries: effective_config
            .app
            .advanced
            .max_retries
            .unwrap_or(RetryPolicy::default().max_retries),
        ..RetryPolicy::default()
    };
    debug!(
        character = char_name,
        model = %resolved.qualified_name,
        max_retries = retry_policy.max_retries,
        "stream_with_retry starting"
    );
    let mut attempt: u32 = 0;

    loop {
        let consumer = StreamConsumer::new(ctx.direct_tx.clone(), request.rid.clone());

        let stream_result = async {
            let mut ledger_stream = ctx
                .llm_client
                .stream_raw(request, CallType::Message, char_name, thinking_enabled)
                .await?;

            match consumer.consume(ledger_stream.reader_mut(), regen).await {
                Ok(result) => {
                    ledger_stream.finalize(&result);
                    Ok(result)
                }
                Err(e) => {
                    ledger_stream.finalize_error();
                    Err(e)
                }
            }
        }
        .await;

        match stream_result {
            Ok(r) => {
                debug!(
                    attempts = attempt + 1,
                    finish_reason = %r.finish_reason,
                    input_tokens = r.usage.input_tokens,
                    output_tokens = r.usage.output_tokens,
                    "stream_with_retry complete"
                );
                return Ok(r);
            }
            Err(e) => match retry::should_retry_error(&e, attempt, &retry_policy) {
                RetryDecision::Retry => {
                    let base_ms = effective_config
                        .app
                        .advanced
                        .retry_backoff
                        .map(|d| d.as_millis())
                        .unwrap_or(500);
                    let delay = std::time::Duration::from_millis(base_ms * 2u64.pow(attempt));
                    warn!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %e,
                        "Retrying after transient LLM error"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                RetryDecision::FallbackModel(_model) => {
                    error!(error = %e, "stream_with_retry failed — fallback model requested");
                    return Err(e.into());
                }
                RetryDecision::Fail => {
                    error!(attempts = attempt + 1, error = %e, "stream_with_retry exhausted retries");
                    return Err(e.into());
                }
            },
        }
    }
}

/// Phase 11: Set up tool context and run the tool loop.
#[instrument(skip(ctx, effective_config, agent_model, _researcher_model, _character_definition, _user_definition, request, result), fields(char = char_name))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_tool_phase(
    ctx: &GenContext,
    data_dir: &std::path::Path,
    char_name: &str,
    effective_config: &LoadedConfig,
    agent_model: &shore_config::models::ResolvedModel,
    _researcher_model: &Option<shore_config::models::ResolvedModel>,
    _character_definition: &Option<String>,
    _user_definition: &Option<String>,
    request: &mut shore_llm_client::types::LlmRequest,
    result: shore_llm_client::types::StreamResult,
) -> Result<tools::ToolLoopResult, Box<dyn std::error::Error + Send + Sync>> {
    debug!(character = char_name, "run_tool_phase starting");
    let image_gen_config = resolve_image_gen_config(
        effective_config.app.defaults.image_generation.as_deref(),
        &effective_config.models.image_generation,
    )
    .ok();

    let character_data_dir = data_dir.join(char_name);
    let config_dir = &effective_config.dirs.config;

    // Bootstrap protected workspace files from config so the assistant
    // can read and edit them during the conversation.
    if let Err(e) = crate::memory::deferred_edits::bootstrap_workspace_files(
        &character_data_dir,
        config_dir,
        char_name,
    ) {
        warn!(
            character = %char_name,
            error = %e,
            "Failed to bootstrap workspace files"
        );
    }

    let tool_ctx = HandlerToolContext {
        inner: SharedToolContext {
            agent_llm: RealAgentLlm::new(
                ctx.llm_client.clone(),
                char_name.to_owned(),
                CallType::MemoryAgent,
            ),
            agent_model_val: agent_model.clone(),
            image_dir_val: character_data_dir
                .join("images")
                .to_string_lossy()
                .into_owned(),
            llm_client_val: ctx.llm_client.inner().clone(),
            image_gen_config_val: image_gen_config,
            search_config_val: effective_config.app.behavior.tool_use.search.clone(),
            character_name_val: char_name.to_owned(),
            scratchpad_dir_val: character_data_dir
                .join("scratchpad")
                .to_string_lossy()
                .into_owned(),
            workspace_dir_val: character_data_dir
                .join("workspace")
                .to_string_lossy()
                .into_owned(),
            markdown_store_val: MarkdownMemoryStore::open_sync(character_data_dir.join("memories"))
                .ok(),
            memory_access_allowed_val: effective_config.app.behavior.tool_use.tools.memory(),
            memory_read_allowed_val: effective_config.app.behavior.tool_use.tools.memory_read(),
            memory_write_allowed_val: effective_config.app.behavior.tool_use.tools.memory_write(),
            config_dir_val: config_dir.to_string_lossy().into_owned(),
            character_data_dir_val: character_data_dir.to_string_lossy().into_owned(),
        },
        autonomy_val: ctx.autonomy.clone(),
    };

    let thinking_enabled = thinking_enabled_from_request(request);

    let tool_loop_result = tools::run_tool_loop(
        &ctx.llm_client,
        &ctx.direct_tx,
        request,
        result,
        &tool_ctx,
        effective_config.app.behavior.tool_use.max_iterations,
        &ctx.diagnostics,
        char_name,
        thinking_enabled,
    )
    .await?;

    debug!(
        character = char_name,
        intermediate_messages = tool_loop_result.intermediate_messages.len(),
        "run_tool_phase complete"
    );
    Ok(tool_loop_result)
}
