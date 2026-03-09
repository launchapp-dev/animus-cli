use super::*;

#[tool_router(router = daemon_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.daemon.start",
        description = "Start the AO daemon. Purpose: Launch the background daemon for task scheduling and agent management. Prerequisites: None. Example: {} or {\"interval-secs\": 5}. Sequencing: After starting, use ao.daemon.status or ao.daemon.health to verify it's running.",
        input_schema = ao_schema_for_type::<DaemonStartInput>()
    )]
    async fn ao_daemon_start(
        &self,
        params: Parameters<DaemonStartInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_daemon_start_args(&input);
        self.run_tool("ao.daemon.start", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.daemon.stop",
        description = "Stop the AO daemon. Purpose: Shutdown the daemon gracefully. Prerequisites: Daemon must be running (check with ao.daemon.status). Example: {}. Sequencing: Use ao.daemon.status first to verify daemon is running, or ao.daemon.agents to see active agents before stopping.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_stop(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.stop",
            vec!["daemon".to_string(), "stop".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.status",
        description = "Get daemon status. Purpose: Check if daemon is running and view basic state. Prerequisites: None. Example: {}. Sequencing: Use after ao.daemon.start to verify startup, or before ao.daemon.stop to confirm running.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_status(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.status",
            vec!["daemon".to_string(), "status".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.health",
        description = "Check daemon health. Purpose: Get detailed health metrics including active agents, queue state, and capacity. Prerequisites: Daemon should be running. Example: {}. Sequencing: Use ao.daemon.status first to check if running, then ao.daemon.health for detailed metrics.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_health(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.health",
            vec!["daemon".to_string(), "health".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.pause",
        description = "Pause the daemon scheduler. Purpose: Temporarily stop the daemon from picking up new tasks without stopping it. Prerequisites: Daemon must be running. Example: {}. Sequencing: Use ao.daemon.status first, then ao.daemon.resume to continue scheduling.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_pause(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.pause",
            vec!["daemon".to_string(), "pause".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.resume",
        description = "Resume the daemon scheduler. Purpose: Continue task scheduling after a pause. Prerequisites: Daemon must be running and previously paused. Example: {}. Sequencing: Use after ao.daemon.pause, or check status with ao.daemon.status first.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_resume(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.resume",
            vec!["daemon".to_string(), "resume".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.events",
        description = "List recent daemon events. Purpose: Debug and monitor daemon activity, task scheduling, and agent lifecycle events. Prerequisites: Daemon should be running. Example: {\"limit\": 50}. Sequencing: Use ao.daemon.status first to confirm running, then ao.daemon.agents to see active agents.",
        input_schema = ao_schema_for_type::<DaemonEventsInput>()
    )]
    async fn ao_daemon_events(
        &self,
        params: Parameters<DaemonEventsInput>,
    ) -> Result<CallToolResult, McpError> {
        match build_daemon_events_poll_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "ao.daemon.events",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "ao.daemon.events",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "ao.daemon.agents",
        description = "List active daemon agents. Purpose: See currently running agent tasks and their status. Prerequisites: Daemon should be running. Example: {}. Sequencing: Use ao.daemon.status first to confirm running, then ao.agent.status for specific agent details.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_agents(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.agents",
            vec!["daemon".to_string(), "agents".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.logs",
        description = "Read daemon log file. Purpose: View daemon process logs for debugging crashes and issues. Prerequisites: Daemon should have been started at least once. Example: {\"limit\": 100} or {\"search\": \"error\"}. Sequencing: Use ao.daemon.status first to check if daemon is running, then ao.daemon.logs to debug issues.",
        input_schema = ao_schema_for_type::<DaemonLogsInput>()
    )]
    async fn ao_daemon_logs(
        &self,
        params: Parameters<DaemonLogsInput>,
    ) -> Result<CallToolResult, McpError> {
        match build_daemon_logs_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "ao.daemon.logs",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "ao.daemon.logs",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "ao.daemon.config",
        description = "Read daemon configuration. Purpose: View current daemon automation settings (auto-merge, auto-PR, etc). Prerequisites: None. Example: {}. Sequencing: Use ao.daemon.config-set to update values, or ao.daemon.status to check if daemon is running.",
        input_schema = ao_schema_for_type::<DaemonConfigInput>()
    )]
    async fn ao_daemon_config(
        &self,
        params: Parameters<DaemonConfigInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.config",
            vec!["daemon".to_string(), "config".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.config-set",
        description = "Update daemon configuration. Purpose: Persist daemon automation settings like auto-merge, auto-PR, auto-commit-before-merge, auto-prune-worktrees-after-merge, and auto-run-ready. Prerequisites: None. Example: {\"auto_merge\": true, \"auto_pr\": true}. Sequencing: Use ao.daemon.config to read current values first.",
        input_schema = ao_schema_for_type::<DaemonConfigSetInput>()
    )]
    async fn ao_daemon_config_set(
        &self,
        params: Parameters<DaemonConfigSetInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_daemon_config_set_args(&input);
        self.run_tool("ao.daemon.config-set", args, input.project_root)
            .await
    }
}
