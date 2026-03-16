use crate::{CompletedProcess, SubjectExecutionFact};
use protocol::orchestrator::WorkflowStatus;

#[derive(Debug, Clone, Default)]
pub struct CompletionReconciliationPlan {
    pub executed_workflow_phases: usize,
    pub failed_workflow_phases: usize,
    pub execution_facts: Vec<SubjectExecutionFact>,
}

pub fn build_completion_reconciliation_plan(
    completed_processes: Vec<CompletedProcess>,
) -> CompletionReconciliationPlan {
    let mut plan = CompletionReconciliationPlan::default();

    for completed in completed_processes {
        let workflow_success = completed.workflow_status.map(workflow_status_is_success).unwrap_or(completed.success);
        let failure_reason = completion_failure_reason(&completed);

        match completed.workflow_status {
            Some(WorkflowStatus::Completed) => {
                plan.executed_workflow_phases = plan.executed_workflow_phases.saturating_add(1);
            }
            Some(WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled) => {
                plan.failed_workflow_phases = plan.failed_workflow_phases.saturating_add(1);
            }
            Some(WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused) => {}
            None => {
                if completed.success {
                    plan.executed_workflow_phases = plan.executed_workflow_phases.saturating_add(1);
                } else {
                    plan.failed_workflow_phases = plan.failed_workflow_phases.saturating_add(1);
                }
            }
        }

        plan.execution_facts.push(SubjectExecutionFact {
            subject_id: completed.subject_id,
            subject_kind: completed.subject_kind,
            task_id: completed.task_id,
            workflow_id: completed.workflow_id,
            workflow_ref: completed.workflow_ref,
            workflow_status: completed.workflow_status,
            schedule_id: completed.schedule_id,
            exit_code: completed.exit_code,
            success: workflow_success,
            failure_reason,
            runner_events: completed.events,
        });
    }

    plan
}

fn completion_reason(completed: &CompletedProcess) -> String {
    completed
        .failure_reason
        .clone()
        .unwrap_or_else(|| format!("workflow runner exited with status {:?}", completed.exit_code))
}

fn completion_failure_reason(completed: &CompletedProcess) -> Option<String> {
    match completed.workflow_status {
        Some(
            WorkflowStatus::Completed | WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused,
        ) => None,
        Some(WorkflowStatus::Failed | WorkflowStatus::Escalated) => {
            Some(format!("workflow runner failed: {}", completion_reason(completed)))
        }
        Some(WorkflowStatus::Cancelled) => Some(format!("workflow runner cancelled: {}", completion_reason(completed))),
        None => {
            if completed.success {
                None
            } else {
                Some(format!("workflow runner exited without workflow status: {}", completion_reason(completed)))
            }
        }
    }
}

