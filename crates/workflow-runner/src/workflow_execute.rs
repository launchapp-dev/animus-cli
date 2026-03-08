use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::process::Command;

use orchestrator_config::workflow_config::MergeStrategy;
use orchestrator_core::{
    ensure_workflow_config_compiled, load_workflow_config,
    providers::{BuiltinGitProvider, GitProvider},
    services::ServiceHub,
    FileServiceHub, OrchestratorTask, PhaseDecisionVerdict, TaskStatus, WorkflowRunInput,
    WorkflowStatus, WorkflowSubject,
};

use crate::executor::{
    ensure_execution_cwd, persist_phase_output, run_workflow_phase, PhaseExecuteOverrides,
    PhaseExecutionOutcome,
};

pub enum PhaseEvent<'a> {
    Started {
        phase_id: &'a str,
        phase_index: usize,
        total_phases: usize,
    },
    Decision {
        phase_id: &'a str,
        decision: &'a orchestrator_core::PhaseDecision,
    },
    Completed {
        phase_id: &'a str,
        duration: Duration,
        success: bool,
    },
}

pub struct WorkflowExecuteParams {
    pub project_root: String,
    pub task_id: Option<String>,
    pub requirement_id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub workflow_ref: Option<String>,
    pub model: Option<String>,
    pub tool: Option<String>,
    pub phase_timeout_secs: Option<u64>,
    pub phase_filter: Option<String>,
    pub stream_level: Option<String>,
    pub on_phase_event: Option<Box<dyn Fn(PhaseEvent<'_>) + Send + Sync>>,
    pub hub: Option<Arc<dyn ServiceHub>>,
}

pub struct WorkflowExecuteResult {
    pub success: bool,
    pub workflow_id: String,
    pub workflow_ref: String,
    pub workflow_status: WorkflowStatus,
    pub subject_id: String,
    pub execution_cwd: String,
    pub phases_requested: Vec<String>,
    pub phases_completed: usize,
    pub phases_total: usize,
    pub total_duration: Duration,
    pub phase_results: Vec<Value>,
    pub post_success: Value,
}

pub async fn execute_workflow(params: WorkflowExecuteParams) -> Result<WorkflowExecuteResult> {
    let stream_level = params.stream_level.as_deref().unwrap_or("quiet");
    std::env::set_var("AO_STREAM_PHASE_OUTPUT", stream_level);

    if let Some(timeout) = params.phase_timeout_secs {
        std::env::set_var("AO_PHASE_TIMEOUT_SECS", timeout.to_string());
    }

    let input = resolve_input(&params)?;
    let subject = input.subject().clone();

    let hub: Arc<dyn ServiceHub> = match params.hub {
        Some(h) => h,
        None => Arc::new(
            FileServiceHub::new(&params.project_root)
                .context("failed to create service hub for project")?,
        ),
    };

    let mut task = if let WorkflowSubject::Task { ref id } = subject {
        Some(
            hub.tasks()
                .get(id)
                .await
                .with_context(|| format!("task '{}' not found", id))?,
        )
    } else {
        None
    };

    let subject_id = subject.id().to_string();
    let mut workflow = hub.workflows().run(input).await.or_else(|run_err| {
        if matches!(subject, WorkflowSubject::Custom { .. }) {
            return Err(run_err);
        }
        let all = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(hub.workflows().list())
        })?;
        all.into_iter()
            .find(|w| w.subject.id() == subject_id || w.task_id == subject_id)
            .ok_or_else(|| anyhow!("no workflow found for subject '{}'", subject_id))
    })?;

    let execution_cwd = ensure_execution_cwd(hub.clone(), &params.project_root, task.as_ref())
        .await
        .context("failed to resolve execution cwd")?;

    if let Some(task_id) = task.as_ref().map(|t| t.id.clone()) {
        task = Some(
            hub.tasks()
                .get(&task_id)
                .await
                .with_context(|| format!("task '{}' not found after cwd preparation", task_id))?,
        );
    }

    let phases_to_run: Vec<String> = if let Some(ref phase_filter) = params.phase_filter {
        vec![phase_filter.clone()]
    } else {
        workflow.phases.iter().map(|p| p.phase_id.clone()).collect()
    };

    if phases_to_run.is_empty() {
        return Err(anyhow!("workflow has no phases to execute"));
    }

    if let Err(err) = hub.daemon().start().await {
        eprintln!("warning: failed to auto-start runner for workflow execute: {err}");
    }

    let (subject_id_str, subject_title, subject_description) = match &task {
        Some(t) => (t.id.clone(), t.title.clone(), t.description.clone()),
        None => (
            subject_id.clone(),
            params.title.clone().unwrap_or_else(|| subject_id.clone()),
            params.description.clone().unwrap_or_default(),
        ),
    };
    let task_complexity = task.as_ref().map(|t| t.complexity);

    ensure_workflow_config_compiled(Path::new(&params.project_root))?;
    let workflow_config = load_workflow_config(Path::new(&params.project_root))?;
    let workflow_ref = workflow
        .workflow_ref
        .clone()
        .unwrap_or_else(|| workflow_config.default_workflow_ref.clone());
    let mut rework_context: Option<String> = None;
    let mut results = Vec::new();
    let workflow_start = Instant::now();

    let emit = |event: PhaseEvent<'_>| {
        if let Some(ref cb) = params.on_phase_event {
            cb(event);
        }
    };

    if let Some(phase_filter) = params.phase_filter.clone() {
        let phase_attempt = workflow
            .phases
            .iter()
            .find(|p| p.phase_id.eq_ignore_ascii_case(&phase_filter))
            .map(|p| p.attempt)
            .unwrap_or(0);

        emit(PhaseEvent::Started {
            phase_id: &phase_filter,
            phase_index: 0,
            total_phases: 1,
        });
        let phase_start = Instant::now();

        let phase_overrides = PhaseExecuteOverrides {
            tool: params.tool.clone(),
            model: params.model.clone(),
            rework_context: rework_context.take(),
        };
        let run_result = run_workflow_phase(
            &params.project_root,
            &execution_cwd,
            &workflow.id,
            workflow_ref.as_str(),
            &subject_id_str,
            &subject_title,
            &subject_description,
            task_complexity,
            &phase_filter,
            phase_attempt,
            Some(&phase_overrides),
            None,
        )
        .await;

        let phase_elapsed = phase_start.elapsed();

        match run_result {
            Ok(result) => {
                if let PhaseExecutionOutcome::Completed {
                    phase_decision: Some(ref decision),
                    ..
                } = &result.outcome
                {
                    emit(PhaseEvent::Decision {
                        phase_id: &phase_filter,
                        decision,
                    });
                }

                let phase_status = phase_result_status(&result.outcome);
                let _ = persist_phase_output(
                    &params.project_root,
                    &workflow.id,
                    &phase_filter,
                    &result.outcome,
                );
                emit(PhaseEvent::Completed {
                    phase_id: &phase_filter,
                    duration: phase_elapsed,
                    success: phase_status != "failed",
                });
                results.push(serde_json::json!({
                    "phase_id": phase_filter,
                    "status": phase_status,
                    "duration_secs": phase_elapsed.as_secs(),
                    "outcome": result.outcome,
                    "metadata": result.metadata,
                }));

                let total_duration = workflow_start.elapsed();
                return Ok(WorkflowExecuteResult {
                    success: phase_status != "failed",
                    workflow_id: workflow.id.clone(),
                    workflow_ref,
                    workflow_status: workflow.status,
                    subject_id: subject_id_str,
                    execution_cwd,
                    phases_requested: vec![phase_filter],
                    phases_completed: usize::from(phase_status == "completed"),
                    phases_total: 1,
                    total_duration,
                    phase_results: results,
                    post_success: serde_json::json!({
                        "status": "skipped",
                        "reason": "post-success actions are not run for single-phase execution",
                    }),
                });
            }
            Err(err) => {
                emit(PhaseEvent::Completed {
                    phase_id: &phase_filter,
                    duration: phase_elapsed,
                    success: false,
                });
                results.push(serde_json::json!({
                    "phase_id": phase_filter,
                    "status": "failed",
                    "duration_secs": phase_elapsed.as_secs(),
                    "error": err.to_string(),
                }));
                let total_duration = workflow_start.elapsed();
                return Ok(WorkflowExecuteResult {
                    success: false,
                    workflow_id: workflow.id.clone(),
                    workflow_ref,
                    workflow_status: workflow.status,
                    subject_id: subject_id_str,
                    execution_cwd,
                    phases_requested: vec![phase_filter],
                    phases_completed: 0,
                    phases_total: 1,
                    total_duration,
                    phase_results: results,
                    post_success: serde_json::json!({
                        "status": "skipped",
                        "reason": "post-success actions are not run for single-phase execution",
                    }),
                });
            }
        }
    }

    let mut phases_to_run: Vec<String> = workflow.phases.iter().map(|p| p.phase_id.clone()).collect();
    if phases_to_run.is_empty() {
        return Err(anyhow!("workflow has no phases to execute"));
    }

    let mut phase_idx: usize = workflow.current_phase_index;
    let mut reported_workflow_status = workflow.status;
    while phase_idx < phases_to_run.len() && !is_terminal_workflow_status(workflow.status) {
        let phase_id = phases_to_run[phase_idx].clone();
        let phase_attempt = workflow
            .phases
            .iter()
            .find(|p| p.phase_id == phase_id)
            .map(|p| p.attempt)
            .unwrap_or(0);

        emit(PhaseEvent::Started {
            phase_id: &phase_id,
            phase_index: phase_idx,
            total_phases: phases_to_run.len(),
        });
        let phase_start = Instant::now();

        let phase_overrides = PhaseExecuteOverrides {
            tool: params.tool.clone(),
            model: params.model.clone(),
            rework_context: rework_context.take(),
        };
        let run_result = run_workflow_phase(
            &params.project_root,
            &execution_cwd,
            &workflow.id,
            workflow_ref.as_str(),
            &subject_id_str,
            &subject_title,
            &subject_description,
            task_complexity,
            &phase_id,
            phase_attempt,
            Some(&phase_overrides),
            None,
        )
        .await;

        let phase_elapsed = phase_start.elapsed();

        match run_result {
            Ok(result) => {
                if let PhaseExecutionOutcome::Completed {
                    phase_decision: Some(ref decision),
                    ..
                } = &result.outcome
                {
                    emit(PhaseEvent::Decision {
                        phase_id: &phase_id,
                        decision,
                    });
                }

                let _ = persist_phase_output(
                    &params.project_root,
                    &workflow.id,
                    &phase_id,
                    &result.outcome,
                );

                match &result.outcome {
                    PhaseExecutionOutcome::Completed {
                        phase_decision: Some(decision),
                        ..
                    } if decision.verdict == PhaseDecisionVerdict::Skip => {
                        let close_reason = decision.reason.trim().to_lowercase();
                        let target_status = if close_reason.contains("already_done") {
                            TaskStatus::Done
                        } else {
                            TaskStatus::Cancelled
                        };

                        if let Ok(mut t) = hub.tasks().get(&subject_id_str).await {
                            t.resolution = Some(decision.reason.clone());
                            if target_status == TaskStatus::Cancelled {
                                t.cancelled = true;
                            }
                            t.status = target_status;
                            t.metadata.updated_by = "workflow:skip".to_string();
                            let _ = hub.tasks().replace(t).await;
                        }

                        workflow = hub.workflows().cancel(&workflow.id).await?;
                        reported_workflow_status = if target_status == TaskStatus::Done {
                            WorkflowStatus::Completed
                        } else {
                            workflow.status
                        };
                        emit(PhaseEvent::Completed {
                            phase_id: &phase_id,
                            duration: phase_elapsed,
                            success: true,
                        });
                        results.push(serde_json::json!({
                            "phase_id": phase_id,
                            "status": "closed",
                            "close_reason": decision.reason,
                            "task_status": format!("{:?}", target_status).to_lowercase(),
                            "duration_secs": phase_elapsed.as_secs(),
                            "outcome": result.outcome,
                            "metadata": result.metadata,
                        }));
                        break;
                    }
                    PhaseExecutionOutcome::Completed {
                        phase_decision,
                        ..
                    } => {
                        let decision = phase_decision.clone();
                        let updated = hub
                            .workflows()
                            .complete_current_phase_with_decision(&workflow.id, decision)
                            .await?;
                        let next_status = updated.status;
                        let next_phase_index = updated.current_phase_index;
                        let next_phase_id = updated
                            .current_phase
                            .clone()
                            .or_else(|| {
                                updated
                                    .phases
                                    .get(updated.current_phase_index)
                                    .map(|phase| phase.phase_id.clone())
                            });
                        let maybe_context = phase_rework_context(&result.outcome);
                        workflow = updated;
                        reported_workflow_status = next_status;
                        phases_to_run = workflow
                            .phases
                            .iter()
                            .map(|phase| phase.phase_id.clone())
                            .collect();

                        let status = phase_result_status(&result.outcome).to_string();
                        let next_success = !matches!(
                            next_status,
                            WorkflowStatus::Failed | WorkflowStatus::Escalated
                        );
                        emit(PhaseEvent::Completed {
                            phase_id: &phase_id,
                            duration: phase_elapsed,
                            success: next_success,
                        });
                        let mut result_value = serde_json::json!({
                            "phase_id": phase_id,
                            "status": status,
                            "duration_secs": phase_elapsed.as_secs(),
                            "workflow_status": format!("{:?}", next_status).to_ascii_lowercase(),
                            "outcome": result.outcome,
                            "metadata": result.metadata,
                        });
                        if let Some(next_phase_id) = next_phase_id {
                            result_value["next_phase_id"] = serde_json::json!(next_phase_id);
                        }
                        results.push(result_value);

                        if matches!(
                            workflow.status,
                            WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled
                        ) {
                            break;
                        }

                        rework_context = maybe_context;
                        phase_idx = next_phase_index;
                        continue;
                    }
                    PhaseExecutionOutcome::NeedsResearch { reason } => {
                        workflow = hub
                            .workflows()
                            .request_research(&workflow.id, reason.clone())
                            .await?;
                        reported_workflow_status = workflow.status;
                        phases_to_run = workflow
                            .phases
                            .iter()
                            .map(|phase| phase.phase_id.clone())
                            .collect();
                        emit(PhaseEvent::Completed {
                            phase_id: &phase_id,
                            duration: phase_elapsed,
                            success: true,
                        });
                        results.push(serde_json::json!({
                            "phase_id": phase_id,
                            "status": "research_requested",
                            "duration_secs": phase_elapsed.as_secs(),
                            "workflow_status": format!("{:?}", workflow.status).to_ascii_lowercase(),
                            "outcome": result.outcome,
                            "metadata": result.metadata,
                        }));
                        phase_idx = workflow.current_phase_index;
                        continue;
                    }
                    PhaseExecutionOutcome::ManualPending { .. } => {
                        reported_workflow_status = workflow.status;
                        emit(PhaseEvent::Completed {
                            phase_id: &phase_id,
                            duration: phase_elapsed,
                            success: true,
                        });
                        results.push(serde_json::json!({
                            "phase_id": phase_id,
                            "status": "manual_pending",
                            "duration_secs": phase_elapsed.as_secs(),
                            "workflow_status": format!("{:?}", workflow.status).to_ascii_lowercase(),
                            "outcome": result.outcome,
                            "metadata": result.metadata,
                        }));
                        break;
                    }
                }
            }
            Err(err) => {
                workflow = hub
                    .workflows()
                    .fail_current_phase(&workflow.id, err.to_string())
                    .await?;
                reported_workflow_status = workflow.status;
                emit(PhaseEvent::Completed {
                    phase_id: &phase_id,
                    duration: phase_elapsed,
                    success: false,
                });
                results.push(serde_json::json!({
                    "phase_id": phase_id,
                    "status": "failed",
                    "duration_secs": phase_elapsed.as_secs(),
                    "workflow_status": format!("{:?}", workflow.status).to_ascii_lowercase(),
                    "error": err.to_string(),
                }));
                break;
            }
        }
    }

    let total_duration = workflow_start.elapsed();
    let mut post_success = serde_json::json!({
        "status": "skipped",
        "reason": "workflow did not complete all phases",
    });
    if workflow.status == WorkflowStatus::Completed {
        post_success = if let Some(ref t) = task {
            execute_post_success_actions(
                &params.project_root,
                t,
                &workflow,
                &workflow_config,
                &execution_cwd,
            )
            .await
        } else {
            serde_json::json!({
                "status": "skipped",
                "reason": "post-success actions require a task subject",
            })
        };

        match post_success["status"].as_str() {
            Some("conflict") => {
                let reason = post_success_failure_reason(&post_success)
                    .unwrap_or_else(|| "post-success merge conflict".to_string());
                workflow = hub.workflows().mark_merge_conflict(&workflow.id, reason).await?;
                reported_workflow_status = workflow.status;
            }
            Some("failed") => {
                let reason = post_success_failure_reason(&post_success)
                    .unwrap_or_else(|| "post-success action failed".to_string());
                workflow = hub.workflows().mark_completed_failed(&workflow.id, reason).await?;
                reported_workflow_status = workflow.status;
            }
            _ => {}
        }
    }

    Ok(WorkflowExecuteResult {
        success: workflow_exit_success(reported_workflow_status),
        workflow_id: workflow.id.clone(),
        workflow_ref,
        workflow_status: reported_workflow_status,
        subject_id: subject_id_str,
        execution_cwd,
        phases_requested: phases_to_run.clone(),
        phases_completed: workflow
            .phases
            .iter()
            .filter(|phase| phase.completed_at.is_some())
            .count(),
        phases_total: phases_to_run.len(),
        total_duration,
        phase_results: results,
        post_success,
    })
}

