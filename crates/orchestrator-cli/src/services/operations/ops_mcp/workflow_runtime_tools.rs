use super::*;

#[tool_router(router = workflow_runtime_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.workflow.list",
        description = "List workflows with optional filters (status, workflow_ref, task_id, phase_id, search), plus sort and pagination hints. Purpose: View workflow executions and their current state. Prerequisites: None. Example: {\"status\": \"running\"} or {\"task_id\": \"TASK-001\", \"sort\": \"started_at\"}. Sequencing: Use ao.workflow.get for specific workflow details, or ao.workflow.run to start a new workflow.",
        input_schema = ao_schema_for_type::<WorkflowListInput>()
    )]
    async fn ao_workflow_list(&self, params: Parameters<WorkflowListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_list_tool(
            "ao.workflow.list",
            build_workflow_list_args(&input),
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "ao.workflow.run",
        description = "Run a workflow for a task. Purpose: Execute a workflow to complete task phases automatically. Prerequisites: Task should exist (use ao.task.get to verify). Example: {\"task_id\": \"TASK-001\"} or {\"task_id\": \"TASK-001\", \"workflow_ref\": \"default\"}. Sequencing: Use ao.task.status to track progress, ao.workflow.get to monitor, ao.workflow.pause/resume/cancel for control.",
        input_schema = ao_schema_for_type::<WorkflowRunInput>()
    )]
    async fn ao_workflow_run(&self, params: Parameters<WorkflowRunInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "run".to_string()];
        push_workflow_run_pipeline_arg(&mut args, input.workflow_ref);
        push_opt(&mut args, "--task-id", input.task_id);
        push_opt(&mut args, "--requirement-id", input.requirement_id);
        push_opt(&mut args, "--title", input.title);
        push_opt(&mut args, "--description", input.description);
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.workflow.run", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.get",
        description = "Get workflow details by ID. Purpose: View full workflow state including current phase, decisions, and checkpoints. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.list to find workflows, or ao.workflow.run to start new ones.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_get(&self, params: Parameters<IdInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "get".to_string(), "--id".to_string(), input.id];
        self.run_tool("ao.workflow.get", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.pause",
        description = "Pause a running workflow. Purpose: Temporarily halt workflow execution without cancelling. Prerequisites: Workflow must be running. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use ao.workflow.get to check status first, then ao.workflow.resume to continue.",
        input_schema = ao_schema_for_type::<WorkflowDestructiveInput>()
    )]
    async fn ao_workflow_pause(
        &self,
        params: Parameters<WorkflowDestructiveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "pause".to_string(), "--id".to_string(), input.id];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.workflow.pause", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.cancel",
        description = "Cancel a running workflow. Purpose: Stop a workflow permanently. Prerequisites: Workflow must be running. Warning: This terminates all phases. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use ao.workflow.get to check status first, or ao.output.artifacts to save any generated artifacts.",
        input_schema = ao_schema_for_type::<WorkflowDestructiveInput>()
    )]
    async fn ao_workflow_cancel(
        &self,
        params: Parameters<WorkflowDestructiveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "cancel".to_string(), "--id".to_string(), input.id];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.workflow.cancel", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.resume",
        description = "Resume a paused workflow. Purpose: Continue execution of a paused workflow. Prerequisites: Workflow must be paused. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.pause, or ao.workflow.get to verify paused state.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_resume(&self, params: Parameters<IdInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "resume".to_string(), "--id".to_string(), input.id];
        self.run_tool("ao.workflow.resume", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.decisions",
        description = "List workflow decisions. Purpose: View automated and manual decisions made during workflow execution. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.get to understand workflow state, or ao.workflow.checkpoints.list for phase boundaries.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_decisions(&self, params: Parameters<IdListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "decisions".to_string(), "--id".to_string(), input.id];
        self.run_list_tool(
            "ao.workflow.decisions",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "ao.workflow.checkpoints.list",
        description = "List workflow checkpoints. Purpose: View saved workflow states for recovery or auditing. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.get to see current state, or ao.workflow.decisions to understand decision history.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_checkpoints_list(&self, params: Parameters<IdListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args =
            vec!["workflow".to_string(), "checkpoints".to_string(), "list".to_string(), "--id".to_string(), input.id];
        self.run_list_tool(
            "ao.workflow.checkpoints.list",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "ao.workflow.run-multiple",
        description = "Run a workflow for multiple tasks in one call.",
        input_schema = ao_schema_for_type::<WorkflowRunMultipleInput>()
    )]
    async fn ao_workflow_run_multiple(
        &self,
        params: Parameters<WorkflowRunMultipleInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        if let Err(msg) = validate_workflow_run_multiple_input("ao.workflow.run-multiple", &input.runs) {
            return Ok(CallToolResult::structured_error(json!({
                "tool": "ao.workflow.run-multiple",
                "error": msg,
            })));
        }
        let items: Vec<BatchItemExec> = input
            .runs
            .into_iter()
            .map(|item| {
                let args = build_bulk_workflow_run_item_args(&item);
                let command = args.join(" ");
                BatchItemExec { target_id: item.task_id, command, args }
            })
            .collect();
        self.run_batch_tool("ao.workflow.run-multiple", items, &input.on_error, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.execute",
        description = "Execute a workflow synchronously. Purpose: Run a workflow without the daemon, blocking until completion. Prerequisites: Task must exist (use ao.task.get to verify). Example: {\"task_id\": \"TASK-001\"} or {\"task_id\": \"TASK-001\", \"phase\": \"implementation\"}. Sequencing: Use ao.task.get to verify the task first, or ao.workflow.config.get to review workflow config.",
        input_schema = ao_schema_for_type::<WorkflowExecuteInput>()
    )]
    async fn ao_workflow_execute(&self, params: Parameters<WorkflowExecuteInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "run".to_string()];
        push_workflow_run_pipeline_arg(&mut args, input.workflow_ref);
        args.push("--sync".to_string());
        args.push("--task-id".to_string());
        args.push(input.task_id);
        push_opt(&mut args, "--phase", input.phase);
        push_opt(&mut args, "--model", input.model);
        push_opt(&mut args, "--tool", input.tool);
        push_opt_num(&mut args, "--phase-timeout-secs", input.phase_timeout_secs);
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.workflow.execute", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.phase.approve",
        description = "Approve a gated workflow phase. Purpose: Unblock gate phases that require manual approval before proceeding. Prerequisites: Workflow must have a pending gate phase. Example: {\"workflow_id\": \"wf-abc123\", \"phase_id\": \"po-review\"} or {\"workflow_id\": \"wf-abc123\", \"phase_id\": \"po-review\", \"feedback\": \"Approved\"}. Sequencing: Use ao.workflow.get first to see pending gates, then ao.workflow.phase.approve to unblock.",
        input_schema = ao_schema_for_type::<WorkflowPhaseApproveInput>()
    )]
    async fn ao_workflow_phase_approve(
        &self,
        params: Parameters<WorkflowPhaseApproveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_workflow_phase_approve_args(&input);
        self.run_tool("ao.workflow.phase.approve", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.notes.add",
        description = "Add a note to a workflow. Purpose: Attach timestamped text notes to workflow timeline for annotations and operator comments. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\", \"text\": \"This phase needs review\"}. Sequencing: Use ao.workflow.notes.list to view all notes on a workflow.",
        input_schema = ao_schema_for_type::<WorkflowNoteAddInput>()
    )]
    async fn ao_workflow_notes_add(
        &self,
        params: Parameters<WorkflowNoteAddInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "note".to_string(),
            "add".to_string(),
            "--id".to_string(),
            input.id,
            "--text".to_string(),
            input.text,
        ];
        self.run_tool("ao.workflow.notes.add", args, input.project_root).await
    }

    #[tool(
        name = "ao.workflow.notes.list",
        description = "List notes for a workflow. Purpose: View all timestamped text notes attached to a workflow timeline. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use ao.workflow.get to view full workflow state, or ao.workflow.notes.add to add new notes.",
        input_schema = ao_schema_for_type::<WorkflowNoteListInput>()
    )]
    async fn ao_workflow_notes_list(
        &self,
        params: Parameters<WorkflowNoteListInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "note".to_string(), "list".to_string(), "--id".to_string(), input.id];
        self.run_list_tool(
            "ao.workflow.notes.list",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }
}

