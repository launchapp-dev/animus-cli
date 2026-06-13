use serde::{Deserialize, Serialize};

/// One budget-cap breach acted on by the daemon's housekeeping sweep.
///
/// Produced by the `ProjectTickHooks::enforce_budget_caps` hook (the CLI's
/// tick services run the cost scanner + cap evaluation there) and carried on
/// [`crate::ProjectTickSummary`] so the daemon run host can fan a
/// `workflow-budget-breach` event out to notifier plugins. Each event
/// represents a breach being enforced for the first time — the hook
/// de-duplicates against already-recorded breaches, so a single breach does
/// not re-notify on every sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetBreachEvent {
    /// Empty for a fleet-level (`limit_kind: "daily"`) breach, which is not
    /// anchored to a single workflow run.
    #[serde(default)]
    pub workflow_run_id: String,
    #[serde(default)]
    pub workflow_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// `workflow`, `phase`, or `daily` (the fleet-level daily spend cap).
    pub limit_kind: String,
    /// `max_tokens` or `max_cost_usd`.
    pub limit_field: String,
    pub actual: f64,
    pub budget: f64,
    /// Declared `on_exceed` action: `pause`, `fail`, or `warn`.
    pub on_exceed: String,
    /// What the sweep actually did: `paused` (workflow paused through the
    /// standard pause path), `failed` (current phase failed terminally for
    /// `on_exceed: fail`), or `recorded` (decision records + notification
    /// only — `on_exceed: warn`, or the workflow was already terminal). For
    /// a `daily` breach this is `dispatch_paused` — the daemon stops picking
    /// up new ready subjects until spend ages out of the rolling window or
    /// the cap is raised.
    pub action: String,
    pub observed_at: String,
}