fn resolve_input(params: &WorkflowExecuteParams) -> Result<WorkflowRunInput> {
    let workflow_ref = params.workflow_ref.clone();
    match (&params.task_id, &params.requirement_id, &params.title) {
        (Some(task_id), _, _) => Ok(WorkflowRunInput::for_task(task_id.clone(), workflow_ref)),
        (None, Some(req_id), _) => Ok(WorkflowRunInput::for_requirement(
            req_id.clone(),
            workflow_ref,
        )),
        (None, None, Some(title)) => Ok(WorkflowRunInput::for_custom(
            title.clone(),
            params.description.clone().unwrap_or_default(),
            workflow_ref,
        )),
        _ => Err(anyhow!(
            "one of --task-id, --requirement-id, or --title must be provided"
        )),
    }
}

fn phase_rework_context(outcome: &PhaseExecutionOutcome) -> Option<String> {
    match outcome {
        PhaseExecutionOutcome::Completed {
            phase_decision: Some(decision),
            ..
        } if matches!(decision.verdict, PhaseDecisionVerdict::Rework) => {
            Some(decision.reason.clone())
        }
        _ => None,
    }
}

fn is_terminal_workflow_status(status: WorkflowStatus) -> bool {
    matches!(
        status,
        WorkflowStatus::Completed
            | WorkflowStatus::Failed
            | WorkflowStatus::Escalated
            | WorkflowStatus::Cancelled
    )
}

