use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::{
    load_agent_runtime_config_or_default, project_task_status, project_task_terminal_workflow_status,
    project_task_workflow_pause_cleared, project_task_workflow_paused, services::ServiceHub, OrchestratorTask,
    OrchestratorWorkflow, PhaseExecutionMode, PhaseManualDefinition, TaskStatus, WorkflowStatus,
};

#[derive(Debug, Clone)]
pub enum WorkflowEvent {
    /// Pause a workflow. `reason_detail` (when present) is appended to the
    /// task's pause annotation as a human cause (e.g. a budget breach
    /// summary), so `animus subject get` can show why it stalled.
    Pause {
        workflow_id: String,
        reason_detail: Option<String>,
    },
    Resume {
        workflow_id: String,
        feedback: Option<String>,
    },
    Cancel {
        workflow_id: String,
    },
    ApproveManualPhase {
        workflow_id: String,
        phase_id: String,
        note: Option<String>,
    },
    RejectManualPhase {
        workflow_id: String,
        phase_id: String,
        note: Option<String>,
    },
    StaleReset {
        task_id: String,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowEventOutcome {
    pub workflow: Option<OrchestratorWorkflow>,
    pub task: Option<OrchestratorTask>,
    pub requires_continuation: bool,
}

pub async fn dispatch_workflow_event(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    event: WorkflowEvent,
) -> Result<WorkflowEventOutcome> {
    match event {
        WorkflowEvent::Pause { workflow_id, reason_detail } => {
            let workflow = hub.workflows().pause(&workflow_id).await?;
            // Annotate the task so `animus subject get` explains the stall.
            // Best-effort, and only when the pause actually landed: a no-op
            // pause (terminal workflow, rejected transition) must not write
            // a misleading marker or clobber a real blocked_reason.
            let task = if let Some(task_id) =
                (workflow.status == WorkflowStatus::Paused).then(|| workflow_task_id(&workflow)).flatten()
            {
                let _ =
                    project_task_workflow_paused(hub.clone(), &task_id, &workflow_id, reason_detail.as_deref()).await;
                hub.tasks().get(&task_id).await.ok()
            } else {
                None
            };
            Ok(WorkflowEventOutcome { workflow: Some(workflow), task, ..WorkflowEventOutcome::default() })
        }
        WorkflowEvent::Resume { workflow_id, feedback } => {
            if let Some(ref feedback_text) = feedback {
                if !feedback_text.trim().is_empty() {
                    hub.workflows().record_feedback(&workflow_id, feedback_text.clone()).await.ok();
                }
            }
            let workflow = hub.workflows().resume(&workflow_id).await?;
            // Clear the matching "paused by workflow <id>" annotation; a
            // genuine failure-projected blocked_reason is left alone.
            let task = if let Some(task_id) = workflow_task_id(&workflow) {
                let _ = project_task_workflow_pause_cleared(hub.clone(), &task_id, &workflow_id).await;
                hub.tasks().get(&task_id).await.ok()
            } else {
                None
            };
            Ok(WorkflowEventOutcome { workflow: Some(workflow), task, ..WorkflowEventOutcome::default() })
        }
        WorkflowEvent::Cancel { workflow_id } => {
            let workflow = hub.workflows().cancel(&workflow_id).await?;
            // Sync the task the same way the daemon-side projections do
            // (`project_task_terminal_workflow_status` / the execution-fact
            // path): a cancelled workflow projects the task to Cancelled
            // unless it is already terminal. This closes the gap where a
            // CLI-local `workflow cancel` left the task in-progress forever.
            // Gated on the cancel actually landing: a no-op cancel (e.g. the
            // workflow already completed) must not touch the task.
            let task = if let Some(task_id) =
                (workflow.status == WorkflowStatus::Cancelled).then(|| workflow_task_id(&workflow)).flatten()
            {
                project_task_terminal_workflow_status(hub.clone(), &task_id, WorkflowStatus::Cancelled, None).await;
                hub.tasks().get(&task_id).await.ok()
            } else {
                None
            };
            Ok(WorkflowEventOutcome { workflow: Some(workflow), task, ..WorkflowEventOutcome::default() })
        }
        WorkflowEvent::ApproveManualPhase { workflow_id, phase_id, note } => {
            let manual = ensure_manual_phase(project_root, &phase_id)?;
            let note = note.unwrap_or_default();
            if manual.approval_note_required && note.trim().is_empty() {
                return Err(anyhow!("phase '{}' requires a non-empty approval note", phase_id));
            }

            let workflow = hub.workflows().get(&workflow_id).await?;
            let current_phase =
                current_phase_id(&workflow).ok_or_else(|| anyhow!("workflow '{}' has no active phase", workflow_id))?;
            if !current_phase.eq_ignore_ascii_case(&phase_id) {
                return Err(anyhow!(
                    "workflow '{}' active phase is '{}' (requested '{}')",
                    workflow_id,
                    current_phase,
                    phase_id
                ));
            }

            match workflow.status {
                WorkflowStatus::Paused => {
                    let _ = hub.workflows().resume(&workflow_id).await?;
                    // This is a resume path too: clear any "paused by
                    // workflow <id>" annotation left on the task by a pause.
                    if let Some(task_id) = workflow_task_id(&workflow) {
                        let _ = project_task_workflow_pause_cleared(hub.clone(), &task_id, &workflow_id).await;
                    }
                }
                WorkflowStatus::Running => {}
                status => {
                    return Err(anyhow!(
                        "workflow '{}' is not waiting for manual approval (status: {})",
                        workflow_id,
                        format!("{status:?}").to_ascii_lowercase()
                    ));
                }
            };

            let updated = hub.workflows().complete_current_phase(&workflow_id).await?;
            Ok(WorkflowEventOutcome {
                requires_continuation: updated.status == WorkflowStatus::Running,
                workflow: Some(updated),
                ..WorkflowEventOutcome::default()
            })
        }
        WorkflowEvent::RejectManualPhase { workflow_id, phase_id, note } => {
            let manual = ensure_manual_phase(project_root, &phase_id)?;
            let note = note.unwrap_or_default();
            if manual.approval_note_required && note.trim().is_empty() {
                return Err(anyhow!("phase '{}' requires a non-empty rejection note", phase_id));
            }

            let workflow = hub.workflows().get(&workflow_id).await?;
            let current_phase =
                current_phase_id(&workflow).ok_or_else(|| anyhow!("workflow '{}' has no active phase", workflow_id))?;
            if !current_phase.eq_ignore_ascii_case(&phase_id) {
                return Err(anyhow!(
                    "workflow '{}' active phase is '{}' (requested '{}')",
                    workflow_id,
                    current_phase,
                    phase_id
                ));
            }

            match workflow.status {
                WorkflowStatus::Paused => {
                    let _ = hub.workflows().resume(&workflow_id).await?;
                    // This is a resume path too: clear any "paused by
                    // workflow <id>" annotation left on the task by a pause.
                    if let Some(task_id) = workflow_task_id(&workflow) {
                        let _ = project_task_workflow_pause_cleared(hub.clone(), &task_id, &workflow_id).await;
                    }
                }
                WorkflowStatus::Running => {}
                status => {
                    return Err(anyhow!(
                        "workflow '{}' is not waiting for manual approval (status: {})",
                        workflow_id,
                        format!("{status:?}").to_ascii_lowercase()
                    ));
                }
            };

            let failure_reason = if note.trim().is_empty() { "manual approval rejected".to_string() } else { note };
            let updated = hub.workflows().fail_current_phase(&workflow_id, failure_reason).await?;
            Ok(WorkflowEventOutcome { workflow: Some(updated), ..WorkflowEventOutcome::default() })
        }
        WorkflowEvent::StaleReset { task_id, reason } => {
            project_task_status(hub.clone(), &task_id, TaskStatus::Ready).await?;
            let task = hub.tasks().get(&task_id).await.ok();
            let _ = reason;
            Ok(WorkflowEventOutcome { task, ..WorkflowEventOutcome::default() })
        }
    }
}

/// Resolve the task id a workflow is bound to, preferring the typed subject
/// ref and falling back to the legacy `task_id` field (older records carry
/// the default `SubjectRef::task("")` with the real id in `task_id`).
/// Returns `None` for non-task subjects (requirements, custom).
pub fn workflow_task_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    match workflow.subject.as_ref().and_then(|s| s.task_id()) {
        Some(id) if !id.trim().is_empty() => Some(id.to_string()),
        Some(_) => {
            let legacy = workflow.task_id.trim();
            (!legacy.is_empty()).then(|| legacy.to_string())
        }
        None => None,
    }
}

fn current_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
}

