use super::*;

#[tool_router(router = queue_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.queue.list",
        description = "List queued subject dispatches. Purpose: View the daemon dispatch queue entries, statuses, and selected metadata. Prerequisites: None. Example: {}. Sequencing: Use ao.queue.stats for aggregate depth, or ao.queue.hold / ao.queue.reorder to adjust queue state.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_queue_list(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("ao.queue.list", vec!["queue".to_string(), "list".to_string()], params.0.project_root).await
    }

    #[tool(
        name = "ao.queue.stats",
        description = "Show queue statistics. Purpose: Get aggregate queue depth and per-status counts for the daemon dispatch queue. Prerequisites: None. Example: {}. Sequencing: Use ao.queue.list for detailed entries or ao.daemon.health for broader capacity context.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_queue_stats(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("ao.queue.stats", vec!["queue".to_string(), "stats".to_string()], params.0.project_root).await
    }

    #[tool(
        name = "ao.queue.enqueue",
        description = "Enqueue a subject dispatch. Purpose: Add a SubjectDispatch to the daemon queue using a task, requirement, or custom subject plus optional workflow/input override. Prerequisites: Task subjects must exist; custom subjects require a title. Example: {\"task_id\": \"TASK-001\", \"workflow_ref\": \"ops\"}. Sequencing: Use ao.queue.list to inspect position or ao.queue.reorder to adjust ordering.",
        input_schema = ao_schema_for_type::<QueueEnqueueInput>()
    )]
    async fn ao_queue_enqueue(&self, params: Parameters<QueueEnqueueInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_queue_enqueue_args(&input);
        self.run_tool("ao.queue.enqueue", args, input.project_root).await
    }

    #[tool(
        name = "ao.queue.hold",
        description = "Hold a queued subject dispatch. Purpose: Prevent a pending subject from being selected for dispatch without removing it from the queue. Prerequisites: Subject must be queued and pending. Example: {\"subject_id\": \"TASK-001\"}. Sequencing: Use ao.queue.release to resume dispatch eligibility.",
        input_schema = ao_schema_for_type::<QueueSubjectInput>()
    )]
    async fn ao_queue_hold(&self, params: Parameters<QueueSubjectInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_tool(
            "ao.queue.hold",
            vec!["queue".to_string(), "hold".to_string(), "--subject-id".to_string(), input.subject_id],
            input.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.queue.release",
        description = "Release a held queued subject dispatch. Purpose: Make a previously held subject eligible for dispatch again. Prerequisites: Subject must be queued and held. Example: {\"subject_id\": \"TASK-001\"}. Sequencing: Use ao.queue.list to verify queue state after release.",
        input_schema = ao_schema_for_type::<QueueSubjectInput>()
    )]
    async fn ao_queue_release(&self, params: Parameters<QueueSubjectInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_tool(
            "ao.queue.release",
            vec!["queue".to_string(), "release".to_string(), "--subject-id".to_string(), input.subject_id],
            input.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.queue.drop",
        description = "Drop (remove) a queued subject dispatch. Purpose: Remove a queue entry regardless of its current status (pending, assigned, or held). Use this to clean up stale or stuck queue entries. Prerequisites: Subject must be in the queue. Example: {\"subject_id\": \"TASK-001\"}. Sequencing: Use ao.queue.list to find subject IDs, then ao.queue.drop to remove stuck entries.",
        input_schema = ao_schema_for_type::<QueueSubjectInput>()
    )]
    async fn ao_queue_drop(&self, params: Parameters<QueueSubjectInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_tool(
            "ao.queue.drop",
            vec!["queue".to_string(), "drop".to_string(), "--subject-id".to_string(), input.subject_id],
            input.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.queue.reorder",
        description = "Reorder queued subject dispatches. Purpose: Set the preferred dispatch order for queued subjects by subject id. Prerequisites: Subjects should already be queued. Example: {\"subject_ids\": [\"TASK-002\", \"TASK-001\"]}. Sequencing: Use ao.queue.list before and after to confirm the effective order.",
        input_schema = ao_schema_for_type::<QueueReorderInput>()
    )]
    async fn ao_queue_reorder(&self, params: Parameters<QueueReorderInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_queue_reorder_args(&input);
        self.run_tool("ao.queue.reorder", args, input.project_root).await
    }
}
