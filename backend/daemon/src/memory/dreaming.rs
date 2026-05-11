use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shore_config::app::DreamingConfig;
use shore_config::cron::CronSchedule;
use shore_config::{
    character_memory_dir, character_workspace_dir, LoadedConfig, SOUL_FILE, USER_FILE,
};

use shore_ledger::{CallType, LedgerClient};
use shore_llm::types::{GenerateResponse, LlmRequest};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::memory::deferred_edits::MEMORY_INDEX_FILE;
use crate::memory::markdown_store::{MarkdownEntry, MarkdownMemoryStore, MarkdownStoreError};
use crate::tools::context::SharedToolContext;
use crate::tools::{self as tool_system, ToolContext};

const MIN_PROMOTION_SCORE: f32 = 0.60;
const MIN_CANDIDATE_LEN: usize = 18;
const MAX_CANDIDATES: usize = 80;
const MAX_DIARY_ITEMS: usize = 12;
const MAX_INDEX_FILES: usize = 40;
const MAX_RECENT_INDEX_FILES: usize = 12;
const MAX_INDEX_THROUGHLINES: usize = 16;
const DREAM_DATA_DIR: &str = "dreams";
const DREAM_REPORTS_DIR: &str = "reports";
const DREAM_STATE_FILE: &str = "state.json";
const DREAM_STATE_REL: &str = "dreams/state.json";
const LEGACY_DREAM_STATE_REL: &str = ".dreams/state.json";
const DREAM_DIARY_HEADER: &str = "# Dreams\n\nThis file is the human-readable Dream Diary for Shore's memory consolidation system.\n\nIt is not long-term memory.\nDurable notes live in ordinary markdown memory files.\n`MEMORY.md` is the prompt-visible memory index.\nMachine-facing dreaming state lives under the character data directory in `dreams/*.json`, not in markdown memory.\n\nEditing or deleting Dream Diary sections does not directly change memory notes or the prompt-visible index.\n\n";

