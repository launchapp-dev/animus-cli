use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use orchestrator_core::{services::ServiceHub, TaskStatus, WorkflowStatus};
use orchestrator_git_ops::{
    cleanup_merge_conflict_worktree, finalize_merge_conflict_resolution, is_branch_merged,
    load_post_success_git_config, post_success_merge_push_and_cleanup, PostMergeOutcome,
    PostSuccessGitConfig,
};
use workflow_runner::executor::attempt_ai_merge_conflict_recovery;

use crate::{
    merge_blocked_reason, project_task_blocked_with_reason, project_task_dispatch_failure,
    project_task_status, remove_terminal_em_work_queue_entry_non_fatal, MERGE_GATE_PREFIX,
};

const MAX_DISPATCH_RETRIES: u32 = 3;

pub async fn sync_task_status_for_workflow_result(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task_id: &str,
    workflow_status: WorkflowStatus,
    workflow_id: Option<&str>,
) {
    match workflow_status {
        WorkflowStatus::Completed => {
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "completed").await;
            }
            let workflow = if let Some(wf_id) = workflow_id {
                hub.workflows().get(wf_id).await.ok()
            } else {
                None
            };
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let task = hub.tasks().get(task_id).await;
            let Ok(task) = task else {
                let _ = project_task_status(hub.clone(), task_id, TaskStatus::Done).await;
                return;
            };
            let cfg = effective_post_success_git_config(project_root, workflow.as_ref());

            match post_success_merge_push_and_cleanup(hub.clone(), project_root, &task, &cfg).await
            {
                Ok(PostMergeOutcome::Completed) => {
                    let _ = project_task_status(hub.clone(), task_id, TaskStatus::Done).await;
                    return;
                }
                Ok(PostMergeOutcome::Skipped) => {}
                Ok(PostMergeOutcome::Conflict { context }) => {
                    let conflict_summary = if context.conflicted_files.is_empty() {
                        "merge conflict detected".to_string()
                    } else {
                        format!(
                            "merge conflict detected in files: {}",
                            context.conflicted_files.join(", ")
                        )
                    };
                    if let Some(workflow_id) = workflow_id {
                        let _ = hub
                            .workflows()
                            .mark_merge_conflict(workflow_id, conflict_summary)
                            .await;
                    }

                    let recovery_result =
                        attempt_ai_merge_conflict_recovery(project_root, &task, &context).await;
                    if let Err(error) = recovery_result {
                        cleanup_merge_conflict_worktree(project_root, &context);
                        let _ = project_task_blocked_with_reason(
                            hub.clone(),
                            &task,
                            format!(
                                "{MERGE_GATE_PREFIX} auto-merge conflict recovery failed: {error}"
                            ),
                            None,
                        )
                        .await;
                        return;
                    }

                    match finalize_merge_conflict_resolution(
                        hub.clone(),
                        project_root,
                        &task,
                        &cfg,
                        &context,
                    )
                    .await
                    {
                        Ok(()) => {
                            if let Some(workflow_id) = workflow_id {
                                let _ = hub.workflows().resolve_merge_conflict(workflow_id).await;
                            }
                            let _ =
                                project_task_status(hub.clone(), task_id, TaskStatus::Done).await;
                        }
                        Err(error) => {
                            cleanup_merge_conflict_worktree(project_root, &context);
                            let _ = project_task_blocked_with_reason(
                                hub.clone(),
                                &task,
                                format!(
                                    "{MERGE_GATE_PREFIX} merge conflict recovery finalize failed: {error}"
                                ),
                                None,
                            )
                            .await;
                        }
                    }
                    return;
                }
                Err(error) => {
                    if let Some(workflow_id) = workflow_id {
                        let _ = hub
                            .workflows()
                            .mark_merge_conflict(workflow_id, error.to_string())
                            .await;
                    }
                    let _ = project_task_blocked_with_reason(
                        hub.clone(),
                        &task,
                        format!("{MERGE_GATE_PREFIX} auto-merge failed: {error}"),
                        None,
                    )
                    .await;
                    return;
                }
            }

            let Some(branch_name) = task
                .branch_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                let _ = project_task_status(hub.clone(), task_id, TaskStatus::Done).await;
                return;
            };

            match is_branch_merged(project_root, branch_name) {
                Ok(Some(true)) | Ok(None) => {
                    let _ = project_task_status(hub.clone(), task_id, TaskStatus::Done).await;
                }
                Ok(Some(false)) => {
                    let _ = project_task_blocked_with_reason(
                        hub.clone(),
                        &task,
                        merge_blocked_reason(branch_name),
                        None,
                    )
                    .await;
                }
                Err(error) => {
                    let _ = project_task_blocked_with_reason(
                        hub.clone(),
                        &task,
                        format!("{MERGE_GATE_PREFIX} unable to verify merge status: {error}"),
                        None,
                    )
                    .await;
                }
            }
        }
        WorkflowStatus::Failed => {
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "failed").await;
            }
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let _ = project_task_dispatch_failure(hub.clone(), task_id, MAX_DISPATCH_RETRIES).await;
        }
        WorkflowStatus::Escalated => {
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "escalated").await;
            }
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let _ = project_task_status(hub.clone(), task_id, TaskStatus::Blocked).await;
        }
        WorkflowStatus::Cancelled => {
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "cancelled").await;
            }
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let _ = project_task_status(hub.clone(), task_id, TaskStatus::Cancelled).await;
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {
            let _ = project_task_status(hub.clone(), task_id, TaskStatus::InProgress).await;
        }
    }
}

