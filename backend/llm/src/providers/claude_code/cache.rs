//! Long-lived `claude` subprocess cache for the hot path.
//!
//! The CLI reads MCP configuration only at startup, so cache hits are
//! valid only while the recipe fingerprint is unchanged. The daemon
//! keeps the MCP URL stable per `subprocess_key`; if any recipe input
//! drifts, this module evicts and respawns.
//!
//! Turns for the same `subprocess_key` are serialized by the cached
//! process mutex. That preserves the CLI's conversation-local context
//! and avoids interleaving stdin frames; callers that want true
//! parallelism should choose distinct subprocess keys.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tempfile::NamedTempFile;
use tokio::io::{AsyncBufReadExt, AsyncWrite, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::providers::claude_code::driver::{
    self, classify_output_error, read_stream_json_lines, read_stream_json_lines_forwarding,
    recipe_for_request, render_static_system_prompt_text, render_system_prompt_text,
    render_user_frame, write_system_prompt_file, write_user_frame_to, DriverOutput, ProviderConfig,
};
use crate::providers::claude_code::session;
use crate::types::LlmRequest;
use crate::LlmError;

const IDLE_EVICT_AFTER: Duration = Duration::from_secs(60 * 60);
const EVICT_EVERY: Duration = Duration::from_secs(60);

static CACHE: OnceLock<SubprocessCache> = OnceLock::new();
static EVICTOR_STARTED: OnceLock<()> = OnceLock::new();

pub(super) async fn run_long_lived(request: &LlmRequest) -> Result<DriverOutput, LlmError> {
    let cfg = ProviderConfig::from_request(request)?;
    let Some(key) = cfg.subprocess_key.clone() else {
        return driver::run_fresh_spawn(request).await;
    };
    start_evictor();
    let key_lock = global_cache()
        .key_locks
        .entry(key.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .value()
        .clone();
    let _key_guard = key_lock.lock_owned().await;

    let fingerprint = RecipeFingerprint::from_request(request, &cfg);
    let user_frame = render_user_frame(request);

    if let Some(entry) = global_cache().entries.get(&key).map(|r| r.value().clone()) {
        let mut proc = entry.lock().await;
        if proc.fingerprint == fingerprint {
            match run_cached_turn(&mut proc, user_frame.clone()).await {
                Ok(output) => return Ok(output),
                Err(CachedTurnError::Dead(reason)) => {
                    warn!(subprocess_key = %key, reason = %reason, "claude_code cached subprocess died; respawning");
                }
                Err(CachedTurnError::Llm(err)) => {
                    if is_quota_error(&err) {
                        global_cache().entries.remove(&key);
                    }
                    return Err(err);
                }
            }
        } else {
            debug!(subprocess_key = %key, "claude_code recipe changed; respawning cached subprocess");
        }
    }

    global_cache().entries.remove(&key);
    let native_session = session::prepare_native_session(request, &cfg)?;
    let prompt_text = if native_session.is_some() {
        render_static_system_prompt_text(request)
    } else {
        render_system_prompt_text(request)
    };
    let proc = spawn_cached_process(
        request,
        &cfg,
        fingerprint,
        prompt_text,
        native_session.is_some(),
    )
    .await?;
    let entry = Arc::new(Mutex::new(proc));
    global_cache().entries.insert(key.clone(), entry.clone());

    let mut proc = entry.lock().await;
    match run_cached_turn(&mut proc, user_frame).await {
        Ok(output) => Ok(output),
        Err(CachedTurnError::Dead(reason)) => {
            global_cache().entries.remove(&key);
            Err(LlmError::Provider {
                message: format!("claude cached subprocess died before producing a turn: {reason}"),
            })
        }
        Err(CachedTurnError::Llm(err)) => {
            if is_quota_error(&err) {
                global_cache().entries.remove(&key);
            }
            Err(err)
        }
    }
}

pub(super) async fn run_long_lived_streaming<W>(
    request: &LlmRequest,
    writer: &mut W,
) -> Result<DriverOutput, LlmError>
where
    W: AsyncWrite + Unpin,
{
    let cfg = ProviderConfig::from_request(request)?;
    let Some(key) = cfg.subprocess_key.clone() else {
        return driver::run_fresh_spawn_streaming(request, writer).await;
    };
    start_evictor();
    let key_lock = global_cache()
        .key_locks
        .entry(key.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .value()
        .clone();
    let _key_guard = key_lock.lock_owned().await;

    let fingerprint = RecipeFingerprint::from_request(request, &cfg);
    let user_frame = render_user_frame(request);

    if let Some(entry) = global_cache().entries.get(&key).map(|r| r.value().clone()) {
        let mut proc = entry.lock().await;
        if proc.fingerprint == fingerprint {
            match run_cached_turn_streaming(&mut proc, user_frame.clone(), writer).await {
                Ok(output) => return Ok(output),
                Err(CachedTurnError::Dead(reason)) => {
                    warn!(subprocess_key = %key, reason = %reason, "claude_code cached subprocess died; respawning");
                }
                Err(CachedTurnError::Llm(err)) => {
                    if is_quota_error(&err) {
                        global_cache().entries.remove(&key);
                    }
                    return Err(err);
                }
            }
        } else {
            debug!(subprocess_key = %key, "claude_code recipe changed; respawning cached subprocess");
        }
    }

    global_cache().entries.remove(&key);
    let native_session = session::prepare_native_session(request, &cfg)?;
    let prompt_text = if native_session.is_some() {
        render_static_system_prompt_text(request)
    } else {
        render_system_prompt_text(request)
    };
    let proc = spawn_cached_process(
        request,
        &cfg,
        fingerprint,
        prompt_text,
        native_session.is_some(),
    )
    .await?;
    let entry = Arc::new(Mutex::new(proc));
    global_cache().entries.insert(key.clone(), entry.clone());

    let mut proc = entry.lock().await;
    match run_cached_turn_streaming(&mut proc, user_frame, writer).await {
        Ok(output) => Ok(output),
        Err(CachedTurnError::Dead(reason)) => {
            global_cache().entries.remove(&key);
            Err(LlmError::Provider {
                message: format!("claude cached subprocess died before producing a turn: {reason}"),
            })
        }
        Err(CachedTurnError::Llm(err)) => {
            if is_quota_error(&err) {
                global_cache().entries.remove(&key);
            }
            Err(err)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecipeFingerprint {
    model: String,
    mcp_endpoint: String,
    allowed_tools: Vec<String>,
    effort: Option<String>,
    include_partial_messages: bool,
    native_session_replay: bool,
    static_system_prompt_text: String,
}

impl RecipeFingerprint {
    fn from_request(request: &LlmRequest, cfg: &ProviderConfig) -> Self {
        Self {
            model: request.model.clone(),
            mcp_endpoint: cfg.mcp_endpoint.clone(),
            allowed_tools: cfg.allowed_tools.clone(),
            effort: cfg.effort.clone(),
            include_partial_messages: cfg.include_partial_messages,
            native_session_replay: cfg.native_session_replay,
            static_system_prompt_text: render_static_system_prompt_text(request),
        }
    }
}

struct CachedSubprocess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    _stderr_task: JoinHandle<()>,
    _prompt_file: NamedTempFile,
    fingerprint: RecipeFingerprint,
    last_access: Instant,
    turns: u64,
}

struct SubprocessCache {
    entries: DashMap<String, Arc<Mutex<CachedSubprocess>>>,
    key_locks: DashMap<String, Arc<Mutex<()>>>,
}

fn global_cache() -> &'static SubprocessCache {
    CACHE.get_or_init(|| SubprocessCache {
        entries: DashMap::new(),
        key_locks: DashMap::new(),
    })
}

fn start_evictor() {
    EVICTOR_STARTED.get_or_init(|| {
        tokio::spawn(async {
            let mut interval = tokio::time::interval(EVICT_EVERY);
            loop {
                interval.tick().await;
                evict_idle().await;
            }
        });
    });
}

async fn evict_idle() {
    let now = Instant::now();
    let entries: Vec<(String, Arc<Mutex<CachedSubprocess>>)> = global_cache()
        .entries
        .iter()
        .map(|r| (r.key().clone(), r.value().clone()))
        .collect();
    for (key, entry) in entries {
        let Ok(proc) = entry.try_lock() else {
            continue;
        };
        if now.duration_since(proc.last_access) >= IDLE_EVICT_AFTER {
            let removed = global_cache().entries.remove_if(&key, |_, current| {
                Arc::ptr_eq(current, &entry) && Arc::strong_count(current) == 2
            });
            if removed.is_some() {
                debug!(subprocess_key = %key, "evicted idle claude_code subprocess");
            } else {
                debug!(
                    subprocess_key = %key,
                    "skipped idle claude_code eviction because the entry became active"
                );
            }
        }
    }
}

async fn spawn_cached_process(
    request: &LlmRequest,
    cfg: &ProviderConfig,
    fingerprint: RecipeFingerprint,
    prompt_text: String,
    resume_session: bool,
) -> Result<CachedSubprocess, LlmError> {
    let prompt_file = write_system_prompt_file(&prompt_text)?;
    let recipe = recipe_for_request(
        request,
        cfg,
        prompt_file.path().to_path_buf(),
        resume_session,
    );
    let mut child = recipe
        .into_command()
        .spawn()
        .map_err(|e| LlmError::Provider {
            message: format!("failed to spawn claude CLI: {e}"),
        })?;
    let stdin = child.stdin.take().ok_or_else(|| LlmError::Provider {
        message: "failed to take child stdin".into(),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| LlmError::Provider {
        message: "failed to take child stdout".into(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| LlmError::Provider {
        message: "failed to take child stderr".into(),
    })?;
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_task = collect_stderr(stderr, stderr_buf.clone());

    Ok(CachedSubprocess {
        child,
        stdin,
        stdout: BufReader::new(stdout).lines(),
        stderr: stderr_buf,
        _stderr_task: stderr_task,
        _prompt_file: prompt_file,
        fingerprint,
        last_access: Instant::now(),
        turns: 0,
    })
}

async fn run_cached_turn(
    proc: &mut CachedSubprocess,
    user_frame: String,
) -> Result<DriverOutput, CachedTurnError> {
    if let Some(status) = proc.child.try_wait().map_err(|e| {
        CachedTurnError::Llm(LlmError::Provider {
            message: format!("failed to poll cached claude subprocess: {e}"),
        })
    })? {
        return Err(CachedTurnError::Dead(format!(
            "already exited with {status}"
        )));
    }

    write_user_frame_to(&mut proc.stdin, user_frame)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                CachedTurnError::Dead(e.to_string())
            } else {
                CachedTurnError::Llm(LlmError::Provider {
                    message: format!("write to cached claude stdin: {e}"),
                })
            }
        })?;

    let mut output = match read_stream_json_lines(&mut proc.stdout).await {
        Ok(output) => output,
        Err(LlmError::IncompleteStream) => {
            return Err(CachedTurnError::Dead("stdout closed before result".into()));
        }
        Err(err) => return Err(CachedTurnError::Llm(err)),
    };
    output.stderr = proc.stderr.lock().await.clone();
    if let Some(err) = classify_output_error(&output) {
        return Err(CachedTurnError::Llm(err));
    }
    proc.turns = proc.turns.saturating_add(1);
    proc.last_access = Instant::now();

    if let Ok(Some(status)) = proc.child.try_wait() {
        warn!(turns = proc.turns, status = %status, "cached claude subprocess exited after a completed turn");
    }

    Ok(output)
}

async fn run_cached_turn_streaming<W>(
    proc: &mut CachedSubprocess,
    user_frame: String,
    writer: &mut W,
) -> Result<DriverOutput, CachedTurnError>
where
    W: AsyncWrite + Unpin,
{
    if let Some(status) = proc.child.try_wait().map_err(|e| {
        CachedTurnError::Llm(LlmError::Provider {
            message: format!("failed to poll cached claude subprocess: {e}"),
        })
    })? {
        return Err(CachedTurnError::Dead(format!(
            "already exited with {status}"
        )));
    }

    write_user_frame_to(&mut proc.stdin, user_frame)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                CachedTurnError::Dead(e.to_string())
            } else {
                CachedTurnError::Llm(LlmError::Provider {
                    message: format!("write to cached claude stdin: {e}"),
                })
            }
        })?;

    let mut output = match read_stream_json_lines_forwarding(&mut proc.stdout, writer).await {
        Ok(output) => output,
        Err(LlmError::IncompleteStream) => {
            return Err(CachedTurnError::Dead("stdout closed before result".into()));
        }
        Err(err) => return Err(CachedTurnError::Llm(err)),
    };
    output.stderr = proc.stderr.lock().await.clone();
    if let Some(err) = classify_output_error(&output) {
        return Err(CachedTurnError::Llm(err));
    }
    proc.turns = proc.turns.saturating_add(1);
    proc.last_access = Instant::now();

    if let Ok(Some(status)) = proc.child.try_wait() {
        warn!(turns = proc.turns, status = %status, "cached claude subprocess exited after a completed turn");
    }

    Ok(output)
}

fn collect_stderr(
    stderr: tokio::process::ChildStderr,
    stderr_buf: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut chunk = String::new();
        while reader.read_line(&mut chunk).await.unwrap_or(0) > 0 {
            stderr_buf.lock().await.push_str(&chunk);
            chunk.clear();
        }
    })
}

