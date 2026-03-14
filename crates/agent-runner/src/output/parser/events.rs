use protocol::ArtifactInfo;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum ParsedEvent {
    Output(String),
    ToolCall {
        tool_name: String,
        parameters: Value,
    },
    Artifact(ArtifactInfo),
    Thinking(String),
}
