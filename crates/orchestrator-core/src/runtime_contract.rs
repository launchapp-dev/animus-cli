use crate::agent_runtime_config::{AgentRuntimeConfig, CliToolConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliCapabilities {
    pub supports_file_editing: bool,
    pub supports_streaming: bool,
    pub supports_tool_use: bool,
    pub supports_vision: bool,
    pub supports_long_context: bool,
    pub max_context_tokens: Option<usize>,
    pub supports_mcp: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliSessionResumeMode {
    None,
    NativeId,
    SummarySeed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliSessionResumePlan {
    pub mode: CliSessionResumeMode,
    pub session_key: String,
    pub session_id: Option<String>,
    pub summary_seed: Option<String>,
    pub reused: bool,
    pub phase_thread_isolated: bool,
}

fn normalized_tool(tool: &str) -> String {
    tool.trim().to_ascii_lowercase()
}

pub fn cli_capabilities_for_tool(tool: &str) -> Option<CliCapabilities> {
    match normalized_tool(tool).as_str() {
        "claude" => Some(CliCapabilities {
            supports_file_editing: true,
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: true,
            supports_long_context: true,
            max_context_tokens: Some(200_000),
            supports_mcp: true,
        }),
        "codex" => Some(CliCapabilities {
            supports_file_editing: true,
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: false,
            supports_long_context: false,
            max_context_tokens: Some(128_000),
            supports_mcp: true,
        }),
        "gemini" => Some(CliCapabilities {
            supports_file_editing: true,
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: true,
            supports_long_context: true,
            max_context_tokens: Some(1_000_000),
            supports_mcp: true,
        }),
        "opencode" => Some(CliCapabilities {
            supports_file_editing: true,
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: false,
            supports_long_context: true,
            max_context_tokens: Some(200_000),
            supports_mcp: true,
        }),
        "oai-runner" => Some(CliCapabilities {
            supports_file_editing: true,
            supports_streaming: true,
            supports_tool_use: true,
            supports_vision: false,
            supports_long_context: true,
            max_context_tokens: Some(200_000),
            supports_mcp: true,
        }),
        "aider" => Some(CliCapabilities {
            supports_file_editing: true,
            supports_streaming: true,
            supports_tool_use: false,
            supports_vision: false,
            supports_long_context: false,
            max_context_tokens: Some(128_000),
            supports_mcp: false,
        }),
        "cursor" | "cline" | "custom" => Some(CliCapabilities {
            supports_file_editing: false,
            supports_streaming: false,
            supports_tool_use: false,
            supports_vision: false,
            supports_long_context: false,
            max_context_tokens: None,
            supports_mcp: false,
        }),
        _ => None,
    }
}

impl CliToolConfig {
    pub fn to_capabilities(&self) -> CliCapabilities {
        CliCapabilities {
            supports_file_editing: self.supports_file_editing.unwrap_or(false),
            supports_streaming: self.supports_streaming.unwrap_or(false),
            supports_tool_use: self.supports_tool_use.unwrap_or(false),
            supports_vision: self.supports_vision.unwrap_or(false),
            supports_long_context: self.supports_long_context.unwrap_or(false),
            max_context_tokens: self.max_context_tokens,
            supports_mcp: self.supports_mcp.unwrap_or(false),
        }
    }
}

pub fn cli_capabilities_from_config(
    tool: &str,
    config: &AgentRuntimeConfig,
) -> Option<CliCapabilities> {
    let normalized = normalized_tool(tool);
    config
        .cli_tools
        .get(&normalized)
        .map(|tc| tc.to_capabilities())
        .or_else(|| cli_capabilities_for_tool(&normalized))
}

pub fn cli_tool_executable(tool: &str, config: &AgentRuntimeConfig) -> String {
    let normalized = normalized_tool(tool);
    config
        .cli_tools
        .get(&normalized)
        .and_then(|tc| tc.executable.clone())
        .unwrap_or_else(|| {
            if normalized == "oai-runner" {
                "ao-oai-runner".to_string()
            } else {
                normalized
            }
        })
}

pub fn cli_tool_read_only_flag(tool: &str, config: &AgentRuntimeConfig) -> Option<String> {
    let normalized = normalized_tool(tool);
    config
        .cli_tools
        .get(&normalized)
        .and_then(|tc| tc.read_only_flag.clone())
}

pub fn cli_tool_response_schema_flag(tool: &str, config: &AgentRuntimeConfig) -> Option<String> {
    let normalized = normalized_tool(tool);
    config
        .cli_tools
        .get(&normalized)
        .and_then(|tc| tc.response_schema_flag.clone())
}

pub fn build_cli_launch_contract(
    tool: &str,
    model_id: &str,
    prompt: &str,
    resume_plan: Option<&CliSessionResumePlan>,
    command_override: Option<&str>,
) -> Option<Value> {
    let normalized = normalized_tool(tool);
    let has_specific_model = !model_id.trim().is_empty() && model_id.trim() != normalized;
    let resume_mode = resume_plan
        .map(|plan| plan.mode)
        .unwrap_or(CliSessionResumeMode::None);
    let resume_id = resume_plan
        .and_then(|plan| plan.session_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string);

    let args = match normalized.as_str() {
        "claude" => {
            let mut args = vec![
                "--print".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--verbose".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ];
            if matches!(resume_mode, CliSessionResumeMode::NativeId) {
                if let Some(session_id) = resume_id.clone() {
                    let is_reused = resume_plan.map(|p| p.reused).unwrap_or(false);
                    if is_reused {
                        args.push("--resume".to_string());
                    } else {
                        args.push("--session-id".to_string());
                    }
                    args.push(session_id);
                }
            }
            if has_specific_model {
                args.push("--model".to_string());
                args.push(model_id.to_string());
            }
            args.push(prompt.to_string());
            args
        }
        "codex" => {
            let is_reused = resume_plan.map(|p| p.reused).unwrap_or(false);
            let mut args = vec!["exec".to_string()];
            if is_reused && matches!(resume_mode, CliSessionResumeMode::NativeId) {
                args.push("resume".to_string());
                args.push("--last".to_string());
            }
            args.push("--json".to_string());
            args.push("--full-auto".to_string());
            args.push("-c".to_string());
            args.push("sandbox_workspace_write.network_access=true".to_string());
            args.push("--skip-git-repo-check".to_string());
            if has_specific_model {
                args.push("--model".to_string());
                args.push(model_id.to_string());
            }
            args.push(prompt.to_string());
            args
        }
        "gemini" => {
            let mut args = Vec::new();
            let is_reused = resume_plan.map(|p| p.reused).unwrap_or(false);
            if is_reused && matches!(resume_mode, CliSessionResumeMode::NativeId) {
                if let Some(session_id) = resume_id.clone() {
                    args.push("--resume".to_string());
                    args.push(session_id);
                }
            }
            if has_specific_model {
                args.push("--model".to_string());
                args.push(model_id.to_string());
            }
            args.push("--output-format".to_string());
            args.push("json".to_string());
            args.push("-p".to_string());
            args.push(prompt.to_string());
            args
        }
        "opencode" => {
            let mut args = vec!["run".to_string()];
            if matches!(resume_mode, CliSessionResumeMode::NativeId) {
                if let Some(session_id) = resume_id {
                    args.push("--session".to_string());
                    args.push(session_id);
                }
            }
            if has_specific_model {
                args.push("-m".to_string());
                args.push(model_id.to_string());
            }
            args.push("--format".to_string());
            args.push("json".to_string());
            args.push(prompt.to_string());
            args
        }
        "oai-runner" => {
            let mut args = vec!["run".to_string()];
            args.push("-m".to_string());
            args.push(model_id.to_string());
            args.push("--format".to_string());
            args.push("json".to_string());
            if matches!(resume_mode, CliSessionResumeMode::NativeId) {
                if let Some(session_id) = resume_id.clone() {
                    args.push("--session-id".to_string());
                    args.push(session_id);
                }
            }
            args.push(prompt.to_string());
            args
        }
        _ => return None,
    };

    let default_command = match normalized.as_str() {
        "oai-runner" => "ao-oai-runner".to_string(),
        _ => normalized,
    };

    let command = command_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or(default_command);

    Some(json!({
        "command": command,
        "args": args,
        "prompt_via_stdin": false
    }))
}

pub fn build_runtime_contract(
    tool: &str,
    model_id: &str,
    prompt: &str,
    resume_plan: Option<&CliSessionResumePlan>,
    command_override: Option<&str>,
    mcp_endpoint: Option<&str>,
    mcp_agent_id: Option<&str>,
) -> Option<Value> {
    let normalized = normalized_tool(tool);
    let launch =
        build_cli_launch_contract(&normalized, model_id, prompt, resume_plan, command_override)?;
    let capabilities = cli_capabilities_for_tool(&normalized)?;
    let enforce_mcp_only = mcp_endpoint.is_some() && capabilities.supports_mcp;

    let mut cli = serde_json::Map::new();
    cli.insert("name".to_string(), json!(normalized));
    cli.insert("capabilities".to_string(), json!(capabilities));
    cli.insert("launch".to_string(), launch);

    if let Some(plan) = resume_plan {
        cli.insert(
            "session".to_string(),
            json!({
                "mode": plan.mode,
                "session_key": plan.session_key,
                "session_id": plan.session_id,
                "summary_seed": plan.summary_seed,
                "reused": plan.reused,
                "phase_thread_isolated": plan.phase_thread_isolated,
            }),
        );
    }

    Some(json!({
        "cli": Value::Object(cli),
        "model": model_id,
        "mcp": {
            "agent_id": mcp_agent_id,
            "endpoint": mcp_endpoint,
            "enforce_only": enforce_mcp_only,
            "allowed_tool_prefixes": if enforce_mcp_only {
                protocol::default_allowed_tool_prefixes(mcp_agent_id.unwrap_or("ao"))
            } else {
                Vec::<String>::new()
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_codex_model() -> &'static str {
        protocol::default_model_for_tool("codex")
            .expect("default model for codex should be configured")
    }

    #[test]
    fn build_runtime_contract_includes_rich_cli_shape() {
        let contract = build_runtime_contract(
            "codex",
            default_codex_model(),
            "Implement feature",
            None,
            None,
            Some("http://127.0.0.1:7000/mcp/agent"),
            Some("agent-1"),
        )
        .expect("runtime contract should build");

        assert_eq!(
            contract.pointer("/cli/name").and_then(Value::as_str),
            Some("codex")
        );
        assert_eq!(
            contract
                .pointer("/cli/capabilities/supports_tool_use")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            contract.pointer("/model").and_then(Value::as_str),
            Some(default_codex_model())
        );
        assert_eq!(
            contract.pointer("/mcp/agent_id").and_then(Value::as_str),
            Some("agent-1")
        );
        assert_eq!(
            contract
                .pointer("/mcp/enforce_only")
                .and_then(Value::as_bool),
            Some(true)
        );
        let allowed_prefixes = contract
            .pointer("/mcp/allowed_tool_prefixes")
            .and_then(Value::as_array)
            .expect("allowed tool prefixes should be present");
        assert!(
            allowed_prefixes
                .iter()
                .filter_map(Value::as_str)
                .any(|prefix| prefix == "ao."),
            "AO prefix should always be allowed under MCP-only enforcement"
        );
    }

    #[test]
    fn build_runtime_contract_enforces_mcp_only_when_supported_tool_has_endpoint() {
        let contract = build_runtime_contract(
            "claude",
            "claude-sonnet-4-6",
            "Implement feature",
            None,
            None,
            Some("http://127.0.0.1:7000/mcp/ao"),
            Some("ao"),
        )
        .expect("runtime contract should build");

        assert_eq!(
            contract
                .pointer("/mcp/enforce_only")
                .and_then(Value::as_bool),
            Some(true)
        );
        let allowed_prefixes = contract
            .pointer("/mcp/allowed_tool_prefixes")
            .and_then(Value::as_array)
            .expect("allowed tool prefixes should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(allowed_prefixes.contains(&"ao."));
        assert!(allowed_prefixes.contains(&"mcp__ao__"));
    }

    #[test]
    fn build_launch_contract_supports_resume_mode() {
        let plan = CliSessionResumePlan {
            mode: CliSessionResumeMode::NativeId,
            session_key: "wf:1".to_string(),
            session_id: Some("session-123".to_string()),
            summary_seed: None,
            reused: true,
            phase_thread_isolated: true,
        };
        let launch =
            build_cli_launch_contract("gemini", "gemini-2.5-pro", "hello", Some(&plan), None)
                .expect("launch contract should build");

        let args = launch
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("launch args should be present");
        let args = args.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        assert!(args.contains(&"--output-format"));
        assert!(args.contains(&"json"));
        assert!(args.contains(&"--resume"));
        assert!(args.contains(&"session-123"));
    }

    #[test]
    fn build_launch_contract_enforces_machine_output_for_supported_tools() {
        let codex = build_cli_launch_contract("codex", default_codex_model(), "hello", None, None)
            .expect("codex launch contract should build");
        let codex_args = codex
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("codex args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(codex_args.contains(&"--json"));
        assert!(codex_args.contains(&"-c"));
        assert!(codex_args.contains(&"sandbox_workspace_write.network_access=true"));

        let claude = build_cli_launch_contract("claude", "claude-opus-4-1", "hello", None, None)
            .expect("claude launch contract should build");
        let claude_args = claude
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("claude args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(claude_args.contains(&"--verbose"));
        assert!(claude_args.contains(&"--output-format"));
        assert!(claude_args.contains(&"stream-json"));

        let gemini = build_cli_launch_contract("gemini", "gemini-2.5-pro", "hello", None, None)
            .expect("gemini launch contract should build");
        let gemini_args = gemini
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("gemini args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(gemini_args.contains(&"--output-format"));
        assert!(gemini_args.contains(&"json"));

        let opencode = build_cli_launch_contract("opencode", "glm-5", "hello", None, None)
            .expect("opencode launch contract should build");
        let opencode_args = opencode
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("opencode args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(opencode_args.contains(&"--format"));
        assert!(opencode_args.contains(&"json"));
    }

    #[test]
    fn build_launch_contract_preserves_opencode_glm_and_minimax_models() {
        for model in ["zai-coding-plan/glm-4.7", "minimax/MiniMax-M2.1"] {
            let launch = build_cli_launch_contract("opencode", model, "hello", None, None)
                .expect("opencode launch contract should build");
            let args = launch
                .pointer("/args")
                .and_then(Value::as_array)
                .expect("opencode args should be present")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();

            let model_flag_index = args
                .iter()
                .position(|entry| *entry == "-m")
                .expect("opencode launch should include -m model flag");
            assert_eq!(
                args.get(model_flag_index + 1).copied(),
                Some(model),
                "opencode launch should preserve provider model id"
            );
            assert!(args.contains(&"--format"));
            assert!(args.contains(&"json"));
        }
    }
}
