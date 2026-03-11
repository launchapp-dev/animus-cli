use super::{session_backend_kind::SessionBackendKind, session_stability::SessionStability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBackendInfo {
    pub kind: SessionBackendKind,
    pub provider_tool: String,
    pub stability: SessionStability,
    pub display_name: String,
}