fn workflow_exit_success(status: WorkflowStatus) -> bool {
    !matches!(status, WorkflowStatus::Failed | WorkflowStatus::Escalated)
}

fn phase_result_status(outcome: &PhaseExecutionOutcome) -> &'static str {
    match outcome {
        PhaseExecutionOutcome::Completed {
            phase_decision: Some(decision),
            ..
        } => match decision.verdict {
            PhaseDecisionVerdict::Advance | PhaseDecisionVerdict::Unknown => "completed",
            PhaseDecisionVerdict::Rework => "rework",
            PhaseDecisionVerdict::Fail => "failed",
            PhaseDecisionVerdict::Skip => "closed",
        },
        PhaseExecutionOutcome::Completed {
            phase_decision: None,
            ..
        } => "completed",
        PhaseExecutionOutcome::NeedsResearch { .. } => "research_requested",
        PhaseExecutionOutcome::ManualPending { .. } => "manual_pending",
    }
}

fn post_success_failure_reason(post_success: &Value) -> Option<String> {
    post_success
        .get("error")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            post_success
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            post_success
                .get("actions")
                .and_then(Value::as_object)
                .and_then(|actions| {
                    actions.values().find_map(|action| {
                        if action.get("status").and_then(Value::as_str) == Some("failed")
                            || action.get("status").and_then(Value::as_str) == Some("conflict")
                        {
                            action
                                .get("error")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        } else {
                            None
                        }
                    })
                })
        })
}