#[derive(Debug)]
enum CachedTurnError {
    Dead(String),
    Llm(LlmError),
}

fn is_quota_error(err: &LlmError) -> bool {
    matches!(err, LlmError::HttpStatus { status: 429, .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_config::models::Sdk;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn request(opts: serde_json::Value) -> LlmRequest {
        LlmRequest {
            sdk: Sdk::ClaudeCode,
            model: "claude-sonnet-4-5".into(),
            api_key: String::new(),
            base_url: None,
            messages: vec![json!({"role": "user", "content": "hi"})],
            system: Some(json!("sys")),
            tools: None,
            max_tokens: 1024,
            temperature: None,
            top_p: None,
            provider_options: Some(opts),
            provider_key: Some("claude_code".into()),
            rid: None,
            forensic_character: None,
            system_suffix: None,
        }
    }

    fn request_with_key(key: &str, endpoint: &str) -> LlmRequest {
        request(json!({
            "mcp_endpoint": endpoint,
            "allowed_tools": [],
            "session_id": "11111111-1111-1111-1111-111111111111",
            "subprocess_key": key,
            "native_session_replay": false,
        }))
    }

    #[cfg(unix)]
    fn install_fake_claude(dir: &std::path::Path, exits_after_first: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("claude");
        let loop_body = if exits_after_first {
            r#"if IFS= read -r line; then
  i=1
  printf '%s\n' '{"type":"system","subtype":"init","model":"fake-claude"}'
  printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"turn%s"}]}}\n' "$i"
  printf '{"type":"result","is_error":false,"result":"turn%s","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"duration_ms":1}\n' "$i"
fi
"#
        } else {
            r#"i=0
while IFS= read -r line; do
  i=$((i + 1))
  printf '%s\n' '{"type":"system","subtype":"init","model":"fake-claude"}'
  printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"turn%s"}]}}\n' "$i"
  printf '{"type":"result","is_error":false,"result":"turn%s","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"duration_ms":1}\n' "$i"
