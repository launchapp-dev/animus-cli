use super::*;

#[tool_router(router = output_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.output.run",
        description = "Get output for an agent run. Purpose: View stdout/stderr from an agent execution. Prerequisites: Run must exist (run_id from animus.agent.run). Example: {\"run_id\": \"abc123\"}. Sequencing: Use animus.agent.status first to check state, or animus.output.jsonl for structured logs.",
        input_schema = ao_schema_for_type::<RunIdInput>()
    )]
    async fn ao_output_run(&self, params: Parameters<RunIdInput>) -> Result<CallToolResult, McpError> {
        Ok(self.output_run_inproc(params.0))
    }

    #[tool(
        name = "animus.output.phase-outputs",
        description = "Get persisted workflow phase outputs. Purpose: Inspect structured phase payloads, decisions, and diagnostics for a workflow. Prerequisites: Workflow must have completed at least one phase. Example: {\"workflow_id\": \"wf-123\"} or {\"workflow_id\": \"wf-123\", \"phase_id\": \"unit-test\"}. Sequencing: Use after a workflow phase runs, or before diagnosis/rework phases.",
        input_schema = ao_schema_for_type::<OutputPhaseOutputsInput>()
    )]
    async fn ao_output_phase_outputs(
        &self,
        params: Parameters<OutputPhaseOutputsInput>,
    ) -> Result<CallToolResult, McpError> {
        Ok(self.output_phase_outputs_inproc(params.0))
    }

    #[tool(
        name = "animus.output.monitor",
        description = "Monitor output for a run, optionally scoped to a task or phase. Purpose: Inspect the currently persisted output from running agents as a bounded snapshot. Prerequisites: Run must exist. Example: {\"run_id\": \"abc123\"} or {\"run_id\": \"abc123\", \"task_id\": \"TASK-001\", \"phase_id\": \"implementation\"}. Sequencing: Use after animus.agent.run or animus.workflow.run to monitor progress.",
        input_schema = ao_schema_for_type::<OutputMonitorInput>()
    )]
    async fn ao_output_monitor(&self, params: Parameters<OutputMonitorInput>) -> Result<CallToolResult, McpError> {
        Ok(self.output_monitor_inproc(params.0))
    }

    #[tool(
        name = "animus.output.tail",
        description = "Get the most recent output, error, or thinking events. Purpose: Quick view of recent agent output without streaming. Prerequisites: Run or task must exist. Example: {\"run_id\": \"abc123\", \"limit\": 100} or {\"task_id\": \"TASK-001\", \"event_types\": [\"stdout\", \"stderr\"]}. Sequencing: Use after animus.agent.run to check progress, or animus.output.run for full output.",
        input_schema = ao_schema_for_type::<OutputTailInput>()
    )]
    async fn ao_output_tail(&self, params: Parameters<OutputTailInput>) -> Result<CallToolResult, McpError> {
        match build_output_tail_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "animus.output.tail",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "animus.output.tail",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "animus.output.jsonl",
        description = "Get JSONL log for an agent run. Purpose: Retrieve structured event logs for parsing or analysis. Prerequisites: Run must exist. Example: {\"run_id\": \"abc123\", \"entries\": true}. Sequencing: Use animus.output.run for human-readable output, or animus.output.artifacts for generated files.",
        input_schema = ao_schema_for_type::<OutputJsonlInput>()
    )]
    async fn ao_output_jsonl(&self, params: Parameters<OutputJsonlInput>) -> Result<CallToolResult, McpError> {
        Ok(self.output_jsonl_inproc(params.0))
    }

    #[tool(
        name = "animus.output.artifacts",
        description = "Get artifacts for an execution. Purpose: Retrieve files generated during agent execution (code, docs, etc). Prerequisites: Execution must have completed. Example: {\"execution_id\": \"exec-abc123\"}. Sequencing: Use after animus.agent.status shows completed, or animus.output.jsonl to find execution_id.",
        input_schema = ao_schema_for_type::<ExecutionIdInput>()
    )]
    async fn ao_output_artifacts(&self, params: Parameters<ExecutionIdInput>) -> Result<CallToolResult, McpError> {
        Ok(self.output_artifacts_inproc(params.0))
    }
}
