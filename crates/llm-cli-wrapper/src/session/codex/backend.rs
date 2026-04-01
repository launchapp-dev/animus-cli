use async_trait::async_trait;

use crate::error::Result;

use super::transport::{start_codex_session, terminate_codex_session};
use crate::session::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
};

pub struct CodexSessionBackend;

impl CodexSessionBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionBackend for CodexSessionBackend {
    fn info(&self) -> SessionBackendInfo {
        SessionBackendInfo {
            kind: SessionBackendKind::CodexSdk,
            provider_tool: "codex".to_string(),
            stability: SessionStability::Experimental,
            display_name: "Codex Native Backend".to_string(),
        }
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            supports_resume: true,
            supports_terminate: true,
            supports_permissions: true,
            supports_mcp: true,
            supports_tool_events: false,
            supports_thinking_events: true,
            supports_artifact_events: false,
            supports_usage_metadata: true,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        start_codex_session(request, false).await
    }

    async fn resume_session(&self, request: SessionRequest, _session_id: &str) -> Result<SessionRun> {
        start_codex_session(request, true).await
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        terminate_codex_session(session_id).await
    }
}

impl Default for CodexSessionBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::super::{parser::parse_codex_stdout_line, transport::codex_invocation_for_request};
    use super::CodexSessionBackend;
    use crate::session::{SessionBackend, SessionEvent, SessionRequest};

    #[test]
    fn codex_invocation_defaults_to_json_and_full_auto() {
        let request = SessionRequest {
            tool: "codex".to_string(),
            model: "gpt-5".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        let invocation = codex_invocation_for_request(&request, false).expect("launch should build");
        assert_eq!(invocation.command, "codex");
        assert!(invocation.args.contains(&"exec".to_string()));
        assert!(invocation.args.contains(&"--json".to_string()));
        assert!(invocation.args.contains(&"--full-auto".to_string()));
    }

    #[test]
    fn codex_parser_emits_thinking_usage_and_final_text() {
        let reasoning = r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"thinking"}}"#;
        let message = r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"done"}}"#;
        let completed = r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}"#;

        assert_eq!(parse_codex_stdout_line(reasoning), vec![SessionEvent::Thinking { text: "thinking".to_string() }]);
        assert_eq!(parse_codex_stdout_line(message), vec![SessionEvent::FinalText { text: "done".to_string() }]);
        assert!(matches!(parse_codex_stdout_line(completed).first(), Some(SessionEvent::Metadata { .. })));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn codex_backend_uses_codex_native_label() {
        let backend = CodexSessionBackend::new();
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({
                "runtime_contract": {
                    "cli": {
                        "launch": {
                            "command": "sh",
                            "args": ["-c", "printf 'codex-native\\n'"],
                            "prompt_via_stdin": false
                        }
                    }
                }
            }),
        };

        let mut run = backend.start_session(request).await.expect("session should start");

        assert_eq!(run.selected_backend, "codex-native");

        let started = run.events.recv().await.expect("started event");
        assert!(matches!(
            started,
            SessionEvent::Started { backend, .. } if backend == "codex-native"
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn codex_backend_emits_thinking_and_final_text_from_fixture() {
        let backend = CodexSessionBackend::new();
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex_real.jsonl");
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
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

        let mut saw_thinking = false;
        let mut saw_final_text = false;
        let mut saw_metadata = false;

        while let Some(event) = run.events.recv().await {
            match event {
                SessionEvent::Thinking { .. } => saw_thinking = true,
                SessionEvent::FinalText { text } if text == "PINEAPPLE_42" => {
                    saw_final_text = true;
                }
                SessionEvent::Metadata { .. } => saw_metadata = true,
                SessionEvent::Finished { .. } => break,
                _ => {}
            }
        }

        assert!(saw_thinking, "expected codex thinking event");
        assert!(saw_metadata, "expected codex usage metadata");
        assert!(saw_final_text, "expected final text from codex fixture");
    }
}
