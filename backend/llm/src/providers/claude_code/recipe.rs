//! CLI flag construction for the `claude` subprocess.
//!
//! The flag set is locked in by `dev/spikes/claude-code-probe/FINDINGS.md`.
//! Every isolation flag (`--strict-mcp-config`, `--tools ""`,
//! `--setting-sources ""`, `--no-session-persistence`) is mandatory; the
//! permission gate (`--allowedTools`) is required for MCP tool calls to
//! reach the server. Optional flags (`--effort`) are gated.

use std::path::PathBuf;
use std::process::Stdio;

use serde_json::json;
use tokio::process::Command;

/// Inputs for one `claude` subprocess invocation.
#[derive(Debug, Clone)]
pub(super) struct CliRecipe {
    /// Anthropic model id passed via `--model`.
    pub model: String,
    /// HTTP URL the daemon's MCP listener is reachable at for this
    /// session (e.g. `http://127.0.0.1:7321/mcp/<uuid>`).
    pub mcp_endpoint: String,
    /// Tool names to permit at the `--allowedTools` gate. Names must
    /// already be in the namespaced form `mcp__shore__<tool>`.
    pub allowed_tools: Vec<String>,
    /// Path to the file containing the rendered system prompt.
    pub system_prompt_path: PathBuf,
    /// Fresh UUID-or-equivalent for `--session-id`.
    pub session_id: String,
    /// Optional reasoning effort for thinking-capable models.
    pub effort: Option<String>,
}

impl CliRecipe {
    pub(super) fn into_command(self) -> Command {
        let mcp_config = json!({
            "mcpServers": {
                "shore": {
                    "type": "http",
                    "url": self.mcp_endpoint,
                }
            }
        })
        .to_string();
        let allowed_csv = self.allowed_tools.join(",");

        let mut cmd = Command::new("claude");
        cmd.arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--no-session-persistence")
            .arg("--setting-sources")
            .arg("")
            .arg("--strict-mcp-config")
            .arg("--mcp-config")
            .arg(mcp_config)
            .arg("--tools")
            .arg("")
            .arg("--allowedTools")
            .arg(allowed_csv)
            .arg("--model")
            .arg(&self.model)
            .arg("--system-prompt-file")
            .arg(&self.system_prompt_path)
            .arg("--session-id")
            .arg(&self.session_id);
        if let Some(effort) = &self.effort {
            cmd.arg("--effort").arg(effort);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a: &OsStr| a.to_string_lossy().into_owned())
            .collect()
    }

    fn sample_recipe() -> CliRecipe {
        CliRecipe {
            model: "claude-sonnet-4-5".into(),
            mcp_endpoint: "http://127.0.0.1:7321/mcp/abc".into(),
            allowed_tools: vec!["mcp__shore__memory".into(), "mcp__shore__search".into()],
            system_prompt_path: PathBuf::from("/tmp/sys.txt"),
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            effort: Some("medium".into()),
        }
    }

    #[test]
    fn includes_required_isolation_flags() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        assert!(args.iter().any(|a| a == "--print"));
        assert!(args.iter().any(|a| a == "--no-session-persistence"));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        let i = args.iter().position(|a| a == "--setting-sources").unwrap();
        assert_eq!(args[i + 1], "");
        let i = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[i + 1], "");
    }

    #[test]
    fn output_and_input_format_are_stream_json() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        let i = args.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(args[i + 1], "stream-json");
        let i = args.iter().position(|a| a == "--input-format").unwrap();
        assert_eq!(args[i + 1], "stream-json");
    }

    #[test]
    fn mcp_config_carries_endpoint_url_as_http_transport() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        let i = args.iter().position(|a| a == "--mcp-config").unwrap();
        let j: serde_json::Value = serde_json::from_str(&args[i + 1]).unwrap();
        assert_eq!(j["mcpServers"]["shore"]["type"], "http");
        assert_eq!(
            j["mcpServers"]["shore"]["url"],
            "http://127.0.0.1:7321/mcp/abc"
        );
    }

    #[test]
    fn allowed_tools_serialized_as_csv() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        let i = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(args[i + 1], "mcp__shore__memory,mcp__shore__search");
    }

    #[test]
    fn allowed_tools_empty_yields_empty_csv() {
        let mut r = sample_recipe();
        r.allowed_tools.clear();
        let cmd = r.into_command();
        let args = args_of(&cmd);
        let i = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(args[i + 1], "");
    }

    #[test]
    fn effort_omitted_when_none() {
        let mut r = sample_recipe();
        r.effort = None;
        let cmd = r.into_command();
        let args = args_of(&cmd);
        assert!(!args.iter().any(|a| a == "--effort"));
    }

    #[test]
    fn effort_passed_when_some() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        let i = args.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(args[i + 1], "medium");
    }

    #[test]
    fn system_prompt_file_points_at_path() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        let i = args
            .iter()
            .position(|a| a == "--system-prompt-file")
            .unwrap();
        assert_eq!(args[i + 1], "/tmp/sys.txt");
    }

    #[test]
    fn model_and_session_id_flow_through() {
        let cmd = sample_recipe().into_command();
        let args = args_of(&cmd);
        let i = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[i + 1], "claude-sonnet-4-5");
        let i = args.iter().position(|a| a == "--session-id").unwrap();
        assert_eq!(args[i + 1], "11111111-2222-3333-4444-555555555555");
    }

    #[test]
    fn program_is_claude() {
        let cmd = sample_recipe().into_command();
        assert_eq!(cmd.as_std().get_program(), "claude");
    }
}