async fn execute_post_success_actions(
    project_root: &str,
    task: &OrchestratorTask,
    workflow: &orchestrator_core::OrchestratorWorkflow,
    workflow_config: &orchestrator_core::WorkflowConfig,
    execution_cwd: &str,
) -> Value {
    let workflow_ref = workflow
        .workflow_ref
        .as_deref()
        .unwrap_or(workflow_config.default_workflow_ref.as_str());
    let workflow_def = workflow_config
        .workflows
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(workflow_ref))
        .or_else(|| {
            workflow_config
                .workflows
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case("standard"))
        })
        .or_else(|| {
            workflow_config.workflows.iter().find(|p| {
                p.id.eq_ignore_ascii_case(&workflow_config.default_workflow_ref)
            })
        })
        .cloned();

    let Some(workflow_def) = workflow_def else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "workflow configuration not found",
        });
    };

    let Some(merge_cfg) = workflow_def
        .post_success
        .and_then(|post_success| post_success.merge)
    else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "post_success.merge not configured",
            "workflow_ref": workflow_def.id,
        });
    };

    let Some(source_branch) = resolve_source_branch(task, execution_cwd).await else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "unable to resolve source branch",
            "workflow_ref": workflow_def.id,
        });
    };

    let git_provider = Arc::new(BuiltinGitProvider::new(project_root));
    let target_branch = merge_cfg.target_branch.clone();

    let mut action_result = serde_json::json!({
        "status": "skipped",
        "workflow_ref": workflow_def.id,
        "target_branch": target_branch,
        "strategy": merge_strategy_name(&merge_cfg.strategy),
        "create_pr": merge_cfg.create_pr,
        "auto_merge": merge_cfg.auto_merge,
        "cleanup_worktree": merge_cfg.cleanup_worktree,
        "actions": {
            "push": { "status": "skipped" },
            "create_pr": { "status": "skipped" },
            "merge": { "status": "skipped" },
            "cleanup_worktree": { "status": "skipped" },
        },
    });

    if merge_cfg.create_pr {
        if let Some(push_action) =
            perform_push_with_fallback(&*git_provider, execution_cwd, "origin", &source_branch)
                .await
        {
            action_result["actions"]["push"] = push_action;
        }

        let title = if task.title.trim().is_empty() {
            format!("[{}] Automated update", task.id)
        } else {
            format!("[{}] {}", task.id, task.title.trim())
        };
        let body = if task.description.trim().is_empty() {
            format!("Automated update for task {}.", task.id)
        } else {
            format!(
                "Automated update for task {}.\n\n{}",
                task.id,
                task.description.trim()
            )
        };
        action_result["actions"]["create_pr"] = create_pull_request_via_gh(
            task,
            project_root,
            &target_branch,
            &source_branch,
            &title,
            &body,
        )
        .await;
        let pr_status = action_result["actions"]["create_pr"]["status"].clone();
        action_result["status"] = pr_status;
        action_result["source_branch"] = serde_json::json!(source_branch);
        if merge_cfg.cleanup_worktree {
            action_result["actions"]["cleanup_worktree"] =
                cleanup_worktree_with_fallback(&*git_provider, project_root, task).await;
        }
        return action_result;
    }

    if merge_cfg.auto_merge {
        action_result["actions"]["merge"] = perform_auto_merge_with_git(
            project_root,
            &source_branch,
            &target_branch,
            &merge_cfg.strategy,
        )
        .await;
        action_result["status"] = action_result["actions"]["merge"]["status"].clone();
    }

    action_result["source_branch"] = serde_json::json!(source_branch);
    if merge_cfg.cleanup_worktree {
        action_result["actions"]["cleanup_worktree"] =
            cleanup_worktree_with_fallback(&*git_provider, project_root, task).await;
        if action_result["actions"]["cleanup_worktree"]["status"] == "completed"
            && action_result["status"] == "skipped"
        {
            action_result["status"] = serde_json::json!("completed");
        }
    }
    action_result
}

