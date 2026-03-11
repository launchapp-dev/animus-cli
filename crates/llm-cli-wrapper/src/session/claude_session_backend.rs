use async_trait::async_trait;

use crate::error::Result;

use super::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
    subprocess_session_backend::SubprocessSessionBackend,
};

pub struct ClaudeSessionBackend {
    subprocess: SubprocessSessionBackend,
}

impl ClaudeSessionBackend {
    pub fn new() -> Self {
        Self {
            subprocess: SubprocessSessionBackend::new(),
        }
    }

    fn augment_request(&self, mut request: SessionRequest) -> SessionRequest {
        if let Some(extras) = request.extras.as_object_mut() {
            extras.insert(
                "backend_label".to_string(),
                serde_json::Value::String("claude-native".to_string()),
            );
        }
        request
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
            supports_resume: false,
            supports_terminate: false,
            supports_permissions: true,
            supports_mcp: true,
            supports_tool_events: false,
            supports_thinking_events: false,
            supports_artifact_events: false,
            supports_usage_metadata: false,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        self.subprocess
            .start_session(self.augment_request(request))
            .await
    }

    async fn resume_session(
        &self,
        request: SessionRequest,
        session_id: &str,
    ) -> Result<SessionRun> {
        self.subprocess
            .resume_session(self.augment_request(request), session_id)
            .await
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        self.subprocess.terminate_session(session_id).await
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

    use super::ClaudeSessionBackend;
    use crate::session::{SessionBackend, SessionEvent, SessionRequest};

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
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
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

        let mut run = backend
            .start_session(request)
            .await
            .expect("session should start");

        assert_eq!(run.selected_backend, "claude-native");

        let started = run.events.recv().await.expect("started event");
        assert!(matches!(
            started,
            SessionEvent::Started { backend, .. } if backend == "claude-native"
        ));
    }
}