fn workflow_status_is_success(status: WorkflowStatus) -> bool {
    matches!(
        status,
        WorkflowStatus::Completed | WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompletedProcess;

    #[test]
    fn builds_task_success_plan() {
        let plan = build_completion_reconciliation_plan(vec![CompletedProcess {
            subject_id: "TASK-123".to_string(),
            subject_kind: Some(protocol::SUBJECT_KIND_TASK.to_string()),
            task_id: Some("TASK-123".to_string()),
            workflow_id: Some("WF-123".to_string()),
            workflow_ref: Some("standard".to_string()),
            workflow_status: Some(WorkflowStatus::Completed),
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            events: Vec::new(),
        }]);

        assert_eq!(plan.executed_workflow_phases, 1);
        assert_eq!(plan.failed_workflow_phases, 0);
        assert_eq!(plan.execution_facts.len(), 1);
        assert_eq!(plan.execution_facts[0].task_id.as_deref(), Some("TASK-123"));
        assert_eq!(plan.execution_facts[0].subject_kind.as_deref(), Some(protocol::SUBJECT_KIND_TASK));
        assert_eq!(plan.execution_facts[0].workflow_id.as_deref(), Some("WF-123"));
        assert_eq!(plan.execution_facts[0].workflow_ref.as_deref(), Some("standard"));
        assert_eq!(plan.execution_facts[0].workflow_status, Some(WorkflowStatus::Completed));
        assert!(plan.execution_facts[0].schedule_id.is_none());
        assert!(plan.execution_facts[0].failure_reason.is_none());
    }

    #[test]
    fn builds_failure_plan_with_schedule_update() {
        let plan = build_completion_reconciliation_plan(vec![CompletedProcess {
            subject_id: "schedule:nightly".to_string(),
            subject_kind: Some(protocol::SUBJECT_KIND_CUSTOM.to_string()),
            task_id: Some("TASK-999".to_string()),
            workflow_id: Some("WF-999".to_string()),
            workflow_ref: Some("ops".to_string()),
            workflow_status: Some(WorkflowStatus::Failed),
            schedule_id: Some("nightly".to_string()),
            exit_code: Some(17),
            success: false,
            failure_reason: None,
            events: Vec::new(),
        }]);

        assert_eq!(plan.executed_workflow_phases, 0);
        assert_eq!(plan.failed_workflow_phases, 1);
        assert_eq!(plan.execution_facts[0].task_id.as_deref(), Some("TASK-999"));
        assert_eq!(plan.execution_facts[0].workflow_ref.as_deref(), Some("ops"));
        assert_eq!(plan.execution_facts[0].schedule_id.as_deref(), Some("nightly"));
        assert_eq!(plan.execution_facts[0].completion_status(), "failed");
    }

    #[test]
    fn preserves_non_task_subjects_without_task_actions() {
        let plan = build_completion_reconciliation_plan(vec![CompletedProcess {
            subject_id: "schedule:daily-review".to_string(),
            subject_kind: Some(protocol::SUBJECT_KIND_CUSTOM.to_string()),
            task_id: None,
            workflow_id: Some("WF-321".to_string()),
            workflow_ref: Some("review".to_string()),
            workflow_status: Some(WorkflowStatus::Running),
            schedule_id: Some("daily-review".to_string()),
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            events: Vec::new(),
        }]);

        assert!(plan.execution_facts[0].task_id.is_none());
        assert_eq!(plan.execution_facts[0].schedule_id.as_deref(), Some("daily-review"));
        assert_eq!(plan.execution_facts[0].completion_status(), "running");
    }

    #[test]
    fn running_workflow_does_not_count_as_terminal_completion() {
        let plan = build_completion_reconciliation_plan(vec![CompletedProcess {
            subject_id: "TASK-777".to_string(),
            subject_kind: Some(protocol::SUBJECT_KIND_TASK.to_string()),
            task_id: Some("TASK-777".to_string()),
            workflow_id: Some("WF-777".to_string()),
            workflow_ref: Some("standard".to_string()),
            workflow_status: Some(WorkflowStatus::Running),
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            events: Vec::new(),
        }]);

        assert_eq!(plan.executed_workflow_phases, 0);
        assert_eq!(plan.failed_workflow_phases, 0);
        assert_eq!(plan.execution_facts[0].completion_status(), "running");
        assert!(plan.execution_facts[0].failure_reason.is_none());
    }

    #[test]
    fn cancelled_workflow_is_not_reported_as_success() {
        let plan = build_completion_reconciliation_plan(vec![CompletedProcess {
            subject_id: "TASK-404".to_string(),
            subject_kind: Some(protocol::SUBJECT_KIND_TASK.to_string()),
            task_id: Some("TASK-404".to_string()),
            workflow_id: Some("WF-404".to_string()),
            workflow_ref: Some("standard".to_string()),
            workflow_status: Some(WorkflowStatus::Cancelled),
            schedule_id: None,
            exit_code: Some(1),
            success: true,
            failure_reason: Some("operator cancelled the workflow".to_string()),
            events: Vec::new(),
        }]);

        assert_eq!(plan.executed_workflow_phases, 0);
        assert_eq!(plan.failed_workflow_phases, 1);
        assert!(!plan.execution_facts[0].success);
        assert_eq!(plan.execution_facts[0].completion_status(), "cancelled");
        assert_eq!(
            plan.execution_facts[0].failure_reason.as_deref(),
            Some("workflow runner cancelled: operator cancelled the workflow")
        );
    }

    #[test]
    fn missing_workflow_status_is_not_inferred_from_exit_code() {
        let plan = build_completion_reconciliation_plan(vec![CompletedProcess {
            subject_id: "TASK-505".to_string(),
            subject_kind: Some(protocol::SUBJECT_KIND_TASK.to_string()),
            task_id: Some("TASK-505".to_string()),
            workflow_id: None,
            workflow_ref: Some("standard".to_string()),
            workflow_status: None,
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            events: Vec::new(),
        }]);

        assert_eq!(plan.executed_workflow_phases, 1);
        assert_eq!(plan.failed_workflow_phases, 0);
        assert!(plan.execution_facts[0].success);
        assert!(plan.execution_facts[0].failure_reason.is_none());
    }
}
