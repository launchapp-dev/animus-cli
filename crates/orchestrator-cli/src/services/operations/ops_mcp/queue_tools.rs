use super::*;

#[tool_router(router = queue_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.queue.list",
        description = "List queued subject dispatches. Purpose: View the daemon dispatch queue entries, statuses, and selected metadata. Prerequisites: None. Example: {}. Sequencing: Use animus.queue.stats for aggregate depth, or animus.queue.hold / animus.queue.reorder to adjust queue state.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_queue_list(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.queue_list_inproc(params.0.project_root).await
    }

    #[tool(
        name = "animus.queue.stats",
        description = "Show queue statistics. Purpose: Get aggregate queue depth and per-status counts for the daemon dispatch queue. Prerequisites: None. Example: {}. Sequencing: Use animus.queue.list for detailed entries or animus.daemon.health for broader capacity context.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_queue_stats(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.queue_stats_inproc(params.0.project_root).await
    }

    #[tool(
        name = "animus.queue.enqueue",
        description = "Enqueue a subject dispatch. Purpose: Add a SubjectDispatch to the daemon queue using any subject kind or a custom title plus optional workflow/input override. Prerequisites: Subjects must exist; custom subjects require a title. Pass subject_id for any subject kind (task, requirement, or dynamic kinds like blog/post) — a qualified id (task:TASK-001, blog:BLOG-001) is trusted, a bare id (TASK-001) is resolved via the subject router. Example: {\"subject_id\": \"TASK-001\", \"workflow_ref\": \"ops\"} or {\"subject_id\": \"blog:BLOG-001\"}. Sequencing: Use animus.queue.list to inspect position or animus.queue.reorder to adjust ordering.",
        input_schema = ao_schema_for_type::<QueueEnqueueInput>()
    )]
    async fn ao_queue_enqueue(&self, params: Parameters<QueueEnqueueInput>) -> Result<CallToolResult, McpError> {
        self.queue_enqueue_inproc(params.0).await
    }

    #[tool(
        name = "animus.queue.hold",
        description = "Hold one or more queued subject dispatches. Purpose: Prevent pending subjects from being selected for dispatch without removing them from the queue. Prerequisites: Subjects must be queued and pending. Example: {\"subject_id\": \"TASK-001\"} or {\"subject_ids\": [\"TASK-001\", \"TASK-002\"]}. Sequencing: Use animus.queue.release to resume dispatch eligibility.",
        input_schema = ao_schema_for_type::<QueueSubjectInput>()
    )]
    async fn ao_queue_hold(&self, params: Parameters<QueueSubjectInput>) -> Result<CallToolResult, McpError> {
        self.queue_bulk_inproc("animus.queue.hold", crate::services::operations::QueueBulkVerb::Hold, params.0).await
    }

    #[tool(
        name = "animus.queue.release",
        description = "Release one or more held queued subject dispatches. Purpose: Make previously held subjects eligible for dispatch again. Prerequisites: Subjects must be queued and held. Example: {\"subject_id\": \"TASK-001\"} or {\"subject_ids\": [\"TASK-001\", \"TASK-002\"]}. Sequencing: Use animus.queue.list to verify queue state after release.",
        input_schema = ao_schema_for_type::<QueueSubjectInput>()
    )]
    async fn ao_queue_release(&self, params: Parameters<QueueSubjectInput>) -> Result<CallToolResult, McpError> {
        self.queue_bulk_inproc("animus.queue.release", crate::services::operations::QueueBulkVerb::Release, params.0)
            .await
    }

    #[tool(
        name = "animus.queue.drop",
        description = "Drop (remove) one or more queued subject dispatches. Purpose: Remove queue entries regardless of their current status (pending, assigned, or held). Use this to clean up stale or stuck queue entries. Prerequisites: Subjects must be in the queue. Example: {\"subject_id\": \"TASK-001\"} or {\"subject_ids\": [\"TASK-001\", \"TASK-002\"]}. Sequencing: Use animus.queue.list to find subject IDs, then animus.queue.drop to remove stuck entries.",
        input_schema = ao_schema_for_type::<QueueSubjectInput>()
    )]
    async fn ao_queue_drop(&self, params: Parameters<QueueSubjectInput>) -> Result<CallToolResult, McpError> {
        self.queue_bulk_inproc("animus.queue.drop", crate::services::operations::QueueBulkVerb::Drop, params.0).await
    }

    #[tool(
        name = "animus.queue.reorder",
        description = "Reorder queued subject dispatches. Purpose: Set the preferred dispatch order for queued subjects by subject id. Prerequisites: Subjects should already be queued. Example: {\"subject_ids\": [\"TASK-002\", \"TASK-001\"]}. Sequencing: Use animus.queue.list before and after to confirm the effective order.",
        input_schema = ao_schema_for_type::<QueueReorderInput>()
    )]
    async fn ao_queue_reorder(&self, params: Parameters<QueueReorderInput>) -> Result<CallToolResult, McpError> {
        self.queue_reorder_inproc(params.0).await
    }
}
