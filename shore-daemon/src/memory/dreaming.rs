use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use shore_config::app::DreamingConfig;
use shore_config::character_memory_dir;

use crate::memory::markdown_store::{MarkdownEntry, MarkdownMemoryStore, MarkdownStoreError};

const MIN_PROMOTION_SCORE: f32 = 0.60;
const MIN_CANDIDATE_LEN: usize = 18;
const MAX_CANDIDATES: usize = 80;
const MAX_DIARY_ITEMS: usize = 12;
const MAX_INDEX_FILES: usize = 40;
const MAX_RECENT_INDEX_FILES: usize = 12;
const MAX_INDEX_THROUGHLINES: usize = 16;
const DREAM_DIARY_HEADER: &str = "# Dreams\n\nThis file is the human-readable Dream Diary for Shore's memory consolidation system.\n\nIt is not long-term memory.\nDurable notes live in ordinary markdown memory files.\n`MEMORY.md` is the prompt-visible memory index.\nMachine-facing dreaming state lives in `.dreams/`.\n\nEditing or deleting Dream Diary sections does not directly change memory notes or the prompt-visible index.\n\n";

#[derive(Debug, thiserror::Error)]
pub enum DreamingError {
    #[error("io: {0}")]
    Io(String),
    #[error("memory: {0}")]
    Memory(String),
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

type PhaseReportPaths<'a> = (&'a str, &'a str, &'a str);

pub async fn dream_status(
    config_dir: &Path,
    character: &str,
    cfg: &DreamingConfig,
) -> Result<DreamStatus, DreamingError> {
    let memory_dir = character_memory_dir(config_dir, character);
    let state = read_state(&memory_dir).await?;
    let due = cfg.enabled && is_due(cfg, state.last_run_at.as_deref())?;
    Ok(DreamStatus {
        character: character.to_string(),
        enabled: cfg.enabled,
        frequency: cfg.frequency.clone(),
        last_run_at: state.last_run_at,
        due,
        state_path: memory_dir.join(".dreams/state.json").display().to_string(),
        dreams_path: memory_dir.join("DREAMS.md").display().to_string(),
    })
}

pub async fn run_sweep(
    config_dir: &Path,
    character: &str,
    cfg: &DreamingConfig,
    dry_run: bool,
    force: bool,
) -> Result<Option<DreamSweepResult>, DreamingError> {
    let memory_dir = character_memory_dir(config_dir, character);
    let state = read_state(&memory_dir).await?;
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

    let candidates_rel = format!(".dreams/candidates-{stamp}.json");
    let signals_rel = format!(".dreams/phase-signals-{stamp}.json");
    let promotions_rel = format!(".dreams/promotions-{stamp}.json");
    let light_report_rel = format!("dreaming/light/{day}.md");
    let rem_report_rel = format!("dreaming/rem/{day}.md");
    let deep_report_rel = format!("dreaming/deep/{day}.md");

    let would_write_paths = vec![
        memory_dir.join(&candidates_rel).display().to_string(),
        memory_dir.join(&signals_rel).display().to_string(),
        memory_dir.join(&promotions_rel).display().to_string(),
        memory_dir.join(".dreams/state.json").display().to_string(),
        memory_dir.join("DREAMS.md").display().to_string(),
        memory_dir.join("MEMORY.md").display().to_string(),
        memory_dir.join(&light_report_rel).display().to_string(),
        memory_dir.join(&rem_report_rel).display().to_string(),
        memory_dir.join(&deep_report_rel).display().to_string(),
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
        }));
    }

    write_json(&store, &candidates_rel, &deep.candidates).await?;
    write_json(&store, &signals_rel, &rem).await?;
    write_json(&store, &promotions_rel, &deep).await?;
    append_dream_diary(&store, &ran_at, &light, &rem, &deep).await?;
    write_phase_reports(
        &store,
        &ran_at,
        (&light_report_rel, &rem_report_rel, &deep_report_rel),
        &light,
        &rem,
        &deep,
    )
    .await?;
    write_memory_index(&store, character, &ran_at, &deep.promoted).await?;

    let mut next_state = state;
    next_state.last_run_at = Some(ran_at.clone());
    next_state.runs += 1;
    next_state.last_candidates_path = Some(candidates_rel.clone());
    next_state.last_signals_path = Some(signals_rel.clone());
    next_state.last_promotions_path = Some(promotions_rel.clone());
    update_seen_state(&mut next_state, &deep.candidates);
    write_state(&memory_dir, &next_state).await?;

    let paths_written = would_write_paths;

    Ok(Some(DreamSweepResult {
        character: character.to_string(),
        dry_run,
        ran_at,
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
        staged_path: Some(memory_dir.join(&candidates_rel).display().to_string()),
        dreams_path: Some(memory_dir.join("DREAMS.md").display().to_string()),
        memory_path: Some(memory_dir.join("MEMORY.md").display().to_string()),
    }))
}

