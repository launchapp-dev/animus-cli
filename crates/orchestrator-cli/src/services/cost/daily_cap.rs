//! Fleet-level daily spend cap.
//!
//! Today's budgets are declared per-workflow / per-phase in workflow YAML
//! ([`super::scanner::evaluate_caps`]). Those caps stop a single runaway
//! workflow, but nothing bounds the FLEET: a hundred small in-budget
//! workflows can still burn through an operator's daily wallet. This module
//! adds that wallet kill-switch.
//!
//! Config surface (first match wins):
//!
//! 1. `max_daily_usd` in the scoped daemon runtime config
//!    (`~/.animus/<repo-scope>/daemon/pm-config.json`, written by
//!    `animus daemon config --max-daily-usd <N>`), and
//! 2. `daemon.budget.max_cost_usd_per_day` in workflow YAML.
//!
//! Window semantics: a ROLLING 24-hour window, not a calendar day. The
//! cost-state only carries `started_at` / `updated_at` / `finished_at`
//! timestamps (no per-day buckets), and `animus cost summary` already
//! aggregates over a rolling window. Reusing that aggregation avoids a
//! timezone decision entirely and makes the resume path fall out for free:
//! as spend ages past the 24h horizon the rolling total drops back under
//! the cap and dispatch resumes on the next sweep. Raising or clearing the
//! cap resumes immediately on the next sweep too, since the cap is read
//! fresh each time.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::aggregator::CostState;
use super::aggregator::{BudgetExceededRecord, BudgetLimitField, BudgetLimitKind, BUDGET_EXCEEDED_SCHEMA_ID};
use super::persistence::{append_decision_record, scoped_root};

/// File holding the daemon's daily-cap latch, alongside `cost-state.v1.json`.
pub(crate) const DAILY_CAP_STATE_FILE_NAME: &str = "daily-cap.v1.json";
pub(crate) const DAILY_CAP_STATE_SCHEMA_ID: &str = "animus.daily-cap.v1";

/// Synthetic workflow-run id used for the fleet-level breach record (the
/// daily cap is not anchored to one workflow run).
pub(crate) const DAILY_CAP_RUN_ID: &str = "fleet:daily-cap";
const ACTION_DISPATCH_PAUSED: &str = "dispatch_paused";
const ON_EXCEED_PAUSE: &str = "pause";

/// Rolling window over which the daily cap is measured.
pub(crate) const DAILY_WINDOW_HOURS: i64 = 24;

/// Read the configured fleet daily spend cap in USD, or `None` when no cap
/// is declared. The scoped daemon runtime config (`max_daily_usd`) is
/// authoritative when the key is PRESENT — even a non-positive value, which
/// reads as "explicitly uncapped" so `animus daemon config --max-daily-usd 0`
/// clears a YAML cap rather than falling through to it. Only when the
/// pm-config key is absent does the workflow YAML `daemon.budget` block
/// supply the cap.
pub(crate) fn read_max_daily_usd(project_root: &Path) -> Option<f64> {
    if let Some(cap) = read_pm_config_max_daily_usd(project_root) {
        // Present key wins, positive or not — a 0/negative override means
        // "uncapped", suppressing the YAML fallback.
        return positive(cap);
    }
    read_yaml_max_daily_usd(project_root).and_then(positive)
}

