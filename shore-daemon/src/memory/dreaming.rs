use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};
use shore_config::app::DreamingConfig;
use shore_config::character_memory_dir;
use tokio::fs;

use crate::memory::markdown_store::{MarkdownEntry, MarkdownMemoryStore};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamCandidate {
    pub source: String,
    pub text: String,
    pub score: u8,
    pub promote: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamSweepResult {
    pub character: String,
    pub dry_run: bool,
    pub ran_at: String,
    pub candidates: Vec<DreamCandidate>,
    pub promoted: Vec<String>,
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
        state_path: state_path(&memory_dir).display().to_string(),
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
    let candidates = collect_candidates(entries);
    let promoted = candidates
        .iter()
        .filter(|c| c.promote)
        .map(|c| c.text.clone())
        .collect::<Vec<_>>();
    let ran_at = Local::now().to_rfc3339();

    if dry_run {
        return Ok(Some(DreamSweepResult {
            character: character.to_string(),
            dry_run,
            ran_at,
            candidates,
            promoted,
            staged_path: None,
            dreams_path: None,
            memory_path: None,
        }));
    }

    fs::create_dir_all(memory_dir.join(".dreams"))
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))?;

    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let staged_rel = format!(".dreams/candidates-{stamp}.json");
    let staged_path = memory_dir.join(&staged_rel);
    let staged_json =
        serde_json::to_string_pretty(&candidates).map_err(|e| DreamingError::Io(e.to_string()))?;
    fs::write(&staged_path, staged_json)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))?;

    append_dream_report(&memory_dir, &ran_at, &candidates, &promoted).await?;
    append_memory_promotions(&memory_dir, &ran_at, &promoted).await?;

    let mut state = state;
    state.last_run_at = Some(ran_at.clone());
    state.runs += 1;
    write_state(&memory_dir, &state).await?;

    Ok(Some(DreamSweepResult {
        character: character.to_string(),
        dry_run,
        ran_at,
        candidates,
        promoted,
        staged_path: Some(staged_path.display().to_string()),
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
    match fs::read_to_string(state_path(memory_dir)).await {
        Ok(content) => serde_json::from_str(&content).map_err(|e| DreamingError::Io(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DreamState::default()),
        Err(e) => Err(DreamingError::Io(e.to_string())),
    }
}

async fn write_state(memory_dir: &Path, state: &DreamState) -> Result<(), DreamingError> {
    let path = state_path(memory_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| DreamingError::Io(e.to_string()))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| DreamingError::Io(e.to_string()))?;
    fs::write(path, json)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

fn state_path(memory_dir: &Path) -> PathBuf {
    memory_dir.join(".dreams").join("state.json")
}

fn collect_candidates(entries: Vec<MarkdownEntry>) -> Vec<DreamCandidate> {
    let mut candidates = Vec::new();
    for entry in entries.into_iter().take(40) {
        for line in entry.content.lines() {
            let text = line.trim().trim_start_matches("- ").trim();
            if text.len() < 12 || text.starts_with('#') {
                continue;
            }
            let lower = text.to_lowercase();
            let mut score = 0;
            for needle in [
                "likes",
                "prefers",
                "remember",
                "important",
                "promised",
                "project",
                "birthday",
                "name is",
            ] {
                if lower.contains(needle) {
                    score += 1;
                }
            }
            if text.starts_with("- ") || line.trim_start().starts_with("- ") {
                score += 1;
            }
            if score > 0 {
                candidates.push(DreamCandidate {
                    source: entry.path.clone(),
                    text: text.to_string(),
                    score,
                    promote: score >= 2,
                });
            }
        }
    }
    candidates.truncate(50);
    candidates
}

async fn append_dream_report(
    memory_dir: &Path,
    ran_at: &str,
    candidates: &[DreamCandidate],
    promoted: &[String],
) -> Result<(), DreamingError> {
    let path = memory_dir.join("DREAMS.md");
    let mut body = fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "# Dreams\n\n".to_string());
    body.push_str(&format!("## {ran_at} - dream sweep\n\n"));
    body.push_str("### Light\n\n");
    if candidates.is_empty() {
        body.push_str("- No candidate memory signals found.\n");
    } else {
        for candidate in candidates.iter().take(12) {
            body.push_str(&format!(
                "- {} (score {}, source `{}`)\n",
                candidate.text, candidate.score, candidate.source
            ));
        }
    }
    body.push_str("\n### REM\n\n");
    body.push_str("- Reviewed recent markdown memory for durable signals and contradictions.\n");
    body.push_str("\n### Deep\n\n");
    if promoted.is_empty() {
        body.push_str("- Nothing met the promotion threshold.\n\n");
    } else {
        for item in promoted {
            body.push_str(&format!("- Promoted: {item}\n"));
        }
        body.push('\n');
    }
    fs::write(path, body)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

async fn append_memory_promotions(
    memory_dir: &Path,
    ran_at: &str,
    promoted: &[String],
) -> Result<(), DreamingError> {
    if promoted.is_empty() {
        return Ok(());
    }
    let path = memory_dir.join("MEMORY.md");
    let mut body = fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "# Memory\n\n".to_string());
    body.push_str(&format!("## Dream consolidation - {ran_at}\n\n"));
    for item in promoted {
        body.push_str(&format!("- {item}\n"));
    }
    body.push('\n');
    fs::write(path, body)
        .await
        .map_err(|e| DreamingError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dry_run_does_not_write_files() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(mem.join("notes.md"), "- Alice likes tea.\n")
            .await
            .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_sweep(&cfg_dir, "alice", &cfg, true, true)
            .await
            .unwrap()
            .unwrap();
        assert!(result.dry_run);
        assert!(!mem.join(".dreams").exists());
        assert!(!mem.join("DREAMS.md").exists());
        assert!(!mem.join("MEMORY.md").exists());
    }

    #[tokio::test]
    async fn deep_promotes_only_threshold_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config");
        let mem = character_memory_dir(&cfg_dir, "alice");
        fs::create_dir_all(&mem).await.unwrap();
        fs::write(
            mem.join("notes.md"),
            "- Alice likes tea and remembers the blue cup.\n- maybe later\n",
        )
        .await
        .unwrap();
        let cfg = DreamingConfig::default();
        let result = run_sweep(&cfg_dir, "alice", &cfg, false, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.promoted.len(), 1);
        assert!(mem.join(".dreams").join("state.json").exists());
        assert!(mem.join("DREAMS.md").exists());
        assert!(mem.join("MEMORY.md").exists());
    }
}
