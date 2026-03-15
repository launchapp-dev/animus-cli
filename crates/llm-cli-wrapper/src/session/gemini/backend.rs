use async_trait::async_trait;

use crate::error::Result;
use crate::session::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
};

use super::transport::{start_gemini_session, terminate_gemini_session};

pub struct GeminiSessionBackend;

impl GeminiSessionBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionBackend for GeminiSessionBackend {
    fn info(&self) -> SessionBackendInfo {
        SessionBackendInfo {
            kind: SessionBackendKind::GeminiSdk,
            provider_tool: "gemini".to_string(),
            stability: SessionStability::Experimental,
            display_name: "Gemini Native Backend".to_string(),
        }
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            supports_resume: true,
            supports_terminate: true,
            supports_permissions: true,
            supports_mcp: true,
            supports_tool_events: false,
            supports_thinking_events: false,
            supports_artifact_events: false,
            supports_usage_metadata: true,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        start_gemini_session(request, None).await
    }

    async fn resume_session(&self, request: SessionRequest, session_id: &str) -> Result<SessionRun> {
        start_gemini_session(request, Some(session_id.to_string())).await
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        terminate_gemini_session(session_id).await
    }
}

impl Default for GeminiSessionBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::super::{parser::parse_gemini_json_chunk, transport::gemini_invocation_for_request};
    use super::GeminiSessionBackend;
    use crate::session::{SessionBackend, SessionEvent, SessionRequest};

    #[test]
    fn gemini_invocation_defaults_to_json_output() {
        let request = SessionRequest {
            tool: "gemini".to_string(),
            model: "gemini-2.5-pro".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        let invocation = gemini_invocation_for_request(&request, None).expect("launch should build");
        assert_eq!(invocation.command, "gemini");
        assert!(invocation.args.contains(&"--output-format".to_string()));
        assert!(invocation.args.contains(&"json".to_string()));
        assert!(invocation.args.contains(&"-p".to_string()));
    }

    #[test]
    fn gemini_parser_emits_metadata_and_final_text() {
        let result = r#"{"session_id":"session-123","response":"done","stats":{"tools":{"totalCalls":0}}}"#;
        let events = parse_gemini_json_chunk(result);

        assert!(events.iter().any(|event| matches!(event, SessionEvent::Metadata { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::FinalText { text } if text == "done"
        )));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn gemini_backend_uses_gemini_native_label() {
        let backend = GeminiSessionBackend::new();
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
                            "args": ["-c", "printf 'gemini-native\\n'"],
                            "prompt_via_stdin": false
                        }
                    }
                }
            }),
        };

        let mut run = backend.start_session(request).await.expect("session should start");

        assert_eq!(run.selected_backend, "gemini-native");

        let started = run.events.recv().await.expect("started event");
        assert!(matches!(
            started,
            SessionEvent::Started { backend, .. } if backend == "gemini-native"
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn gemini_backend_emits_metadata_and_final_text_from_fixture() {
        let backend = GeminiSessionBackend::new();
        let fixture = "/Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/gemini_real.jsonl";
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

        assert!(saw_metadata, "expected gemini metadata event");
        assert!(saw_final_text, "expected final text from gemini fixture");
    }
}
