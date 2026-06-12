use super::*;

#[tool_router(router = subject_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.subject.list",
        description = "List subjects of a given kind via the active subject_backend plugin. Purpose: Inspect subjects (tasks, requirements, issues, etc) from any installed backend. Prerequisites: A subject_backend plugin for the kind must be installed (for example a task, requirement, or external issue backend). Example: {\"kind\": \"task\"} or {\"kind\": \"task\", \"status\": \"ready\", \"limit\": 25}. Sequencing: Use animus.subject.get for a specific subject, or animus.subject.next to dispatch the next ready one.",
        input_schema = ao_schema_for_type::<SubjectListInput>()
    )]
    async fn ao_subject_list(&self, params: Parameters<SubjectListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_subject_list_args(&input);
        self.run_tool("animus.subject.list", args, project_root).await
    }

    #[tool(
        name = "animus.subject.get",
        description = "Fetch a single subject by id from the active subject_backend plugin. Purpose: Read the full state of a subject. Prerequisites: Subject must exist in the backend for the requested kind. Example: {\"kind\": \"task\", \"id\": \"sqlite:01ABCD...\"}. Sequencing: Use after animus.subject.list to discover ids.",
        input_schema = ao_schema_for_type::<SubjectGetInput>()
    )]
    async fn ao_subject_get(&self, params: Parameters<SubjectGetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_subject_get_args(&input);
        self.run_tool("animus.subject.get", args, project_root).await
    }

    #[tool(
        name = "animus.subject.create",
        description = "Create a subject through the active subject_backend. Purpose: Add a new task/requirement/issue without touching the underlying backend directly. Prerequisites: The backend must support create (write-capable). Example: {\"kind\": \"task\", \"title\": \"Fix flaky test\"} or with optional fields {\"kind\": \"task\", \"title\": \"Investigate\", \"status\": \"ready\", \"priority\": \"p1\", \"labels\": [\"bug\"]}. Sequencing: Use animus.subject.list afterwards to confirm.",
        input_schema = ao_schema_for_type::<SubjectCreateInput>()
    )]
    async fn ao_subject_create(&self, params: Parameters<SubjectCreateInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_subject_create_args(&input);
        self.run_tool("animus.subject.create", args, project_root).await
    }

    #[tool(
        name = "animus.subject.update",
        description = "Apply a patch to a subject through the active subject_backend. Purpose: Mutate status, priority, or labels. Prerequisites: Subject must exist; at least one of --status / --priority / --labels must be provided. Example: {\"kind\": \"task\", \"id\": \"TASK-1\", \"status\": \"in_progress\"}. Sequencing: Use animus.subject.get afterwards to confirm.",
        input_schema = ao_schema_for_type::<SubjectUpdateInput>()
    )]
    async fn ao_subject_update(&self, params: Parameters<SubjectUpdateInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_subject_update_args(&input);
        self.run_tool("animus.subject.update", args, project_root).await
    }

    #[tool(
        name = "animus.subject.batch-create",
        description = "Create multiple subjects of one kind in a single call. Purpose: Bulk-seed tasks/requirements without one MCP round-trip per item. Prerequisites: A write-capable subject_backend plugin for the kind must be installed. Example: {\"kind\": \"task\", \"items\": [{\"title\": \"Fix login\"}, {\"title\": \"Add tests\", \"priority\": \"p1\"}], \"on_error\": \"continue\"}. Items are dispatched one at a time in order; on_error=stop (default) marks remaining items skipped after the first failure, on_error=continue processes every item. Maximum 100 items per call. Returns an animus.mcp.batch.result.v1 envelope with per-item results. Sequencing: Use animus.subject.list afterwards to confirm.",
        input_schema = ao_schema_for_type::<SubjectBatchCreateInput>()
    )]
    async fn ao_subject_batch_create(
        &self,
        params: Parameters<SubjectBatchCreateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        if let Err(msg) = validate_subject_batch_create_input("animus.subject.batch-create", &input.kind, &input.items)
        {
            return Ok(CallToolResult::structured_error(json!({
                "tool": "animus.subject.batch-create",
                "error": msg,
            })));
        }
        let items: Vec<BatchItemExec> = input
            .items
            .iter()
            .map(|item| {
                let args = build_subject_batch_create_item_args(&input.kind, item);
                let command = args.join(" ");
                BatchItemExec { target_id: item.title.clone(), command, args }
            })
            .collect();
        self.run_batch_tool("animus.subject.batch-create", items, &input.on_error, input.project_root).await
    }

    #[tool(
        name = "animus.subject.batch-update",
        description = "Apply patches to multiple subjects of one kind in a single call. Purpose: Bulk-mutate status, priority, or labels without one MCP round-trip per item. Prerequisites: Subjects must exist; each item needs at least one of status / priority / labels. Example: {\"kind\": \"task\", \"items\": [{\"id\": \"TASK-1\", \"status\": \"ready\"}, {\"id\": \"TASK-2\", \"priority\": \"p0\"}], \"on_error\": \"stop\"}. Items are dispatched one at a time in order; on_error=stop (default) marks remaining items skipped after the first failure, on_error=continue processes every item. Maximum 100 items per call. Returns an animus.mcp.batch.result.v1 envelope with per-item results. Sequencing: Use animus.subject.get afterwards to confirm.",
        input_schema = ao_schema_for_type::<SubjectBatchUpdateInput>()
    )]
    async fn ao_subject_batch_update(
        &self,
        params: Parameters<SubjectBatchUpdateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        if let Err(msg) = validate_subject_batch_update_input("animus.subject.batch-update", &input.kind, &input.items)
        {
            return Ok(CallToolResult::structured_error(json!({
                "tool": "animus.subject.batch-update",
                "error": msg,
            })));
        }
        let items: Vec<BatchItemExec> = input
            .items
            .iter()
            .map(|item| {
                let args = build_subject_batch_update_item_args(&input.kind, item);
                let command = args.join(" ");
                BatchItemExec { target_id: item.id.clone(), command, args }
            })
            .collect();
        self.run_batch_tool("animus.subject.batch-update", items, &input.on_error, input.project_root).await
    }

    #[tool(
        name = "animus.subject.next",
        description = "Return the highest-priority Ready subject for the given kind. Purpose: Pick the next subject to dispatch. Prerequisites: Backend must implement <kind>/next. Example: {\"kind\": \"task\"}. Returns null when no eligible subject exists. Sequencing: Use animus.subject.status to mark in_progress before working on it.",
        input_schema = ao_schema_for_type::<SubjectNextInput>()
    )]
    async fn ao_subject_next(&self, params: Parameters<SubjectNextInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_subject_next_args(&input);
        self.run_tool("animus.subject.next", args, project_root).await
    }

    #[tool(
        name = "animus.subject.status",
        description = "Set the normalized status of a subject through the active subject_backend. Purpose: Transition a subject between normalized states (ready / in_progress / blocked / done). Prerequisites: Subject must exist. Example: {\"kind\": \"task\", \"id\": \"TASK-1\", \"status\": \"in_progress\"}. Sequencing: Pairs with animus.subject.next when starting work.",
        input_schema = ao_schema_for_type::<SubjectStatusInput>()
    )]
    async fn ao_subject_status(&self, params: Parameters<SubjectStatusInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = input.project_root.clone();
        let args = build_subject_status_args(&input);
        self.run_tool("animus.subject.status", args, project_root).await
    }
}
