use async_trait::async_trait;

use crate::error::Result;

use super::{
    session_backend_info::SessionBackendInfo, session_capabilities::SessionCapabilities,
    session_request::SessionRequest, session_run::SessionRun,
};

#[async_trait]
pub trait SessionBackend: Send + Sync {
    fn info(&self) -> SessionBackendInfo;

    fn capabilities(&self) -> SessionCapabilities;

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun>;

    async fn resume_session(&self, request: SessionRequest, session_id: &str)
        -> Result<SessionRun>;

    async fn terminate_session(&self, session_id: &str) -> Result<()>;
}