fn positive(value: f64) -> Option<f64> {
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

fn read_pm_config_max_daily_usd(project_root: &Path) -> Option<f64> {
    let config = orchestrator_core::load_daemon_project_config(project_root).ok()?;
    config.extra.get("max_daily_usd").and_then(serde_json::Value::as_f64)
}

fn read_yaml_max_daily_usd(project_root: &Path) -> Option<f64> {
    let config = orchestrator_core::load_workflow_config_or_default(project_root).config;
    config.daemon.as_ref()?.budget.as_ref()?.max_cost_usd_per_day
}

/// Total USD spend observed inside the rolling daily window. Mirrors the
/// windowing `animus cost summary` uses: a workflow rollup is counted when
/// its most-recent activity (`updated_at`, else `started_at`) falls inside
/// the window, and archived history rows are counted by `finished_at`.
/// Per-row totals are lifetime spend, not in-window deltas — the same
/// documented approximation `cost summary` carries (per-event windowing
/// would need a time-series sidecar).
///
// TODO(codex-p2): the archived-history ring is capped at
// `aggregator::HISTORY_RING_CAP` (200). A project that completes more than
// 200 workflows inside a single rolling 24h window evicts the oldest
// same-day rows before this rollup runs, so the daily total only sees the
// retained tail and can undercount. The accurate fix is a persisted
// same-day spend ledger incremented by the archive hook (independent of the
// history ring); that is a larger change than this cap wiring and is
// deferred. Active (un-archived) run spend is always counted in full, so the
// undercount only affects the long tail of already-completed same-day runs.
pub(crate) fn daily_spend_usd(state: &CostState) -> f64 {
    let window_start = Utc::now() - chrono::Duration::hours(DAILY_WINDOW_HOURS);
    let mut total = 0.0;
    for workflow in state.workflows.values() {
        let last_seen = workflow.updated_at.unwrap_or(workflow.started_at);
        if last_seen >= window_start || workflow.started_at >= window_start {
            total += workflow.total_cost_usd;
        }
    }
    for history in &state.history {
        if history.finished_at >= window_start {
            total += history.total_cost_usd;
        }
    }
    total
}

/// Observable rollup of the daily cap for `daemon health` / `cost summary`.
/// `None` cap means uncapped; `remaining`/`exceeded` are then absent.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct DailyCapStatus {
    /// Window the spend is measured over, in hours (always 24 today).
    pub window_hours: i64,
    /// Today's rolling spend in USD.
    pub spent_usd: f64,
    /// Configured cap, or `None` when uncapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_daily_usd: Option<f64>,
    /// `cap - spent` (floored at 0), or `None` when uncapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_usd: Option<f64>,
    /// `true` when a cap is set and rolling spend is at or above it.
    pub exceeded: bool,
}

impl DailyCapStatus {
    pub(crate) fn evaluate(project_root: &Path, state: &CostState) -> Self {
        let spent = daily_spend_usd(state);
        let cap = read_max_daily_usd(project_root);
        let (remaining, exceeded) = match cap {
            Some(cap) => (Some((cap - spent).max(0.0)), spent >= cap),
            None => (None, false),
        };
        Self {
            window_hours: DAILY_WINDOW_HOURS,
            spent_usd: spent,
            max_daily_usd: cap,
            remaining_usd: remaining,
            exceeded,
        }
    }
}

/// Persisted latch: is the daemon currently pausing dispatch because the
/// daily cap is blown? Written by the housekeeping sweep, read by the tick
/// to gate ready-task / queue dispatch. Survives daemon restarts so a crash
/// mid-breach does not silently resume over-budget spend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct DailyCapState {
    pub schema: String,
    /// `true` while new dispatch is suppressed.
    pub dispatch_paused: bool,
    /// Cap value that triggered the latch, for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_usd: Option<f64>,
    /// When the latch last engaged (most recent breach).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breached_at: Option<DateTime<Utc>>,
}

impl Default for DailyCapState {
    fn default() -> Self {
        Self { schema: DAILY_CAP_STATE_SCHEMA_ID.to_string(), dispatch_paused: false, cap_usd: None, breached_at: None }
    }
}

fn daily_cap_state_path(project_root: &Path) -> PathBuf {
    scoped_root(project_root).join(DAILY_CAP_STATE_FILE_NAME)
}

/// Read the persisted latch, or the default (not paused) when absent /
/// unreadable / malformed — a missing latch must never block dispatch.
pub(crate) fn load_daily_cap_state(project_root: &Path) -> DailyCapState {
    let path = daily_cap_state_path(project_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return DailyCapState::default();
    };
    serde_json::from_str(text.trim()).unwrap_or_default()
}

