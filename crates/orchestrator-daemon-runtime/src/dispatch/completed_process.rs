use crate::RunnerEvent;
use animus_execution_protocol::ExecutionFence;
use protocol::orchestrator::WorkflowStatus;

#[derive(Debug)]
pub struct CompletedProcess {
    pub subject_id: String,
    pub subject_kind: Option<String>,
    pub task_id: Option<String>,
    pub workflow_id: Option<String>,
    pub workflow_ref: Option<String>,
    pub workflow_status: Option<WorkflowStatus>,
    /// Exact queue authority handed to this process at spawn. Completion must
    /// use this copy so an old process cannot terminalize a recovered lease.
    pub execution_fence: Option<ExecutionFence>,
    pub schedule_id: Option<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub failure_reason: Option<String>,
    pub events: Vec<RunnerEvent>,
}