async fn resolve_source_branch(task: &OrchestratorTask, execution_cwd: &str) -> Option<String> {
    if let Some(branch) = task
        .branch_name
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Some(branch.to_string());
    }

    if execution_cwd.is_empty() || !Path::new(execution_cwd).exists() {
        return None;
    }

    let output = run_git_output("git", execution_cwd, &["branch", "--show-current"])
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn merge_strategy_name(strategy: &MergeStrategy) -> &'static str {
    match strategy {
        MergeStrategy::Squash => "squash",
        MergeStrategy::Merge => "merge",
        MergeStrategy::Rebase => "rebase",
    }
}

fn command_summary(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

fn looks_like_merge_conflict(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("merge conflict")
        || text.contains("conflict")
        || text.contains("automatic merge failed")
        || text.contains("merge blocked")
}

async fn run_git_output(program: &str, cwd: &str, args: &[&str]) -> Result<std::process::Output> {
    Command::new(program)
        .current_dir(cwd)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run command {program} in {cwd}"))
}

async fn perform_push_with_fallback(
    git_provider: &dyn GitProvider,
    execution_cwd: &str,
    remote: &str,
    branch: &str,
) -> Option<Value> {
    match git_provider
        .push_branch(execution_cwd, remote, branch)
        .await
    {
        Ok(_) => Some(serde_json::json!({
            "status": "completed",
            "method": "git-provider",
            "branch": branch,
            "remote": remote,
        })),
        Err(provider_error) => {
            let direct = run_git_output("git", execution_cwd, &["push", remote, branch]).await;
            match direct {
                Ok(output) if output.status.success() => Some(serde_json::json!({
                    "status": "completed",
                    "method": "git-direct",
                    "branch": branch,
                    "remote": remote,
                    "provider_error": provider_error.to_string(),
                })),
                Ok(output) => Some(serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "branch": branch,
                    "remote": remote,
                    "error": command_summary(&output),
                    "provider_error": provider_error.to_string(),
                })),
                Err(command_error) => Some(serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "branch": branch,
                    "remote": remote,
                    "error": command_error.to_string(),
                    "provider_error": provider_error.to_string(),
                })),
            }
        }
    }
}