fn save_daily_cap_state(project_root: &Path, state: &DailyCapState) -> Result<()> {
    let path = daily_cap_state_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create daily-cap parent {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(state).context("serialize daily-cap state")?;
    std::fs::write(&path, serialized).with_context(|| format!("write daily-cap state {}", path.display()))
}

/// `true` when the daemon should suppress NEW dispatch because the daily cap
/// is currently blown — checking BOTH the persisted latch AND the live cap
/// status against the cached cost-state.
///
/// The live check closes a one-tick gap: `run_project_tick` processes due
/// schedules + triggers at the START of the tick, BEFORE the housekeeping
/// `enforce_budget_caps` leg reconciles the latch. Relying on the persisted
/// latch alone would let the first over-cap tick still dispatch scheduled /
/// triggered work before the latch engages. Reading the live cap status here
/// (cached cost-state JSON + config — no live run rescan) suppresses that
/// first tick too. The latch still drives the notify-once edge and survives
/// restarts; this is the dispatch gate.
pub(crate) fn is_dispatch_paused(project_root: &Path) -> bool {
    if load_daily_cap_state(project_root).dispatch_paused {
        return true;
    }
    let state = super::persistence::load_cost_state(project_root).unwrap_or_default();
    DailyCapStatus::evaluate(project_root, &state).exceeded
}

/// Evaluate the fleet daily cap against fresh cost state and reconcile the
/// dispatch latch. Returns one [`DailyCapBreach`] when the latch transitions
/// from open to engaged (so the caller notifies + records exactly once per
/// breach); returns `None` on steady state or on resume. The resume path is
/// implicit: when spend drops back under the cap (rolling window ages out)
/// or the cap is raised/cleared, the latch flips back to open and dispatch
/// resumes on the next tick — no separate operator action required.
pub(crate) fn reconcile_daily_cap(project_root: &Path, state: &CostState) -> Result<Option<DailyCapBreach>> {
    let status = DailyCapStatus::evaluate(project_root, state);
    let mut latch = load_daily_cap_state(project_root);

    if !status.exceeded {
        // Under the cap (or uncapped): clear the latch if it was set so
        // dispatch resumes automatically.
        if latch.dispatch_paused {
            latch = DailyCapState::default();
            save_daily_cap_state(project_root, &latch)?;
        }
        return Ok(None);
    }

    // Over the cap. Engage the latch if it was open; this is the
    // notify-once edge.
    if latch.dispatch_paused {
        return Ok(None);
    }
    let now = Utc::now();
    let cap = status.max_daily_usd;
    latch = DailyCapState {
        schema: DAILY_CAP_STATE_SCHEMA_ID.to_string(),
        dispatch_paused: true,
        cap_usd: cap,
        breached_at: Some(now),
    };
    save_daily_cap_state(project_root, &latch)?;

    // Append a fleet-level breach record for `animus cost decisions`.
    let record = daily_breach_record(status.spent_usd, cap.unwrap_or_default(), now);
    if let Err(error) = append_decision_record(project_root, &record) {
        return Err(error.context("append daily-cap breach record"));
    }
    Ok(Some(DailyCapBreach { spent_usd: status.spent_usd, cap_usd: cap.unwrap_or_default(), observed_at: now }))
}

/// A fleet daily-cap breach the sweep just latched on, surfaced so the
/// daemon run host can emit one notifier event.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DailyCapBreach {
    pub spent_usd: f64,
    pub cap_usd: f64,
    pub observed_at: DateTime<Utc>,
}

impl DailyCapBreach {
    /// Project into the shared budget-breach notifier event shape with the
    /// fleet `daily` discriminator.
    pub(crate) fn to_breach_event(&self) -> orchestrator_daemon_runtime::BudgetBreachEvent {
        orchestrator_daemon_runtime::BudgetBreachEvent {
            workflow_run_id: String::new(),
            workflow_id: String::new(),
            phase_id: None,
            limit_kind: "daily".to_string(),
            limit_field: BudgetLimitField::MaxCostUsd.as_str().to_string(),
            actual: self.spent_usd,
            budget: self.cap_usd,
            on_exceed: ON_EXCEED_PAUSE.to_string(),
            action: ACTION_DISPATCH_PAUSED.to_string(),
            observed_at: self.observed_at.to_rfc3339(),
        }
    }
}

