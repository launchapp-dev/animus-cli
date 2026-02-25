use protocol::{AgentRunEvent, AgentRunRequest, Timestamp};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info};

use super::process::spawn_cli_process;
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
    ) {
        let run_id = req.run_id.clone();
        let start_time = Instant::now();
        let request_timeout_secs = req.timeout_secs;

        let started_evt = AgentRunEvent::Started {
            run_id: run_id.clone(),
            timestamp: Timestamp::now(),
        };
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
        let timeout_secs = req
            .timeout_secs
            .or_else(|| context.get("timeout_secs").and_then(|v| v.as_u64()));
        let model = req.model.0.as_str();
        let runtime_contract = context.get("runtime_contract");
        let auth_profile_id = context
            .pointer("/auth_profile/profile_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let auth_env_mappings = context
            .pointer("/auth_profile/env_map")
            .and_then(|v| v.as_object())
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(required_env, source_env)| {
                        let required = required_env.trim();
                        let source = source_env.as_str().map(str::trim).unwrap_or_default();
                        if required.is_empty() || source.is_empty() {
                            return None;
                        }
                        Some((required.to_string(), source.to_string()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut auth_extra_env_keys = Vec::new();
        for (required_env, source_env) in &auth_env_mappings {
            auth_extra_env_keys.push(required_env.clone());
            auth_extra_env_keys.push(source_env.clone());
        }

        info!(
            run_id = %run_id.0.as_str(),
            model,
            tool,
            cwd,
            hard_timeout_secs = ?timeout_secs,
            request_timeout_secs = ?request_timeout_secs,
            has_runtime_contract = runtime_contract.is_some(),
            has_project_root = project_root.is_some(),
            auth_profile_id = ?auth_profile_id,
            auth_mapping_count = auth_env_mappings.len(),
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
                return;
            }
            info!(
                run_id = %run_id.0.as_str(),
                cwd,
                project_root = root,
                "Workspace validation passed"
            );
        }

        let mut env = if auth_extra_env_keys.is_empty() {
            env_sanitizer::sanitize_env()
        } else {
            env_sanitizer::sanitize_env_with_extra_vars(&auth_extra_env_keys)
        };
        let base_env_count = env.len();
        let mut missing_required_auth_envs = Vec::new();
        for (required_env, source_env) in &auth_env_mappings {
            if let Some(value) = env.get(source_env).cloned() {
                env.insert(required_env.clone(), value);
            } else if let Ok(value) = std::env::var(source_env) {
                env.insert(required_env.clone(), value);
            } else {
                missing_required_auth_envs.push(required_env.clone());
            }
        }

        // Add Claude settings path if working in a worktree
        if let Some(_root) = project_root {
            let settings_path = std::path::Path::new(cwd).join(".claude/settings.local.json");
            if settings_path.exists() {
                env.insert(
                    "CLAUDE_CODE_SETTINGS_PATH".to_string(),
                    settings_path.to_string_lossy().to_string(),
                );
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
        let mcp_endpoint = context
            .pointer("/runtime_contract/mcp/endpoint")
            .and_then(|v| v.as_str());

        if supports_mcp {
            if let Some(endpoint) = mcp_endpoint {
                // Keep names generic so different CLIs can opt in without per-vendor wiring.
                env.insert("MCP_ENDPOINT".to_string(), endpoint.to_string());
                env.insert("AO_MCP_ENDPOINT".to_string(), endpoint.to_string());
                env.insert("OPENCODE_MCP_ENDPOINT".to_string(), endpoint.to_string());
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
            auth_profile_id = ?auth_profile_id,
            missing_required_auth_envs = ?missing_required_auth_envs,
            "Launching CLI process"
        );

        match spawn_cli_process(
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
        {
            Ok(exit_code) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let finished_evt = AgentRunEvent::Finished {
                    run_id: run_id.clone(),
                    exit_code: Some(exit_code),
                    duration_ms,
                };
                let _ = event_tx.send(finished_evt).await;
                info!(
                    run_id = %run_id.0.as_str(),
                    exit_code,
                    duration_ms,
                    "Agent completed"
                );
            }
            Err(e) => {
                let error_evt = AgentRunEvent::Error {
                    run_id: run_id.clone(),
                    error: format!("Process execution failed: {}", e),
                };
                let _ = event_tx.send(error_evt).await;
                error!(run_id = %run_id.0.as_str(), error = %e, "Agent failed");
            }
        }
    }
}