pub fn is_due(cfg: &DreamingConfig, last_run_at: Option<&str>) -> Result<bool, DreamingError> {
    let (minute, hour) = parse_daily_cron(&cfg.frequency)?;
    let now = Local::now();
    let today_due = now
        .with_hour(hour)
        .and_then(|dt| dt.with_minute(minute))
        .and_then(|dt| dt.with_second(0))
        .and_then(|dt| dt.with_nanosecond(0))
        .ok_or_else(|| DreamingError::Schedule(cfg.frequency.clone()))?;
    if now < today_due {
        return Ok(false);
    }
    let Some(last) = last_run_at else {
        return Ok(true);
    };
    let last = DateTime::parse_from_rfc3339(last)
        .map_err(|e| DreamingError::Schedule(e.to_string()))?
        .with_timezone(&Local);
    Ok(last.year() != now.year() || last.ordinal() != now.ordinal())
}

fn parse_daily_cron(expr: &str) -> Result<(u32, u32), DreamingError> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(DreamingError::Schedule(expr.to_string()));
    }
    let minute = parts[0]
        .parse::<u32>()
        .map_err(|_| DreamingError::Schedule(expr.to_string()))?;
    let hour = parts[1]
        .parse::<u32>()
        .map_err(|_| DreamingError::Schedule(expr.to_string()))?;
    if minute > 59 || hour > 23 || parts[2] != "*" || parts[3] != "*" || parts[4] != "*" {
        return Err(DreamingError::Schedule(expr.to_string()));
    }
    Ok((minute, hour))
}

async fn read_state(memory_dir: &Path) -> Result<DreamState, DreamingError> {
    let store = MarkdownMemoryStore::open(memory_dir)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    match store.read(".dreams/state.json").await {
        Ok(entry) => {
            serde_json::from_str(&entry.content).map_err(|e| DreamingError::Io(e.to_string()))
        }
        Err(MarkdownStoreError::NotFound(_)) => Ok(DreamState::default()),
        Err(e) => Err(DreamingError::Memory(e.to_string())),
    }
}

async fn write_state(memory_dir: &Path, state: &DreamState) -> Result<(), DreamingError> {
    let json = serde_json::to_string_pretty(state).map_err(|e| DreamingError::Io(e.to_string()))?;
    let store = MarkdownMemoryStore::open(memory_dir)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    store
        .write(".dreams/state.json", &json)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))
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
                .filter(|path| path.contains("candidates-") || path.contains("dreaming/light/"))
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
                .filter(|path| path.contains("phase-signals-") || path.contains("dreaming/rem/"))
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
                        || path.contains("dreaming/deep/")
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

async fn write_json<T: Serialize>(
    store: &MarkdownMemoryStore,
    rel_path: &str,
    value: &T,
) -> Result<(), DreamingError> {
    let json = serde_json::to_string_pretty(value).map_err(|e| DreamingError::Io(e.to_string()))?;
    store
        .write(rel_path, &json)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))
}

async fn append_dream_diary(
    store: &MarkdownMemoryStore,
    ran_at: &str,
    light: &LightPhaseOutput,
    rem: &RemPhaseOutput,
    deep: &DeepPhaseOutput,
) -> Result<(), DreamingError> {
    let mut body = match store.read("DREAMS.md").await {
        Ok(entry) => normalize_dream_diary(entry.content),
        Err(MarkdownStoreError::NotFound(_)) => DREAM_DIARY_HEADER.to_string(),
        Err(e) => return Err(DreamingError::Memory(e.to_string())),
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

    store
        .write("DREAMS.md", &body)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))
}

