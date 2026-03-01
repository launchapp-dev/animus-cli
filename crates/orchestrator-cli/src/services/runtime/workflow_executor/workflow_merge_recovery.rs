use crate::services::runtime::runtime_daemon::daemon_scheduler::MergeConflictContext;
use crate::shared::{
    build_runtime_contract, collect_json_payload_lines, connect_runner, event_matches_run,
    runner_config_dir, write_json_line,
};
use anyhow::{anyhow, Context, Result};
use protocol::{
    default_primary_model_for_phase, tool_for_model_id, AgentRunEvent, AgentRunRequest, ModelId,
    RunId, PROTOCOL_VERSION,
};
use serde::Deserialize;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

pub(crate) const MERGE_CONFLICT_RECOVERY_TIMEOUT_SECS: u64 = 300;
pub(crate) const MERGE_CONFLICT_RECOVERY_RESULT_KIND: &str = "merge_conflict_resolution_result";
pub(crate) const MERGE_CONFLICT_RECOVERY_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/runtime/merge_conflict_recovery.prompt"
));

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MergeConflictRecoveryResponse {
    pub(crate) kind: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) commit_message: String,
    #[serde(default)]
    pub(crate) reason: String,
}

pub(crate) fn build_merge_conflict_recovery_prompt(
    task: &orchestrator_core::OrchestratorTask,
    context: &MergeConflictContext,
) -> String {
    let conflicted_files = if context.conflicted_files.is_empty() {
        "- (none detected by git)".to_string()
    } else {
        context
            .conflicted_files
            .iter()
            .map(|path| format!("- {}", path))
            .collect::<Vec<_>>()
            .join("\n")
    };

    MERGE_CONFLICT_RECOVERY_PROMPT_TEMPLATE
        .replace("__TASK_TITLE__", task.title.trim())
        .replace(
            "__TASK_DESCRIPTION__",
            task.description
                .chars()
                .take(2000)
                .collect::<String>()
                .as_str(),
        )
        .replace("__SOURCE_BRANCH__", context.source_branch.as_str())
        .replace("__TARGET_BRANCH__", context.target_branch.as_str())
        .replace(
            "__MERGE_WORKTREE_PATH__",
            context.merge_worktree_path.as_str(),
        )
        .replace("__CONFLICTED_FILES__", conflicted_files.as_str())
}

pub(crate) async fn run_merge_conflict_recovery_prompt_against_runner(
    project_root: &str,
    execution_cwd: &str,
    prompt: &str,
    model: &str,
    tool: &str,
    timeout_secs: u64,
) -> Result<String> {
    let run_id = RunId(format!("merge-conflict-recovery-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": execution_cwd,
        "project_root": project_root,
        "planning_stage": "merge-conflict-recovery",
        "allowed_tools": ["Read", "Glob", "Grep", "Edit", "Write", "Bash"],
        "timeout_secs": timeout_secs,
    });
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    let mut finished_successfully = false;
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                anyhow::bail!("merge conflict recovery run failed: {error}");
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    anyhow::bail!(
                        "merge conflict recovery run exited with non-zero code: {:?}",
                        exit_code
                    );
                }
                finished_successfully = true;
                break;
            }
            _ => {}
        }
    }

    if !finished_successfully {
        anyhow::bail!("runner disconnected before merge conflict recovery completed");
    }

    if transcript.trim().is_empty() {
        anyhow::bail!("merge conflict recovery run produced empty output");
    }

    Ok(transcript)
}

pub(crate) async fn attempt_ai_merge_conflict_recovery(
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    context: &MergeConflictContext,
) -> Result<()> {
    let impl_caps = protocol::PhaseCapabilities::defaults_for_phase("implementation");
    let model = default_primary_model_for_phase(None, &impl_caps).to_string();
    let tool = tool_for_model_id(&model).to_string();
    let prompt = build_merge_conflict_recovery_prompt(task, context);
    let transcript = run_merge_conflict_recovery_prompt_against_runner(
        project_root,
        context.merge_worktree_path.as_str(),
        &prompt,
        &model,
        &tool,
        MERGE_CONFLICT_RECOVERY_TIMEOUT_SECS,
    )
    .await?;

    let response = parse_merge_conflict_recovery_response(&transcript)
        .ok_or_else(|| anyhow!("merge conflict recovery output was not parseable JSON"))?;

    let status = merge_conflict_recovery_status(response.status.as_str())
        .ok_or_else(|| anyhow!("merge conflict recovery output has invalid status"))?;

    match status {
        "resolved" => {
            if response.commit_message.trim().is_empty() {
                anyhow::bail!("merge conflict recovery output is missing non-empty commit_message");
            }
            run_cargo_check(context.merge_worktree_path.as_str())?;
            Ok(())
        }
        "failed" => {
            let reason = response.reason.trim();
            if reason.is_empty() {
                anyhow::bail!("merge conflict recovery agent reported failure");
            }
            anyhow::bail!("merge conflict recovery agent reported failure: {reason}");
        }
        _ => anyhow::bail!("merge conflict recovery output has invalid status"),
    }
}

pub(crate) fn merge_conflict_recovery_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "resolved" => Some("resolved"),
        "failed" => Some("failed"),
        _ => None,
    }
}

pub(crate) fn is_valid_merge_conflict_recovery_response(
    response: &MergeConflictRecoveryResponse,
) -> bool {
    response
        .kind
        .trim()
        .eq_ignore_ascii_case(MERGE_CONFLICT_RECOVERY_RESULT_KIND)
        && merge_conflict_recovery_status(response.status.as_str()).is_some()
}

pub(crate) fn parse_merge_conflict_recovery_response(
    text: &str,
) -> Option<MergeConflictRecoveryResponse> {
    let mut parsed_response = None;
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Ok(response) = serde_json::from_value::<MergeConflictRecoveryResponse>(payload) {
            if is_valid_merge_conflict_recovery_response(&response) {
                parsed_response = Some(response);
            }
        }
    }
    if parsed_response.is_some() {
        return parsed_response;
    }

    if let Ok(response) = serde_json::from_str::<MergeConflictRecoveryResponse>(text.trim()) {
        if is_valid_merge_conflict_recovery_response(&response) {
            return Some(response);
        }
    }
    None
}

pub(crate) fn run_cargo_check(cwd: &str) -> Result<()> {
    let status = ProcessCommand::new("cargo")
        .current_dir(cwd)
        .arg("check")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run cargo check in {}", cwd))?;
    if !status.success() {
        anyhow::bail!("cargo check failed in {}", cwd);
    }
    Ok(())
}
