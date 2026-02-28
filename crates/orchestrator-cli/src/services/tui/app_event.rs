use crate::services::tui::task_snapshot::TaskSnapshot;

#[derive(Debug, Clone)]
pub(crate) enum AppEvent {
    AgentOutput { line: String, is_error: bool },
    AgentFinished { summary: String, success: bool },
    TasksRefreshed(Vec<TaskSnapshot>),
    TaskOpError(String),
}
