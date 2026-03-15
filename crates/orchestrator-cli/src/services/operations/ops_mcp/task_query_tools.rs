use super::*;

#[tool_router(router = task_query_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.task.list",
        description = "List tasks with optional filters (status, priority, type, assignee, tags, linked requirements), plus sort and pagination hints. Purpose: Find tasks matching criteria for work planning. Prerequisites: None. Example: {\"status\": \"in-progress\"} or {\"priority\": \"high\", \"tag\": [\"frontend\"], \"sort\": \"updated_at\"}. Sequencing: Filter results, then use ao.task.get for details or ao.task.status to update.",
        input_schema = ao_schema_for_type::<TaskListInput>()
    )]
    async fn ao_task_list(&self, params: Parameters<TaskListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_list_args(&input);
        self.run_list_tool(
            "ao.task.list",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "ao.task.get",
        description = "Fetch a task by its ID. Purpose: Get full task details including description, checklist, dependencies, and metadata. Prerequisites: None. Example: {\"id\": \"TASK-001\"}. Sequencing: Use after ao.task.list to get details of a specific task, or before ao.task.status to verify task exists.",
        input_schema = ao_schema_for_type::<TaskGetInput>()
    )]
    async fn ao_task_get(&self, params: Parameters<TaskGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_get_args(input.id);
        self.run_tool("ao.task.get", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.prioritized",
        description = "List tasks in priority order. Purpose: Get ordered list of tasks ready for work (by priority, then dependencies). Prerequisites: None. Example: {\"limit\": 10}. Sequencing: Use ao.task.next for single best task, or ao.task.list for filtered views.",
        input_schema = ao_schema_for_type::<TaskPrioritizedInput>()
    )]
    async fn ao_task_prioritized(&self, params: Parameters<TaskPrioritizedInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_prioritized_args(&input);
        self.run_list_tool(
            "ao.task.prioritized",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "ao.task.next",
        description = "Get the next task to work on. Purpose: Get the single highest priority task ready for work. Prerequisites: None. Example: {}. Sequencing: Use ao.task.prioritized to see all available tasks, or ao.task.get for details before starting.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_task_next(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("ao.task.next", vec!["task".to_string(), "next".to_string()], params.0.project_root).await
    }

    #[tool(
        name = "ao.task.stats",
        description = "Get task statistics. Purpose: View aggregate task metrics (counts by status, priority, type). Prerequisites: None. Example: {}. Sequencing: Use ao.task.list for detailed listings, or ao.workflow.list for workflow stats.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_task_stats(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("ao.task.stats", vec!["task".to_string(), "stats".to_string()], params.0.project_root).await
    }

    #[tool(
        name = "ao.task.history",
        description = "Get workflow dispatch history for a task. Purpose: View past workflow executions including timing, outcomes, and failure details. Prerequisites: Task must exist. Example: {\"id\": \"TASK-001\"}. Sequencing: Use ao.task.get first to verify task exists, or ao.task.list to find tasks.",
        input_schema = ao_schema_for_type::<TaskGetInput>()
    )]
    async fn ao_task_history(&self, params: Parameters<TaskGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["task".to_string(), "history".to_string(), "--id".to_string(), input.id];
        self.run_tool("ao.task.history", args, input.project_root).await
    }
}
