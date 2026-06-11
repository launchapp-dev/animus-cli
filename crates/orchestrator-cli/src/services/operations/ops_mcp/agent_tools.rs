use super::*;

#[tool_router(router = agent_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.agent.list",
        description = "List configured project agent profiles, including names, roles, memory, and communication flags.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_agent_list(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("animus.agent.list", vec!["agent".to_string(), "list".to_string()], params.0.project_root).await
    }

    #[tool(
        name = "animus.agent.get",
        description = "Get a configured agent profile by id.",
        input_schema = ao_schema_for_type::<AgentProfileInput>()
    )]
    async fn ao_agent_get(&self, params: Parameters<AgentProfileInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["agent".to_string(), "get".to_string(), "--id".to_string(), input.id];
        self.run_tool("animus.agent.get", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.run",
        description = "Run an agent to execute work. Purpose: Launch an AI agent to perform tasks. Prerequisites: A provider plugin for the requested tool must be installed (check animus.plugin.list). Example: {\"tool\": \"claude\", \"model\": \"claude-3-opus\", \"prompt\": \"Fix the bug\"}. Sequencing: Use animus.agent.status to monitor, animus.agent.control to pause/resume/terminate.",
        input_schema = ao_schema_for_type::<AgentRunInput>()
    )]
    async fn ao_agent_run(&self, params: Parameters<AgentRunInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_agent_run_args(&input);
        self.run_tool("animus.agent.run", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.control",
        description = "Control a running agent. Purpose: Pause, resume, or terminate an active agent run. Prerequisites: Agent must be running (use animus.agent.status to verify). Example: {\"run_id\": \"abc123\", \"action\": \"terminate\"}. Valid actions: pause, resume, terminate. Sequencing: Use animus.agent.status first to check state, animus.output.monitor to see output.",
        input_schema = ao_schema_for_type::<AgentControlInput>()
    )]
    async fn ao_agent_control(&self, params: Parameters<AgentControlInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "agent".to_string(),
            "control".to_string(),
            "--run-id".to_string(),
            input.run_id,
            "--action".to_string(),
            input.action,
        ];
        self.run_tool("animus.agent.control", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.status",
        description = "Get status of an agent run. Purpose: Check if an agent is running, completed, or failed. Prerequisites: None (run_id from animus.agent.run). Example: {\"run_id\": \"abc123\"}. Sequencing: Use after animus.agent.run to track progress, or animus.agent.control to take action.",
        input_schema = ao_schema_for_type::<AgentStatusInput>()
    )]
    async fn ao_agent_status(&self, params: Parameters<AgentStatusInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["agent".to_string(), "status".to_string(), "--run-id".to_string(), input.run_id];
        self.run_tool("animus.agent.status", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.memory.get",
        description = "Read project-scoped memory for a configured agent profile.",
        input_schema = ao_schema_for_type::<AgentMemoryGetInput>()
    )]
    async fn ao_agent_memory_get(&self, params: Parameters<AgentMemoryGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args =
            vec!["agent".to_string(), "memory".to_string(), "get".to_string(), "--agent".to_string(), input.agent];
        self.run_tool("animus.agent.memory.get", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.memory.append",
        description = "Append a project-scoped memory entry for a configured agent profile.",
        input_schema = ao_schema_for_type::<AgentMemoryAppendInput>()
    )]
    async fn ao_agent_memory_append(
        &self,
        params: Parameters<AgentMemoryAppendInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "agent".to_string(),
            "memory".to_string(),
            "append".to_string(),
            "--agent".to_string(),
            input.agent,
            "--text".to_string(),
            input.text,
        ];
        push_opt(&mut args, "--source", input.source);
        self.run_tool("animus.agent.memory.append", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.memory.clear",
        description = "Clear project-scoped memory for a configured agent profile.",
        input_schema = ao_schema_for_type::<AgentMemoryGetInput>()
    )]
    async fn ao_agent_memory_clear(&self, params: Parameters<AgentMemoryGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args =
            vec!["agent".to_string(), "memory".to_string(), "clear".to_string(), "--agent".to_string(), input.agent];
        self.run_tool("animus.agent.memory.clear", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.message.send",
        description = "Send a project-scoped message on a configured agent channel.",
        input_schema = ao_schema_for_type::<AgentMessageSendInput>()
    )]
    async fn ao_agent_message_send(
        &self,
        params: Parameters<AgentMessageSendInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "agent".to_string(),
            "message".to_string(),
            "send".to_string(),
            "--channel".to_string(),
            input.channel,
            "--from".to_string(),
            input.from,
            "--text".to_string(),
            input.text,
        ];
        push_opt(&mut args, "--to", input.to);
        push_opt(&mut args, "--workflow-id", input.workflow_id);
        push_opt(&mut args, "--phase-id", input.phase_id);
        self.run_tool("animus.agent.message.send", args, input.project_root).await
    }

    #[tool(
        name = "animus.agent.message.list",
        description = "List project-scoped agent messages, optionally filtered by channel or agent.",
        input_schema = ao_schema_for_type::<AgentMessageListInput>()
    )]
    async fn ao_agent_message_list(
        &self,
        params: Parameters<AgentMessageListInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["agent".to_string(), "message".to_string(), "list".to_string()];
        push_opt(&mut args, "--channel", input.channel);
        push_opt(&mut args, "--agent", input.agent);
        push_opt(&mut args, "--limit", input.limit.map(|value| value.to_string()));
        self.run_tool("animus.agent.message.list", args, input.project_root).await
    }
}
