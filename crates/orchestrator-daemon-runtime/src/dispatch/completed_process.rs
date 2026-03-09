use crate::RunnerEvent;
use protocol::orchestrator::WorkflowStatus;

#[derive(Debug)]
pub struct CompletedProcess {
    pub subject_id: String,
    pub task_id: Option<String>,
    pub workflow_id: Option<String>,
    pub workflow_ref: Option<String>,
    pub workflow_status: Option<WorkflowStatus>,
    pub schedule_id: Option<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub failure_reason: Option<String>,
    pub events: Vec<RunnerEvent>,
}