fn push_workflow_run_pipeline_arg(args: &mut Vec<String>, workflow_ref: Option<String>) {
    if let Some(workflow_ref) = workflow_ref {
        args.push(workflow_ref);
    }
}

fn build_workflow_phase_approve_args(input: &WorkflowPhaseApproveInput) -> Vec<String> {
    let mut args = vec![
        "workflow".to_string(),
        "phase".to_string(),
        "approve".to_string(),
        "--id".to_string(),
        input.workflow_id.clone(),
    ];
    args.push("--phase".to_string());
    args.push(input.phase_id.clone());
    push_opt(&mut args, "--note", input.feedback.clone());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_workflow_phase_approve_args_uses_cli_flags() {
        let input = WorkflowPhaseApproveInput {
            workflow_id: "wf-123".to_string(),
            phase_id: "review".to_string(),
            feedback: Some("Approved".to_string()),
            project_root: None,
        };

        let args = build_workflow_phase_approve_args(&input);

        assert_eq!(
            args,
            vec![
                "workflow".to_string(),
                "phase".to_string(),
                "approve".to_string(),
                "--id".to_string(),
                "wf-123".to_string(),
                "--phase".to_string(),
                "review".to_string(),
                "--note".to_string(),
                "Approved".to_string(),
            ]
        );
    }
}