fn resolve_workflow_pipeline_definition<'a>(
    config: &'a orchestrator_core::WorkflowConfig,
    pipeline_id: &str,
) -> Option<&'a orchestrator_core::workflow_config::PipelineDefinition> {
    config
        .pipelines
        .iter()
        .find(|pipeline| pipeline.id.eq_ignore_ascii_case(pipeline_id))
}

fn effective_post_success_git_config(
    project_root: &str,
    workflow: Option<&orchestrator_core::OrchestratorWorkflow>,
) -> PostSuccessGitConfig {
    let mut cfg = load_post_success_git_config(project_root);

    let workflow = match workflow {
        Some(workflow) => workflow,
        None => return cfg,
    };
    let workflow_config = match orchestrator_core::load_workflow_config(Path::new(project_root)) {
        Ok(config) => config,
        Err(_) => return cfg,
    };

    let requested_pipeline_id = workflow
        .pipeline_id
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .unwrap_or_else(|| workflow_config.default_pipeline_id.as_str());
    let Some(pipeline) =
        resolve_workflow_pipeline_definition(&workflow_config, requested_pipeline_id)
            .or_else(|| {
                resolve_workflow_pipeline_definition(
                    &workflow_config,
                    workflow_config.default_pipeline_id.as_str(),
                )
            })
            .or_else(|| {
                resolve_workflow_pipeline_definition(
                    &workflow_config,
                    orchestrator_core::STANDARD_PIPELINE_ID,
                )
            })
    else {
        return cfg;
    };

    let Some(merge_cfg) = pipeline
        .post_success
        .as_ref()
        .and_then(|post_success| post_success.merge.as_ref())
    else {
        eprintln!(
            "warning: using daemon post-success merge flags (`--auto-merge`, `--auto-pr`) because workflow pipeline `{}` is missing `post_success.merge`; prefer configuring it in `.ao/workflows.yaml` (or `.ao/state/workflow-config.v2.json`) for deprecation-safe behavior",
            pipeline.id
        );
        return cfg;
    };

    cfg.auto_merge_enabled = merge_cfg.auto_merge;
    cfg.auto_pr_enabled = merge_cfg.create_pr;
    if let Some(target_branch) = Some(merge_cfg.target_branch.trim()).filter(|v| !v.is_empty()) {
        cfg.auto_merge_target_branch = target_branch.to_string();
    }
    cfg
}

async fn record_dispatch_history_entry(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    workflow_id: &str,
    outcome: &str,
) {
    let workflow = hub.workflows().get(workflow_id).await.ok();
    let now = Utc::now();
    let started_at = workflow
        .as_ref()
        .map(|w| w.started_at.to_rfc3339())
        .unwrap_or_else(|| now.to_rfc3339());
    let ended_at = now.to_rfc3339();
    let duration_secs = workflow.as_ref().map(|w| {
        let start = w.started_at;
        (now - start).num_milliseconds() as f64 / 1000.0
    });
    let failed_phase = workflow.as_ref().and_then(|w| w.current_phase.clone());
    let failure_reason = workflow.as_ref().and_then(|w| w.failure_reason.clone());

    let entry = orchestrator_core::DispatchHistoryEntry {
        workflow_id: workflow_id.to_string(),
        started_at,
        ended_at: Some(ended_at),
        duration_secs,
        outcome: outcome.to_string(),
        failed_phase: if outcome != "completed" {
            failed_phase
        } else {
            None
        },
        failure_reason: if outcome != "completed" {
            failure_reason
        } else {
            None
        },
    };

    let Ok(mut task) = hub.tasks().get(task_id).await else {
        return;
    };
    task.dispatch_history.push(entry);
    let max = orchestrator_core::MAX_DISPATCH_HISTORY_ENTRIES;
    if task.dispatch_history.len() > max {
        let drain_count = task.dispatch_history.len() - max;
        task.dispatch_history.drain(..drain_count);
    }
    let _ = hub.tasks().replace(task).await;
}
