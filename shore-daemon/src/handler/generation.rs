//! Stream retry and tool-phase execution for the generation pipeline.

use tracing::{debug, error, instrument, warn};

use crate::engine::tools;
use crate::memory::agent::{AgentSearchContext, CallerIdentity, MemoryAgent};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::compaction_impls::resolve_embed_config;
use crate::memory::researcher::MemoryResearcher;
use crate::tools::context::{NoopRag, SharedToolContext};
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
#[instrument(skip(ctx, effective_config, agent_model, researcher_model, character_definition, user_definition, request, result), fields(char = char_name))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_tool_phase(
    ctx: &GenContext,
    data_dir: &std::path::Path,
    char_name: &str,
    effective_config: &LoadedConfig,
    agent_model: &shore_config::models::ResolvedModel,
    researcher_model: &Option<shore_config::models::ResolvedModel>,
    character_definition: &Option<String>,
    user_definition: &Option<String>,
    request: &mut shore_llm_client::types::LlmRequest,
    result: shore_llm_client::types::StreamResult,
) -> Result<tools::ToolLoopResult, Box<dyn std::error::Error + Send + Sync>> {
    debug!(character = char_name, "run_tool_phase starting");
    let memory_db = {
        let mut registry = ctx.registry.lock().await;
        match registry.get_or_open_db(char_name) {
            Ok(db) => db,
            Err(e) => {
                warn!(
                    character = char_name,
                    error = %e,
                    "Failed to open memory DB — memory tools disabled for this turn"
                );
                return Ok(tools::ToolLoopResult {
                    result,
                    intermediate_messages: vec![],
                });
            }
        }
    };

    let char_def = character_definition.clone().unwrap_or_default();
    let user_def = user_definition.clone().unwrap_or_default();

    let image_gen_config = crate::memory::compaction_impls::resolve_image_gen_config(
        effective_config.app.defaults.image_generation.as_deref(),
        &effective_config.models.image_generation,
    )
    .ok();

    // Build semantic search context (graceful: None if no embedding model configured).
    let search_ctx = match resolve_embed_config(
        effective_config.app.defaults.embedding.as_deref(),
        &effective_config.models.embedding,
    ) {
        Ok(embed_config) => {
            let mut registry = ctx.registry.lock().await;
            match registry
                .get_or_open_vs(char_name, embed_config.dimensions)
                .await
            {
                Ok(vs) => Some(AgentSearchContext::new(
                    vs,
                    ctx.llm_client.inner().clone(),
                    embed_config,
                )),
                Err(e) => {
                    warn!("Failed to open vector store for semantic search: {e}");
                    None
                }
            }
        }
        Err(_) => None,
    };

    let tool_ctx = HandlerToolContext {
        inner: SharedToolContext {
            db: memory_db,
            agent: MemoryAgent::one_shot(
                CallerIdentity::Char,
                char_name,
                &effective_config.app.defaults.resolve_display_name(),
            ),
            agent_llm: RealAgentLlm::new(
                ctx.llm_client.clone(),
                char_name.to_owned(),
                CallType::MemoryAgent,
            ),
            agent_model_val: agent_model.clone(),
            researcher: researcher_model
                .as_ref()
                .map(|_| MemoryResearcher::new(char_def, user_def)),
            researcher_llm_val: researcher_model.as_ref().map(|_| {
                RealAgentLlm::new(
                    ctx.llm_client.clone(),
                    char_name.to_owned(),
                    CallType::Researcher,
                )
            }),
            researcher_model_val: researcher_model.clone(),
            rag: NoopRag,
            search_ctx,
            image_dir_val: data_dir
                .join(char_name)
                .join("images")
                .to_string_lossy()
                .into_owned(),
            llm_client_val: ctx.llm_client.inner().clone(),
            image_gen_config_val: image_gen_config,
            search_config_val: effective_config.app.behavior.tool_use.search.clone(),
            character_name_val: char_name.to_owned(),
            scratchpad_dir_val: data_dir
                .join(char_name)
                .join("scratchpad")
                .to_string_lossy()
                .into_owned(),
        },
        autonomy_val: ctx.autonomy.clone(),
    };

    let thinking_enabled = request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get("budget_tokens"))
        .and_then(|v| v.as_u64())
        .is_some_and(|b| b > 0);

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
