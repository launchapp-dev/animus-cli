use super::*;

#[tool_router(router = runner_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.runner.health",
        description = "Check runner process health. Purpose: Verify runner is running and has capacity for agent execution. Prerequisites: None. Example: {}. Sequencing: Use before animus.agent.run to ensure runner is ready, or animus.runner.orphans-detect if issues suspected.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_runner_health(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("animus.runner.health", vec!["runner".to_string(), "health".to_string()], params.0.project_root)
            .await
    }

    #[tool(
        name = "animus.runner.orphans-detect",
        description = "Detect orphaned runner processes. Purpose: Find runner processes that are no longer managed by the daemon. Prerequisites: None. Example: {}. Sequencing: Use if agents aren't starting or animus.runner.health shows issues, then animus.runner.orphans-cleanup to fix.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_runner_orphans_detect(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "animus.runner.orphans-detect",
            vec!["runner".to_string(), "orphans".to_string(), "detect".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "animus.runner.orphans-cleanup",
        description = "Clean up orphaned runner processes. Purpose: Remove runner processes that are no longer managed by the daemon. Prerequisites: Use animus.runner.orphans-detect first to identify orphaned run IDs. Example: {\"run_id\": [\"abc123\"]}. Sequencing: Use after animus.runner.orphans-detect to find orphan IDs, then animus.runner.health to verify cleanup.",
        input_schema = ao_schema_for_type::<RunnerOrphansCleanupInput>()
    )]
    async fn ao_runner_orphans_cleanup(
        &self,
        params: Parameters<RunnerOrphansCleanupInput>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["runner".to_string(), "orphans".to_string(), "cleanup".to_string()];
        for id in &params.0.run_id {
            args.push("--run-id".to_string());
            args.push(id.clone());
        }
        self.run_tool("animus.runner.orphans-cleanup", args, params.0.project_root).await
    }
}
