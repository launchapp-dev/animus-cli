use async_trait::async_trait;

use crate::error::Result;
use crate::session::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
};

use super::transport::{start_oai_runner_session, terminate_oai_runner_session};

pub struct OaiRunnerSessionBackend;

impl OaiRunnerSessionBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionBackend for OaiRunnerSessionBackend {
    fn info(&self) -> SessionBackendInfo {
        SessionBackendInfo {
            kind: SessionBackendKind::OaiRunnerSdk,
            provider_tool: "oai-runner".to_string(),
            stability: SessionStability::Experimental,
            display_name: "AO OAI Runner Native Backend".to_string(),
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
            supports_usage_metadata: false,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        start_oai_runner_session(request, None).await
    }

    async fn resume_session(&self, request: SessionRequest, session_id: &str) -> Result<SessionRun> {
        start_oai_runner_session(request, Some(session_id.to_string())).await
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        terminate_oai_runner_session(session_id).await
    }
}

impl Default for OaiRunnerSessionBackend {
    fn default() -> Self {
        Self::new()
    }
}
