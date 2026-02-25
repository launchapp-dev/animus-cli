#[derive(Debug, Clone)]
pub(crate) enum AppEvent {
    AgentOutput { line: String, is_error: bool },
    AgentFinished { summary: String, success: bool },
}
