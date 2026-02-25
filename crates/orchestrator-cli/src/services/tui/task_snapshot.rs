use orchestrator_core::OrchestratorTask;

#[derive(Debug, Clone)]
pub(crate) struct TaskSnapshot {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) title: String,
}

impl TaskSnapshot {
    pub(crate) fn from_task(task: OrchestratorTask) -> Self {
        Self {
            id: task.id,
            status: status_label(task.status),
            title: task.title,
        }
    }

    pub(crate) fn label(&self) -> String {
        format!("{} [{}] {}", self.id, self.status, self.title)
    }
}

fn status_label(status: orchestrator_core::TaskStatus) -> String {
    match status {
        orchestrator_core::TaskStatus::Backlog => "backlog",
        orchestrator_core::TaskStatus::Ready => "ready",
        orchestrator_core::TaskStatus::InProgress => "in-progress",
        orchestrator_core::TaskStatus::Blocked => "blocked",
        orchestrator_core::TaskStatus::OnHold => "on-hold",
        orchestrator_core::TaskStatus::Done => "done",
        orchestrator_core::TaskStatus::Cancelled => "cancelled",
    }
    .to_string()
}