done
"#
        };
        let script =
            format!("#!/bin/sh\nprintf 'spawn\\n' >> \"$FAKE_CLAUDE_SPAWNS\"\n{loop_body}");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    fn with_fake_path(dir: &std::path::Path) -> Option<std::ffi::OsString> {
        let old = std::env::var_os("PATH");
        let mut path = std::ffi::OsString::from(dir.as_os_str());
        path.push(":");
        if let Some(ref old) = old {
            path.push(old);
        }
        std::env::set_var("PATH", path);
        old
    }

    fn restore_path(old: Option<std::ffi::OsString>) {
        match old {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }

    fn done_content(output: &DriverOutput) -> String {
        output
            .events
            .iter()
            .find_map(|event| match event {
                crate::types::StreamEvent::Done { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn fingerprint_ignores_session_id_but_tracks_endpoint_and_prompt() {
        let r1 = request(json!({
            "mcp_endpoint": "http://127.0.0.1:1/mcp/a",
            "allowed_tools": ["mcp__shore__check_time"],
            "session_id": "11111111-1111-1111-1111-111111111111",
            "subprocess_key": "char",
        }));
        let r2 = request(json!({
            "mcp_endpoint": "http://127.0.0.1:1/mcp/a",
            "allowed_tools": ["mcp__shore__check_time"],
            "session_id": "22222222-2222-2222-2222-222222222222",
            "subprocess_key": "char",
        }));
        let c1 = ProviderConfig::from_request(&r1).unwrap();
        let c2 = ProviderConfig::from_request(&r2).unwrap();
        assert_eq!(
            RecipeFingerprint::from_request(&r1, &c1),
            RecipeFingerprint::from_request(&r2, &c2)
        );
    }

    #[test]
    fn fingerprint_changes_when_mcp_endpoint_changes() {
        let r1 = request(json!({"mcp_endpoint": "http://a/mcp/s", "allowed_tools": []}));
        let r2 = request(json!({"mcp_endpoint": "http://b/mcp/s", "allowed_tools": []}));
        let c1 = ProviderConfig::from_request(&r1).unwrap();
        let c2 = ProviderConfig::from_request(&r2).unwrap();
        assert_ne!(
            RecipeFingerprint::from_request(&r1, &c1),
            RecipeFingerprint::from_request(&r2, &c2)
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cache_hit_reuses_fake_subprocess() {
        let _guard = ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().unwrap();
        install_fake_claude(temp.path(), false);
        let count_path = temp.path().join("spawns");
        std::env::set_var("FAKE_CLAUDE_SPAWNS", &count_path);
        let old_path = with_fake_path(temp.path());
        let key = format!("test-cache-hit-{}", uuid::Uuid::new_v4());
        let req = request_with_key(&key, "http://127.0.0.1:1/mcp/stable");

        let first = run_long_lived(&req).await.unwrap();
        let second = run_long_lived(&req).await.unwrap();

        assert_eq!(done_content(&first), "turn1");
        assert_eq!(done_content(&second), "turn2");
        let spawns = std::fs::read_to_string(&count_path).unwrap();
        assert_eq!(spawns.lines().count(), 1);

        global_cache().entries.remove(&key);
        restore_path(old_path);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cache_hit_survives_growing_chat_history() {
        let _guard = ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().unwrap();
        install_fake_claude(temp.path(), false);
        let count_path = temp.path().join("spawns");
        std::env::set_var("FAKE_CLAUDE_SPAWNS", &count_path);
        let old_path = with_fake_path(temp.path());
        let key = format!("test-cache-history-{}", uuid::Uuid::new_v4());

        let first_req = request_with_key(&key, "http://127.0.0.1:1/mcp/stable-history");
        let mut second_req = request_with_key(&key, "http://127.0.0.1:1/mcp/stable-history");
        second_req.messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "turn1"}),
            json!({"role": "user", "content": "again"}),
        ];

        let first = run_long_lived(&first_req).await.unwrap();
        let second = run_long_lived(&second_req).await.unwrap();

        assert_eq!(done_content(&first), "turn1");
        assert_eq!(done_content(&second), "turn2");
        let spawns = std::fs::read_to_string(&count_path).unwrap();
        assert_eq!(spawns.lines().count(), 1);

        global_cache().entries.remove(&key);
        restore_path(old_path);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_same_key_calls_share_one_cached_subprocess() {
        let _guard = ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().unwrap();
        install_fake_claude(temp.path(), false);
        let count_path = temp.path().join("spawns");
        std::env::set_var("FAKE_CLAUDE_SPAWNS", &count_path);
        let old_path = with_fake_path(temp.path());
        let key = format!("test-cache-concurrent-{}", uuid::Uuid::new_v4());
        let req = request_with_key(&key, "http://127.0.0.1:1/mcp/stable");

        let (first, second) = tokio::join!(run_long_lived(&req), run_long_lived(&req));
        let mut contents = vec![
            done_content(&first.unwrap()),
            done_content(&second.unwrap()),
        ];
        contents.sort();

        assert_eq!(contents, vec!["turn1".to_string(), "turn2".to_string()]);
        let spawns = std::fs::read_to_string(&count_path).unwrap();
        assert_eq!(spawns.lines().count(), 1);

        global_cache().entries.remove(&key);
        restore_path(old_path);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn recipe_mismatch_evicts_and_respawns() {
        let _guard = ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().unwrap();
        install_fake_claude(temp.path(), false);
        let count_path = temp.path().join("spawns");
        std::env::set_var("FAKE_CLAUDE_SPAWNS", &count_path);
        let old_path = with_fake_path(temp.path());
        let key = format!("test-cache-mismatch-{}", uuid::Uuid::new_v4());

        let _ = run_long_lived(&request_with_key(&key, "http://127.0.0.1:1/mcp/a"))
            .await
            .unwrap();
        let second = run_long_lived(&request_with_key(&key, "http://127.0.0.1:1/mcp/b"))
            .await
            .unwrap();

        assert_eq!(done_content(&second), "turn1");
        let spawns = std::fs::read_to_string(&count_path).unwrap();
        assert_eq!(spawns.lines().count(), 2);

        global_cache().entries.remove(&key);
        restore_path(old_path);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn dead_cached_subprocess_is_evicted_and_respawned() {
        let _guard = ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().unwrap();
        install_fake_claude(temp.path(), true);
        let count_path = temp.path().join("spawns");
        std::env::set_var("FAKE_CLAUDE_SPAWNS", &count_path);
        let old_path = with_fake_path(temp.path());
        let key = format!("test-cache-dead-{}", uuid::Uuid::new_v4());
        let req = request_with_key(&key, "http://127.0.0.1:1/mcp/stable-dead");

        let first = run_long_lived(&req).await.unwrap();
        let second = run_long_lived(&req).await.unwrap();

        assert_eq!(done_content(&first), "turn1");
        assert_eq!(done_content(&second), "turn1");
        let spawns = std::fs::read_to_string(&count_path).unwrap();
        assert_eq!(spawns.lines().count(), 2);

        global_cache().entries.remove(&key);
        restore_path(old_path);
    }
}