async fn create_pull_request_via_gh(
    task: &OrchestratorTask,
    execution_cwd: &str,
    target_branch: &str,
    source_branch: &str,
    title: &str,
    body: &str,
) -> Value {
    let args = [
        "pr",
        "create",
        "--base",
        target_branch,
        "--head",
        source_branch,
        "--title",
        title,
        "--body",
        body,
    ];
    match run_git_output("gh", execution_cwd, &args).await {
        Ok(output) if output.status.success() => {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            serde_json::json!({
                "status": "completed",
                "method": "gh",
                "task_id": task.id,
                "source_branch": source_branch,
                "target_branch": target_branch,
                "url": if url.is_empty() { None::<String> } else { Some(url) },
            })
        }
        Ok(output) => {
            let message = command_summary(&output);
            if message.to_ascii_lowercase().contains("already exists")
                || message.to_ascii_lowercase().contains("already open")
            {
                serde_json::json!({
                    "status": "completed",
                    "method": "gh",
                    "task_id": task.id,
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "error": message,
                })
            } else {
                serde_json::json!({
                    "status": "failed",
                    "method": "gh",
                    "task_id": task.id,
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "error": message,
                })
            }
        }
        Err(error) => serde_json::json!({
            "status": "failed",
            "method": "gh",
            "task_id": task.id,
            "source_branch": source_branch,
            "target_branch": target_branch,
            "error": error.to_string(),
        }),
    }
}