#[derive(Debug, thiserror::Error)]
pub enum DreamingError {
    #[error("io: {0}")]
    Io(String),
    #[error("memory: {0}")]
    Memory(String),
    #[error("config: {0}")]
    Config(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("invalid schedule: {0}")]
    Schedule(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DreamState {
    pub last_run_at: Option<String>,
    pub runs: u64,
    #[serde(default)]
    pub last_candidates_path: Option<String>,
    #[serde(default)]
    pub last_signals_path: Option<String>,
    #[serde(default)]
    pub last_promotions_path: Option<String>,
    #[serde(default)]
    pub seen_candidates: BTreeMap<String, DreamSeenState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DreamSeenState {
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub recall_count: u32,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DreamEvidence {
    pub source: String,
    pub line: Option<usize>,
    pub source_kind: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DreamGate {
    pub name: String,
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamCandidate {
    pub id: String,
    pub text: String,
    pub source: String,
    pub line: Option<usize>,
    pub source_kind: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub recall_count: u32,
    pub unique_source_count: usize,
    pub unique_query_count: u32,
    pub theme_hits: Vec<String>,
    pub recency_score: f32,
    pub durability_score: f32,
    pub specificity_score: f32,
    pub promotion_score: f32,
    pub score: f32,
    pub gates: Vec<DreamGate>,
    pub promote: bool,
    pub decision_reason: String,
    pub evidence: Vec<DreamEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamTheme {
    pub theme: String,
    pub hits: usize,
    pub candidate_ids: Vec<String>,
    pub example: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamPromotion {
    pub text: String,
    pub score: f32,
    pub evidence: Vec<DreamEvidence>,
    pub gates_passed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamRejection {
    pub text: String,
    pub score: f32,
    pub reason: String,
    pub failed_gates: Vec<String>,
    pub evidence: Vec<DreamEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamPhaseSummary {
    pub phase: String,
    pub summary: String,
    pub candidate_count: usize,
    pub promoted_count: usize,
    pub rejected_count: usize,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamSweepResult {
    pub character: String,
    pub dry_run: bool,
    pub ran_at: String,
    #[serde(default)]
    pub mode: String,
    pub phase_summaries: Vec<DreamPhaseSummary>,
    pub candidate_count: usize,
    pub indexed_count: usize,
    pub promoted_count: usize,
    pub rejected_count: usize,
    pub candidates: Vec<DreamCandidate>,
    pub rem_themes: Vec<DreamTheme>,
    pub promotions: Vec<DreamPromotion>,
    pub rejected: Vec<DreamRejection>,
    pub indexed: Vec<String>,
    pub promoted: Vec<String>,
    pub paths_written: Vec<String>,
    pub would_write_paths: Vec<String>,
    pub staged_path: Option<String>,
    pub dreams_path: Option<String>,
    pub memory_path: Option<String>,
    #[serde(default)]
    pub inspected: Vec<String>,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub tool_rounds: u32,
    #[serde(default)]
    pub audit_appended: bool,
    #[serde(default)]
    pub final_report: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamStatus {
    pub character: String,
    pub enabled: bool,
    pub frequency: String,
    pub last_run_at: Option<String>,
    pub due: bool,
    pub state_path: String,
    pub dreams_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LightPhaseOutput {
    sources_reviewed: usize,
    candidates_staged: usize,
    duplicates_ignored: usize,
    generated_sources_ignored: usize,
    candidates: Vec<DreamCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemPhaseOutput {
    themes: Vec<DreamTheme>,
    reinforcement_signals: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeepPhaseOutput {
    candidates: Vec<DreamCandidate>,
    promoted: Vec<DreamPromotion>,
    rejected: Vec<DreamRejection>,
}

type PhaseReportPaths<'a> = (&'a Path, &'a Path, &'a Path);

pub async fn dream_status(
    data_dir: &Path,
    config_dir: &Path,
    character: &str,
    cfg: &DreamingConfig,
) -> Result<DreamStatus, DreamingError> {
    let _ = character_memory_dir(config_dir, character);
    let state = read_state(data_dir, config_dir, character).await?;
    let due = cfg.enabled && is_due(cfg, state.last_run_at.as_deref())?;
    Ok(DreamStatus {
        character: character.to_string(),
        enabled: cfg.enabled,
        frequency: cfg.frequency.clone(),
        last_run_at: state.last_run_at,
        due,
        state_path: dream_state_path(data_dir, character).display().to_string(),
        dreams_path: crate::memory::dreams_log::dreams_log_path(data_dir, character)
            .display()
            .to_string(),
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run_librarian_sweep(
    loaded_config: &LoadedConfig,
    data_dir: &Path,
    llm_client: &LedgerClient,
    character: &str,
    cached_request: Option<&LlmRequest>,
    dry_run: bool,
    force: bool,
    http: Option<std::sync::Arc<crate::http::DaemonHttpState>>,
) -> Result<Option<DreamSweepResult>, DreamingError> {
    let cfg = &loaded_config.app.memory.dreaming;
    let memory_dir = character_memory_dir(&loaded_config.dirs.config, character);
    let workspace_dir = character_workspace_dir(&loaded_config.dirs.config, character);
    let memory_index_path = workspace_dir.join(MEMORY_INDEX_FILE);
    let state_path = dream_state_path(data_dir, character);
    let state = read_state(data_dir, &loaded_config.dirs.config, character).await?;
    if !force && !dry_run && !is_due(cfg, state.last_run_at.as_deref())? {
        return Ok(None);
    }

    let store = MarkdownMemoryStore::open(&memory_dir)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    let before = snapshot_memory_files(&store, &memory_index_path).await?;
    let now = Local::now();
    let ran_at = now.to_rfc3339();

    let mut request =
        build_librarian_request(loaded_config, character, cached_request, dry_run, &ran_at).await?;
    request.forensic_character = Some(character.to_string());
    request.rid = None;
    let tool_ctx = std::sync::Arc::new(
        build_librarian_tool_context(loaded_config, data_dir, llm_client, character, dry_run)
            .await
            .ok_or_else(|| DreamingError::Config("failed to build dreaming tool context".into()))?,
    );
    let claude_code_session =
        crate::claude_code::prepare_request(&mut request, http.as_ref(), None, tool_ctx.clone())
            .await
            .map_err(DreamingError::Llm)?;

    info!(
        character,
        dry_run,
        max_tool_rounds = cfg.max_tool_rounds,
        "Dreaming: starting AI librarian pass"
    );

    let loop_result = run_private_librarian_loop(
        llm_client,
        &mut request,
        tool_ctx.as_ref(),
        character,
        cfg.max_tool_rounds,
        dry_run,
        claude_code_session.as_ref(),
    )
    .await?;

    if dry_run {
        let would_write_paths = vec![
            memory_index_path.display().to_string(),
            crate::memory::dreams_log::dreams_log_path(data_dir, character)
                .display()
                .to_string(),
            state_path.display().to_string(),
        ];
        return Ok(Some(DreamSweepResult {
            character: character.to_string(),
            dry_run,
            ran_at,
            mode: "ai_librarian".to_string(),
            phase_summaries: vec![DreamPhaseSummary {
                phase: "librarian".to_string(),
                summary: format!(
                    "dry-run AI librarian pass inspected memory with {} tool round(s); writes were disabled",
                    loop_result.tool_rounds
                ),
                candidate_count: 0,
                promoted_count: 0,
                rejected_count: 0,
                paths: Vec::new(),
            }],
            candidate_count: 0,
            indexed_count: 0,
            promoted_count: 0,
            rejected_count: 0,
            candidates: Vec::new(),
            rem_themes: Vec::new(),
            promotions: Vec::new(),
            rejected: Vec::new(),
            indexed: Vec::new(),
            promoted: Vec::new(),
            paths_written: Vec::new(),
            would_write_paths,
            staged_path: None,
            dreams_path: None,
            memory_path: None,
            inspected: loop_result.inspected,
            changed: Vec::new(),
            tools_used: loop_result.tools_used,
            tool_rounds: loop_result.tool_rounds,
            audit_appended: false,
            final_report: loop_result.final_report,
        }));
    }

    let memory_created_by_fallback =
        ensure_memory_index_after_librarian(&store, &memory_index_path, character, &ran_at).await?;
    if memory_created_by_fallback {
        if let Err(e) =
            crate::memory::deferred_edits::note_memory_index_deferred(&data_dir.join(character))
        {
            warn!(
                character,
                error = %e,
                "Dreaming: failed to defer fallback MEMORY.md activation"
            );
        }
    }

    // Always write the daemon-controlled audit entry. DREAMS.md lives in the
    // data directory, outside the workspace, so the model cannot reach it
    // through the write tool — every audit is daemon-generated.
    append_librarian_audit(
        data_dir,
        character,
        &ran_at,
        &loop_result.inspected,
        &loop_result.changed,
        memory_created_by_fallback,
        loop_result.final_report.as_deref(),
    )
    .await?;
    let audit_appended = true;

    let mut next_state = state;
    next_state.last_run_at = Some(ran_at.clone());
    next_state.runs += 1;
    next_state.last_candidates_path = None;
    next_state.last_signals_path = None;
    next_state.last_promotions_path = None;
    write_state(data_dir, character, &next_state).await?;

    let after = snapshot_memory_files(&store, &memory_index_path).await?;
    let mut changed = changed_paths(&before, &after);
    if !changed.iter().any(|path| path == DREAM_STATE_REL) {
        changed.push(DREAM_STATE_REL.to_string());
    }
    let paths_written = changed
        .iter()
        .map(|path| {
            if path == MEMORY_INDEX_FILE {
                memory_index_path.display().to_string()
            } else if path == DREAM_STATE_REL {
                state_path.display().to_string()
            } else {
                memory_dir.join(path).display().to_string()
            }
        })
        .collect::<Vec<_>>();
    let indexed_count = usize::from(after.contains_key(MEMORY_INDEX_FILE));

    Ok(Some(DreamSweepResult {
        character: character.to_string(),
        dry_run,
        ran_at,
        mode: "ai_librarian".to_string(),
        phase_summaries: vec![DreamPhaseSummary {
            phase: "librarian".to_string(),
            summary: format!(
                "AI librarian pass used {} tool round(s), changed {} file(s), and {} DREAMS.md audit fallback",
                loop_result.tool_rounds,
                changed.len(),
                if audit_appended { "needed a" } else { "did not need a" }
            ),
            candidate_count: 0,
            promoted_count: indexed_count,
            rejected_count: 0,
            paths: paths_written.clone(),
        }],
        candidate_count: 0,
        indexed_count,
        promoted_count: 0,
        rejected_count: 0,
        candidates: Vec::new(),
        rem_themes: Vec::new(),
        promotions: Vec::new(),
        rejected: Vec::new(),
        indexed: if after.contains_key(MEMORY_INDEX_FILE) {
            vec![MEMORY_INDEX_FILE.to_string()]
        } else {
            Vec::new()
        },
        promoted: Vec::new(),
        paths_written,
        would_write_paths: Vec::new(),
        staged_path: Some(state_path.display().to_string()),
        dreams_path: Some(
            crate::memory::dreams_log::dreams_log_path(data_dir, character)
                .display()
                .to_string(),
        ),
        memory_path: Some(memory_index_path.display().to_string()),
        inspected: loop_result.inspected,
        changed,
        tools_used: loop_result.tools_used,
        tool_rounds: loop_result.tool_rounds,
        audit_appended,
        final_report: loop_result.final_report,
    }))
}

/// Legacy deterministic sweep retained only for dry-run diagnostics and
/// fallback-oriented unit coverage. Production scheduled and command-driven
/// dreaming uses [`run_librarian_sweep`].
pub async fn run_legacy_diagnostic_sweep(
    data_dir: &Path,
    config_dir: &Path,
    character: &str,
    cfg: &DreamingConfig,
    dry_run: bool,
    force: bool,
) -> Result<Option<DreamSweepResult>, DreamingError> {
    let memory_dir = character_memory_dir(config_dir, character);
    let workspace_dir = character_workspace_dir(config_dir, character);
    let memory_index_path = workspace_dir.join(MEMORY_INDEX_FILE);
    let character_data_dir = data_dir.join(character);
    let dream_dir = dream_data_dir(data_dir, character);
    let state_path = dream_state_path(data_dir, character);
    let state = read_state(data_dir, config_dir, character).await?;
    if !force && !dry_run && !is_due(cfg, state.last_run_at.as_deref())? {
        return Ok(None);
    }

    let store = MarkdownMemoryStore::open(&memory_dir)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    let entries = store
        .list_all()
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    let now = Local::now();
    let ran_at = now.to_rfc3339();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    let day = now.format("%Y-%m-%d").to_string();

    let light = run_light_phase(entries, &state, &ran_at);
    let rem = run_rem_phase(&light.candidates);
    let deep = run_deep_phase(&store, light.candidates.clone(), &rem).await?;

    let candidates_file = format!("candidates-{stamp}.json");
    let signals_file = format!("phase-signals-{stamp}.json");
    let promotions_file = format!("promotions-{stamp}.json");
    let candidates_rel = dream_data_rel(&candidates_file);
    let signals_rel = dream_data_rel(&signals_file);
    let promotions_rel = dream_data_rel(&promotions_file);
    let candidates_path = dream_dir.join(&candidates_file);
    let signals_path = dream_dir.join(&signals_file);
    let promotions_path = dream_dir.join(&promotions_file);
    let light_report_path = dream_dir
        .join(DREAM_REPORTS_DIR)
        .join("light")
        .join(format!("{day}.md"));
    let rem_report_path = dream_dir
        .join(DREAM_REPORTS_DIR)
        .join("rem")
        .join(format!("{day}.md"));
    let deep_report_path = dream_dir
        .join(DREAM_REPORTS_DIR)
        .join("deep")
        .join(format!("{day}.md"));

    let would_write_paths = vec![
        candidates_path.display().to_string(),
        signals_path.display().to_string(),
        promotions_path.display().to_string(),
        state_path.display().to_string(),
        crate::memory::dreams_log::dreams_log_path(data_dir, character)
            .display()
            .to_string(),
        memory_index_path.display().to_string(),
        light_report_path.display().to_string(),
        rem_report_path.display().to_string(),
        deep_report_path.display().to_string(),
    ];

    let initial_phase_summaries = phase_summaries(
        &light,
        &rem,
        &deep,
        if dry_run { &[] } else { &would_write_paths },
    );
    let promoted = deep
        .promoted
        .iter()
        .map(|promotion| promotion.text.clone())
        .collect::<Vec<_>>();

    if dry_run {
        return Ok(Some(DreamSweepResult {
            character: character.to_string(),
            dry_run,
            ran_at,
            mode: "legacy_diagnostic".to_string(),
            phase_summaries: initial_phase_summaries,
            candidate_count: deep.candidates.len(),
            indexed_count: deep.promoted.len(),
            promoted_count: deep.promoted.len(),
            rejected_count: deep.rejected.len(),
            candidates: deep.candidates,
            rem_themes: rem.themes,
            promotions: deep.promoted,
            rejected: deep.rejected,
            indexed: promoted.clone(),
            promoted,
            paths_written: Vec::new(),
            would_write_paths,
            staged_path: None,
            dreams_path: None,
            memory_path: None,
            inspected: Vec::new(),
            changed: Vec::new(),
            tools_used: Vec::new(),
            tool_rounds: 0,
            audit_appended: false,
            final_report: None,
        }));
    }

    write_data_json(&character_data_dir, &candidates_path, &deep.candidates).await?;
    write_data_json(&character_data_dir, &signals_path, &rem).await?;
    write_data_json(&character_data_dir, &promotions_path, &deep).await?;
    append_dream_diary(data_dir, character, &ran_at, &light, &rem, &deep).await?;
    write_phase_reports(
        &character_data_dir,
        &ran_at,
        (&light_report_path, &rem_report_path, &deep_report_path),
        &light,
        &rem,
        &deep,
    )
    .await?;
    write_memory_index(
        &store,
        &memory_index_path,
        character,
        &ran_at,
        &deep.promoted,
    )
    .await?;

    let mut next_state = state;
    next_state.last_run_at = Some(ran_at.clone());
    next_state.runs += 1;
    next_state.last_candidates_path = Some(candidates_rel.clone());
    next_state.last_signals_path = Some(signals_rel.clone());
    next_state.last_promotions_path = Some(promotions_rel.clone());
    update_seen_state(&mut next_state, &deep.candidates);
    write_state(data_dir, character, &next_state).await?;

    let paths_written = would_write_paths;

    Ok(Some(DreamSweepResult {
        character: character.to_string(),
        dry_run,
        ran_at,
        mode: "legacy_diagnostic".to_string(),
        phase_summaries: phase_summaries(&light, &rem, &deep, &paths_written),
        candidate_count: deep.candidates.len(),
        indexed_count: deep.promoted.len(),
        promoted_count: deep.promoted.len(),
        rejected_count: deep.rejected.len(),
        candidates: deep.candidates,
        rem_themes: rem.themes,
        promotions: deep.promoted,
        rejected: deep.rejected,
        indexed: promoted.clone(),
        promoted,
        paths_written,
        would_write_paths: Vec::new(),
        staged_path: Some(candidates_path.display().to_string()),
        dreams_path: Some(
            crate::memory::dreams_log::dreams_log_path(data_dir, character)
                .display()
                .to_string(),
        ),
        memory_path: Some(memory_index_path.display().to_string()),
        inspected: Vec::new(),
        changed: Vec::new(),
        tools_used: Vec::new(),
        tool_rounds: 0,
        audit_appended: false,
        final_report: None,
    }))
}

#[derive(Debug, Default)]
struct LibrarianLoopResult {
    final_report: Option<String>,
    inspected: Vec<String>,
    changed: Vec<String>,
    tools_used: Vec<String>,
    tool_rounds: u32,
}

type MemorySnapshot = BTreeMap<String, String>;

async fn build_librarian_request(
    loaded_config: &LoadedConfig,
    character: &str,
    cached_request: Option<&LlmRequest>,
    dry_run: bool,
    ran_at: &str,
) -> Result<LlmRequest, DreamingError> {
    let display_name = loaded_config.app.defaults.resolve_display_name();
    let character_data_dir = loaded_config.dirs.data.join(character);
    if let Err(e) = crate::memory::deferred_edits::ensure_active_prompt_snapshot(
        &character_data_dir,
        &loaded_config.dirs.config,
        character,
    ) {
        warn!(
            character,
            error = %e,
            "Dreaming: failed to prepare active prompt snapshot"
        );
    }

    let character_definition =
        crate::memory::deferred_edits::load_active_prompt_file(&character_data_dir, SOUL_FILE);
    let user_definition =
        crate::memory::deferred_edits::load_active_prompt_file(&character_data_dir, USER_FILE);
    let system = build_librarian_prompt(
        character,
        &display_name,
        character_definition.as_deref(),
        user_definition.as_deref(),
        dry_run,
        ran_at,
    );
    let user_prompt = if dry_run {
        "Run the dry-run memory librarian pass now. Inspect memory files with read-only tools and finish with a proposed plan. Do not write, edit, or emit a user-facing message."
    } else {
        "Run the memory librarian pass now. Use memory tools to inspect and improve workspace/memory, update MEMORY.md (at the workspace root), and finish with a concise summary of what you inspected and changed. The daemon writes the dreams audit log automatically; do not try to write DREAMS.md yourself. Do not emit a user-facing message."
    };
    if let Some(cached) = cached_request {
        let mut request = cached.clone();
        request.rid = None;
        request.messages.push(json!({
            "role": "system",
            "content": format!("{system}\n\n{user_prompt}"),
        }));
        return Ok(request);
    }

    let resolved = resolve_dreaming_model(loaded_config)?;
    let tools = build_librarian_tool_defs(character, &display_name, dry_run);
    LedgerClient::build_request_with_provider_keys(
        resolved,
        &loaded_config.providers,
        vec![json!({"role": "user", "content": user_prompt})],
        Some(json!(system)),
        Some(tools),
        None,
    )
    .map_err(|e| DreamingError::Llm(e.to_string()))
}

fn resolve_dreaming_model(
    loaded_config: &LoadedConfig,
) -> Result<&shore_config::models::ResolvedModel, DreamingError> {
    if let Some(name) = loaded_config
        .app
        .defaults
        .resolve_background_model_name(shore_config::app::BackgroundTask::Dreaming)
    {
        return loaded_config
            .models
            .find_model(name)
            .map_err(|e| DreamingError::Config(e.to_string()));
    }
    loaded_config
        .models
        .first_chat_model()
        .ok_or_else(|| DreamingError::Config("no chat model configured for dreaming".into()))
}

fn build_librarian_prompt(
    character: &str,
    display_name: &str,
    character_definition: Option<&str>,
    user_definition: Option<&str>,
    dry_run: bool,
    _ran_at: &str,
) -> String {
    const TEMPLATE: &str = crate::include_prompt!("../../prompts/memory/dreaming/librarian.md");
    let mut prompt = format!(
        "{}\n",
        TEMPLATE
            .replace("{{character}}", character)
            .replace("{{display_name}}", display_name)
    );

    if dry_run {
        prompt.push_str(
            "\nThis is a dry run. Write and edit tools are unavailable. Inspect files and produce a concise internal plan of what would change.\n",
        );
    }

    if let Some(definition) = character_definition.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("\n<character_identity>\n");
        prompt.push_str(definition);
        prompt.push_str("\n</character_identity>\n");
    }
    if let Some(definition) = user_definition.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("\n<user_profile>\n");
        prompt.push_str(definition);
        prompt.push_str("\n</user_profile>\n");
    }
    prompt
}

fn build_librarian_tool_defs(character: &str, display_name: &str, dry_run: bool) -> Vec<Value> {
    let toggles = shore_config::app::ToolToggles::default();
    let allowed = |name: &str| {
        if dry_run {
            matches!(
                name,
                "read" | "list_files" | "search" | "search_history" | "check_time"
            )
        } else {
            matches!(
                name,
                "read"
                    | "write"
                    | "edit"
                    | "list_files"
                    | "search"
                    | "search_history"
                    | "check_time"
            )
        }
    };
    tool_system::render_tool_defs(false, &toggles, character, display_name)
        .into_iter()
        .filter(|tool| {
            tool.get("name")
                .and_then(|name| name.as_str())
                .is_some_and(allowed)
        })
        .collect()
}

async fn build_librarian_tool_context(
    loaded_config: &LoadedConfig,
    data_dir: &Path,
    llm_client: &LedgerClient,
    character: &str,
    _dry_run: bool,
) -> Option<SharedToolContext> {
    let character_data_dir = data_dir.join(character);
    let image_gen_config = crate::memory::compaction_impls::resolve_image_gen_config(
        loaded_config.app.defaults.image_generation.as_deref(),
        &loaded_config.models.image_generation,
    )
    .ok();
    let embedder = crate::memory::retrieval::resolve_embedder(
        loaded_config.app.defaults.embedding.as_deref(),
        &loaded_config.models.embedding,
        llm_client.inner().http_client(),
    )
    .ok();

    Some(SharedToolContext {
        image_dir_val: character_data_dir
            .join("images")
            .to_string_lossy()
            .into_owned(),
        llm_client_val: llm_client.inner().clone(),
        image_gen_config_val: image_gen_config,
        search_config_val: loaded_config.app.behavior.tool_use.search.clone(),
        character_name_val: character.to_string(),
        workspace_dir_val: character_workspace_dir(&loaded_config.dirs.config, character)
            .to_string_lossy()
            .into_owned(),
        markdown_store_val: MarkdownMemoryStore::open_sync(character_memory_dir(
            &loaded_config.dirs.config,
            character,
        ))
        .ok(),
        memory_retrieval_config_val: loaded_config.app.memory.retrieval.clone(),
        embedder_val: embedder,
        memory_index_path_val: crate::memory::workspace_index::index_path(
            &loaded_config.dirs.cache,
            character,
        ),
        config_dir_val: loaded_config.dirs.config.to_string_lossy().into_owned(),
        character_data_dir_val: character_data_dir.to_string_lossy().into_owned(),
    })
}

async fn run_private_librarian_loop(
    client: &LedgerClient,
    request: &mut LlmRequest,
    tool_ctx: &dyn ToolContext,
    character: &str,
    max_tool_rounds: u32,
    dry_run: bool,
    claude_code_session: Option<&crate::engine::mcp_session::McpSessionGuard>,
) -> Result<LibrarianLoopResult, DreamingError> {
    let mut loop_result = LibrarianLoopResult::default();

    for iteration in 0..max_tool_rounds {
        let mut resp = client
            .generate(request, CallType::Dreaming, character, false)
            .await
            .map_err(|e| DreamingError::Llm(e.to_string()))?;
        crate::claude_code::splice_generate_response_from_session(&mut resp, claude_code_session)
            .await;
        remember_final_report(&mut loop_result, &resp);
        push_assistant_response(request, &resp);

        let tool_uses = crate::content_util::extract_tool_uses(&resp.content_blocks);
        if tool_uses.is_empty() || resp.finish_reason != "tool_use" {
            return Ok(loop_result);
        }

        loop_result.tool_rounds += 1;
        let mut tool_results = Vec::new();
        for (id, name, input) in tool_uses {
            loop_result.tools_used.push(name.clone());
            record_librarian_tool_intent(&mut loop_result, &name, &input);
            debug!(
                character,
                iteration,
                tool = %name,
                input = %input,
                "Dreaming: executing private librarian tool"
            );

            let (output, is_error) =
                if let Some(blocked) = blocked_librarian_tool_result(&name, &input, dry_run) {
                    blocked
                } else {
                    crate::content_util::dispatch_result_to_output(
                        tool_system::dispatch_tool(&name, input.clone(), tool_ctx).await,
                    )
                };

            if !is_error && matches!(name.as_str(), "write" | "edit") {
                if let Some(path) = tool_path(&input) {
                    loop_result.changed.push(path.to_string());
                }
            }

            tool_results.push(crate::content_util::build_tool_result_json(
                &id, &output, is_error,
            ));
        }
        request
            .messages
            .push(json!({"role": "user", "content": tool_results}));
    }

    warn!(
        character,
        max_tool_rounds, "Dreaming: private librarian tool loop hit configured cap"
    );
    Ok(loop_result)
}

fn remember_final_report(loop_result: &mut LibrarianLoopResult, resp: &GenerateResponse) {
    let text = resp.extract_text();
    if !text.trim().is_empty() {
        loop_result.final_report = Some(text.trim().to_string());
    }
}

fn push_assistant_response(request: &mut LlmRequest, resp: &GenerateResponse) {
    let assistant_content: Vec<Value> = resp
        .content_blocks
        .iter()
        .filter_map(|block| {
            crate::content_util::content_block_to_request_json_for_sdk(block, &request.sdk)
        })
        .collect();

    if !assistant_content.is_empty() {
        request
            .messages
            .push(json!({"role": "assistant", "content": assistant_content}));
    } else if !resp.content.trim().is_empty() {
        request
            .messages
            .push(json!({"role": "assistant", "content": resp.content}));
    }
}

fn blocked_librarian_tool_result(
    name: &str,
    _input: &Value,
    dry_run: bool,
) -> Option<(String, bool)> {
    if name == "exec" {
        return Some((
            "exec is not available during private dreaming passes".to_string(),
            true,
        ));
    }
    if dry_run && matches!(name, "write" | "edit") {
        return Some((
            "dry-run dreaming does not write or edit files".to_string(),
            true,
        ));
    }
    None
}

fn record_librarian_tool_intent(result: &mut LibrarianLoopResult, name: &str, input: &Value) {
    match name {
        "read" | "list_files" => {
            if let Some(path) = tool_path(input) {
                push_unique(&mut result.inspected, path.to_string());
            }
        }
        "search" | "search_history" => {
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("<missing query>");
            let scope = tool_path(input).unwrap_or("memory");
            push_unique(&mut result.inspected, format!("{name}:{scope}:{query}"));
        }
        _ => {}
    }
}

fn tool_path(input: &Value) -> Option<&str> {
    input.get("path").and_then(|v| v.as_str())
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if !items.iter().any(|existing| existing == &value) {
        items.push(value);
    }
}

async fn snapshot_memory_files(
    store: &MarkdownMemoryStore,
    memory_index_path: &Path,
) -> Result<MemorySnapshot, DreamingError> {
    let mut snapshot = BTreeMap::new();
    for entry in store
        .list_all()
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?
    {
        snapshot.insert(entry.path, entry.content);
    }
    match fs::read_to_string(memory_index_path).await {
        Ok(content) => {
            snapshot.insert(MEMORY_INDEX_FILE.to_string(), content);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(DreamingError::Io(e.to_string())),
    }
    Ok(snapshot)
}

fn changed_paths(before: &MemorySnapshot, after: &MemorySnapshot) -> Vec<String> {
    let mut keys = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.retain(|path| before.get(path) != after.get(path));
    keys.into_iter().collect()
}

async fn ensure_memory_index_after_librarian(
    store: &MarkdownMemoryStore,
    memory_index_path: &Path,
    character: &str,
    ran_at: &str,
) -> Result<bool, DreamingError> {
    match fs::read_to_string(memory_index_path).await {
        Ok(content) if !content.trim().is_empty() => Ok(false),
        Ok(_) => {
            write_fallback_memory_index(store, memory_index_path, character, ran_at).await?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_fallback_memory_index(store, memory_index_path, character, ran_at).await?;
            Ok(true)
        }
        Err(e) => Err(DreamingError::Io(e.to_string())),
    }
}

async fn write_fallback_memory_index(
    store: &MarkdownMemoryStore,
    memory_index_path: &Path,
    character: &str,
    ran_at: &str,
) -> Result<(), DreamingError> {
    let entries = store
        .list_all()
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    let mut body = String::new();
    body.push_str("# Memory Index\n\n");
    body.push_str(
        "This file is the character's map of long-term memory. It is not the full memory itself.\n",
    );
    body.push_str("Use it to decide which memory files to inspect before answering.\n\n");
    body.push_str("Core user facts and standing behavior guidance are already loaded from USER.md and AGENTS.md; do not duplicate them here unless needed as pointers to memory files.\n\n");
    body.push_str(&format!("Character: {character}\n"));
    body.push_str(&format!("Last updated: {ran_at}\n"));
    body.push_str("Fallback note: Rust created this minimal index because the AI librarian pass did not leave a usable MEMORY.md.\n\n");
    body.push_str("## Memory areas\n\n");
    if entries.is_empty() {
        body.push_str("- No ordinary memory files were found yet.\n");
    } else {
        for entry in entries.iter().take(MAX_INDEX_FILES) {
            body.push_str(&format!(
                "- `{}` - {}\n",
                entry.path,
                memory_file_summary(entry)
            ));
        }
    }
    body.push_str("\n## Recently updated files\n\n");
    body.push_str("- Needs review during the next AI librarian pass.\n");
    body.push_str("\n## Current conversational throughlines\n\n");
    body.push_str("- Needs review during the next AI librarian pass.\n");
    body.push_str("\n## Needs review\n\n");
    body.push_str("- Previous dreaming pass did not update MEMORY.md directly.\n");
    if let Some(parent) = memory_index_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DreamingError::Io(e.to_string()))?;
    }
    fs::write(memory_index_path, body)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

async fn append_librarian_audit(
    data_dir: &Path,
    character: &str,
    ran_at: &str,
    inspected: &[String],
    changed: &[String],
    memory_created_by_fallback: bool,
    final_report: Option<&str>,
) -> Result<(), DreamingError> {
    let mut body = String::new();
    body.push_str(&format!("AI librarian dreaming pass at `{ran_at}`.\n\n"));
    body.push_str("Files inspected:\n");
    push_markdown_list_or_none(&mut body, inspected);
    body.push_str("\nFiles changed by tools:\n");
    push_markdown_list_or_none(&mut body, changed);
    body.push_str("\nMEMORY.md updated:\n");
    body.push_str(if memory_created_by_fallback {
        "- Yes, by Rust fallback (the model left it missing or empty).\n"
    } else {
        "- Present after the pass.\n"
    });
    if let Some(report) = final_report.filter(|report| !report.trim().is_empty()) {
        body.push_str("\nFinal internal report:\n");
        body.push_str(report.trim());
        body.push('\n');
    }

    crate::memory::dreams_log::append_dream_entry(
        data_dir,
        character,
        Local::now().fixed_offset(),
        "AI librarian dreaming pass",
        &body,
    )
    .await
    .map_err(|e| DreamingError::Io(e.to_string()))
}

fn push_markdown_list_or_none(body: &mut String, items: &[String]) {
    if items.is_empty() {
        body.push_str("- None recorded.\n");
        return;
    }
    for item in items {
        body.push_str(&format!("- `{}`\n", item.replace('`', "'")));
    }
}

pub fn is_due(cfg: &DreamingConfig, last_run_at: Option<&str>) -> Result<bool, DreamingError> {
    let now = Local::now();
    is_due_at(cfg, last_run_at, now)
}

fn is_due_at(
    cfg: &DreamingConfig,
    last_run_at: Option<&str>,
    now: DateTime<Local>,
) -> Result<bool, DreamingError> {
    let schedule = CronSchedule::parse(&cfg.frequency)
        .map_err(|e| DreamingError::Schedule(format!("{}: {e}", cfg.frequency)))?;
    let after = match last_run_at {
        Some(last) => DateTime::parse_from_rfc3339(last)
            .map_err(|e| DreamingError::Schedule(format!("invalid last_run_at {last:?}: {e}")))?
            .with_timezone(&Local),
        None => schedule.initial_due_window_start(now) - Duration::minutes(1),
    };
    let Some(next_due) = schedule.next_after(after) else {
        return Err(DreamingError::Schedule(format!(
            "{}: no matching time found",
            cfg.frequency
        )));
    };
    Ok(next_due <= now)
}

fn dream_data_dir(data_dir: &Path, character: &str) -> PathBuf {
    data_dir.join(character).join(DREAM_DATA_DIR)
}

fn dream_state_path(data_dir: &Path, character: &str) -> PathBuf {
    dream_data_dir(data_dir, character).join(DREAM_STATE_FILE)
}

fn dream_data_rel(file_name: &str) -> String {
    format!("{DREAM_DATA_DIR}/{file_name}")
}

async fn read_state(
    data_dir: &Path,
    config_dir: &Path,
    character: &str,
) -> Result<DreamState, DreamingError> {
    let character_data_dir = data_dir.join(character);
    let state_path = dream_state_path(data_dir, character);
    if let Some(content) = read_data_text(&character_data_dir, &state_path).await? {
        return serde_json::from_str(&content).map_err(|e| DreamingError::Io(e.to_string()));
    }

    read_legacy_state(&character_memory_dir(config_dir, character)).await
}

async fn read_legacy_state(memory_dir: &Path) -> Result<DreamState, DreamingError> {
    let store = MarkdownMemoryStore::open(memory_dir)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    match store.read(LEGACY_DREAM_STATE_REL).await {
        Ok(entry) => {
            serde_json::from_str(&entry.content).map_err(|e| DreamingError::Io(e.to_string()))
        }
        Err(MarkdownStoreError::NotFound(_)) => Ok(DreamState::default()),
        Err(e) => Err(DreamingError::Memory(e.to_string())),
    }
}

async fn read_data_text(
    character_data_dir: &Path,
    path: &Path,
) -> Result<Option<String>, DreamingError> {
    if !path.starts_with(character_data_dir) {
        return Err(DreamingError::Io(format!(
            "dreaming data path escapes character data dir: {}",
            path.display()
        )));
    }

    let base = match fs::canonicalize(character_data_dir).await {
        Ok(base) => base,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DreamingError::Io(e.to_string())),
    };
    let canonical = match fs::canonicalize(path).await {
        Ok(canonical) => canonical,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DreamingError::Io(e.to_string())),
    };
    if !canonical.starts_with(&base) {
        return Err(DreamingError::Io(format!(
            "dreaming data path escapes character data dir: {}",
            path.display()
        )));
    }
    fs::read_to_string(path)
        .await
        .map(Some)
        .map_err(|e| DreamingError::Io(e.to_string()))
}

async fn write_state(
    data_dir: &Path,
    character: &str,
    state: &DreamState,
) -> Result<(), DreamingError> {
    let character_data_dir = data_dir.join(character);
    write_data_json(
        &character_data_dir,
        &dream_state_path(data_dir, character),
        state,
    )
    .await
}

async fn ensure_data_write_path(
    character_data_dir: &Path,
    path: &Path,
) -> Result<(), DreamingError> {
    if !path.starts_with(character_data_dir) {
        return Err(DreamingError::Io(format!(
            "dreaming data path escapes character data dir: {}",
            path.display()
        )));
    }

    fs::create_dir_all(character_data_dir)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DreamingError::Io(e.to_string()))?;
    }

    let base = fs::canonicalize(character_data_dir)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))?;
    let parent = path.parent().unwrap_or(character_data_dir);
    let canonical_parent = fs::canonicalize(parent)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))?;
    if !canonical_parent.starts_with(&base) {
        return Err(DreamingError::Io(format!(
            "dreaming data path escapes character data dir: {}",
            path.display()
        )));
    }

    match fs::canonicalize(path).await {
        Ok(canonical) if !canonical.starts_with(&base) => Err(DreamingError::Io(format!(
            "dreaming data path escapes character data dir: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DreamingError::Io(e.to_string())),
    }
}

fn run_light_phase(
    entries: Vec<MarkdownEntry>,
    state: &DreamState,
    ran_at: &str,
) -> LightPhaseOutput {
    let mut by_key: BTreeMap<String, DreamCandidate> = BTreeMap::new();
    let mut sources_reviewed = 0;
    let mut duplicates_ignored = 0;
    let mut generated_sources_ignored = 0;

    for entry in entries {
        if is_generated_dreaming_path(&entry.path) {
            generated_sources_ignored += 1;
            continue;
        }
        if !is_candidate_source_path(&entry.path) {
            continue;
        }
        sources_reviewed += 1;
        for (idx, line) in entry.content.lines().enumerate() {
            let Some(text) = candidate_text_from_line(line) else {
                continue;
            };
            let key = normalize_candidate_text(&text);
            if key.is_empty() {
                continue;
            }
            let evidence = DreamEvidence {
                source: entry.path.clone(),
                line: Some(idx + 1),
                source_kind: source_kind(&entry.path).to_string(),
                snippet: text.clone(),
            };
            if let Some(existing) = by_key.get_mut(&key) {
                if !existing.evidence.iter().any(|seen| seen == &evidence) {
                    existing.evidence.push(evidence);
                    existing.recall_count += 1;
                    existing.unique_source_count = unique_source_count(&existing.evidence);
                }
                duplicates_ignored += 1;
                continue;
            }

            let prior = state.seen_candidates.get(&key);
            let mut recall_count = prior.map_or(1, |seen| seen.recall_count.saturating_add(1));
            if recall_count == 0 {
                recall_count = 1;
            }
            let mut evidence_sources = prior
                .map(|seen| seen.sources.iter().cloned().collect::<BTreeSet<_>>())
                .unwrap_or_default();
            evidence_sources.insert(evidence.source.clone());
            let first_seen_at = prior
                .filter(|seen| !seen.first_seen_at.is_empty())
                .map(|seen| seen.first_seen_at.clone())
                .unwrap_or_else(|| ran_at.to_string());
            let themes = detect_themes(&text);
            let recency = recency_score(&entry.modified_at);
            let durability = durability_score(&text, &themes);
            let specificity = specificity_score(&text);
            let candidate = DreamCandidate {
                id: candidate_id(&key),
                text: text.clone(),
                source: entry.path.clone(),
                line: Some(idx + 1),
                source_kind: source_kind(&entry.path).to_string(),
                first_seen_at,
                last_seen_at: ran_at.to_string(),
                recall_count,
                unique_source_count: evidence_sources.len(),
                unique_query_count: 0,
                theme_hits: themes,
                recency_score: recency,
                durability_score: durability,
                specificity_score: specificity,
                promotion_score: 0.0,
                score: 0.0,
                gates: Vec::new(),
                promote: false,
                decision_reason: "staged by Light Sleep; Deep has not evaluated it yet".to_string(),
                evidence: vec![evidence],
            };
            by_key.insert(key, candidate);
        }
    }

    let mut candidates = by_key.into_values().collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.durability_score
            .partial_cmp(&a.durability_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.line.cmp(&b.line))
    });
    candidates.truncate(MAX_CANDIDATES);

    LightPhaseOutput {
        sources_reviewed,
        candidates_staged: candidates.len(),
        duplicates_ignored,
        generated_sources_ignored,
        candidates,
    }
}

fn run_rem_phase(candidates: &[DreamCandidate]) -> RemPhaseOutput {
    let mut theme_candidates: BTreeMap<String, Vec<&DreamCandidate>> = BTreeMap::new();
    for candidate in candidates {
        for theme in &candidate.theme_hits {
            theme_candidates
                .entry(theme.clone())
                .or_default()
                .push(candidate);
        }
    }

    let mut themes = theme_candidates
        .iter()
        .map(|(theme, hits)| DreamTheme {
            theme: theme.clone(),
            hits: hits.len(),
            candidate_ids: hits.iter().map(|candidate| candidate.id.clone()).collect(),
            example: hits
                .first()
                .map(|candidate| candidate.text.clone())
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    themes.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| a.theme.cmp(&b.theme)));

    let reinforcement_signals = themes
        .iter()
        .filter(|theme| theme.hits > 1)
        .map(|theme| (theme.theme.clone(), theme.hits))
        .collect::<BTreeMap<_, _>>();

    RemPhaseOutput {
        themes,
        reinforcement_signals,
    }
}

async fn run_deep_phase(
    store: &MarkdownMemoryStore,
    candidates: Vec<DreamCandidate>,
    rem: &RemPhaseOutput,
) -> Result<DeepPhaseOutput, DreamingError> {
    let mut evaluated = Vec::with_capacity(candidates.len());
    let mut promoted = Vec::new();
    let mut rejected = Vec::new();

    for mut candidate in candidates {
        let source_still_present = source_still_contains(store, &candidate).await?;
        candidate.promotion_score = score_candidate(&candidate, rem);
        candidate.score = candidate.promotion_score;
        candidate.gates = promotion_gates(&candidate, source_still_present);
        candidate.promote = candidate.gates.iter().all(|gate| gate.passed);
        if candidate.promote {
            candidate.decision_reason = "qualified for memory-index throughline".to_string();
            promoted.push(DreamPromotion {
                text: candidate.text.clone(),
                score: candidate.promotion_score,
                evidence: candidate.evidence.clone(),
                gates_passed: candidate
                    .gates
                    .iter()
                    .filter(|gate| gate.passed)
                    .map(|gate| gate.name.clone())
                    .collect(),
            });
        } else {
            let failed_gates = candidate
                .gates
                .iter()
                .filter(|gate| !gate.passed)
                .map(|gate| gate.name.clone())
                .collect::<Vec<_>>();
            let reason = candidate
                .gates
                .iter()
                .find(|gate| !gate.passed)
                .map(|gate| gate.reason.clone())
                .unwrap_or_else(|| "deferred for more evidence".to_string());
            candidate.decision_reason = reason.clone();
            rejected.push(DreamRejection {
                text: candidate.text.clone(),
                score: candidate.promotion_score,
                reason,
                failed_gates,
                evidence: candidate.evidence.clone(),
            });
        }
        evaluated.push(candidate);
    }

    Ok(DeepPhaseOutput {
        candidates: evaluated,
        promoted,
        rejected,
    })
}

fn phase_summaries(
    light: &LightPhaseOutput,
    rem: &RemPhaseOutput,
    deep: &DeepPhaseOutput,
    paths: &[String],
) -> Vec<DreamPhaseSummary> {
    vec![
        DreamPhaseSummary {
            phase: "light".to_string(),
            summary: format!(
                "reviewed {} sources, staged {} candidates, ignored {} duplicate signals; no durable memory was written",
                light.sources_reviewed, light.candidates_staged, light.duplicates_ignored
            ),
            candidate_count: light.candidates_staged,
            promoted_count: 0,
            rejected_count: 0,
            paths: paths
                .iter()
                .filter(|path| {
                    path.contains("candidates-")
                        || path.contains(&format!("{DREAM_DATA_DIR}/{DREAM_REPORTS_DIR}/light/"))
                })
                .cloned()
                .collect(),
        },
        DreamPhaseSummary {
            phase: "rem".to_string(),
            summary: format!(
                "noticed {} themes and {} reinforcement signals; no durable memory was written",
                rem.themes.len(),
                rem.reinforcement_signals.len()
            ),
            candidate_count: light.candidates_staged,
            promoted_count: 0,
            rejected_count: 0,
            paths: paths
                .iter()
                .filter(|path| {
                    path.contains("phase-signals-")
                        || path.contains(&format!("{DREAM_DATA_DIR}/{DREAM_REPORTS_DIR}/rem/"))
                })
                .cloned()
                .collect(),
        },
        DreamPhaseSummary {
            phase: "deep".to_string(),
            summary: format!(
                "indexed {} throughlines and deferred {} candidates after scoring gates",
                deep.promoted.len(),
                deep.rejected.len()
            ),
            candidate_count: deep.candidates.len(),
            promoted_count: deep.promoted.len(),
            rejected_count: deep.rejected.len(),
            paths: paths
                .iter()
                .filter(|path| {
                    path.contains("promotions-")
                        || path.ends_with("MEMORY.md")
                        || path.contains(&format!("{DREAM_DATA_DIR}/{DREAM_REPORTS_DIR}/deep/"))
                })
                .cloned()
                .collect(),
        },
    ]
}

fn score_candidate(candidate: &DreamCandidate, rem: &RemPhaseOutput) -> f32 {
    let evidence_score = (candidate.unique_source_count as f32 / 3.0).min(1.0);
    let recall_score = (candidate.recall_count as f32 / 4.0).min(1.0);
    let theme_score = if candidate.theme_hits.is_empty() {
        0.0
    } else {
        let reinforced = candidate
            .theme_hits
            .iter()
            .filter_map(|theme| rem.reinforcement_signals.get(theme))
            .copied()
            .sum::<usize>() as f32;
        ((candidate.theme_hits.len() as f32 * 0.20) + (reinforced * 0.10)).min(1.0)
    };
    round_score(
        candidate.durability_score * 0.30
            + candidate.specificity_score * 0.25
            + candidate.recency_score * 0.15
            + evidence_score * 0.10
            + recall_score * 0.10
            + theme_score * 0.10,
    )
}

fn promotion_gates(candidate: &DreamCandidate, source_still_present: bool) -> Vec<DreamGate> {
    let generated_source = is_generated_dreaming_path(&candidate.source);
    let too_short = candidate.text.len() < MIN_CANDIDATE_LEN;
    let heading = is_heading_line(&candidate.text);
    let transient = is_obviously_transient(&candidate.text);

    vec![
        gate(
            "minimum_score",
            candidate.promotion_score >= MIN_PROMOTION_SCORE,
            format!(
                "index score {:.2} is below {:.2}",
                candidate.promotion_score, MIN_PROMOTION_SCORE
            ),
        ),
        gate(
            "minimum_evidence",
            !candidate.evidence.is_empty() && candidate.unique_source_count >= 1,
            "candidate has no usable source evidence".to_string(),
        ),
        gate(
            "not_generated_from_dreaming_files",
            !generated_source,
            "generated dreaming artifacts are never index sources".to_string(),
        ),
        gate(
            "not_too_short",
            !too_short,
            format!("candidate is shorter than {MIN_CANDIDATE_LEN} characters"),
        ),
        gate(
            "not_heading",
            !heading,
            "headings are structure, not durable memory candidates".to_string(),
        ),
        gate(
            "not_obviously_transient",
            !transient,
            "candidate looks temporary or task-like".to_string(),
        ),
        gate(
            "source_still_present",
            source_still_present,
            "source snippet is stale, deleted, or changed".to_string(),
        ),
    ]
}

fn gate(name: &str, passed: bool, failure_reason: String) -> DreamGate {
    DreamGate {
        name: name.to_string(),
        passed,
        reason: if passed {
            "passed".to_string()
        } else {
            failure_reason
        },
    }
}

async fn source_still_contains(
    store: &MarkdownMemoryStore,
    candidate: &DreamCandidate,
) -> Result<bool, DreamingError> {
    if is_generated_dreaming_path(&candidate.source) || !is_candidate_source_path(&candidate.source)
    {
        return Ok(false);
    }
    let entry = match store.read(&candidate.source).await {
        Ok(entry) => entry,
        Err(MarkdownStoreError::NotFound(_)) => return Ok(false),
        Err(e) => return Err(DreamingError::Memory(e.to_string())),
    };
    let wanted = normalize_candidate_text(&candidate.text);
    if let Some(line) = candidate.line {
        if let Some(current) = entry.content.lines().nth(line.saturating_sub(1)) {
            if candidate_text_from_line(current)
                .map(|text| normalize_candidate_text(&text) == wanted)
                .unwrap_or(false)
            {
                return Ok(true);
            }
        }
    }
    Ok(entry
        .content
        .lines()
        .filter_map(candidate_text_from_line)
        .any(|text| normalize_candidate_text(&text) == wanted))
}

async fn write_data_json<T: Serialize>(
    character_data_dir: &Path,
    path: &Path,
    value: &T,
) -> Result<(), DreamingError> {
    let json = serde_json::to_string_pretty(value).map_err(|e| DreamingError::Io(e.to_string()))?;
    ensure_data_write_path(character_data_dir, path).await?;
    fs::write(path, json)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

async fn write_data_markdown(
    character_data_dir: &Path,
    path: &Path,
    content: &str,
) -> Result<(), DreamingError> {
    ensure_data_write_path(character_data_dir, path).await?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DreamingError::Io(e.to_string()))?;
    }
    fs::write(path, content)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

async fn append_dream_diary(
    data_dir: &Path,
    character: &str,
    ran_at: &str,
    light: &LightPhaseOutput,
    rem: &RemPhaseOutput,
    deep: &DeepPhaseOutput,
) -> Result<(), DreamingError> {
    let mut body = match crate::memory::dreams_log::read_dreams_log(data_dir, character).await {
        Ok(Some(content)) => normalize_dream_diary(content),
        Ok(None) => DREAM_DIARY_HEADER.to_string(),
        Err(e) => return Err(DreamingError::Io(e.to_string())),
    };

    body.push_str(&format!("## Dream Cycle - {ran_at}\n\n"));
    body.push_str("### Light Sleep - Staging\n\n");
    body.push_str(&format!("- Sources reviewed: {}\n", light.sources_reviewed));
    body.push_str(&format!(
        "- Candidates staged: {}\n",
        light.candidates_staged
    ));
    body.push_str(&format!(
        "- Duplicates ignored: {}\n",
        light.duplicates_ignored
    ));
    body.push_str("- No durable memory was written\n\n");

    if !light.candidates.is_empty() {
        body.push_str("Staged examples:\n\n");
        for candidate in light.candidates.iter().take(MAX_DIARY_ITEMS) {
            body.push_str(&format!(
                "- {}\n  - source: `{}`{}\n",
                diary_text(&candidate.text),
                candidate.source,
                candidate
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default()
            ));
        }
        body.push('\n');
    }

    body.push_str("### REM Sleep - Reflection\n\n");
    if rem.themes.is_empty() {
        body.push_str("- Themes noticed: none\n");
    } else {
        body.push_str("- Themes noticed:\n");
        for theme in rem.themes.iter().take(MAX_DIARY_ITEMS) {
            body.push_str(&format!("  - {} ({} hits)\n", theme.theme, theme.hits));
        }
    }
    if rem.reinforcement_signals.is_empty() {
        body.push_str("- Reinforcement signals: none\n");
    } else {
        body.push_str("- Reinforcement signals:\n");
        for (theme, hits) in rem.reinforcement_signals.iter().take(MAX_DIARY_ITEMS) {
            body.push_str(&format!("  - {theme}: {hits} supporting candidates\n"));
        }
    }
    body.push_str("- No durable memory was written\n\n");

    body.push_str("### Deep Sleep - Indexing\n\n");
    body.push_str("Indexed in `MEMORY.md`:\n\n");
    if deep.promoted.is_empty() {
        body.push_str("- None\n");
    } else {
        for promotion in deep.promoted.iter().take(MAX_DIARY_ITEMS) {
            body.push_str(&format!("- {}\n", diary_text(&promotion.text)));
            body.push_str(&format!("  - score: {:.2}\n", promotion.score));
            if let Some(evidence) = promotion.evidence.first() {
                body.push_str(&format!(
                    "  - evidence/source: `{}`{}\n",
                    evidence.source,
                    evidence
                        .line
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default()
                ));
            }
            body.push_str(&format!(
                "  - gates passed: {}\n",
                promotion.gates_passed.join(", ")
            ));
        }
    }
    body.push_str("\nRejected/deferred:\n\n");
    if deep.rejected.is_empty() {
        body.push_str("- None\n");
    } else {
        for rejection in deep.rejected.iter().take(MAX_DIARY_ITEMS) {
            body.push_str(&format!("- {}\n", diary_text(&rejection.text)));
            body.push_str(&format!("  - reason: {}\n", rejection.reason));
        }
    }
    body.push_str("\n### Notes for Review\n\n");
    body.push_str("- Safe to edit/delete for human review.\n");
    body.push_str("- Does not directly control memory notes or the prompt-visible index.\n\n");

    let path = crate::memory::dreams_log::dreams_log_path(data_dir, character);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DreamingError::Io(e.to_string()))?;
    }
    fs::write(&path, body)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

async fn write_phase_reports(
    character_data_dir: &Path,
    ran_at: &str,
    report_paths: PhaseReportPaths<'_>,
    light: &LightPhaseOutput,
    rem: &RemPhaseOutput,
    deep: &DeepPhaseOutput,
) -> Result<(), DreamingError> {
    let (light_report_rel, rem_report_rel, deep_report_rel) = report_paths;
    let mut light_report = format!("# Light Sleep - {ran_at}\n\n");
    light_report.push_str(&format!("- Sources reviewed: {}\n", light.sources_reviewed));
    light_report.push_str(&format!(
        "- Generated sources ignored: {}\n",
        light.generated_sources_ignored
    ));
    light_report.push_str(&format!(
        "- Candidates staged: {}\n",
        light.candidates_staged
    ));
    light_report.push_str(&format!(
        "- Duplicates ignored: {}\n",
        light.duplicates_ignored
    ));
    light_report.push_str("- No durable memory was written\n");

    let mut rem_report = format!("# REM Sleep - {ran_at}\n\n");
    rem_report.push_str("## Themes\n\n");
    for theme in &rem.themes {
        rem_report.push_str(&format!("- {}: {} hits\n", theme.theme, theme.hits));
    }
    if rem.themes.is_empty() {
        rem_report.push_str("- None\n");
    }
    rem_report.push_str("\nNo durable memory was written.\n");

    let mut deep_report = format!("# Deep Sleep - {ran_at}\n\n");
    deep_report.push_str("## Indexed Throughlines\n\n");
    if deep.promoted.is_empty() {
        deep_report.push_str("- None\n");
    } else {
        for promotion in &deep.promoted {
            deep_report.push_str(&format!("- {} ({:.2})\n", promotion.text, promotion.score));
        }
    }
    deep_report.push_str("\n## Rejected/deferred\n\n");
    if deep.rejected.is_empty() {
        deep_report.push_str("- None\n");
    } else {
        for rejection in &deep.rejected {
            deep_report.push_str(&format!("- {} - {}\n", rejection.text, rejection.reason));
        }
    }

    write_data_markdown(character_data_dir, light_report_rel, &light_report).await?;
    write_data_markdown(character_data_dir, rem_report_rel, &rem_report).await?;
    write_data_markdown(character_data_dir, deep_report_rel, &deep_report).await
}

async fn write_memory_index(
    store: &MarkdownMemoryStore,
    memory_index_path: &Path,
    character: &str,
    ran_at: &str,
    throughlines: &[DreamPromotion],
) -> Result<(), DreamingError> {
    let mut entries = store
        .list_all()
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?
        .into_iter()
        .filter(|entry| is_candidate_source_path(&entry.path))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut recent = entries.clone();
    recent.sort_by(|a, b| {
        modified_sort_key(b)
            .cmp(&modified_sort_key(a))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut body = String::new();
    body.push_str("# Memory Index\n\n");
    body.push_str(&format!("Character: {character}\n"));
    body.push_str(&format!("Last updated: {ran_at}\n\n"));
    body.push_str("This file is the prompt-visible memory index. It maps durable memory files in workspace/memory, recent updates, and still-relevant conversational throughlines.\n\n");
    body.push_str("It is not the character definition, user profile, standing behavior, tool guide, or heartbeat guide. Those roles stay in SOUL.md, USER.md, AGENTS.md, TOOLS.md, and HEARTBEAT.md.\n\n");

    body.push_str("## Memory Files\n\n");
    if entries.is_empty() {
        body.push_str("- No memory files yet.\n");
    } else {
        for entry in entries.iter().take(MAX_INDEX_FILES) {
            body.push_str(&format!(
                "- `{}` - {}\n",
                entry.path,
                memory_file_summary(entry)
            ));
        }
        if entries.len() > MAX_INDEX_FILES {
            body.push_str(&format!(
                "- {} additional memory files omitted from this index.\n",
                entries.len() - MAX_INDEX_FILES
            ));
        }
    }

    body.push_str("\n## Recently Updated\n\n");
    if recent.is_empty() {
        body.push_str("- No recent memory file updates.\n");
    } else {
        for entry in recent.iter().take(MAX_RECENT_INDEX_FILES) {
            body.push_str(&format!(
                "- `{}` - modified {}\n",
                entry.path,
                display_modified_at(&entry.modified_at)
            ));
        }
    }

    body.push_str("\n## Conversational Throughlines\n\n");
    if throughlines.is_empty() {
        body.push_str("- No high-confidence throughlines selected in the latest dream cycle.\n");
    } else {
        for item in throughlines.iter().take(MAX_INDEX_THROUGHLINES) {
            body.push_str(&format!("- {}\n", diary_text(&item.text)));
            if let Some(evidence) = item.evidence.first() {
                body.push_str(&format!(
                    "  - source: `{}`{}\n",
                    evidence.source,
                    evidence
                        .line
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default()
                ));
            }
        }
    }

    body.push_str("\n## Use\n\n");
    body.push_str("- Treat this as a map, not a full memory dump.\n");
    body.push_str(
        "- Read or search the referenced memory files for details before relying on them.\n",
    );

    if let Some(parent) = memory_index_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DreamingError::Io(e.to_string()))?;
    }
    fs::write(memory_index_path, body)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

fn memory_file_summary(entry: &MarkdownEntry) -> String {
    let title = entry
        .content
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix('#')
                .map(|rest| rest.trim_matches('#').trim())
                .filter(|rest| !rest.is_empty())
        })
        .unwrap_or("untitled");
    let detail = entry
        .content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || is_heading_line(trimmed) {
                return None;
            }
            Some(strip_list_marker(trimmed).trim())
        })
        .find(|line| !line.is_empty());

    match detail {
        Some(detail) => format!("{title}; {detail}"),
        None => title.to_string(),
    }
}

fn modified_sort_key(entry: &MarkdownEntry) -> i64 {
    DateTime::parse_from_rfc3339(&entry.modified_at)
        .map(|dt| dt.timestamp())
        .unwrap_or_default()
}

fn display_modified_at(modified_at: &str) -> &str {
    if modified_at.is_empty() {
        "unknown"
    } else {
        modified_at
    }
}

fn update_seen_state(state: &mut DreamState, candidates: &[DreamCandidate]) {
    for candidate in candidates {
        let key = normalize_candidate_text(&candidate.text);
        let mut sources = candidate
            .evidence
            .iter()
            .map(|evidence| evidence.source.clone())
            .collect::<BTreeSet<_>>();
        if let Some(existing) = state.seen_candidates.get(&key) {
            sources.extend(existing.sources.iter().cloned());
        }
        state.seen_candidates.insert(
            key,
            DreamSeenState {
                first_seen_at: candidate.first_seen_at.clone(),
                last_seen_at: candidate.last_seen_at.clone(),
                recall_count: candidate.recall_count,
                sources: sources.into_iter().collect(),
            },
        );
    }
}

pub fn is_generated_dreaming_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    lower == "memory.md"
        || lower == "dreams.md"
        || lower == "dreams"
        || lower == "dreams/"
        || lower.starts_with(".dreams/")
        || lower.starts_with("dreaming/")
}

fn is_candidate_source_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    !is_generated_dreaming_path(path) && lower.ends_with(".md")
}

fn candidate_text_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || is_heading_line(trimmed)
        || trimmed.starts_with("```")
        || trimmed == "---"
        || trimmed.starts_with('|')
    {
        return None;
    }
    let text = strip_list_marker(trimmed).trim();
    if text.len() < MIN_CANDIDATE_LEN || is_obviously_transient(text) {
        return None;
    }
    Some(text.to_string())
}

fn strip_list_marker(text: &str) -> &str {
    for prefix in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ ", "> "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    if let Some((left, right)) = text.split_once(". ") {
        if !left.is_empty() && left.len() <= 3 && left.chars().all(|c| c.is_ascii_digit()) {
            return right.trim();
        }
    }
    text
}

