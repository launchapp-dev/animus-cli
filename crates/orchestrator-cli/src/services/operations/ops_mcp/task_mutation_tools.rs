use super::*;

#[tool_router(router = task_mutation_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.task.create",
        description = "Create a new task in AO. Purpose: Add new work items to the task backlog. Prerequisites: None. Example: {\"title\": \"Fix login bug\", \"description\": \"Users cannot login with OAuth\", \"priority\": \"high\", \"linked_requirement\": [\"REQ-001\"]}. Sequencing: After creation, use ao.task.assign to assign owner, or ao.workflow.run to start working.",
        input_schema = ao_schema_for_type::<TaskCreateInput>()
    )]
    async fn ao_task_create(&self, params: Parameters<TaskCreateInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_create_args(&input);
        self.run_tool("ao.task.create", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.status",
        description = "Update the status of a task. Purpose: Progress tasks through workflow states. Prerequisites: Task must exist (use ao.task.get to verify). Example: {\"id\": \"TASK-001\", \"status\": \"in-progress\"}. Valid statuses: backlog, todo, ready, in_progress, blocked, on_hold, done, cancelled. Sequencing: After marking done, consider ao.task.create for follow-up work.",
        input_schema = ao_schema_for_type::<TaskStatusInput>()
    )]
    async fn ao_task_status(&self, params: Parameters<TaskStatusInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task".to_string(),
            "status".to_string(),
            "--id".to_string(),
            input.id,
            "--status".to_string(),
            input.status,
        ];
        self.run_tool("ao.task.status", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.delete",
        description = "Delete a task from AO. Purpose: Remove unwanted or duplicate tasks. Prerequisites: Task must exist. Warning: This is destructive. Use dry_run first. Example: {\"id\": \"TASK-999\", \"confirm\": true, \"dry_run\": false}. Sequencing: Use ao.task.get to verify task details first, or ao.task.list to find tasks.",
        input_schema = ao_schema_for_type::<TaskDeleteInput>()
    )]
    async fn ao_task_delete(&self, params: Parameters<TaskDeleteInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_delete_args(input.id, input.confirm, input.dry_run);
        self.run_tool("ao.task.delete", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.pause",
        description = "Pause a running task. Purpose: Temporarily halt task execution without cancelling. Prerequisites: Task must be in-progress. Example: {\"id\": \"TASK-001\"}. Sequencing: Use ao.agent.control for running agents, or ao.task.status for workflow-managed tasks.",
        input_schema = ao_schema_for_type::<TaskControlInput>()
    )]
    async fn ao_task_pause(&self, params: Parameters<TaskControlInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_control_args("pause", input.id);
        self.run_tool("ao.task.pause", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.resume",
        description = "Resume a paused task. Purpose: Continue execution of a task that was previously paused. Prerequisites: Task must be paused. Example: {\"id\": \"TASK-001\"}. Sequencing: Use after ao.task.pause, or check status with ao.task.get first.",
        input_schema = ao_schema_for_type::<TaskControlInput>()
    )]
    async fn ao_task_resume(&self, params: Parameters<TaskControlInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_control_args("resume", input.id);
        self.run_tool("ao.task.resume", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.update",
        description = "Update task fields. Purpose: Modify task properties like title, description, priority, status, or assignee. Prerequisites: Task must exist. Example: {\"id\": \"TASK-001\", \"priority\": \"high\", \"description\": \"Updated description\"}. Sequencing: Use ao.task.get first to see current values, or ao.task.status for simple status changes.",
        input_schema = ao_schema_for_type::<TaskUpdateInput>()
    )]
    async fn ao_task_update(&self, params: Parameters<TaskUpdateInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["task".to_string(), "update".to_string(), "--id".to_string(), input.id];
        push_opt(&mut args, "--title", input.title);
        push_opt(&mut args, "--description", input.description);
        push_opt(&mut args, "--priority", input.priority);
        push_opt(&mut args, "--status", input.status);
        push_opt(&mut args, "--assignee", input.assignee);
        for entity_id in input.linked_architecture_entity {
            args.push("--linked-architecture-entity".to_string());
            args.push(entity_id);
        }
        if input.replace_linked_architecture_entities {
            args.push("--replace-linked-architecture-entities".to_string());
        }
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.task.update", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.assign",
        description = "Assign a task to a user or agent. Purpose: Set task ownership for work assignment. Prerequisites: Task must exist. Example: {\"id\": \"TASK-001\", \"assignee\": \"user@email.com\"} or {\"id\": \"TASK-001\", \"assignee\": \"agent:claude\"}. Sequencing: Use ao.task.get first to verify assignee format, or ao.task.create to create and assign in one step.",
        input_schema = ao_schema_for_type::<TaskAssignInput>()
    )]
    async fn ao_task_assign(&self, params: Parameters<TaskAssignInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "task".to_string(),
            "assign".to_string(),
            "--id".to_string(),
            input.id,
            "--assignee".to_string(),
            input.assignee,
        ];
        push_opt(&mut args, "--assignee-type", input.assignee_type);
        push_opt(&mut args, "--agent-role", input.agent_role);
        push_opt(&mut args, "--model", input.model);
        self.run_tool("ao.task.assign", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.cancel",
        description = "Cancel a task. Purpose: Stop a task and mark it as cancelled. Prerequisites: Task must exist. Warning: This may leave work incomplete. Example: {\"id\": \"TASK-001\"} or {\"id\": \"TASK-001\", \"confirm\": true, \"dry_run\": false}. Sequencing: Use ao.task.status to check current state first, or ao.agent.control to stop running agents.",
        input_schema = ao_schema_for_type::<TaskCancelInput>()
    )]
    async fn ao_task_cancel(&self, params: Parameters<TaskCancelInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["task".to_string(), "cancel".to_string(), "--id".to_string(), input.id];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.task.cancel", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.set-priority",
        description = "Set task priority. Purpose: Change the priority of a task for scheduling. Prerequisites: Task must exist. Example: {\"id\": \"TASK-001\", \"priority\": \"critical\"}. Valid priorities: critical, high, medium, low. Sequencing: Use ao.task.get first to check current priority, or ao.task.stats to see distribution.",
        input_schema = ao_schema_for_type::<TaskSetPriorityInput>()
    )]
    async fn ao_task_set_priority(&self, params: Parameters<TaskSetPriorityInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task".to_string(),
            "set-priority".to_string(),
            "--id".to_string(),
            input.id,
            "--priority".to_string(),
            input.priority,
        ];
        self.run_tool("ao.task.set-priority", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.set-deadline",
        description = "Set or clear a task deadline. Purpose: Add a due date for time-sensitive tasks. Prerequisites: Task must exist. Example: {\"id\": \"TASK-001\", \"deadline\": \"2024-12-31\"} or {\"id\": \"TASK-001\"} to clear. Sequencing: Use ao.task.get first to check, or ao.task.stats to see overdue tasks.",
        input_schema = ao_schema_for_type::<TaskSetDeadlineInput>()
    )]
    async fn ao_task_set_deadline(&self, params: Parameters<TaskSetDeadlineInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["task".to_string(), "set-deadline".to_string(), "--id".to_string(), input.id];
        push_opt(&mut args, "--deadline", input.deadline);
        self.run_tool("ao.task.set-deadline", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.checklist-add",
        description = "Add a checklist item to a task. Purpose: Track subtasks or acceptance criteria within a task. Prerequisites: Task must exist. Example: {\"id\": \"TASK-001\", \"description\": \"Write unit tests\"}. Sequencing: Use ao.task.get first to see existing checklist, or ao.task.checklist-update to toggle completion.",
        input_schema = ao_schema_for_type::<TaskChecklistAddInput>()
    )]
    async fn ao_task_checklist_add(
        &self,
        params: Parameters<TaskChecklistAddInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task".to_string(),
            "checklist-add".to_string(),
            "--id".to_string(),
            input.id,
            "--description".to_string(),
            input.description,
        ];
        self.run_tool("ao.task.checklist-add", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.checklist-update",
        description = "Mark a checklist item complete or incomplete. Purpose: Track progress on subtasks within a task. Prerequisites: Task and checklist item must exist. Example: {\"id\": \"TASK-001\", \"item_id\": \"chk-1\", \"completed\": true}. Sequencing: Use ao.task.get first to find item_id values, or ao.task.checklist-add to create items.",
        input_schema = ao_schema_for_type::<TaskChecklistUpdateInput>()
    )]
    async fn ao_task_checklist_update(
        &self,
        params: Parameters<TaskChecklistUpdateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task".to_string(),
            "checklist-update".to_string(),
            "--id".to_string(),
            input.id,
            "--item-id".to_string(),
            input.item_id,
            "--completed".to_string(),
            input.completed.to_string(),
        ];
        self.run_tool("ao.task.checklist-update", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.bulk-status",
        description = "Batch-update status for multiple tasks in one call.",
        input_schema = ao_schema_for_type::<TaskBulkStatusInput>()
    )]
    async fn ao_task_bulk_status(&self, params: Parameters<TaskBulkStatusInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        if let Err(msg) = validate_bulk_status_input("ao.task.bulk-status", &input.updates) {
            return Ok(CallToolResult::structured_error(json!({
                "tool": "ao.task.bulk-status",
                "error": msg,
            })));
        }
        let items: Vec<BatchItemExec> = input
            .updates
            .into_iter()
            .map(|item| {
                let args = build_bulk_status_item_args(&item);
                let command = args.join(" ");
                BatchItemExec { target_id: item.id, command, args }
            })
            .collect();
        self.run_batch_tool("ao.task.bulk-status", items, &input.on_error, input.project_root).await
    }

    #[tool(
        name = "ao.task.bulk-update",
        description = "Batch-update fields for multiple tasks in one call.",
        input_schema = ao_schema_for_type::<TaskBulkUpdateInput>()
    )]
    async fn ao_task_bulk_update(&self, params: Parameters<TaskBulkUpdateInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        if let Err(msg) = validate_bulk_update_input("ao.task.bulk-update", &input.updates) {
            return Ok(CallToolResult::structured_error(json!({
                "tool": "ao.task.bulk-update",
                "error": msg,
            })));
        }
        let items: Vec<BatchItemExec> = input
            .updates
            .into_iter()
            .map(|item| {
                let args = build_bulk_update_item_args(&item);
                let command = args.join(" ");
                BatchItemExec { target_id: item.id, command, args }
            })
            .collect();
        self.run_batch_tool("ao.task.bulk-update", items, &input.on_error, input.project_root).await
    }
}
