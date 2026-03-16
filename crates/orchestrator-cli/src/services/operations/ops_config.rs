use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use crate::cli_types::ConfigCommand;
use crate::print_value;

const CONFIG_LIST_SCHEMA: &str = "ao.config.v1";

pub(crate) async fn handle_config(
    command: ConfigCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        ConfigCommand::List => {
            let payload = build_config_list_payload(project_root);
            print_value(payload, json)
        }
    }
}

fn build_config_list_payload(project_root: &str) -> Value {
    let root = Path::new(project_root);

    let daemon = build_daemon_section(root);
    let agent_runtime = build_agent_runtime_section(root);
    let workflow = build_workflow_section(root);

    serde_json::json!({
        "schema": CONFIG_LIST_SCHEMA,
        "project_root": project_root,
        "generated_at": Utc::now(),
        "daemon": daemon,
        "agent_runtime": agent_runtime,
        "workflow": workflow,
    })
}

fn build_daemon_section(project_root: &Path) -> Value {
    match orchestrator_core::load_daemon_project_config(project_root) {
        Ok(config) => {
            let path = orchestrator_core::daemon_project_config_path(project_root);
            let source = if path.exists() { "file" } else { "default" };
            serde_json::json!({
                "source": source,
                "path": path.display().to_string(),
                "auto_merge_enabled": config.auto_merge_enabled,
                "auto_pr_enabled": config.auto_pr_enabled,
                "auto_commit_before_merge": config.auto_commit_before_merge,
                "auto_merge_target_branch": config.auto_merge_target_branch,
                "auto_merge_no_ff": config.auto_merge_no_ff,
                "auto_push_remote": config.auto_push_remote,
                "auto_cleanup_worktree_enabled": config.auto_cleanup_worktree_enabled,
                "auto_prune_worktrees_after_merge": config.auto_prune_worktrees_after_merge,
            })
        }
        Err(error) => serde_json::json!({
            "source": "error",
            "error": error.to_string(),
        }),
    }
}

fn build_agent_runtime_section(project_root: &Path) -> Value {
    match orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(
        project_root,
    ) {
        Ok(loaded) => {
            let agent_profiles: serde_json::Map<String, Value> = loaded
                .config
                .agents
                .iter()
                .map(|(id, profile)| {
                    let entry = serde_json::json!({
                        "tool": profile.tool,
                        "model": profile.model,
                        "fallback_models": profile.fallback_models,
                        "timeout_secs": profile.timeout_secs,
                        "max_attempts": profile.max_attempts,
                        "capabilities": profile.capabilities,
                    });
                    (id.clone(), entry)
                })
                .collect();

            let phase_routing: serde_json::Map<String, Value> = loaded
                .config
                .phases
                .iter()
                .map(|(id, phase)| {
                    let mut entry = serde_json::json!({
                        "mode": phase.mode.to_string(),
                        "agent_id": phase.agent_id,
                    });
                    if let Some(rt) = &phase.runtime {
                        entry["runtime_tool"] = rt.tool.clone().into();
                        entry["runtime_model"] = rt.model.clone().into();
                        entry["runtime_timeout_secs"] = rt.timeout_secs.into();
                    }
                    if let Some(retry) = &phase.retry {
                        entry["max_rework_attempts"] = retry.max_attempts.into();
                    }
                    (id.clone(), entry)
                })
                .collect();

            serde_json::json!({
                "source": loaded.metadata.source,
                "path": loaded.path.display().to_string(),
                "schema": loaded.metadata.schema,
                "version": loaded.metadata.version,
                "hash": loaded.metadata.hash,
                "agent_profiles": agent_profiles,
                "phase_routing": phase_routing,
            })
        }
        Err(error) => serde_json::json!({
            "source": "error",
            "error": error.to_string(),
        }),
    }
}

fn build_workflow_section(project_root: &Path) -> Value {
    match orchestrator_core::load_workflow_config_with_metadata(project_root) {
        Ok(loaded) => {
            let workflow_ids: Vec<&str> = loaded
                .config
                .workflows
                .iter()
                .map(|w| w.id.as_str())
                .collect();
            serde_json::json!({
                "source": loaded.metadata.source,
                "path": loaded.path.display().to_string(),
                "schema": loaded.metadata.schema,
                "version": loaded.metadata.version,
                "hash": loaded.metadata.hash,
                "workflow_count": loaded.config.workflows.len(),
                "workflow_ids": workflow_ids,
                "phase_definition_count": loaded.config.phase_definitions.len(),
                "agent_profile_count": loaded.config.agent_profiles.len(),
            })
        }
        Err(error) => serde_json::json!({
            "source": "error",
            "error": error.to_string(),
        }),
    }
}
