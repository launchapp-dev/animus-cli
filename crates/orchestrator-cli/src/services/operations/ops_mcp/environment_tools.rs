//! `animus.environment.*` MCP tools.
//!
//! Surface the CLI's `animus environment {list,get,teardown,reap}` node-
//! management verbs through MCP so agents + the portal can inspect and reap
//! ephemeral run nodes without shelling out. Mirrors the logs_tools pattern —
//! typed input struct, args builder, `run_tool` shell-out — so the environment
//! plugin's node-management logic is shared between the CLI and MCP callers.

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
    /// Also reap non-dead nodes with no live owning run (requires `force`).
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

pub(super) fn build_environment_list_args(input: &EnvironmentListInput) -> Vec<String> {
    let mut args = vec!["environment".to_string(), "list".to_string()];
    push_opt(&mut args, "--environment", input.environment.clone());
    args
}

pub(super) fn build_environment_get_args(input: &EnvironmentGetInput) -> Vec<String> {
    let mut args = vec!["environment".to_string(), "get".to_string(), input.id.clone()];
    push_opt(&mut args, "--environment", input.environment.clone());
    args
}

pub(super) fn build_environment_teardown_args(input: &EnvironmentTeardownInput) -> Vec<String> {
    let mut args = vec!["environment".to_string(), "teardown".to_string(), input.id.clone()];
    push_opt(&mut args, "--environment", input.environment.clone());
    args
}

pub(super) fn build_environment_reap_args(input: &EnvironmentReapInput) -> Vec<String> {
    let mut args = vec!["environment".to_string(), "reap".to_string()];
    if input.all {
        args.push("--all".to_string());
    }
    if input.force {
        args.push("--force".to_string());
    }
    if input.dry_run {
        args.push("--dry-run".to_string());
    }
    if let Some(secs) = input.older_than_secs {
        args.push("--older-than-secs".to_string());
        args.push(secs.to_string());
    }
    push_opt(&mut args, "--environment", input.environment.clone());
    args
}

#[tool_router(router = environment_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.environment.list",
        description = "List managed environment nodes (ephemeral run instances) with their state + orphan flag, via the installed environment plugin. Purpose: See what run nodes exist and which are dead/orphaned before reaping. Prerequisites: an environment plugin installed. Example: {} or {\"environment\": \"animus-environment-railway\"}. Sequencing: pair with animus.environment.reap to clean up.",
        input_schema = ao_schema_for_type::<EnvironmentListInput>()
    )]
    async fn ao_environment_list(&self, params: Parameters<EnvironmentListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_environment_list_args(&input);
        self.run_tool("animus.environment.list", args, project_root).await
    }

    #[tool(
        name = "animus.environment.get",
        description = "Describe one managed environment node by substrate id or name. Purpose: inspect a single run node's state. Prerequisites: an environment plugin installed. Example: {\"id\": \"animus-run-abc123\"}.",
        input_schema = ao_schema_for_type::<EnvironmentGetInput>()
    )]
    async fn ao_environment_get(&self, params: Parameters<EnvironmentGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_environment_get_args(&input);
        self.run_tool("animus.environment.get", args, project_root).await
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
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_environment_teardown_args(&input);
        self.run_tool("animus.environment.teardown", args, project_root).await
    }

    #[tool(
        name = "animus.environment.reap",
        description = "Reap orphaned/dead environment nodes. Default reaps ONLY dead (FAILED/CRASHED) nodes — always safe. Purpose: clean up leaked run nodes without a dashboard/2FA delete. Prerequisites: an environment plugin installed. Example: {} (reap dead) or {\"dry_run\": true} (preview) or {\"all\": true, \"force\": true} (also reap healthy orphans). Sequencing: run with dry_run first to preview.",
        input_schema = ao_schema_for_type::<EnvironmentReapInput>()
    )]
    async fn ao_environment_reap(&self, params: Parameters<EnvironmentReapInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_environment_reap_args(&input);
        self.run_tool("animus.environment.reap", args, project_root).await
    }
}
