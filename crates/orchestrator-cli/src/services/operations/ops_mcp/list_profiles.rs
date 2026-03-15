use super::ListToolProfile;

const TASK_SUMMARY_FIELDS: &[&str] =
    &["id", "title", "status", "priority", "type", "linked_requirements", "dependencies", "tags", "assignee"];

const REQUIREMENT_SUMMARY_FIELDS: &[&str] =
    &["id", "title", "status", "priority", "category", "type", "linked_task_ids"];

const QUEUE_SUMMARY_FIELDS: &[&str] = &["subject_id", "task_id", "status", "workflow_id"];

const WORKFLOW_SUMMARY_FIELDS: &[&str] = &[
    "id",
    "task_id",
    "workflow_ref",
    "status",
    "current_phase",
    "current_phase_index",
    "started_at",
    "completed_at",
    "failure_reason",
    "total_reworks",
];

const WORKFLOW_DECISION_SUMMARY_FIELDS: &[&str] =
    &["timestamp", "phase_id", "source", "decision", "target_phase", "reason", "confidence", "risk"];

const WORKFLOW_CHECKPOINT_SUMMARY_FIELDS: &[&str] =
    &["id", "workflow_id", "task_id", "phase_id", "phase_index", "reason", "created_at"];

const TASK_LIST_PROFILE: ListToolProfile = ListToolProfile {
    summary_fields: TASK_SUMMARY_FIELDS,
    digest_id_fields: &["id", "title"],
    digest_status_fields: &["status", "priority"],
};

const REQUIREMENT_LIST_PROFILE: ListToolProfile = ListToolProfile {
    summary_fields: REQUIREMENT_SUMMARY_FIELDS,
    digest_id_fields: &["id", "title"],
    digest_status_fields: &["status", "priority"],
};

const QUEUE_LIST_PROFILE: ListToolProfile = ListToolProfile {
    summary_fields: QUEUE_SUMMARY_FIELDS,
    digest_id_fields: &["subject_id", "task_id"],
    digest_status_fields: &["status", "workflow_id"],
};

const WORKFLOW_LIST_PROFILE: ListToolProfile = ListToolProfile {
    summary_fields: WORKFLOW_SUMMARY_FIELDS,
    digest_id_fields: &["id", "task_id"],
    digest_status_fields: &["status", "current_phase"],
};

const WORKFLOW_DECISION_LIST_PROFILE: ListToolProfile = ListToolProfile {
    summary_fields: WORKFLOW_DECISION_SUMMARY_FIELDS,
    digest_id_fields: &["phase_id", "timestamp"],
    digest_status_fields: &["decision", "risk", "source"],
};

const WORKFLOW_CHECKPOINT_LIST_PROFILE: ListToolProfile = ListToolProfile {
    summary_fields: WORKFLOW_CHECKPOINT_SUMMARY_FIELDS,
    digest_id_fields: &["id", "workflow_id", "task_id", "phase_id"],
    digest_status_fields: &["status", "reason"],
};

pub(super) fn list_tool_profile(tool_name: &str) -> Option<ListToolProfile> {
    match tool_name {
        "ao.task.list" | "ao.task.prioritized" => Some(TASK_LIST_PROFILE),
        "ao.requirements.list" => Some(REQUIREMENT_LIST_PROFILE),
        "ao.queue.list" => Some(QUEUE_LIST_PROFILE),
        "ao.workflow.list" => Some(WORKFLOW_LIST_PROFILE),
        "ao.workflow.decisions" => Some(WORKFLOW_DECISION_LIST_PROFILE),
        "ao.workflow.checkpoints.list" => Some(WORKFLOW_CHECKPOINT_LIST_PROFILE),
        _ => None,
    }
}