fn is_heading_line(text: &str) -> bool {
    text.trim_start().starts_with('#')
}

fn is_obviously_transient(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "maybe later",
        "temporary",
        "transient",
        "scratch",
        "draft",
        "wip",
        "todo",
        "tomorrow",
        "today i",
        "today we",
        "remind me",
        "meeting at",
        "next week",
        "for now",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn source_kind(path: &str) -> &'static str {
    let lower = path.replace('\\', "/").to_lowercase();
    if lower.starts_with("daily/") || lower.starts_with("journal/") || lower.contains("/daily/") {
        "daily"
    } else if lower.contains("compact") {
        "compacted_note"
    } else {
        "curated_markdown"
    }
}

fn detect_themes(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut themes = BTreeSet::new();
    if contains_any(
        &lower,
        &[
            "likes",
            "prefers",
            "favorite",
            "favourite",
            "dislikes",
            "hates",
            "enjoys",
            "wants",
        ],
    ) {
        themes.insert("preference".to_string());
    }
    if contains_any(
        &lower,
        &[
            "name is",
            "birthday",
            "born",
            "lives in",
            "pronouns",
            "calls themself",
        ],
    ) {
        themes.insert("identity".to_string());
    }
    if contains_any(
        &lower,
        &["project", "working on", "building", "repo", "branch"],
    ) {
        themes.insert("project".to_string());
    }
    if contains_any(
        &lower,
        &[
            "remember",
            "important",
            "promised",
            "agreed",
            "commitment",
            "must not forget",
        ],
    ) {
        themes.insert("commitment".to_string());
    }
    if contains_any(
        &lower,
        &["friend", "partner", "family", "works with", "relationship"],
    ) {
        themes.insert("relationship".to_string());
    }
    if contains_any(
        &lower,
        &["always", "usually", "never", "long-term", "durable"],
    ) {
        themes.insert("stable_context".to_string());
    }
    themes.into_iter().collect()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn recency_score(modified_at: &str) -> f32 {
    if let Ok(modified) = DateTime::parse_from_rfc3339(modified_at) {
        let age_days = Utc::now()
            .signed_duration_since(modified.with_timezone(&Utc))
            .num_days();
        return if age_days <= 7 {
            1.0
        } else if age_days <= 30 {
            0.8
        } else if age_days <= 180 {
            0.55
        } else {
            0.30
        };
    }
    0.50
}

fn durability_score(text: &str, themes: &[String]) -> f32 {
    let lower = text.to_lowercase();
    let mut score = 0.20 + (themes.len() as f32 * 0.16);
    if contains_any(
        &lower,
        &[
            "always",
            "usually",
            "prefers",
            "favorite",
            "important",
            "remember",
            "birthday",
            "name is",
            "project",
        ],
    ) {
        score += 0.25;
    }
    if text.len() >= 48 {
        score += 0.10;
    }
    round_score(score.min(1.0))
}

fn specificity_score(text: &str) -> f32 {
    let mut score: f32 = 0.15;
    let words = text.split_whitespace().count();
    if words >= 5 {
        score += 0.25;
    }
    if words >= 9 {
        score += 0.15;
    }
    if text.chars().any(|c| c.is_ascii_digit()) {
        score += 0.10;
    }
    if text.split_whitespace().any(|word| {
        word.chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            && word.len() > 2
    }) {
        score += 0.20;
    }
    if text.contains(':') || text.contains('/') || text.contains('@') {
        score += 0.05;
    }
    round_score(score.min(1.0))
}

fn normalize_candidate_text(text: &str) -> String {
    strip_list_marker(text.trim())
        .trim_matches(|c: char| c == '-' || c == '*' || c == ' ' || c == '\t')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '.' || c == ';' || c == ',')
        .to_lowercase()
}

