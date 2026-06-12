use super::*;

#[tool_router(router = workflow_runtime_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.workflow.list",
        description = "List workflows with optional filters (status, workflow_ref, task_id, phase_id, search), plus sort and pagination hints. Purpose: View workflow executions and their current state. Prerequisites: None. Example: {\"status\": \"running\"} or {\"task_id\": \"TASK-001\", \"sort\": \"started_at\"}. Sequencing: Use animus.workflow.get for specific workflow details, or animus.workflow.run to start a new workflow.",
        input_schema = ao_schema_for_type::<WorkflowListInput>()
    )]
    async fn ao_workflow_list(&self, params: Parameters<WorkflowListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        self.run_list_tool(
            "animus.workflow.list",
            build_workflow_list_args(&input),
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "animus.workflow.run",
        description = "Run a workflow for a task. Purpose: Execute a workflow to complete task phases automatically. Prerequisites: Task should exist (use animus.subject.get with {\"kind\":\"task\"} to verify). Example: {\"task_id\": \"TASK-001\"} or {\"task_id\": \"TASK-001\", \"workflow_ref\": \"default\"}. Sequencing: Use animus.subject.status with {\"kind\":\"task\", ...} to track progress, animus.workflow.get to monitor, and animus.workflow.pause/resume/cancel for control.",
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
        self.run_tool("animus.workflow.run", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.get",
        description = "Get workflow details by ID. Purpose: View full workflow state including current phase, decisions, and checkpoints. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.list to find workflows, or animus.workflow.run to start new ones.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_get(&self, params: Parameters<IdInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "get".to_string(), "--id".to_string(), input.id];
        self.run_tool("animus.workflow.get", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.pause",
        description = "Pause a running workflow. Purpose: Temporarily halt workflow execution without cancelling. Prerequisites: Workflow must be running. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use animus.workflow.get to check status first, then animus.workflow.resume to continue.",
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
        self.run_tool("animus.workflow.pause", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.cancel",
        description = "Cancel a running workflow. Purpose: Stop a workflow permanently. Prerequisites: Workflow must be running. Warning: This terminates all phases. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use animus.workflow.get to check status first, or animus.output.artifacts to save any generated artifacts.",
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
        self.run_tool("animus.workflow.cancel", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.resume",
        description = "Resume a paused workflow. Purpose: Continue execution of a paused workflow. Prerequisites: Workflow must be paused. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.pause, or animus.workflow.get to verify paused state.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_resume(&self, params: Parameters<IdInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "resume".to_string(), "--id".to_string(), input.id];
        self.run_tool("animus.workflow.resume", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.decisions",
        description = "List workflow decisions. Purpose: View automated and manual decisions made during workflow execution. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.get to understand workflow state, or animus.workflow.checkpoints.list for phase boundaries.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_decisions(&self, params: Parameters<IdListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["workflow".to_string(), "decisions".to_string(), "--id".to_string(), input.id];
        self.run_list_tool(
            "animus.workflow.decisions",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "animus.workflow.checkpoints.list",
        description = "List workflow checkpoints. Purpose: View saved workflow states for recovery or auditing. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.get to see current state, or animus.workflow.decisions to understand decision history.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_checkpoints_list(&self, params: Parameters<IdListInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args =
            vec!["workflow".to_string(), "checkpoints".to_string(), "list".to_string(), "--id".to_string(), input.id];
        self.run_list_tool(
            "animus.workflow.checkpoints.list",
            args,
            input.project_root,
            ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens },
        )
        .await
    }

    #[tool(
        name = "animus.workflow.run-multiple",
        description = "Run a workflow for multiple tasks in one call.",
        input_schema = ao_schema_for_type::<WorkflowRunMultipleInput>()
    )]
    async fn ao_workflow_run_multiple(
        &self,
        params: Parameters<WorkflowRunMultipleInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        if let Err(msg) = validate_workflow_run_multiple_input("animus.workflow.run-multiple", &input.runs) {
            return Ok(CallToolResult::structured_error(json!({
                "tool": "animus.workflow.run-multiple",
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
        self.run_batch_tool("animus.workflow.run-multiple", items, &input.on_error, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.execute",
        description = "Execute a workflow synchronously. Purpose: Run a workflow without the daemon, blocking until completion. Prerequisites: Task must exist (use animus.subject.get with {\"kind\":\"task\"} to verify). Example: {\"task_id\": \"TASK-001\"} or {\"task_id\": \"TASK-001\", \"phase\": \"implementation\"}. Sequencing: Use animus.subject.get to verify the task first, or animus.workflow.config.get to review workflow config.",
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
        self.run_tool("animus.workflow.execute", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.phase.approve",
        description = "Approve a gated workflow phase. Purpose: Unblock gate phases that require manual approval before proceeding. Prerequisites: Workflow must have a pending gate phase. Example: {\"workflow_id\": \"wf-abc123\", \"phase_id\": \"po-review\"} or {\"workflow_id\": \"wf-abc123\", \"phase_id\": \"po-review\", \"feedback\": \"Approved\"}. Sequencing: Use animus.workflow.get first to see pending gates, then animus.workflow.phase.approve to unblock.",
        input_schema = ao_schema_for_type::<WorkflowPhaseApproveInput>()
    )]
    async fn ao_workflow_phase_approve(
        &self,
        params: Parameters<WorkflowPhaseApproveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_workflow_phase_approve_args(&input);
        self.run_tool("animus.workflow.phase.approve", args, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.phase.reject",
        description = "Reject a gated workflow phase. Purpose: Decline a gate phase that requires manual sign-off, recording the rejection note. Prerequisites: Workflow must have a pending gate phase (use animus.workflow.get to confirm a gate is pending). Example: {\"workflow_id\": \"wf-abc123\", \"phase_id\": \"po-review\", \"reason\": \"Spec mismatch\"}. Sequencing: Use animus.workflow.get first to see pending gates; mirror of animus.workflow.phase.approve for the decline path.",
        input_schema = ao_schema_for_type::<WorkflowPhaseRejectInput>()
    )]
    async fn ao_workflow_phase_reject(
        &self,
        params: Parameters<WorkflowPhaseRejectInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_workflow_phase_reject_args(&input);
        self.run_tool("animus.workflow.phase.reject", args, input.project_root).await
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

fn build_workflow_phase_reject_args(input: &WorkflowPhaseRejectInput) -> Vec<String> {
    vec![
        "workflow".to_string(),
        "phase".to_string(),
        "reject".to_string(),
        "--id".to_string(),
        input.workflow_id.clone(),
        "--phase".to_string(),
        input.phase_id.clone(),
        "--note".to_string(),
        input.reason.clone(),
    ]
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

    #[test]
    fn build_workflow_phase_reject_args_uses_cli_flags() {
        let input = WorkflowPhaseRejectInput {
            workflow_id: "wf-123".to_string(),
            phase_id: "review".to_string(),
            reason: "Spec mismatch".to_string(),
            project_root: None,
        };

        let args = build_workflow_phase_reject_args(&input);

        assert_eq!(
            args,
            vec![
                "workflow".to_string(),
                "phase".to_string(),
                "reject".to_string(),
                "--id".to_string(),
                "wf-123".to_string(),
                "--phase".to_string(),
                "review".to_string(),
                "--note".to_string(),
                "Spec mismatch".to_string(),
            ]
        );
    }
}
