use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Started {
        backend: String,
        session_id: Option<String>,
    },
    TextDelta {
        text: String,
    },
    FinalText {
        text: String,
    },
    ToolCall {
        tool_name: String,
        arguments: Value,
        server: Option<String>,
    },
    ToolResult {
        tool_name: String,
        output: Value,
        success: bool,
    },
    Thinking {
        text: String,
    },
    Artifact {
        artifact_id: String,
        metadata: Value,
    },
    Metadata {
        metadata: Value,
    },
    Error {
        message: String,
        recoverable: bool,
    },
    Finished {
        exit_code: Option<i32>,
    },
}