async fn write_phase_reports(
    store: &MarkdownMemoryStore,
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

    store
        .write(light_report_rel, &light_report)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    store
        .write(rem_report_rel, &rem_report)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))?;
    store
        .write(deep_report_rel, &deep_report)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))
}

async fn write_memory_index(
    store: &MarkdownMemoryStore,
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
    body.push_str("This file is the prompt-visible index for workspace/memory. It maps durable memory files, recent updates, and still-relevant conversational throughlines.\n\n");
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

    store
        .write("MEMORY.md", &body)
        .await
        .map_err(|e| DreamingError::Memory(e.to_string()))
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
    lower == "dreams.md"
        || lower == "dreams"
        || lower == "dreams/"
        || lower.starts_with(".dreams/")
        || lower.starts_with("dreaming/")
}

fn is_candidate_source_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    !is_generated_dreaming_path(path) && lower != "memory.md" && lower.ends_with(".md")
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
    use tokio::fs;

    #[tokio::test]
    async fn dry_run_does_not_write_files() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n",
        )
        .await
        .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_sweep(&cfg_dir, "alice", &cfg, true, true)
            .await
            .unwrap()
            .unwrap();
        assert!(result.dry_run);
        assert_eq!(result.paths_written.len(), 0);
        assert!(result
            .would_write_paths
            .iter()
            .any(|path| path.contains(".dreams")));
        assert!(result
            .would_write_paths
            .iter()
            .any(|path| path.ends_with("MEMORY.md")));
        assert!(!mem.join(".dreams").exists());
        assert!(!mem.join("DREAMS.md").exists());
        assert!(!mem.join("MEMORY.md").exists());
    }

    #[tokio::test]
    async fn sweep_writes_memory_index_even_without_throughlines() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(mem.join("notes.md"), "- maybe later\n")
            .await
            .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_sweep(&cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted_count, 0);
        assert!(mem.join("MEMORY.md").exists());
        let memory = fs::read_to_string(mem.join("MEMORY.md")).await.unwrap();
        assert!(memory.contains("# Memory Index"));
        assert!(memory.contains("notes.md"));
        assert!(mem.join("DREAMS.md").exists());
        assert!(mem.join(".dreams").join("state.json").exists());
    }

    #[tokio::test]
    async fn deep_indexes_only_qualified_throughlines() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n- maybe later\n",
        )
        .await
        .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_sweep(&cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted.len(), 1);
        assert!(result.promoted[0].contains("jasmine tea"));
        let memory = fs::read_to_string(mem.join("MEMORY.md")).await.unwrap();
        assert!(memory.contains("# Memory Index"));
        assert!(memory.contains("notes.md"));
        assert!(memory.contains("jasmine tea"));
        assert!(!memory.contains("maybe later"));
    }

    #[tokio::test]
    async fn generated_dreaming_outputs_are_not_reingested() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
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
        let result = run_sweep(&cfg_dir, "alice", &cfg, true, true)
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
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(
            mem.join("MEMORY.md"),
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
        let result = run_sweep(&cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted_count, 1);
        let memory = fs::read_to_string(mem.join("MEMORY.md")).await.unwrap();
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

    #[cfg(unix)]
    #[tokio::test]
    async fn sweep_rejects_symlinked_dream_report_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&mem).await.unwrap();
        fs::create_dir_all(&outside).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n",
        )
        .await
        .unwrap();
        fs::write(outside.join("DREAMS.md"), "outside")
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.join("DREAMS.md"), mem.join("DREAMS.md")).unwrap();

        let cfg = DreamingConfig::default();
        assert!(matches!(
            run_sweep(&cfg_dir, "alice", &cfg, false, true).await,
            Err(DreamingError::Memory(_))
        ));
        assert_eq!(
            fs::read_to_string(outside.join("DREAMS.md")).await.unwrap(),
            "outside"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sweep_rejects_symlinked_dream_state_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&mem).await.unwrap();
        fs::create_dir_all(&outside).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice prefers jasmine tea and remembers the blue cup.\n",
        )
        .await
        .unwrap();
        std::os::unix::fs::symlink(&outside, mem.join(".dreams")).unwrap();

        let cfg = DreamingConfig::default();
        assert!(matches!(
            run_sweep(&cfg_dir, "alice", &cfg, false, true).await,
            Err(DreamingError::Memory(_))
        ));
        assert!(!outside.join("state.json").exists());
    }
}
