use super::*;
use rmcp::model::CallToolResult;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct CostDecisionsInput {
    /// Only show budget-cap breaches observed inside this window. Accepts
    /// `30m`, `12h`, `7d`, `2w`. Omit to list the full scoped breach log.
    #[serde(default)]
    pub(super) since: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

fn build_cost_decisions_args(input: &CostDecisionsInput) -> Vec<String> {
    let mut args = vec!["cost".to_string(), "decisions".to_string()];
    push_opt(&mut args, "--since", input.since.clone());
    args
}

#[tool_router(router = cost_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.cost.decisions",
        description = "List recorded budget-cap breaches from the scoped breach log. Purpose: Audit which workflow runs hit a configured spend cap and what enforcement decision was recorded. Works offline (reads the scoped breach log; the daemon need not be running). Prerequisites: None. Example: {} (full log) or {\"since\": \"7d\"} (recent window). Sequencing: Use after a workflow run to inspect enforcement; pair with animus.workflow.decisions for per-workflow decision history.",
        input_schema = ao_schema_for_type::<CostDecisionsInput>()
    )]
    async fn ao_cost_decisions(&self, params: Parameters<CostDecisionsInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_cost_decisions_args(&input);
        self.run_tool("animus.cost.decisions", args, input.project_root).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cost_decisions_args_defaults_minimal() {
        let args = build_cost_decisions_args(&CostDecisionsInput::default());
        assert_eq!(args, vec!["cost".to_string(), "decisions".to_string()]);
    }

    #[test]
    fn build_cost_decisions_args_wires_since() {
        let input = CostDecisionsInput { since: Some("7d".to_string()), project_root: Some("/repo".to_string()) };
        let args = build_cost_decisions_args(&input);
        assert_eq!(args, vec!["cost".to_string(), "decisions".to_string(), "--since".to_string(), "7d".to_string(),]);
    }
}
