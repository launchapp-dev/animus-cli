use protocol::{AgentRunEvent, AgentRunRequest, AgentStatus, Timestamp};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info};

use super::process::spawn_cli_process;
use super::session_process::{require_native_session_backend, spawn_session_process, use_native_session_backend};
use crate::sandbox::{env_sanitizer, workspace_guard};

pub struct Supervisor;

impl Supervisor {
    pub fn new() -> Self {
        Self
    }

    pub async fn spawn_agent(
        &self,
        req: AgentRunRequest,
        event_tx: mpsc::Sender<AgentRunEvent>,
        cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> AgentStatus {
        let run_id = req.run_id.clone();
        let start_time = Instant::now();
        let request_timeout_secs = req.timeout_secs;

        let started_evt = AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() };
        let _ = event_tx.send(started_evt).await;

        let context: serde_json::Value = req.context.clone();
        let tool = context
            .pointer("/runtime_contract/cli/name")
            .and_then(|v| v.as_str())
            .or_else(|| context.get("tool").and_then(|v| v.as_str()))
            .unwrap_or("claude");
        let prompt = context.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let cwd = context.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");
        let project_root = context.get("project_root").and_then(|v| v.as_str());
        let timeout_secs = req.timeout_secs.or_else(|| context.get("timeout_secs").and_then(|v| v.as_u64()));
        let model = req.model.0.as_str();
        let runtime_contract = context.get("runtime_contract");

        info!(
            run_id = %run_id.0.as_str(),
            model,
            tool,
            cwd,
            hard_timeout_secs = ?timeout_secs,
            request_timeout_secs = ?request_timeout_secs,
            has_runtime_contract = runtime_contract.is_some(),
            has_project_root = project_root.is_some(),
            "Supervisor accepted agent run"
        );

        if let Some(root) = project_root {
            if let Err(e) = workspace_guard::validate_workspace(cwd, root) {
                error!(
                    run_id = %run_id.0.as_str(),
                    cwd,
                    project_root = root,
                    error = %e,
                    "Workspace validation failed"
                );
                let error_evt = AgentRunEvent::Error {
                    run_id: run_id.clone(),
                    error: format!("Workspace validation failed: {}", e),
                };
                let _ = event_tx.send(error_evt).await;
                return AgentStatus::Failed;
            }
            info!(
                run_id = %run_id.0.as_str(),
                cwd,
                project_root = root,
                "Workspace validation passed"
            );
        }

        let mut env = env_sanitizer::sanitize_env();
        let base_env_count = env.len();

        // Add Claude settings path if working in a worktree
        if let Some(_root) = project_root {
            let settings_path = std::path::Path::new(cwd).join(".claude/settings.local.json");
            if settings_path.exists() {
                env.insert("CLAUDE_CODE_SETTINGS_PATH".to_string(), settings_path.to_string_lossy().to_string());
                info!(
                    run_id = %run_id.0.as_str(),
                    settings_path = %settings_path.display(),
                    "Configured Claude settings path for run"
                );
            }
        }

        let supports_mcp = context
            .pointer("/runtime_contract/cli/capabilities/supports_mcp")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mcp_endpoint = context.pointer("/runtime_contract/mcp/endpoint").and_then(|v| v.as_str());

        if supports_mcp {
            if let Some(endpoint) = mcp_endpoint {
                // Keep names generic so different CLIs can opt in without per-vendor wiring.
                env.insert("AO_MCP_ENDPOINT".to_string(), endpoint.to_string());
                info!(
                    run_id = %run_id.0.as_str(),
                    endpoint,
                    "Injected MCP endpoint environment for run"
                );
            } else {
                info!(
                    run_id = %run_id.0.as_str(),
                    "Run supports MCP but no endpoint was provided"
                );
            }
        }

        info!(
            run_id = %run_id.0.as_str(),
            base_env_count,
            final_env_count = env.len(),
            "Launching CLI process"
        );

        if let Err(error) = require_native_session_backend(tool, runtime_contract) {
            let error_message = format!("Process execution failed: {}", error);
            let error_evt = AgentRunEvent::Error { run_id: run_id.clone(), error: error_message.clone() };
            let _ = event_tx.send(error_evt).await;
            error!(
                run_id = %run_id.0.as_str(),
                tool,
                error = %error,
                "AI tool is not allowed to fall back to legacy subprocess execution"
            );
            return AgentStatus::Failed;
        }

        let use_native_sessions = use_native_session_backend(tool, runtime_contract);
        info!(
            run_id = %run_id.0.as_str(),
            tool,
            use_native_sessions,
            "Selected runner execution path"
        );

        let execution_result = if use_native_sessions {
            spawn_session_process(
                tool,
                model,
                prompt,
                runtime_contract,
                cwd,
                env,
                timeout_secs,
                &run_id,
                event_tx.clone(),
                cancel_rx,
            )
            .await
        } else {
            spawn_cli_process(
                tool,
                model,
                prompt,
                runtime_contract,
                cwd,
                env,
                timeout_secs,
                &run_id,
                event_tx.clone(),
                cancel_rx,
            )
            .await
        };

        match execution_result {
            Ok(exit_code) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let finished_evt =
                    AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(exit_code), duration_ms };
                let _ = event_tx.send(finished_evt).await;
                info!(
                    run_id = %run_id.0.as_str(),
                    exit_code,
                    duration_ms,
                    "Agent completed"
                );
                status_from_exit_code(exit_code)
            }
            Err(e) => {
                let error_evt =
                    AgentRunEvent::Error { run_id: run_id.clone(), error: format!("Process execution failed: {}", e) };
                let _ = event_tx.send(error_evt).await;
                error!(run_id = %run_id.0.as_str(), error = %e, "Agent failed");
                AgentStatus::Failed
            }
        }
    }
}

fn status_from_exit_code(exit_code: i32) -> AgentStatus {
    if exit_code == 0 {
        AgentStatus::Completed
    } else {
        AgentStatus::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_from_exit_code_maps_success_to_completed() {
        assert_eq!(status_from_exit_code(0), AgentStatus::Completed);
    }

    #[test]
    fn status_from_exit_code_maps_non_zero_to_failed() {
        assert_eq!(status_from_exit_code(1), AgentStatus::Failed);
    }
}
