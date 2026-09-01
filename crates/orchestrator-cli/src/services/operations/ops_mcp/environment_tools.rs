//! `animus.environment.*` MCP tools.
//!
//! Surface the CLI's `animus environment {list,get,teardown,reap}` node-
//! management verbs through typed application services so agents + the portal
//! can inspect and reap ephemeral run nodes without a child CLI. The installed
//! environment plugin remains the intentional out-of-process state owner.

use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct EnvironmentListInput {
    /// Environment plugin id to target. Omit to use the sole installed one.
    #[serde(default)]
    pub(super) environment: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct EnvironmentGetInput {
    /// Substrate id or name of the node (e.g. a service id or `animus-run-*`).
    pub(super) id: String,
    #[serde(default)]
    pub(super) environment: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct EnvironmentTeardownInput {
    /// Substrate id or name of the node to destroy.
    pub(super) id: String,
    #[serde(default)]
    pub(super) environment: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct EnvironmentReapInput {
    /// Also reap non-dead nodes with no live owning run (requires `force`),
    /// WITHOUT consulting the journal's live run set. Without `all`, the
    /// default reap runs owner-known: healthy nodes of terminal/gone workflows
    /// are reaped too (subject to the plugin's age grace).
    #[serde(default)]
    pub(super) all: bool,
    /// Confirm reaping healthy orphans; required with `all`.
    #[serde(default)]
    pub(super) force: bool,
    /// Report what WOULD be reaped without deleting anything.
    #[serde(default)]
    pub(super) dry_run: bool,
    /// Only reap nodes at least this many seconds old.
    #[serde(default)]
    pub(super) older_than_secs: Option<u64>,
    #[serde(default)]
    pub(super) environment: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[tool_router(router = environment_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.environment.list",
        description = "List managed environment nodes (ephemeral run instances) with their state + orphan flag, via the installed environment plugin. Purpose: See what run nodes exist and which are dead/orphaned before reaping. Prerequisites: an environment plugin installed. Example: {} or {\"environment\": \"animus-environment-railway\"}. Sequencing: pair with animus.environment.reap to clean up.",
        input_schema = ao_schema_for_type::<EnvironmentListInput>()
    )]
    async fn ao_environment_list(&self, params: Parameters<EnvironmentListInput>) -> Result<CallToolResult, McpError> {
        Ok(self.environment_list_inproc(params.0))
    }

    #[tool(
        name = "animus.environment.get",
        description = "Describe one managed environment node by substrate id or name. Purpose: inspect a single run node's state. Prerequisites: an environment plugin installed. Example: {\"id\": \"animus-run-abc123\"}.",
        input_schema = ao_schema_for_type::<EnvironmentGetInput>()
    )]
    async fn ao_environment_get(&self, params: Parameters<EnvironmentGetInput>) -> Result<CallToolResult, McpError> {
        Ok(self.environment_get_inproc(params.0))
    }

    #[tool(
        name = "animus.environment.teardown",
        description = "Destroy ONE managed environment node by substrate id or name (idempotent). Purpose: remove a specific leaked/stuck run node. Prerequisites: an environment plugin installed. Example: {\"id\": \"animus-run-abc123\"}. Sequencing: use animus.environment.list first to find the id.",
        input_schema = ao_schema_for_type::<EnvironmentTeardownInput>()
    )]
    async fn ao_environment_teardown(
        &self,
        params: Parameters<EnvironmentTeardownInput>,
    ) -> Result<CallToolResult, McpError> {
        Ok(self.environment_teardown_inproc(params.0))
    }

    #[tool(
        name = "animus.environment.reap",
        description = "Reap orphaned/dead environment nodes. Default reaps dead (FAILED/CRASHED) nodes AND, when the journal's live run set is readable, healthy (SUCCESS) nodes whose owning workflow is terminal/gone (owner-known mode; plugin age grace applies) — always safe. Purpose: clean up leaked run nodes without a dashboard/2FA delete. Prerequisites: an environment plugin installed. Example: {} (default owner-known reap) or {\"dry_run\": true} (preview) or {\"all\": true, \"force\": true} (legacy healthy-orphan sweep). Sequencing: run with dry_run first to preview.",
        input_schema = ao_schema_for_type::<EnvironmentReapInput>()
    )]
    async fn ao_environment_reap(&self, params: Parameters<EnvironmentReapInput>) -> Result<CallToolResult, McpError> {
        Ok(self.environment_reap_inproc(params.0))
    }
}
