use async_trait::async_trait;

use crate::error::Result;

use super::transport::{start_claude_session, terminate_claude_session};
use crate::session::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
};

pub struct ClaudeSessionBackend;

impl ClaudeSessionBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionBackend for ClaudeSessionBackend {
    fn info(&self) -> SessionBackendInfo {
        SessionBackendInfo {
            kind: SessionBackendKind::ClaudeSdk,
            provider_tool: "claude".to_string(),
            stability: SessionStability::Experimental,
            display_name: "Claude Native Backend".to_string(),
        }
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            supports_resume: true,
            supports_terminate: true,
            supports_permissions: true,
            supports_mcp: true,
            supports_tool_events: true,
            supports_thinking_events: true,
            supports_artifact_events: false,
            supports_usage_metadata: true,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        start_claude_session(request, None).await
    }

    async fn resume_session(&self, request: SessionRequest, session_id: &str) -> Result<SessionRun> {
        start_claude_session(request, Some(session_id.to_string())).await
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        terminate_claude_session(session_id).await
    }
}

impl Default for ClaudeSessionBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::super::{parser::parse_claude_stdout_line, transport::claude_invocation_for_request};
    use super::ClaudeSessionBackend;
    use crate::session::{SessionBackend, SessionEvent, SessionRequest};

    #[test]
    fn claude_invocation_defaults_to_machine_output_and_permissions() {
        let request = SessionRequest {
            tool: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoints: Vec::new(),
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        let invocation = claude_invocation_for_request(&request, None).expect("launch should build");
        assert_eq!(invocation.command, "claude");
        assert!(invocation.args.contains(&"--print".to_string()));
        assert!(invocation.args.contains(&"--verbose".to_string()));
        assert!(invocation.args.contains(&"--output-format".to_string()));
        assert!(invocation.args.contains(&"stream-json".to_string()));
        assert!(invocation.args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn claude_parser_emits_metadata_tool_call_and_result() {
        let init = r#"{"type":"system","subtype":"init","session_id":"session-123","model":"claude-sonnet-4-6"}"#;
        let tool_call = r#"{"type":"content_block_start","content_block":{"type":"tool_use","name":"Read","input":{"path":"README.md"}}}"#;
        let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#;

        let init_events = parse_claude_stdout_line(init);
        assert!(matches!(init_events.first(), Some(SessionEvent::Metadata { .. })));

        let tool_events = parse_claude_stdout_line(tool_call);
        assert_eq!(
            tool_events,
            vec![SessionEvent::ToolCall {
                tool_name: "Read".to_string(),
                arguments: json!({"path": "README.md"}),
                server: None,
            }]
        );

        let result_events = parse_claude_stdout_line(result);
        assert_eq!(result_events, vec![SessionEvent::FinalText { text: "done".to_string() }]);
    }

    #[test]
    fn claude_parser_emits_tool_result_from_user_event_with_structured_content() {
        let user_event = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_014","type":"tool_result","content":"{\"result\":{\"items\":[]}}"}]},"tool_use_result":{"content":"{\"result\":{\"items\":[]},\"tool\":\"ao.task.list\"}","structuredContent":{"result":{"items":[]},"tool":"ao.task.list"}}}"#;

        let events = parse_claude_stdout_line(user_event);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], SessionEvent::ToolResult { tool_name, success, .. }
            if tool_name == "ao.task.list" && *success));
    }

    #[test]
    fn claude_parser_emits_tool_result_with_tool_reference() {
        let user_event = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_018q","content":[{"type":"tool_reference","tool_name":"mcp__ao__ao_task_list"}]}]}}"#;

        let events = parse_claude_stdout_line(user_event);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], SessionEvent::ToolResult { tool_name, success, .. }
            if tool_name == "mcp__ao__ao_task_list" && *success));
    }

    #[test]
    fn claude_parser_falls_back_to_tool_use_id_when_no_name_available() {
        let user_event = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_abc123","type":"tool_result","content":"some text"}]}}"#;

        let events = parse_claude_stdout_line(user_event);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], SessionEvent::ToolResult { tool_name, .. }
            if tool_name == "toolu_abc123"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn claude_backend_uses_claude_native_label() {
        let backend = ClaudeSessionBackend::new();
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoints: Vec::new(),
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({
                "runtime_contract": {
                    "cli": {
                        "launch": {
                            "command": "sh",
                            "args": ["-c", "printf 'claude-native\\n'"],
                            "prompt_via_stdin": false
                        }
                    }
                }
            }),
        };

        let mut run = backend.start_session(request).await.expect("session should start");

        assert_eq!(run.selected_backend, "claude-native");

        let started = run.events.recv().await.expect("started event");
        assert!(matches!(
            started,
            SessionEvent::Started { backend, .. } if backend == "claude-native"
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn claude_backend_emits_metadata_and_final_text_from_fixture() {
        let backend = ClaudeSessionBackend::new();
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude_real.jsonl");
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoints: Vec::new(),
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({
                "runtime_contract": {
                    "cli": {
                        "launch": {
                            "command": "sh",
                            "args": ["-c", format!("cat {fixture}")],
                            "prompt_via_stdin": false
                        }
                    }
                }
            }),
        };

        let mut run = backend.start_session(request).await.expect("session should start");

        let mut saw_metadata = false;
        let mut saw_final_text = false;

        while let Some(event) = run.events.recv().await {
            match event {
                SessionEvent::Metadata { .. } => saw_metadata = true,
                SessionEvent::FinalText { text } if text == "PINEAPPLE_42" => {
                    saw_final_text = true;
                }
                SessionEvent::Finished { .. } => break,
                _ => {}
            }
        }

        assert!(saw_metadata, "expected claude metadata event");
        assert!(saw_final_text, "expected final text from claude fixture");
    }
}
