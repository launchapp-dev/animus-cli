use super::*;

#[tool_router(router = workflow_runtime_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.workflow.list",
        description = "List workflows. Purpose: View workflow executions and their current state. Prerequisites: None. Example: {\"limit\": 10} or {\"status\": \"running\"}. Sequencing: Use ao.workflow.get for specific workflow details, or ao.workflow.run to start a new workflow.",
        input_schema = ao_schema_for_type::<PaginatedProjectRootInput>()
    )]
    async fn ao_workflow_list(
        &self,
        params: Parameters<PaginatedProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_list_tool(
            "ao.workflow.list",
            vec!["workflow".to_string(), "list".to_string()],
            input.project_root,
            ListGuardInput {
                limit: input.limit,
                offset: input.offset,
                max_tokens: input.max_tokens,
            },
        )
        .await
    }

    #[tool(
        name = "ao.workflow.run",
        description = "Run a workflow for a task. Purpose: Execute a workflow to complete task phases automatically. Prerequisites: Task should exist (use ao.task.get to verify). Example: {\"task_id\": \"TASK-001\"} or {\"task_id\": \"TASK-001\", \"workflow_ref\": \"default\"}. Sequencing: Use ao.task.status to track progress, ao.workflow.get to monitor, ao.workflow.pause/resume/cancel for control.",
        input_schema = ao_schema_for_type::<WorkflowRunInput>()
    )]
    async fn ao_workflow_run(
        &self,
        params: Parameters<WorkflowRunInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "run".to_string()];
        push_opt(&mut args, "--task-id", input.task_id);
        push_opt(&mut args, "--requirement-id", input.requirement_id);
        push_opt(&mut args, "--title", input.title);
        push_opt(&mut args, "--description", input.description);
        push_opt(&mut args, "--workflow-ref", input.workflow_ref);
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.workflow.run", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.get",
        description = "Get workflow details by ID. Purpose: View full workflow state including current phase, decisions, and checkpoints. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.list to find workflows, or ao.workflow.run to start new ones.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_get(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "get".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.get", args, input.project_root)
            .await
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
        let mut args = vec![
            "workflow".to_string(),
            "pause".to_string(),
            "--id".to_string(),
            input.id,
        ];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.workflow.pause", args, input.project_root)
            .await
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
        let mut args = vec![
            "workflow".to_string(),
            "cancel".to_string(),
            "--id".to_string(),
            input.id,
        ];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.workflow.cancel", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.resume",
        description = "Resume a paused workflow. Purpose: Continue execution of a paused workflow. Prerequisites: Workflow must be paused. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.pause, or ao.workflow.get to verify paused state.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_resume(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "resume".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.resume", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.decisions",
        description = "List workflow decisions. Purpose: View automated and manual decisions made during workflow execution. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.get to understand workflow state, or ao.workflow.checkpoints.list for phase boundaries.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_decisions(
        &self,
        params: Parameters<IdListInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "decisions".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_list_tool(
            "ao.workflow.decisions",
            args,
            input.project_root,
            ListGuardInput {
                limit: input.limit,
                offset: input.offset,
                max_tokens: input.max_tokens,
            },
        )
        .await
    }

    #[tool(
        name = "ao.workflow.checkpoints.list",
        description = "List workflow checkpoints. Purpose: View saved workflow states for recovery or auditing. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.get to see current state, or ao.workflow.decisions to understand decision history.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_checkpoints_list(
        &self,
        params: Parameters<IdListInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "checkpoints".to_string(),
            "list".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_list_tool(
            "ao.workflow.checkpoints.list",
            args,
            input.project_root,
            ListGuardInput {
                limit: input.limit,
                offset: input.offset,
                max_tokens: input.max_tokens,
            },
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
        if let Err(msg) =
            validate_workflow_run_multiple_input("ao.workflow.run-multiple", &input.runs)
        {
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
                BatchItemExec {
                    target_id: item.task_id,
                    command,
                    args,
                }
            })
            .collect();
        self.run_batch_tool(
            "ao.workflow.run-multiple",
            items,
            &input.on_error,
            input.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.workflow.execute",
        description = "Execute a workflow synchronously. Purpose: Run a workflow without the daemon, blocking until completion. Prerequisites: Task must exist (use ao.task.get to verify). Example: {\"task_id\": \"TASK-001\"} or {\"task_id\": \"TASK-001\", \"phase\": \"implementation\"}. Sequencing: Use ao.task.get to verify the task first, or ao.workflow.config.get to review workflow config.",
        input_schema = ao_schema_for_type::<WorkflowExecuteInput>()
    )]
    async fn ao_workflow_execute(
        &self,
        params: Parameters<WorkflowExecuteInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "workflow".to_string(),
            "execute".to_string(),
            "--task-id".to_string(),
            input.task_id,
        ];
        push_opt(&mut args, "--workflow-ref", input.workflow_ref);
        push_opt(&mut args, "--phase", input.phase);
        push_opt(&mut args, "--model", input.model);
        push_opt(&mut args, "--tool", input.tool);
        push_opt_num(&mut args, "--phase-timeout-secs", input.phase_timeout_secs);
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.workflow.execute", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.context",
        description = "Get current workflow execution context. Purpose: Retrieve current phase, pipeline state, rework counts, and last decision without reading raw logs. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.list to find workflows; use ao.workflow.last-phase for the last phase artifact.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_context(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "context".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.context", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.last-phase",
        description = "Get the last completed or running phase result. Purpose: Retrieve the last phase ID, status, attempt, timestamps, error, and phase decision — enabling failure inspection without log archaeology. Prerequisites: Workflow must exist and have at least one executed phase. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after ao.workflow.context to drill into the last phase detail.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_last_phase(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "last-phase".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.last-phase", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.phase.approve",
        description = "Approve a gated workflow phase. Purpose: Unblock gate phases that require manual approval before proceeding. Prerequisites: Workflow must have a pending gate phase. Example: {\"workflow_id\": \"wf-abc123\"} or {\"workflow_id\": \"wf-abc123\", \"phase_id\": \"po-review\", \"feedback\": \"Approved\"}. Sequencing: Use ao.workflow.get first to see pending gates, then ao.workflow.phase.approve to unblock.",
        input_schema = ao_schema_for_type::<WorkflowPhaseApproveInput>()
    )]
    async fn ao_workflow_phase_approve(
        &self,
        params: Parameters<WorkflowPhaseApproveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "workflow".to_string(),
            "phase".to_string(),
            "approve".to_string(),
            "--id".to_string(),
            input.workflow_id,
        ];
        push_opt(&mut args, "--phase-id", input.phase_id);
        push_opt(&mut args, "--feedback", input.feedback);
        self.run_tool("ao.workflow.phase.approve", args, input.project_root)
            .await
    }
}