fn daily_breach_record(spent: f64, cap: f64, observed_at: DateTime<Utc>) -> BudgetExceededRecord {
    BudgetExceededRecord {
        schema: BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
        workflow_run_id: DAILY_CAP_RUN_ID.to_string(),
        workflow_id: DAILY_CAP_RUN_ID.to_string(),
        phase_id: None,
        limit_kind: BudgetLimitKind::Workflow,
        limit_field: BudgetLimitField::MaxCostUsd,
        actual: spent,
        budget: cap,
        on_exceed: ON_EXCEED_PAUSE.to_string(),
        observed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cost::aggregator::{HistorySummary, WorkflowCost, WorkflowCostStatus};
    use crate::shared::test_env_lock;
    use chrono::Duration;
    use protocol::test_utils::EnvVarGuard;
    use tempfile::TempDir;

    fn state_with_recent_and_old() -> CostState {
        let now = Utc::now();
        let mut state = CostState::default();

        let mut recent = WorkflowCost::new("recent", now);
        recent.updated_at = Some(now - Duration::hours(1));
        recent.total_cost_usd = 3.0;
        state.workflows.insert("wf-recent".to_string(), recent);

        let mut old = WorkflowCost::new("old", now - Duration::hours(48));
        old.updated_at = Some(now - Duration::hours(40));
        old.total_cost_usd = 99.0;
        state.workflows.insert("wf-old".to_string(), old);

        state.history.push(HistorySummary {
            workflow_run_id: "wf-hist-recent".to_string(),
            workflow_id: "hist".to_string(),
            started_at: now - Duration::hours(5),
            finished_at: now - Duration::hours(2),
            total_tokens: 0,
            total_cost_usd: 2.0,
            final_status: WorkflowCostStatus::Completed,
        });
        state.history.push(HistorySummary {
            workflow_run_id: "wf-hist-old".to_string(),
            workflow_id: "hist".to_string(),
            started_at: now - Duration::hours(50),
            finished_at: now - Duration::hours(48),
            total_tokens: 0,
            total_cost_usd: 50.0,
            final_status: WorkflowCostStatus::Completed,
        });
        state
    }

    #[test]
    fn daily_spend_counts_only_the_rolling_window() {
        let state = state_with_recent_and_old();
        // recent active ($3) + recent history ($2) = $5; the 48h-old rows
        // are outside the rolling window and must not count.
        assert!((daily_spend_usd(&state) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn positive_rejects_zero_and_negative_and_nan() {
        assert_eq!(positive(0.0), None, "0 clears the cap");
        assert_eq!(positive(-1.0), None);
        assert_eq!(positive(f64::NAN), None);
        assert_eq!(positive(f64::INFINITY), None);
        assert_eq!(positive(2.5), Some(2.5));
    }

    /// Arrange a HOME + scoped-state override so both pm-config (keyed on
    /// HOME via `scoped_state_root`) and the cost-state seam
    /// (`ANIMUS_COST_STATE_ROOT`) resolve to the same tempdir.
    fn arrange(tmp: &TempDir) -> (EnvVarGuard, EnvVarGuard, PathBuf) {
        let home = EnvVarGuard::set("HOME", Some(tmp.path().to_string_lossy().as_ref()));
        let scope = tmp.path().join("scope");
        std::fs::create_dir_all(&scope).unwrap();
        let override_guard = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(scope.to_string_lossy().as_ref()));
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        (home, override_guard, project_root)
    }

    fn state_with_spend(usd: f64) -> CostState {
        let now = Utc::now();
        let mut state = CostState::default();
        let mut wf = WorkflowCost::new("flow", now);
        wf.updated_at = Some(now);
        wf.total_cost_usd = usd;
        state.workflows.insert("wf-1".to_string(), wf);
        state
    }

    fn write_pm_config_cap(project_root: &Path, cap: f64) {
        let config = orchestrator_core::DaemonProjectConfig {
            extra: std::iter::once(("max_daily_usd".to_string(), serde_json::json!(cap))).collect(),
            ..Default::default()
        };
        orchestrator_core::write_daemon_project_config(project_root, &config).unwrap();
    }

    #[test]
    fn evaluate_status_marks_exceeded_at_or_above_cap() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);

        let status = DailyCapStatus::evaluate(&project_root, &state_with_spend(5.0));
        assert!(status.exceeded, "spend == cap is a breach");
        assert_eq!(status.max_daily_usd, Some(5.0));
        assert_eq!(status.remaining_usd, Some(0.0));

        let under = DailyCapStatus::evaluate(&project_root, &state_with_spend(4.0));
        assert!(!under.exceeded);
        assert_eq!(under.remaining_usd, Some(1.0));
    }

    #[test]
    fn evaluate_status_uncapped_never_exceeds() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        // No pm-config cap and no YAML cap → uncapped.
        let status = DailyCapStatus::evaluate(&project_root, &state_with_spend(1000.0));
        assert!(!status.exceeded);
        assert_eq!(status.max_daily_usd, None);
        assert_eq!(status.remaining_usd, None);
    }

    #[test]
    fn pm_config_cap_takes_precedence_over_yaml() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        std::fs::create_dir_all(project_root.join(".animus")).unwrap();
        std::fs::write(
            project_root.join(".animus").join("workflows.yaml"),
            "workflows: []\ndaemon:\n  budget:\n    max_cost_usd_per_day: 99.0\n",
        )
        .unwrap();
        write_pm_config_cap(&project_root, 5.0);
        assert_eq!(read_max_daily_usd(&project_root), Some(5.0), "pm-config wins over YAML");
    }

    #[test]
    fn explicit_zero_pm_config_clears_yaml_cap() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        std::fs::create_dir_all(project_root.join(".animus")).unwrap();
        std::fs::write(
            project_root.join(".animus").join("workflows.yaml"),
            "workflows: []\ndaemon:\n  budget:\n    max_cost_usd_per_day: 99.0\n",
        )
        .unwrap();
        // An explicit 0 override (what `--max-daily-usd 0` persists) reads as
        // uncapped and must NOT fall through to the YAML cap.
        write_pm_config_cap(&project_root, 0.0);
        assert_eq!(read_max_daily_usd(&project_root), None, "explicit 0 override clears the YAML cap");
    }

    #[test]
    fn yaml_cap_used_when_pm_config_absent() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        std::fs::create_dir_all(project_root.join(".animus")).unwrap();
        std::fs::write(
            project_root.join(".animus").join("workflows.yaml"),
            "workflows: []\ndaemon:\n  budget:\n    max_cost_usd_per_day: 12.5\n",
        )
        .unwrap();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&project_root);
        assert_eq!(read_max_daily_usd(&project_root), Some(12.5));
    }

    #[test]
    fn reconcile_latches_pauses_dispatch_and_notifies_once() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);

        assert!(!is_dispatch_paused(&project_root), "open before any breach");

        let first = reconcile_daily_cap(&project_root, &state_with_spend(7.0)).unwrap();
        assert!(first.is_some(), "first over-cap sweep latches and notifies");
        let breach = first.unwrap();
        assert!((breach.cap_usd - 5.0).abs() < 1e-9);
        assert!((breach.spent_usd - 7.0).abs() < 1e-9);
        assert!(is_dispatch_paused(&project_root), "dispatch paused after breach");
        assert_eq!(breach.to_breach_event().limit_kind, "daily");
        assert_eq!(breach.to_breach_event().action, "dispatch_paused");

        // Second sweep still over cap: latch holds, no second notification.
        let second = reconcile_daily_cap(&project_root, &state_with_spend(8.0)).unwrap();
        assert!(second.is_none(), "already-latched breach must not re-notify");
        assert!(is_dispatch_paused(&project_root));

        // One fleet decision record, not two.
        let records = crate::services::cost::read_decision_records(&project_root).unwrap();
        assert_eq!(records.len(), 1, "exactly one fleet breach recorded");
        assert_eq!(records[0].workflow_run_id, DAILY_CAP_RUN_ID);
    }

    #[test]
    fn reconcile_resumes_when_spend_falls_under_cap() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);

        reconcile_daily_cap(&project_root, &state_with_spend(7.0)).unwrap();
        assert!(is_dispatch_paused(&project_root));

        // Spend ages out / cap raised: rolling spend now under cap → resume.
        let resumed = reconcile_daily_cap(&project_root, &state_with_spend(2.0)).unwrap();
        assert!(resumed.is_none());
        assert!(!is_dispatch_paused(&project_root), "dispatch resumes automatically under the cap");
    }

    #[test]
    fn reconcile_resumes_when_cap_is_raised() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);
        reconcile_daily_cap(&project_root, &state_with_spend(7.0)).unwrap();
        assert!(is_dispatch_paused(&project_root));

        // Operator raises the cap above current spend.
        write_pm_config_cap(&project_root, 20.0);
        let resumed = reconcile_daily_cap(&project_root, &state_with_spend(7.0)).unwrap();
        assert!(resumed.is_none());
        assert!(!is_dispatch_paused(&project_root), "raising the cap clears the latch");
    }
}
