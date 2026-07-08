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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct BudgetGetInput {
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct BudgetSetInput {
    /// New fleet daily spend cap in USD. Omit to clear the cap (with
    /// `clear: true`). Mutually exclusive with `clear`.
    #[serde(default)]
    pub(super) max_daily_usd: Option<f64>,
    /// Clear the fleet daily spend cap (persists an explicit "uncapped"
    /// override that also suppresses any workflow-YAML `daemon.budget` cap).
    #[serde(default)]
    pub(super) clear: bool,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

/// Build the `daemon config` args for `budget_set`, or an error message when
/// the caller passed neither / both of `max_daily_usd` and `clear`.
fn build_budget_set_args(input: &BudgetSetInput) -> Result<Vec<String>, String> {
    let mut args = vec!["daemon".to_string(), "config".to_string()];
    match (input.max_daily_usd, input.clear) {
        (Some(_), true) => Err("pass either max_daily_usd or clear, not both".to_string()),
        (Some(value), false) => {
            args.push("--max-daily-usd".to_string());
            args.push(value.to_string());
            Ok(args)
        }
        (None, true) => {
            // An explicit 0 persists as "uncapped" (see daily_cap::read_max_daily_usd).
            args.push("--max-daily-usd".to_string());
            args.push("0".to_string());
            Ok(args)
        }
        (None, false) => Err("provide max_daily_usd (USD cap) or clear=true".to_string()),
    }
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

    #[tool(
        name = "animus.budget.get",
        description = "Read the fleet budget posture. Purpose: See the fleet daily spend cap (max_daily_usd), today's rolling-24h spend, remaining headroom, whether the cap is exceeded, whether the daemon has paused dispatch on the cap, and every configured per-workflow / per-phase budget cap. Works offline (reads scoped cost-state + compiled workflow config; the daemon need not be running). Prerequisites: None. Example: {}. Sequencing: Use animus.budget.set to change the fleet cap; pair with animus.daemon.health to see the same daily_cap in the health snapshot.",
        input_schema = ao_schema_for_type::<BudgetGetInput>()
    )]
    async fn ao_budget_get(&self, params: Parameters<BudgetGetInput>) -> Result<CallToolResult, McpError> {
        let project_root =
            super::daemon_inproc::resolve_project_root(&self.default_project_root, params.0.project_root);
        let report = crate::services::cost::build_budget_report(Path::new(&project_root));
        let payload = json!({
            "tool": "animus.budget.get",
            "result": serde_json::to_value(report).unwrap_or(Value::Null),
        });
        Ok(CallToolResult::structured(payload))
    }

    #[tool(
        name = "animus.budget.set",
        description = "Set or clear the fleet daily spend cap (admin). Purpose: Bound the daemon's TOTAL rolling-24h spend; when crossed the daemon pauses new dispatch until spend ages out of the window or the cap is raised. Wraps `daemon config --max-daily-usd`; hot-reloaded by the running daemon. Prerequisites: None. Example: {\"max_daily_usd\": 25.0} to cap at $25/day, or {\"clear\": true} to remove the cap. Sequencing: Use animus.budget.get to read the current cap first.",
        input_schema = ao_schema_for_type::<BudgetSetInput>()
    )]
    async fn ao_budget_set(&self, params: Parameters<BudgetSetInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        match build_budget_set_args(&input) {
            Ok(args) => self.run_tool("animus.budget.set", args, input.project_root).await,
            Err(message) => Ok(CallToolResult::structured_error(json!({
                "tool": "animus.budget.set",
                "error": message,
            }))),
        }
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

    #[test]
    fn budget_set_args_wires_max_daily_usd() {
        let input = BudgetSetInput { max_daily_usd: Some(25.0), clear: false, project_root: None };
        let args = build_budget_set_args(&input).expect("valid");
        assert_eq!(args, vec!["daemon", "config", "--max-daily-usd", "25"]);
    }

    #[test]
    fn budget_set_args_clear_persists_zero() {
        let input = BudgetSetInput { max_daily_usd: None, clear: true, project_root: None };
        let args = build_budget_set_args(&input).expect("valid");
        assert_eq!(args, vec!["daemon", "config", "--max-daily-usd", "0"]);
    }

    #[test]
    fn budget_set_args_rejects_empty_and_conflicting_input() {
        let empty = BudgetSetInput::default();
        assert!(build_budget_set_args(&empty).is_err(), "neither cap nor clear must error");
        let both = BudgetSetInput { max_daily_usd: Some(10.0), clear: true, project_root: None };
        assert!(build_budget_set_args(&both).is_err(), "cap + clear together must error");
    }
}
