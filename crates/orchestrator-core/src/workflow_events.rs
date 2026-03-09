use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::{
    load_agent_runtime_config, project_task_status, services::ServiceHub, OrchestratorTask,
    OrchestratorWorkflow, PhaseExecutionMode, PhaseManualDefinition, TaskStatus, WorkflowStatus,
};

#[derive(Debug, Clone)]
pub enum WorkflowEvent {
    Pause {
        workflow_id: String,
    },
    Resume {
        workflow_id: String,
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
    ResearchRequested {
        workflow_id: String,
        reason: String,
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
        WorkflowEvent::Pause { workflow_id } => {
            let workflow = hub.workflows().pause(&workflow_id).await?;
            Ok(WorkflowEventOutcome {
                workflow: Some(workflow),
                ..WorkflowEventOutcome::default()
            })
        }
        WorkflowEvent::Resume { workflow_id } => {
            let workflow = hub.workflows().resume(&workflow_id).await?;
            Ok(WorkflowEventOutcome {
                workflow: Some(workflow),
                ..WorkflowEventOutcome::default()
            })
        }
        WorkflowEvent::Cancel { workflow_id } => {
            let workflow = hub.workflows().cancel(&workflow_id).await?;
            Ok(WorkflowEventOutcome {
                workflow: Some(workflow),
                ..WorkflowEventOutcome::default()
            })
        }
        WorkflowEvent::ApproveManualPhase {
            workflow_id,
            phase_id,
            note,
        } => {
            let manual = ensure_manual_phase(project_root, &phase_id)?;
            let note = note.unwrap_or_default();
            if manual.approval_note_required && note.trim().is_empty() {
                return Err(anyhow!(
                    "phase '{}' requires a non-empty approval note",
                    phase_id
                ));
            }

            let workflow = hub.workflows().get(&workflow_id).await?;
            let current_phase = current_phase_id(&workflow).ok_or_else(|| {
                anyhow!("workflow '{}' has no active phase", workflow_id)
            })?;
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
        WorkflowEvent::RejectManualPhase {
            workflow_id,
            phase_id,
            note,
        } => {
            let manual = ensure_manual_phase(project_root, &phase_id)?;
            let note = note.unwrap_or_default();
            if manual.approval_note_required && note.trim().is_empty() {
                return Err(anyhow!(
                    "phase '{}' requires a non-empty rejection note",
                    phase_id
                ));
            }

            let workflow = hub.workflows().get(&workflow_id).await?;
            let current_phase = current_phase_id(&workflow).ok_or_else(|| {
                anyhow!("workflow '{}' has no active phase", workflow_id)
            })?;
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

            let failure_reason = if note.trim().is_empty() {
                "manual approval rejected".to_string()
            } else {
                note
            };
            let updated = hub
                .workflows()
                .fail_current_phase(&workflow_id, failure_reason)
                .await?;
            Ok(WorkflowEventOutcome {
                workflow: Some(updated),
                ..WorkflowEventOutcome::default()
            })
        }
        WorkflowEvent::ResearchRequested { workflow_id, reason } => {
            let workflow = hub.workflows().request_research(&workflow_id, reason).await?;
            Ok(WorkflowEventOutcome {
                workflow: Some(workflow),
                ..WorkflowEventOutcome::default()
            })
        }
        WorkflowEvent::StaleReset { task_id, reason } => {
            project_task_status(hub.clone(), &task_id, TaskStatus::Ready).await?;
            let task = hub.tasks().get(&task_id).await.ok();
            let _ = reason;
            Ok(WorkflowEventOutcome {
                task,
                ..WorkflowEventOutcome::default()
            })
        }
    }
}

fn current_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow.current_phase.clone().or_else(|| {
        workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.phase_id.clone())
    })
}

fn ensure_manual_phase(project_root: &str, phase_id: &str) -> Result<PhaseManualDefinition> {
    let runtime = load_agent_runtime_config(Path::new(project_root))?;
    let definition = runtime
        .phase_execution(phase_id)
        .ok_or_else(|| anyhow!("phase '{}' is not configured", phase_id))?;
    if !matches!(definition.mode, PhaseExecutionMode::Manual) {
        return Err(anyhow!("phase '{}' is not in manual mode", phase_id));
    }
    definition
        .manual
        .clone()
        .ok_or_else(|| anyhow!("phase '{}' missing manual configuration", phase_id))
}