async fn checkout_target_branch(execution_cwd: &str, target_branch: &str) -> Result<()> {
    let checkout_output = run_git_output("git", execution_cwd, &["checkout", target_branch]).await;
    match checkout_output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let primary_error = command_summary(&output);
            let fallback_ref = format!("origin/{target_branch}");
            let fallback = run_git_output(
                "git",
                execution_cwd,
                &["checkout", "-b", target_branch, fallback_ref.as_str()],
            )
            .await;
            match fallback {
                Ok(fb_output) if fb_output.status.success() => Ok(()),
                Ok(fb_output) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {primary_error}; fallback failed: {}",
                    command_summary(&fb_output),
                ),
                Err(fb_err) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {primary_error}; fallback failed: {fb_err}",
                ),
            }
        }
        Err(error) => {
            let fallback_ref = format!("origin/{target_branch}");
            let fallback = run_git_output(
                "git",
                execution_cwd,
                &["checkout", "-b", target_branch, fallback_ref.as_str()],
            )
            .await;
            match fallback {
                Ok(fb_output) if fb_output.status.success() => Ok(()),
                Ok(fb_output) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {error}; fallback failed: {}",
                    command_summary(&fb_output),
                ),
                Err(fb_err) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {error}; fallback failed: {fb_err}",
                ),
            }
        }
    }
}

