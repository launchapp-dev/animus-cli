use rmcp::model::CallToolResult;
use serde_json::json;

use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::logs_tools::LogsTailInput;
use super::AoMcpServer;
use crate::services::operations::{logs_tail_application, LogsTailApplicationRequest};

impl AoMcpServer {
    pub(super) async fn logs_tail_inproc(&self, input: LogsTailInput) -> CallToolResult {
        self.audit_actor_tool_decision("animus.logs.tail", false, "management-only");
        let project_root = resolve_project_root(&self.default_project_root, input.project_root);
        let request = LogsTailApplicationRequest {
            plugin: input.plugin,
            level: input.level.unwrap_or_else(|| "info".to_string()),
            since: input.since.unwrap_or_else(|| "1h".to_string()),
            limit: input.limit.map(|limit| limit as usize).unwrap_or(100),
        };
        match logs_tail_application(request, &project_root).await {
            Ok(result) => CallToolResult::structured(json!({ "tool": "animus.logs.tail", "result": result })),
            Err(error) => CallToolResult::structured_error(build_inproc_tool_error_payload("animus.logs.tail", &error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::logs_tools::LogsTailInput;
    use super::super::new_ao_mcp_server;

    #[tokio::test]
    async fn logs_tail_validation_is_typed_and_in_process() {
        let temp = tempfile::tempdir().expect("project root");
        let server = new_ao_mcp_server(temp.path().to_string_lossy().as_ref());
        let result = server
            .logs_tail_inproc(LogsTailInput {
                plugin: None,
                level: Some("trace".to_string()),
                since: None,
                limit: None,
                project_root: None,
            })
            .await;

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed log error");
        assert_eq!(payload.get("tool").and_then(Value::as_str), Some("animus.logs.tail"));
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
        assert!(
            payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("expected one of debug|info|warn|error")),
            "{payload}"
        );
    }
}