fn ensure_manual_phase(project_root: &str, phase_id: &str) -> Result<PhaseManualDefinition> {
    let runtime = load_agent_runtime_config_or_default(Path::new(project_root));
    let definition =
        runtime.phase_execution(phase_id).ok_or_else(|| anyhow!("phase '{}' is not configured", phase_id))?;
    if !matches!(definition.mode, PhaseExecutionMode::Manual) {
        return Err(anyhow!("phase '{}' is not in manual mode", phase_id));
    }
    definition.manual.clone().ok_or_else(|| anyhow!("phase '{}' missing manual configuration", phase_id))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::{dispatch_workflow_event, WorkflowEvent};
    use crate::{
        services::ServiceHub, InMemoryServiceHub, OrchestratorTask, Priority, ResourceRequirements, Scope,
        TaskMetadata, TaskStatus, TaskType, WorkflowMetadata, WorkflowRunInput, WorkflowStatus,
        WORKFLOW_PAUSED_REASON_PREFIX,
    };

    async fn upsert_task(hub: &Arc<InMemoryServiceHub>, id: &str, status: TaskStatus) -> OrchestratorTask {
        let now = Utc::now();
        let task = OrchestratorTask {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: "workflow event task sync".to_string(),
            task_type: TaskType::Feature,
            status,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Priority::Medium,
            risk: crate::RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: crate::Complexity::default(),
            impact_area: Vec::new(),
            assignee: crate::Assignee::Unassigned,
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: Vec::new(),
            workflow_metadata: WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: TaskMetadata {
                created_at: now,
                updated_at: now,
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at: None,
                status_changed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resolution: None,
            resource_requirements: ResourceRequirements::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::new(),
        };
        hub.tasks().replace(task.clone()).await.expect("upsert task");
        task
    }

    async fn start_workflow_for_task(hub: &Arc<InMemoryServiceHub>, task_id: &str) -> String {
        hub.workflows()
            .run(WorkflowRunInput::for_task(task_id.to_string(), None), None)
            .await
            .expect("bootstrap workflow")
            .id
    }

    #[tokio::test]
    async fn pause_annotates_task_with_pause_marker_without_flipping_status() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-1", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-1").await;

        let outcome = dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Pause { workflow_id: workflow_id.clone(), reason_detail: None },
        )
        .await
        .expect("pause dispatch");
        assert_eq!(outcome.workflow.expect("workflow").status, WorkflowStatus::Paused);

        let task = hub.tasks().get("TASK-1").await.expect("task");
        assert_eq!(task.status, TaskStatus::InProgress, "pause must not change task status");
        assert!(!task.paused, "pause annotation must not set the paused ghost flag");
        assert_eq!(task.blocked_reason.as_deref(), Some(format!("paused by workflow {workflow_id}").as_str()));
        assert_eq!(task.blocked_by.as_deref(), Some(workflow_id.as_str()));
    }

    #[tokio::test]
    async fn resume_clears_pause_marker() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-2", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-2").await;

        dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Pause { workflow_id: workflow_id.clone(), reason_detail: None },
        )
        .await
        .expect("pause dispatch");
        dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Resume { workflow_id: workflow_id.clone(), feedback: None },
        )
        .await
        .expect("resume dispatch");

        let task = hub.tasks().get("TASK-2").await.expect("task");
        assert_eq!(task.blocked_reason, None, "resume must clear the pause marker");
        assert_eq!(task.blocked_by, None, "resume must clear blocked_by set by pause");
        assert_eq!(task.status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn resume_preserves_foreign_blocked_reason() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let mut task = upsert_task(&hub, "TASK-3", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-3").await;
        task.blocked_reason = Some("merge conflict in src/lib.rs".to_string());
        hub.tasks().replace(task).await.expect("seed foreign reason");

        dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Resume { workflow_id, feedback: None },
        )
        .await
        .expect("resume dispatch");

        let task = hub.tasks().get("TASK-3").await.expect("task");
        assert_eq!(
            task.blocked_reason.as_deref(),
            Some("merge conflict in src/lib.rs"),
            "resume must only clear its own pause marker"
        );
    }

    #[tokio::test]
    async fn pause_does_not_overwrite_existing_blocked_reason() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let mut task = upsert_task(&hub, "TASK-10", TaskStatus::Blocked).await;
        task.blocked_reason = Some("workflow runner failed: boom".to_string());
        task.blocked_by = Some("other-source".to_string());
        hub.tasks().replace(task).await.expect("seed failure reason");
        let workflow_id = start_workflow_for_task(&hub, "TASK-10").await;

        dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Pause { workflow_id, reason_detail: None },
        )
        .await
        .expect("pause dispatch");

        let task = hub.tasks().get("TASK-10").await.expect("task");
        assert_eq!(
            task.blocked_reason.as_deref(),
            Some("workflow runner failed: boom"),
            "pause marker must not clobber a real blocked_reason"
        );
        assert_eq!(task.blocked_by.as_deref(), Some("other-source"));
    }

    #[tokio::test]
    async fn cancel_projects_task_to_cancelled() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-4", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-4").await;

        let outcome =
            dispatch_workflow_event(hub.clone() as Arc<dyn ServiceHub>, ".", WorkflowEvent::Cancel { workflow_id })
                .await
                .expect("cancel dispatch");
        assert_eq!(outcome.workflow.expect("workflow").status, WorkflowStatus::Cancelled);

        let task = hub.tasks().get("TASK-4").await.expect("task");
        assert_eq!(task.status, TaskStatus::Cancelled, "cancelled workflow must sync the task terminal state");
        assert!(task.blocked_reason.is_none(), "terminal sync must not leave blocked bookkeeping");
        assert!(!task.paused, "terminal sync must not leave the paused ghost flag");
        let outcome_task = outcome.task.expect("outcome carries the synced task");
        assert_eq!(outcome_task.status, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancel_leaves_done_task_untouched() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-5", TaskStatus::Done).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-5").await;

        dispatch_workflow_event(hub.clone() as Arc<dyn ServiceHub>, ".", WorkflowEvent::Cancel { workflow_id })
            .await
            .expect("cancel dispatch");

        let task = hub.tasks().get("TASK-5").await.expect("task");
        assert_eq!(task.status, TaskStatus::Done, "cancel must not regress an already-done task");
    }

    #[tokio::test]
    async fn ready_reset_clears_pause_marker_after_workflow_pause() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-6", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-6").await;
        dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Pause { workflow_id, reason_detail: None },
        )
        .await
        .expect("pause dispatch");

        hub.tasks().set_status("TASK-6", TaskStatus::Ready, false).await.expect("reset to ready");

        let task = hub.tasks().get("TASK-6").await.expect("task");
        assert_eq!(task.status, TaskStatus::Ready);
        assert!(!task.paused);
        assert!(task.blocked_reason.is_none(), "ready reset clears the pause annotation");
        assert!(task.blocked_by.is_none());
    }

    async fn complete_workflow(hub: &Arc<InMemoryServiceHub>, workflow_id: &str) {
        for _ in 0..32 {
            let workflow = hub.workflows().get(workflow_id).await.expect("workflow");
            if workflow.status == WorkflowStatus::Completed {
                return;
            }
            hub.workflows().complete_current_phase(workflow_id).await.expect("complete phase");
        }
        panic!("workflow did not complete");
    }

    #[tokio::test]
    async fn pause_of_completed_workflow_does_not_annotate_task() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-8", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-8").await;
        complete_workflow(&hub, &workflow_id).await;

        let outcome = dispatch_workflow_event(
            hub.clone() as Arc<dyn ServiceHub>,
            ".",
            WorkflowEvent::Pause { workflow_id, reason_detail: None },
        )
        .await
        .expect("pause dispatch");
        assert_eq!(outcome.workflow.expect("workflow").status, WorkflowStatus::Completed, "pause is a no-op");

        let task = hub.tasks().get("TASK-8").await.expect("task");
        assert!(task.blocked_reason.is_none(), "no-op pause must not write a pause marker");
        assert!(task.blocked_by.is_none());
        assert_eq!(task.status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn cancel_of_completed_workflow_leaves_task_untouched() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-9", TaskStatus::InProgress).await;
        let workflow_id = start_workflow_for_task(&hub, "TASK-9").await;
        complete_workflow(&hub, &workflow_id).await;

        let outcome =
            dispatch_workflow_event(hub.clone() as Arc<dyn ServiceHub>, ".", WorkflowEvent::Cancel { workflow_id })
                .await
                .expect("cancel dispatch");
        assert_eq!(outcome.workflow.expect("workflow").status, WorkflowStatus::Completed, "cancel is a no-op");

        let task = hub.tasks().get("TASK-9").await.expect("task");
        assert_eq!(task.status, TaskStatus::InProgress, "no-op cancel must not cancel the task");
    }

    #[test]
    fn pause_marker_prefix_is_stable() {
        // The prefix is matched verbatim by `project_task_workflow_pause_cleared`;
        // changing it would orphan annotations written by older builds.
        assert_eq!(WORKFLOW_PAUSED_REASON_PREFIX, "paused by workflow ");
    }
}