fn unique_source_count(evidence: &[DreamEvidence]) -> usize {
    evidence
        .iter()
        .map(|item| item.source.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

fn candidate_id(normalized: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("dc-{hash:016x}")
}

fn round_score(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

fn normalize_dream_diary(existing: String) -> String {
    if existing.contains("human-readable Dream Diary") {
        let mut body = if existing.contains("prompt-visible memory index") {
            existing
        } else if let Some(cycle_start) = existing.find("## Dream Cycle") {
            format!("{DREAM_DIARY_HEADER}{}", &existing[cycle_start..])
        } else {
            DREAM_DIARY_HEADER.to_string()
        };
        if !body.ends_with("\n\n") {
            if !body.ends_with('\n') {
                body.push('\n');
            }
            body.push('\n');
        }
        return body;
    }
    let old = existing.trim();
    if old.is_empty() || old == "# Dreams" {
        DREAM_DIARY_HEADER.to_string()
    } else {
        format!("{DREAM_DIARY_HEADER}<!-- Previous review output preserved below. -->\n\n{old}\n\n")
    }
}

fn diary_text(text: &str) -> String {
    text.replace('\n', " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use shore_ledger::LedgerClient;
    use shore_llm::LlmClient;
    use shore_test_harness::{MockLlmServer, TestConfigBuilder};
    use tokio::fs;

    fn test_ledger(tmp: &tempfile::TempDir) -> LedgerClient {
        LedgerClient::new(LlmClient::new(), &tmp.path().join("ledger.db")).unwrap()
    }

    fn local_dt(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .unwrap()
    }

    fn librarian_config(
        tmp: &tempfile::TempDir,
        mock: &MockLlmServer,
        character: &str,
        max_tool_rounds: u32,
    ) -> LoadedConfig {
        let mut config = TestConfigBuilder::new()
            .character_name(character)
            .build(tmp.path(), &mock.base_url());
        config.app.memory.dreaming.enabled = true;
        config.app.memory.dreaming.max_tool_rounds = max_tool_rounds;
        config
    }

    #[tokio::test]
    async fn ai_librarian_sweep_uses_tools_and_updates_memory_and_audit() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockLlmServer::start().await;
        let config = librarian_config(&tmp, &mock, "alice", 10);
        let mem = character_memory_dir(&config.dirs.config, "alice");
        fs::create_dir_all(mem.join("daily")).await.unwrap();
        fs::write(
            mem.join("daily/2026-04.md"),
            "# Daily April Notes\n\n- Trevor wants Shore memory to use MEMORY.md as an index.\n- Trevor wants Shore memory to use MEMORY.md as an index.\n- Old recap block should be replaced.\n",
        )
        .await
        .unwrap();
        fs::write(
            mem.join("shore-notes.md"),
            "# Shore Notes\n\n- Older note.\n",
        )
        .await
        .unwrap();

        mock.enqueue_json_tool_use("t_list", "list_files", json!({"path": "memory"}))
            .await;
        mock.enqueue_json_tool_use("t_read", "read", json!({"path": "memory/daily/2026-04.md"}))
            .await;
        mock.enqueue_json_tool_use(
            "t_search",
            "search",
            json!({"path": "memory", "query": "MEMORY.md", "max_results": 10}),
        )
        .await;
        mock.enqueue_json_tool_use(
            "t_write_notes",
            "write",
            json!({
                "path": "memory/shore-notes.md",
                "content": "# Shore Notes\n\n- Shore memory uses `MEMORY.md` as a prompt-visible index rather than an old recap block.\n- Duplicate daily notes about the index direction were consolidated here.\n"
            }),
        )
        .await;
        mock.enqueue_json_tool_use(
            "t_write_memory",
            "write",
            json!({
                "path": "MEMORY.md",
                "content": "# Memory Index\n\nThis file is the character's map of long-term memory. It is not the full memory itself.\nUse it to decide which memory files to inspect before answering.\n\nCore user facts and standing behavior guidance are already loaded from USER.md and AGENTS.md; do not duplicate them here unless needed as pointers to memory files.\n\n## Memory areas\n\n- `shore-notes.md` - Durable Shore memory architecture notes.\n- `daily/2026-04.md` - Raw April notes; read only for source context.\n\n## Recently updated files\n\n- `shore-notes.md` - Consolidated duplicate daily notes about MEMORY.md replacing recap.\n\n## Current conversational throughlines\n\n- Shore memory should remain markdown-first, with dreaming acting as an AI librarian.\n\n## Needs review\n\n- None.\n"
            }),
        )
        .await;
        mock.enqueue_json_tool_use(
            "t_write_dreams",
            "write",
            json!({
                "path": "memory/DREAMS.md",
                "content": "# Dreams\n\n## 2026-04-27 - AI librarian dreaming pass\n\n- Files inspected: `daily/2026-04.md`, `shore-notes.md`.\n- Files changed: `shore-notes.md`, `MEMORY.md`, `DREAMS.md`.\n- Moved/deduped/superseded: duplicate daily notes about MEMORY.md were consolidated into `shore-notes.md`; old recap language was superseded.\n- Unresolved issues: none.\n- MEMORY.md updated: yes.\n"
            }),
        )
        .await;
        mock.enqueue_json_text("Librarian pass complete.").await;

        let result = run_librarian_sweep(
            &config,
            &config.dirs.data,
            &test_ledger(&tmp),
            "alice",
            None,
            false,
            true,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(result.mode, "ai_librarian");
        // The daemon now always writes the audit; model writes to DREAMS.md
        // are no longer wired (DREAMS lives in data_dir, outside the workspace).
        assert!(result.audit_appended);
        assert!(result.tools_used.contains(&"list_files".to_string()));
        assert!(result.tools_used.contains(&"read".to_string()));
        assert!(result.tools_used.contains(&"search".to_string()));
        assert!(result.tools_used.contains(&"write".to_string()));
        assert_eq!(result.tool_rounds, 6);

        let notes = fs::read_to_string(mem.join("shore-notes.md"))
            .await
            .unwrap();
        assert!(notes.contains("consolidated"));
        let workspace = character_workspace_dir(&config.dirs.config, "alice");
        let memory = fs::read_to_string(workspace.join("MEMORY.md"))
            .await
            .unwrap();
        assert!(memory.contains("# Memory Index"));
        assert!(memory.contains("shore-notes.md"));
        assert!(!memory
            .contains("Trevor wants Shore memory to use MEMORY.md as an index.\n- Trevor wants"));
        let dreams_path = crate::memory::dreams_log::dreams_log_path(&config.dirs.data, "alice");
        let dreams = fs::read_to_string(&dreams_path).await.unwrap();
        assert!(dreams.contains("AI librarian dreaming pass"));
        assert!(dreams.contains("MEMORY.md updated:"));
        assert!(
            crate::memory::deferred_edits::load_memory_index(
                &config.dirs.data.join("alice"),
                &config.dirs.config,
                "alice"
            )
            .is_none(),
            "new MEMORY.md content should stay out of the prompt snapshot until compaction"
        );
        let soul = fs::read_to_string(
            character_workspace_dir(&config.dirs.config, "alice").join(SOUL_FILE),
        )
        .await
        .unwrap();
        assert!(soul.contains("concise test assistant"));
        assert!(config.dirs.data.join("alice/dreams/state.json").exists());
        assert!(!mem.join(".dreams/state.json").exists());
    }

    #[tokio::test]
    async fn ai_librarian_sweep_appends_after_cached_request_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockLlmServer::start().await;
        let config = librarian_config(&tmp, &mock, "alice", 3);
        let resolved = config.models.first_chat_model().unwrap();
        let cached_request = LedgerClient::build_request(
            resolved,
            vec![
                json!({"role": "user", "content": "original user turn"}),
                json!({"role": "assistant", "content": "original assistant turn"}),
            ],
            Some(json!([{ "type": "text", "text": "cached system prefix" }])),
            Some(vec![json!({
                "name": "read",
                "description": "sentinel cached tool definition",
                "input_schema": { "type": "object", "properties": {} }
            })]),
            None,
        )
        .unwrap();

        mock.enqueue_json_text("Librarian pass complete.").await;

        run_librarian_sweep(
            &config,
            &config.dirs.data,
            &test_ledger(&tmp),
            "alice",
            Some(&cached_request),
            false,
            true,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        let requests = mock.received_requests().await;
        assert_eq!(requests.len(), 1);
        let body = &requests[0];
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[2]["role"], "user");
        assert!(messages[0].to_string().contains("original user turn"));
        assert!(messages[1].to_string().contains("original assistant turn"));
        assert!(messages[2].to_string().contains("memory librarian pass"));
        assert!(body["system"].to_string().contains("cached system prefix"));
        assert_eq!(body["tools"][0]["name"], "read");
        assert!(body["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("sentinel cached"));
    }

    #[tokio::test]
    async fn zero_max_tool_rounds_sends_no_librarian_request() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockLlmServer::start().await;
        let config = librarian_config(&tmp, &mock, "alice", 0);
        let mem = character_memory_dir(&config.dirs.config, "alice");
        let workspace = character_workspace_dir(&config.dirs.config, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(mem.join("notes.md"), "# Notes\n\n- Durable note.\n")
            .await
            .unwrap();

        let result = run_librarian_sweep(
            &config,
            &config.dirs.data,
            &test_ledger(&tmp),
            "alice",
            None,
            false,
            true,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(result.tool_rounds, 0);
        assert!(mock.received_requests().await.is_empty());
        assert!(workspace.join("MEMORY.md").exists());
        assert!(
            crate::memory::deferred_edits::load_memory_index(
                &config.dirs.data.join("alice"),
                &config.dirs.config,
                "alice"
            )
            .is_none(),
            "fallback MEMORY.md should not become prompt-active before compaction"
        );
    }

    #[tokio::test]
    async fn dry_run_librarian_sweep_blocks_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockLlmServer::start().await;
        let config = librarian_config(&tmp, &mock, "alice", 3);
        let mem = character_memory_dir(&config.dirs.config, "alice");
        let workspace = character_workspace_dir(&config.dirs.config, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(mem.join("notes.md"), "# Notes\n").await.unwrap();

        mock.enqueue_json_tool_use(
            "t_write",
            "write",
            json!({"path": "MEMORY.md", "content": "# Bad"}),
        )
        .await;
        mock.enqueue_json_text("Would update MEMORY.md.").await;

        let result = run_librarian_sweep(
            &config,
            &config.dirs.data,
            &test_ledger(&tmp),
            "alice",
            None,
            true,
            true,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(result.dry_run);
        assert!(result.paths_written.is_empty());
        assert!(result
            .would_write_paths
            .iter()
            .any(|path| path.ends_with("MEMORY.md")));
        assert!(!workspace.join("MEMORY.md").exists());
        assert!(!mem.join("DREAMS.md").exists());
        assert!(!config.dirs.data.join("alice/dreams/state.json").exists());
        assert!(!mem.join(".dreams/state.json").exists());
    }

    #[tokio::test]
    async fn librarian_sweep_fallback_creates_memory_index_and_audit() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockLlmServer::start().await;
        let config = librarian_config(&tmp, &mock, "alice", 3);
        let mem = character_memory_dir(&config.dirs.config, "alice");
        let workspace = character_workspace_dir(&config.dirs.config, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(mem.join("notes.md"), "# Notes\n\n- Durable note.\n")
            .await
            .unwrap();

        mock.enqueue_json_text("I inspected nothing and forgot to write files.")
            .await;

        let result = run_librarian_sweep(
            &config,
            &config.dirs.data,
            &test_ledger(&tmp),
            "alice",
            None,
            false,
            true,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(result.audit_appended);
        let memory = fs::read_to_string(workspace.join("MEMORY.md"))
            .await
            .unwrap();
        assert!(memory.contains("Fallback note"));
        assert!(memory.contains("notes.md"));
        let dreams_path = crate::memory::dreams_log::dreams_log_path(&config.dirs.data, "alice");
        let dreams = fs::read_to_string(&dreams_path).await.unwrap();
        assert!(dreams.contains("AI librarian dreaming pass"));
        let _ = mem;
    }

    #[tokio::test]
    async fn librarian_sweep_writes_protected_prompt_file_via_deferred_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockLlmServer::start().await;
        let config = librarian_config(&tmp, &mock, "alice", 3);
        let workspace = character_workspace_dir(&config.dirs.config, "alice");
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(workspace.join(SOUL_FILE), "original soul")
            .await
            .unwrap();
        let character_data_dir = config.dirs.data.join("alice");
        crate::memory::deferred_edits::ensure_active_prompt_snapshot(
            &character_data_dir,
            &config.dirs.config,
            "alice",
        )
        .unwrap();

        mock.enqueue_json_tool_use(
            "t_write_soul",
            "write",
            json!({"path": "SOUL.md", "content": "new soul"}),
        )
        .await;
        mock.enqueue_json_text("Updated soul.").await;

        let result = run_librarian_sweep(
            &config,
            &config.dirs.data,
            &test_ledger(&tmp),
            "alice",
            None,
            false,
            true,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(result.tools_used.contains(&"write".to_string()));
        assert_eq!(
            fs::read_to_string(workspace.join(SOUL_FILE)).await.unwrap(),
            "new soul"
        );
        let pending =
            crate::memory::deferred_edits::pending_deferred_edit_paths(&character_data_dir)
                .unwrap();
        assert!(
            pending.iter().any(|p| p == SOUL_FILE),
            "expected SOUL.md in deferred-edit queue, got {pending:?}"
        );
        let active_soul =
            crate::memory::deferred_edits::load_active_prompt_file(&character_data_dir, SOUL_FILE)
                .unwrap_or_default();
        assert_eq!(
            active_soul, "original soul",
            "active_prompt snapshot should not refresh until next reload boundary"
        );
    }

    #[tokio::test]
    async fn dry_run_does_not_write_files() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let workspace = character_workspace_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n",
        )
        .await
        .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_legacy_diagnostic_sweep(&data_dir, &cfg_dir, "alice", &cfg, true, true)
            .await
            .unwrap()
            .unwrap();
        assert!(result.dry_run);
        assert_eq!(result.paths_written.len(), 0);
        assert!(result
            .would_write_paths
            .iter()
            .any(|path| path.replace('\\', "/").contains("alice/dreams")));
        assert!(result
            .would_write_paths
            .iter()
            .any(|path| path.ends_with("MEMORY.md")));
        assert!(!data_dir.join("alice/dreams").exists());
        assert!(!mem.join(".dreams").exists());
        assert!(!mem.join("DREAMS.md").exists());
        assert!(!workspace.join("MEMORY.md").exists());
    }

    #[tokio::test]
    async fn sweep_writes_memory_index_even_without_throughlines() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let workspace = character_workspace_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(mem.join("notes.md"), "- maybe later\n")
            .await
            .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_legacy_diagnostic_sweep(&data_dir, &cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted_count, 0);
        assert!(workspace.join("MEMORY.md").exists());
        let memory = fs::read_to_string(workspace.join("MEMORY.md"))
            .await
            .unwrap();
        assert!(memory.contains("# Memory Index"));
        assert!(memory.contains("notes.md"));
        assert!(crate::memory::dreams_log::dreams_log_path(&data_dir, "alice").exists());
        assert!(data_dir.join("alice/dreams/state.json").exists());
        assert!(!mem.join(".dreams").join("state.json").exists());
    }

    #[tokio::test]
    async fn deep_indexes_only_qualified_throughlines() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let workspace = character_workspace_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n- maybe later\n",
        )
        .await
        .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_legacy_diagnostic_sweep(&data_dir, &cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted.len(), 1);
        assert!(result.promoted[0].contains("jasmine tea"));
        let memory = fs::read_to_string(workspace.join("MEMORY.md"))
            .await
            .unwrap();
        assert!(memory.contains("# Memory Index"));
        assert!(memory.contains("notes.md"));
        assert!(memory.contains("jasmine tea"));
        assert!(!memory.contains("maybe later"));
    }

    #[tokio::test]
    async fn generated_dreaming_outputs_are_not_reingested() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(mem.join(".dreams")).await.unwrap();
        fs::create_dir_all(mem.join("dreaming/light"))
            .await
            .unwrap();
        fs::write(
            mem.join("DREAMS.md"),
            "# Dreams\n\n- Alice prefers imported dream diary tea and remembers it.\n",
        )
        .await
        .unwrap();
        fs::write(
            mem.join("dreams.md"),
            "- Alice prefers lowercase dream diary tea and remembers it.\n",
        )
        .await
        .unwrap();
        fs::write(
            mem.join(".dreams/candidates.md"),
            "- Alice prefers staged dream tea and remembers it.\n",
        )
        .await
        .unwrap();
        fs::write(
            mem.join("dreaming/light/2026-04-26.md"),
            "- Alice prefers phase report tea and remembers it.\n",
        )
        .await
        .unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers real tea and remembers it.\n",
        )
        .await
        .unwrap();

        let cfg = DreamingConfig::default();
        let result = run_legacy_diagnostic_sweep(&data_dir, &cfg_dir, "alice", &cfg, true, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.candidate_count, 1);
        assert!(result.candidates[0].text.contains("real tea"));
    }

    #[tokio::test]
    async fn existing_memory_index_is_regenerated_not_reingested() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let workspace = character_workspace_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::create_dir_all(&workspace).await.unwrap();
        fs::write(
            workspace.join("MEMORY.md"),
            "# Memory Index\n\n- Alice prefers stale index tea and remembers it.\n",
        )
        .await
        .unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n",
        )
        .await
        .unwrap();

        let cfg = DreamingConfig::default();
        let result = run_legacy_diagnostic_sweep(&data_dir, &cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted_count, 1);
        let memory = fs::read_to_string(workspace.join("MEMORY.md"))
            .await
            .unwrap();
        assert!(memory.contains("# Memory Index"));
        assert!(memory.contains("jasmine tea"));
        assert!(!memory.contains("stale index tea"));
    }

    #[test]
    fn due_and_schedule_validation_still_work() {
        let cfg = DreamingConfig {
            frequency: "0 0 * * *".to_string(),
            ..DreamingConfig::default()
        };
        assert!(is_due(&cfg, None).is_ok());
        let invalid = DreamingConfig {
            frequency: "bad schedule".to_string(),
            ..DreamingConfig::default()
        };
        assert!(matches!(
            is_due(&invalid, None),
            Err(DreamingError::Schedule(_))
        ));
    }

    #[test]
    fn weekly_cron_due_checks_catch_up_once_per_occurrence() {
        let cfg = DreamingConfig {
            frequency: "0 6 * * 1".to_string(),
            ..DreamingConfig::default()
        };
        let before_monday = local_dt(2026, 5, 11, 5, 59, 0);
        let after_monday = local_dt(2026, 5, 11, 6, 1, 0);
        let tuesday = local_dt(2026, 5, 12, 9, 0, 0);
        let already_ran = local_dt(2026, 5, 11, 6, 2, 0).to_rfc3339();
        let stale_run = local_dt(2026, 5, 4, 6, 2, 0).to_rfc3339();

        assert!(!is_due_at(&cfg, None, before_monday).unwrap());
        assert!(is_due_at(&cfg, None, after_monday).unwrap());
        assert!(is_due_at(&cfg, None, tuesday).unwrap());
        assert!(!is_due_at(&cfg, Some(&already_ran), tuesday).unwrap());
        assert!(is_due_at(&cfg, Some(&stale_run), tuesday).unwrap());
    }

    #[tokio::test]
    async fn dream_status_reads_legacy_state_when_data_state_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(mem.join(".dreams")).await.unwrap();
        let legacy_state = DreamState {
            last_run_at: Some("2026-04-01T00:00:00+00:00".to_string()),
            runs: 7,
            ..DreamState::default()
        };
        let legacy_json = serde_json::to_string_pretty(&legacy_state).unwrap();
        fs::write(mem.join(".dreams/state.json"), legacy_json)
            .await
            .unwrap();

        let cfg = DreamingConfig::default();
        let status = dream_status(&data_dir, &cfg_dir, "alice", &cfg)
            .await
            .unwrap();

        assert_eq!(
            status.last_run_at.as_deref(),
            Some("2026-04-01T00:00:00+00:00")
        );
        assert!(status
            .state_path
            .replace('\\', "/")
            .ends_with("alice/dreams/state.json"));
        assert!(!data_dir.join("alice/dreams/state.json").exists());
    }

    // sweep_rejects_symlinked_dream_report_escape: removed.
    // The DREAMS.md log no longer lives in the workspace memory store; it is
    // written directly to `data_dir/{character}/DREAMS.md`, so the markdown-
    // store symlink-escape protection no longer applies to this path.

    #[cfg(unix)]
    #[tokio::test]
    async fn sweep_rejects_symlinked_dream_state_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let character_data_dir = data_dir.join("alice");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&mem).await.unwrap();
        fs::create_dir_all(&character_data_dir).await.unwrap();
        fs::create_dir_all(&outside).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n",
        )
        .await
        .unwrap();
        std::os::unix::fs::symlink(&outside, character_data_dir.join("dreams")).unwrap();

        let cfg = DreamingConfig::default();
        assert!(matches!(
            run_legacy_diagnostic_sweep(&data_dir, &cfg_dir, "alice", &cfg, false, true).await,
            Err(DreamingError::Io(_))
        ));
        assert!(!outside.join("state.json").exists());
    }
}
