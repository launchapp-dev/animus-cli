#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCapabilities {
    pub supports_resume: bool,
    pub supports_terminate: bool,
    pub supports_permissions: bool,
    pub supports_mcp: bool,
    pub supports_tool_events: bool,
    pub supports_thinking_events: bool,
    pub supports_artifact_events: bool,
    pub supports_usage_metadata: bool,
}
