use protocol::ArtifactInfo;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum ParsedEvent {
    Output(#[allow(dead_code)] String),
    ToolCall { tool_name: String, parameters: Value },
    ToolResult { tool_name: String, result: Value, success: bool },
    Artifact(ArtifactInfo),
    Thinking(String),
}
