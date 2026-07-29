use super::*;

#[tool_router(router = agent_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.agent.list",
        description = "List configured project agent profiles, including names, roles, memory, and communication flags.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_agent_list(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_list_inproc(params.0.project_root))
    }

    #[tool(
        name = "animus.agent.get",
        description = "Get a configured agent profile by id.",
        input_schema = ao_schema_for_type::<AgentProfileInput>()
    )]
    async fn ao_agent_get(&self, params: Parameters<AgentProfileInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_get_inproc(params.0))
    }

    #[tool(
        name = "animus.agent.run",
        description = "Run an agent to execute work. Purpose: Launch an AI agent to perform tasks. Prerequisites: A provider plugin for the requested tool must be installed (check animus.plugin.list). Example: {\"tool\": \"claude\", \"model\": \"claude-3-opus\", \"prompt\": \"Fix the bug\"}. Sequencing: Use animus.agent.status to monitor, animus.agent.control to pause/resume/terminate.",
        input_schema = ao_schema_for_type::<AgentRunInput>()
    )]
    async fn ao_agent_run(&self, params: Parameters<AgentRunInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_run_inproc(params.0).await)
    }

    #[tool(
        name = "animus.agent.control",
        description = "Control a running agent. Purpose: Pause, resume, or terminate an active agent run. Prerequisites: Agent must be running (use animus.agent.status to verify). Example: {\"run_id\": \"abc123\", \"action\": \"terminate\"}. Valid actions: pause, resume, terminate. Sequencing: Use animus.agent.status first to check state, animus.output.monitor to see output.",
        input_schema = ao_schema_for_type::<AgentControlInput>()
    )]
    async fn ao_agent_control(&self, params: Parameters<AgentControlInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_control_inproc(params.0))
    }

    #[tool(
        name = "animus.agent.status",
        description = "Get status of an agent run. Purpose: Check if an agent is running, completed, or failed. Prerequisites: None (run_id from animus.agent.run). Example: {\"run_id\": \"abc123\"}. Sequencing: Use after animus.agent.run to track progress, or animus.agent.control to take action.",
        input_schema = ao_schema_for_type::<AgentStatusInput>()
    )]
    async fn ao_agent_status(&self, params: Parameters<AgentStatusInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_status_inproc(params.0))
    }

    #[tool(
        name = "animus.agent.memory.get",
        description = "Read project-scoped memory for a configured agent profile.",
        input_schema = ao_schema_for_type::<AgentMemoryGetInput>()
    )]
    async fn ao_agent_memory_get(&self, params: Parameters<AgentMemoryGetInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_memory_get_inproc(params.0))
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
        Ok(self.agent_memory_append_inproc(params.0))
    }

    #[tool(
        name = "animus.agent.memory.clear",
        description = "Clear project-scoped memory for a configured agent profile.",
        input_schema = ao_schema_for_type::<AgentMemoryGetInput>()
    )]
    async fn ao_agent_memory_clear(&self, params: Parameters<AgentMemoryGetInput>) -> Result<CallToolResult, McpError> {
        Ok(self.agent_memory_clear_inproc(params.0))
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
        Ok(self.agent_message_send_inproc(params.0))
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
        Ok(self.agent_message_list_inproc(params.0))
    }
}