async fn perform_rebase_strategy(
    execution_cwd: &str,
    source_branch: &str,
    target_branch: &str,
) -> Value {
    let rebase_output = run_git_output(
        "git",
        execution_cwd,
        &["rebase", target_branch, source_branch],
    )
    .await;
    match rebase_output {
        Ok(output) if output.status.success() => {
            let ff_merge =
                run_git_output("git", execution_cwd, &["merge", "--ff-only", source_branch]).await;
            match ff_merge {
                Ok(merge_out) if merge_out.status.success() => serde_json::json!({
                    "status": "completed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": "rebase",
                }),
                Ok(merge_out) => serde_json::json!({
                    "status": "failed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": "rebase",
                    "error": format!("rebase succeeded but ff-merge failed: {}", command_summary(&merge_out)),
                }),
                Err(err) => serde_json::json!({
                    "status": "failed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": "rebase",
                    "error": format!("rebase succeeded but ff-merge failed: {err}"),
                }),
            }
        }
        Ok(output) => {
            let _ = run_git_output("git", execution_cwd, &["rebase", "--abort"]).await;
            let summary = command_summary(&output);
            let status = if looks_like_merge_conflict(&summary) {
                "conflict"
            } else {
                "failed"
            };
            serde_json::json!({
                "status": status,
                "method": "git",
                "source_branch": source_branch,
                "target_branch": target_branch,
                "strategy": "rebase",
                "error": summary,
            })
        }
        Err(error) => serde_json::json!({
            "status": "failed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": "rebase",
            "error": error.to_string(),
        }),
    }
}

async fn perform_auto_merge_with_git(
    execution_cwd: &str,
    source_branch: &str,
    target_branch: &str,
    strategy: &MergeStrategy,
) -> Value {
    if let Err(error) = checkout_target_branch(execution_cwd, target_branch).await {
        return serde_json::json!({
            "status": "failed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": merge_strategy_name(strategy),
            "error": error.to_string(),
        });
    }

    if matches!(strategy, MergeStrategy::Rebase) {
        return perform_rebase_strategy(execution_cwd, source_branch, target_branch).await;
    }

    let merge_args = {
        let mut args: Vec<String> = vec!["merge".to_string()];
        match strategy {
            MergeStrategy::Squash => args.push("--squash".to_string()),
            MergeStrategy::Merge => args.push("--no-ff".to_string()),
            MergeStrategy::Rebase => unreachable!(),
        };
        args.push("--no-edit".to_string());
        args.push(source_branch.to_string());
        args
    };
    let arg_refs: Vec<&str> = merge_args.iter().map(String::as_str).collect();
    let output = run_git_output("git", execution_cwd, &arg_refs).await;
    match output {
        Ok(output) if output.status.success() => serde_json::json!({
            "status": "completed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": merge_strategy_name(strategy),
        }),
        Ok(output) => {
            let summary = command_summary(&output);
            let status = if looks_like_merge_conflict(&summary) {
                "conflict"
            } else {
                "failed"
            };
            serde_json::json!({
                "status": status,
                "method": "git",
                "source_branch": source_branch,
                "target_branch": target_branch,
                "strategy": merge_strategy_name(strategy),
                "error": summary,
            })
        }
        Err(error) => serde_json::json!({
            "status": "failed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": merge_strategy_name(strategy),
            "error": error.to_string(),
        }),
    }
}

async fn cleanup_worktree_with_fallback(
    git_provider: &dyn GitProvider,
    project_root: &str,
    task: &OrchestratorTask,
) -> Value {
    let Some(worktree_path) = task
        .worktree_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
    else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "worktree path not available",
        });
    };

    match git_provider
        .remove_worktree(project_root, worktree_path)
        .await
    {
        Ok(()) => serde_json::json!({
            "status": "completed",
            "method": "git-provider",
            "worktree_path": worktree_path,
        }),
        Err(provider_error) => {
            let output = run_git_output(
                "git",
                project_root,
                &["worktree", "remove", worktree_path, "--force"],
            )
            .await;
            match output {
                Ok(output) if output.status.success() => serde_json::json!({
                    "status": "completed",
                    "method": "git-direct",
                    "worktree_path": worktree_path,
                }),
                Ok(output) => serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "worktree_path": worktree_path,
                    "error": command_summary(&output),
                    "provider_error": provider_error.to_string(),
                }),
                Err(error) => serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "worktree_path": worktree_path,
                    "error": error.to_string(),
                    "provider_error": provider_error.to_string(),
                }),
            }
        }
    }
}
