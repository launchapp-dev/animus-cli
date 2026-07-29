//! `animus.logs.*` MCP tools.
//!
//! Surface the CLI's `animus logs tail` through a typed application service so
//! agents can pull the daemon's log tail without a child CLI. The service keeps
//! the daemon control-wire and local events.jsonl fallback logic shared between
//! CLI and MCP callers.

use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct LogsTailInput {
    /// Filter entries to the named source plugin (matches the `provider`
    /// field on each structured entry). Omit to include every emitter.
    #[serde(default)]
    pub(super) plugin: Option<String>,
    /// Minimum severity. One of `debug`, `info`, `warn`, `error`. Defaults
    /// to `info` when omitted.
    #[serde(default)]
    pub(super) level: Option<String>,
    /// Only return entries newer than this duration. Accepts `1h`, `30m`,
    /// `15s`, `2d`. Defaults to `1h` when omitted.
    #[serde(default)]
    pub(super) since: Option<String>,
    /// Maximum number of entries to return. Defaults to 100 when omitted.
    #[serde(default)]
    pub(super) limit: Option<u32>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[tool_router(router = logs_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.logs.tail",
        description = "Tail recent daemon and plugin log entries through the active log_storage_backend. Purpose: Inspect what the daemon and its supervised plugins have been logging without shelling out. Prerequisites: None — falls back to the in-tree events.jsonl reader when the daemon is not running. Example: {\"limit\": 25} or {\"level\": \"warn\", \"plugin\": \"kimi-code\", \"since\": \"30m\"}. Sequencing: Use animus.daemon.status to confirm the daemon is up if you want the wire transport instead of the local fallback.",
        input_schema = ao_schema_for_type::<LogsTailInput>()
    )]
    async fn ao_logs_tail(&self, params: Parameters<LogsTailInput>) -> Result<CallToolResult, McpError> {
        Ok(self.logs_tail_inproc(params.0).await)
    }
}
