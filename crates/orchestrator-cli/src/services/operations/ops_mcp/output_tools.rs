use super::*;

#[tool_router(router = output_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "ao.output.run",
        description = "Get output for an agent run. Purpose: View stdout/stderr from an agent execution. Prerequisites: Run must exist (run_id from ao.agent.run). Example: {\"run_id\": \"abc123\"}. Sequencing: Use ao.agent.status first to check state, or ao.output.jsonl for structured logs.",
        input_schema = ao_schema_for_type::<RunIdInput>()
    )]
    async fn ao_output_run(&self, params: Parameters<RunIdInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec!["output".to_string(), "run".to_string(), "--run-id".to_string(), input.run_id];
        self.run_tool("ao.output.run", args, input.project_root).await
    }

    #[tool(
        name = "ao.output.phase-outputs",
        description = "Get persisted workflow phase outputs. Purpose: Inspect structured phase payloads, decisions, and diagnostics for a workflow. Prerequisites: Workflow must have completed at least one phase. Example: {\"workflow_id\": \"wf-123\"} or {\"workflow_id\": \"wf-123\", \"phase_id\": \"unit-test\"}. Sequencing: Use after a workflow phase runs, or before diagnosis/rework phases.",
        input_schema = ao_schema_for_type::<OutputPhaseOutputsInput>()
    )]
    async fn ao_output_phase_outputs(
        &self,
        params: Parameters<OutputPhaseOutputsInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args =
            vec!["output".to_string(), "phase-outputs".to_string(), "--workflow-id".to_string(), input.workflow_id];
        push_opt(&mut args, "--phase-id", input.phase_id);
        self.run_tool("ao.output.phase-outputs", args, input.project_root).await
    }

    #[tool(
        name = "ao.output.monitor",
        description = "Monitor output for a run, task, or phase. Purpose: Stream real-time output from running agents. Prerequisites: Run/task/phase must be active. Example: {\"run_id\": \"abc123\"} or {\"task_id\": \"TASK-001\", \"phase_id\": \"implementation\"}. Sequencing: Use after ao.agent.run or ao.workflow.run to monitor progress.",
        input_schema = ao_schema_for_type::<OutputMonitorInput>()
    )]
    async fn ao_output_monitor(&self, params: Parameters<OutputMonitorInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["output".to_string(), "monitor".to_string(), "--run-id".to_string(), input.run_id];
        push_opt(&mut args, "--task-id", input.task_id);
        push_opt(&mut args, "--phase-id", input.phase_id);
        self.run_tool("ao.output.monitor", args, input.project_root).await
    }

    #[tool(
        name = "ao.output.tail",
        description = "Get the most recent output, thinking, tool, artifact, metadata, or finished events. Purpose: Quick view of recent structured agent activity without streaming. Prerequisites: Run or task must exist. Example: {\"run_id\": \"abc123\", \"limit\": 100} or {\"task_id\": \"TASK-001\", \"event_types\": [\"tool_result\", \"artifact\"]}. Sequencing: Use after ao.agent.run to check progress, or ao.output.run for full output.",
        input_schema = ao_schema_for_type::<OutputTailInput>()
    )]
    async fn ao_output_tail(&self, params: Parameters<OutputTailInput>) -> Result<CallToolResult, McpError> {
        match build_output_tail_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "ao.output.tail",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "ao.output.tail",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "ao.output.jsonl",
        description = "Get JSONL log for an agent run. Purpose: Retrieve structured event logs for parsing or analysis. Prerequisites: Run must exist. Example: {\"run_id\": \"abc123\", \"entries\": true}. Sequencing: Use ao.output.run for human-readable output, or ao.output.artifacts for generated files.",
        input_schema = ao_schema_for_type::<OutputJsonlInput>()
    )]
    async fn ao_output_jsonl(&self, params: Parameters<OutputJsonlInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["output".to_string(), "jsonl".to_string(), "--run-id".to_string(), input.run_id];
        if input.entries {
            args.push("--entries".to_string());
        }
        self.run_tool("ao.output.jsonl", args, input.project_root).await
    }

    #[tool(
        name = "ao.output.artifacts",
        description = "Get artifacts for an execution. Purpose: Retrieve files generated during agent execution (code, docs, etc). Prerequisites: Execution must have completed. Example: {\"execution_id\": \"exec-abc123\"}. Sequencing: Use after ao.agent.status shows completed, or ao.output.jsonl to find execution_id.",
        input_schema = ao_schema_for_type::<ExecutionIdInput>()
    )]
    async fn ao_output_artifacts(&self, params: Parameters<ExecutionIdInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args =
            vec!["output".to_string(), "artifacts".to_string(), "--execution-id".to_string(), input.execution_id];
        self.run_tool("ao.output.artifacts", args, input.project_root).await
    }
}
