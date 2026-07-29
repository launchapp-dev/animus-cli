use super::*;

#[tool_router(router = workflow_runtime_tools, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.workflow.list",
        description = "List workflows with optional filters (status, workflow_ref, subject_id, phase_id, search), plus sort and pagination hints. Purpose: View workflow executions and their current state. Prerequisites: None. Example: {\"status\": \"running\"} or {\"subject_id\": \"TASK-001\", \"sort\": \"started_at\"}. Sequencing: Use animus.workflow.get for specific workflow details, or animus.workflow.run to start a new workflow.",
        input_schema = ao_schema_for_type::<WorkflowListInput>()
    )]
    async fn ao_workflow_list(&self, params: Parameters<WorkflowListInput>) -> Result<CallToolResult, McpError> {
        self.workflow_list_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.run",
        description = "Run a workflow for a subject. Purpose: Execute a workflow to complete subject phases automatically. Prerequisites: Subject should exist. Pass subject_id for any subject kind (task, requirement, or dynamic kinds like blog/post) — a qualified id (task:TASK-001, blog:BLOG-001) is trusted, a bare id (TASK-001) is resolved via the subject router. Example: {\"subject_id\": \"TASK-001\"} or {\"subject_id\": \"blog:BLOG-001\", \"workflow_ref\": \"draft-post\"}. Sequencing: Use animus.subject.status to track progress, animus.workflow.get to monitor, and animus.workflow.pause/resume/cancel for control.",
        input_schema = ao_schema_for_type::<WorkflowRunInput>()
    )]
    async fn ao_workflow_run(&self, params: Parameters<WorkflowRunInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "run".to_string()];
        push_workflow_run_pipeline_arg(&mut args, input.workflow_ref);
        push_opt(&mut args, "--title", input.title);
        push_opt(&mut args, "--subject-id", input.subject_id);
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
        self.workflow_get_inproc(params.0).await
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
        self.workflow_pause_inproc(params.0).await
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
        self.workflow_cancel_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.resume",
        description = "Resume a paused workflow. Purpose: Continue execution of a paused workflow. Prerequisites: Workflow must be paused. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.pause, or animus.workflow.get to verify paused state.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_resume(&self, params: Parameters<IdInput>) -> Result<CallToolResult, McpError> {
        self.workflow_resume_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.decisions",
        description = "List workflow decisions. Purpose: View automated and manual decisions made during workflow execution. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.get to understand workflow state, or animus.workflow.checkpoints.list for phase boundaries.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_decisions(&self, params: Parameters<IdListInput>) -> Result<CallToolResult, McpError> {
        self.workflow_decisions_inproc(params.0).await
    }

    #[tool(
        name = "animus.workflow.checkpoints.list",
        description = "List workflow checkpoints. Purpose: View saved workflow states for recovery or auditing. Prerequisites: Workflow must exist. Example: {\"id\": \"wf-abc123\"}. Sequencing: Use after animus.workflow.get to see current state, or animus.workflow.decisions to understand decision history.",
        input_schema = ao_schema_for_type::<IdListInput>()
    )]
    async fn ao_workflow_checkpoints_list(&self, params: Parameters<IdListInput>) -> Result<CallToolResult, McpError> {
        self.workflow_checkpoints_list_inproc(params.0).await
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
                BatchItemExec { target_id: item.subject_id, command, args }
            })
            .collect();
        self.run_batch_tool("animus.workflow.run-multiple", items, &input.on_error, input.project_root).await
    }

    #[tool(
        name = "animus.workflow.execute",
        description = "Execute a workflow synchronously. Purpose: Run a workflow without the daemon, blocking until completion. Prerequisites: Subject must exist (use animus.subject.get to verify). Pass subject_id for any kind — a qualified id (task:TASK-001) is trusted, a bare id (TASK-001) is resolved via the subject router. Example: {\"subject_id\": \"TASK-001\"} or {\"subject_id\": \"TASK-001\", \"phase\": \"implementation\"}. Sequencing: Use animus.subject.get to verify the subject first, or animus.workflow.config.get to review workflow config.",
        input_schema = ao_schema_for_type::<WorkflowExecuteInput>()
    )]
    async fn ao_workflow_execute(&self, params: Parameters<WorkflowExecuteInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["workflow".to_string(), "run".to_string()];
        push_workflow_run_pipeline_arg(&mut args, input.workflow_ref);
        args.push("--sync".to_string());
        args.push("--subject-id".to_string());
        args.push(input.subject_id);
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
        self.workflow_phase_approve_inproc(params.0).await
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
        self.workflow_phase_reject_inproc(params.0).await
    }
}

fn push_workflow_run_pipeline_arg(args: &mut Vec<String>, workflow_ref: Option<String>) {
    if let Some(workflow_ref) = workflow_ref {
        args.push(workflow_ref);
    }
}
