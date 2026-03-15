use super::*;

#[tool_router(router = agent_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.agent.run",
        description = "Run an agent to execute work. Purpose: Launch an AI agent to perform tasks. Prerequisites: Runner must be healthy (check ao.runner.health). Example: {\"tool\": \"claude\", \"model\": \"claude-3-opus\", \"prompt\": \"Fix the bug\"}. Sequencing: Use ao.agent.status to monitor, ao.agent.control to pause/resume/terminate.",
        input_schema = ao_schema_for_type::<AgentRunInput>()
    )]
    async fn ao_agent_run(&self, params: Parameters<AgentRunInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_agent_run_args(&input);
        self.run_tool("ao.agent.run", args, input.project_root).await
    }

    #[tool(
        name = "ao.agent.control",
        description = "Control a running agent. Purpose: Pause, resume, or terminate an active agent run. Prerequisites: Agent must be running (use ao.agent.status to verify). Example: {\"run_id\": \"abc123\", \"action\": \"terminate\"}. Valid actions: pause, resume, terminate. Sequencing: Use ao.agent.status first to check state, ao.output.monitor to see output.",
        input_schema = ao_schema_for_type::<AgentControlInput>()
    )]
    async fn ao_agent_control(&self, params: Parameters<AgentControlInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "agent".to_string(),
            "control".to_string(),
            "--run-id".to_string(),
            input.run_id,
            "--action".to_string(),
            input.action,
        ];
        push_opt(&mut args, "--runner-scope", input.runner_scope);
        self.run_tool("ao.agent.control", args, input.project_root).await
    }

    #[tool(
        name = "ao.agent.status",
        description = "Get status of an agent run. Purpose: Check if an agent is running, completed, or failed. Prerequisites: None (run_id from ao.agent.run). Example: {\"run_id\": \"abc123\"}. Sequencing: Use after ao.agent.run to track progress, or ao.agent.control to take action.",
        input_schema = ao_schema_for_type::<AgentStatusInput>()
    )]
    async fn ao_agent_status(&self, params: Parameters<AgentStatusInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["agent".to_string(), "status".to_string(), "--run-id".to_string(), input.run_id];
        push_opt(&mut args, "--runner-scope", input.runner_scope);
        self.run_tool("ao.agent.status", args, input.project_root).await
    }
}
