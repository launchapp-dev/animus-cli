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

#[tool_router(router = cost_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.cost.decisions",
        description = "List recorded budget-cap breaches from the scoped breach log. Purpose: Audit which workflow runs hit a configured spend cap and what enforcement decision was recorded. Works offline (reads the scoped breach log; the daemon need not be running). Prerequisites: None. Example: {} (full log) or {\"since\": \"7d\"} (recent window). Sequencing: Use after a workflow run to inspect enforcement; pair with animus.workflow.decisions for per-workflow decision history.",
        input_schema = ao_schema_for_type::<CostDecisionsInput>()
    )]
    async fn ao_cost_decisions(&self, params: Parameters<CostDecisionsInput>) -> Result<CallToolResult, McpError> {
        Ok(self.cost_decisions_inproc(params.0))
    }

    #[tool(
        name = "animus.budget.get",
        description = "Read the fleet budget posture. Purpose: See the fleet daily spend cap (max_daily_usd), today's rolling-24h spend, remaining headroom, whether the cap is exceeded, whether the daemon has paused dispatch on the cap, and every configured per-workflow / per-phase budget cap. Works offline (reads scoped cost-state + compiled workflow config; the daemon need not be running). Prerequisites: None. Example: {}. Sequencing: Use animus.budget.set to change the fleet cap; pair with animus.daemon.health to see the same daily_cap in the health snapshot.",
        input_schema = ao_schema_for_type::<BudgetGetInput>()
    )]
    async fn ao_budget_get(&self, params: Parameters<BudgetGetInput>) -> Result<CallToolResult, McpError> {
        Ok(self.budget_get_inproc(params.0.project_root))
    }

    #[tool(
        name = "animus.budget.set",
        description = "Set or clear the fleet daily spend cap (admin). Purpose: Bound the daemon's TOTAL rolling-24h spend; when crossed the daemon pauses new dispatch until spend ages out of the window or the cap is raised. Uses the same typed daemon-configuration service and is hot-reloaded by the running daemon. Prerequisites: None. Example: {\"max_daily_usd\": 25.0} to cap at $25/day, or {\"clear\": true} to remove the cap. Sequencing: Use animus.budget.get to read the current cap first.",
        input_schema = ao_schema_for_type::<BudgetSetInput>()
    )]
    async fn ao_budget_set(&self, params: Parameters<BudgetSetInput>) -> Result<CallToolResult, McpError> {
        Ok(self.budget_set_inproc(params.0))
    }
}
