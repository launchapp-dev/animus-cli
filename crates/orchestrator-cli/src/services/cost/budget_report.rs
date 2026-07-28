//! Read-only budget report and fleet-pause visibility helpers.
//!
//! Backs the `animus.budget.get` MCP tool (fleet daily cap + configured
//! per-workflow / per-phase caps) and the operator-facing "dispatch paused"
//! reason surfaced through `daemon health` / `daemon status`. Enforcement
//! itself lives in [`super::daily_cap`] and [`super::scanner`]; this module
//! only reads state and projects it into serializable views.

use std::path::Path;

use serde::Serialize;

use super::daily_cap::{is_dispatch_paused, DailyCapStatus};

pub(crate) const BUDGET_REPORT_SCHEMA: &str = "animus.budget.v1";

/// Fleet daily spend cap rollup: configured cap, rolling-24h spend,
/// remaining headroom, whether the cap is blown, and whether the daemon is
/// currently suppressing new dispatch on the latch.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct DailyCapReport {
    pub window_hours: i64,
    pub spent_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_daily_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_usd: Option<f64>,
    pub exceeded: bool,
    /// `true` when the daemon is suppressing new dispatch on the latch.
    pub dispatch_paused: bool,
}

/// Evaluate the fleet daily cap against the cached cost-state and the
/// persisted dispatch latch. Cheap reads only (no live run rescan).
///
/// `dispatch_paused` mirrors the daemon's actual dispatch gate
/// ([`daemon_tick_executor::dispatch_suppressed`]): the enforcement
/// kill-switch (`ANIMUS_DAEMON_DISABLE_BUDGET_ENFORCEMENT`) wins, so a stale
/// latch left over from a prior breach never reports as paused (nor degrades
/// health) when enforcement is disabled and the daemon is free to dispatch.
/// The kill-switch is read from the daemon's PERSISTED sweep status (via
/// [`super::effective_budget_enforcement_enabled`]) so a CLI/MCP reader in a
/// different process than the daemon still sees the daemon's real posture.
pub(crate) fn daily_cap_report(project_root: &Path) -> DailyCapReport {
    let state = super::load_cost_state(project_root).unwrap_or_default();
    let status = DailyCapStatus::evaluate(project_root, &state);
    let dispatch_paused = super::effective_budget_enforcement_enabled(project_root) && is_dispatch_paused(project_root);
    DailyCapReport {
        window_hours: status.window_hours,
        spent_usd: status.spent_usd,
        max_daily_usd: status.max_daily_usd,
        remaining_usd: status.remaining_usd,
        exceeded: status.exceeded,
        dispatch_paused,
    }
}

/// One-line operator-facing reason when the fleet daily cap has paused
/// dispatch, or `None` when dispatch is not paused. Surfaced as a
/// `last_error` / `degraded_reasons` entry so a latched cap is no longer a
/// silent `healthy: true`.
pub(crate) fn dispatch_paused_reason(project_root: &Path) -> Option<String> {
    let report = daily_cap_report(project_root);
    report.dispatch_paused.then(|| dispatch_paused_reason_text(&report))
}

/// Render the dispatch-paused reason from an already-evaluated report (used
/// by callers that computed the daily-cap rollup once and want the matching
/// message without a second state read).
pub(crate) fn dispatch_paused_reason_text(report: &DailyCapReport) -> String {
    match report.max_daily_usd {
        Some(cap) => format!(
            "fleet daily spend cap exceeded (${:.2}/${:.2} over the last {}h) — new dispatch paused; \
             raise or clear with `animus daemon config --max-daily-usd <N>`",
            report.spent_usd, cap, report.window_hours
        ),
        None => "fleet daily spend cap exceeded — new dispatch paused".to_string(),
    }
}

/// A single configured budget ceiling (workflow- or phase-scoped).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct CapConfigView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    pub on_exceed: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct PhaseCapView {
    pub phase_id: String,
    pub budget: CapConfigView,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct WorkflowCapView {
    pub workflow_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<CapConfigView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseCapView>,
}

/// Full budget report: the fleet daily cap plus every workflow / phase that
/// declares a non-empty [`BudgetConfig`](orchestrator_config::workflow_config::BudgetConfig).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct BudgetReport {
    pub schema: &'static str,
    pub daily_cap: DailyCapReport,
    pub workflow_caps: Vec<WorkflowCapView>,
}

fn cap_view(budget: &orchestrator_config::workflow_config::BudgetConfig) -> CapConfigView {
    CapConfigView {
        max_tokens: budget.max_tokens,
        max_cost_usd: budget.max_cost_usd,
        on_exceed: budget.on_exceed.as_str().to_string(),
    }
}

/// Build the read-only budget report for `budget_get`. Enumerates the
/// compiled workflow config for declared per-workflow / per-phase caps and
/// folds in the fleet daily cap. Workflows with no declared caps are
/// omitted so the report answers "what caps are in force", not "list every
/// workflow".
pub(crate) fn build_budget_report(project_root: &Path) -> BudgetReport {
    let config = orchestrator_core::load_workflow_config_or_default(project_root).config;
    let mut workflow_caps = Vec::new();
    for workflow in &config.workflows {
        let budget = workflow.budget.as_ref().filter(|budget| !budget.is_empty()).map(cap_view);
        let mut phases = Vec::new();
        for entry in &workflow.phases {
            if let Some(phase_budget) = entry.budget() {
                if !phase_budget.is_empty() {
                    phases
                        .push(PhaseCapView { phase_id: entry.phase_id().to_string(), budget: cap_view(phase_budget) });
                }
            }
        }
        if budget.is_some() || !phases.is_empty() {
            workflow_caps.push(WorkflowCapView { workflow_id: workflow.id.clone(), budget, phases });
        }
    }
    BudgetReport { schema: BUDGET_REPORT_SCHEMA, daily_cap: daily_cap_report(project_root), workflow_caps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_env_lock;
    use protocol::test_utils::EnvVarGuard;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn arrange(tmp: &TempDir) -> (EnvVarGuard, EnvVarGuard, PathBuf) {
        let home = EnvVarGuard::set("HOME", Some(tmp.path().to_string_lossy().as_ref()));
        let scope = tmp.path().join("scope");
        std::fs::create_dir_all(&scope).unwrap();
        let override_guard = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(scope.to_string_lossy().as_ref()));
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        (home, override_guard, project_root)
    }

    fn write_pm_config_cap(project_root: &Path, cap: f64) {
        let config = orchestrator_core::DaemonProjectConfig {
            extra: std::iter::once(("max_daily_usd".to_string(), serde_json::json!(cap))).collect(),
            ..Default::default()
        };
        orchestrator_core::write_daemon_project_config(project_root, &config).unwrap();
    }

    #[test]
    fn dispatch_paused_reason_none_when_uncapped() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        assert_eq!(dispatch_paused_reason(&project_root), None, "no cap → never paused");
    }

    #[test]
    fn dispatch_paused_reason_names_the_cap_when_latched() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);

        let report = DailyCapReport {
            window_hours: 24,
            spent_usd: 7.5,
            max_daily_usd: Some(5.0),
            remaining_usd: Some(0.0),
            exceeded: true,
            dispatch_paused: true,
        };
        let text = dispatch_paused_reason_text(&report);
        assert!(text.contains("$7.50/$5.00"), "reason names spend vs cap: {text}");
        assert!(text.contains("dispatch paused"), "reason states the effect: {text}");
    }

    #[test]
    fn kill_switch_suppresses_reported_dispatch_pause() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);

        // Record $7 of rolling spend so the live cap check latches.
        let now = chrono::Utc::now();
        let mut state = super::super::CostState::default();
        let mut workflow = super::super::WorkflowCost::new("flow", now);
        workflow.updated_at = Some(now);
        workflow.total_cost_usd = 7.0;
        state.workflows.insert("wf-1".to_string(), workflow);
        super::super::save_cost_state(&project_root, &state).unwrap();

        let _on = EnvVarGuard::set(super::super::DISABLE_BUDGET_ENFORCEMENT_ENV, None);
        assert!(daily_cap_report(&project_root).dispatch_paused, "enforcement on → latched cap pauses dispatch");
        assert!(dispatch_paused_reason(&project_root).is_some());

        // With enforcement disabled in this process (and no persisted sweep
        // status yet) the report falls back to the env and must not claim
        // paused / degrade health.
        let _off = EnvVarGuard::set(super::super::DISABLE_BUDGET_ENFORCEMENT_ENV, Some("1"));
        assert!(!daily_cap_report(&project_root).dispatch_paused, "kill-switch → never reported paused");
        assert_eq!(dispatch_paused_reason(&project_root), None, "kill-switch → no degraded reason");
    }

    #[test]
    fn persisted_enforcement_status_overrides_process_env() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 5.0);

        let now = chrono::Utc::now();
        let mut state = super::super::CostState::default();
        let mut workflow = super::super::WorkflowCost::new("flow", now);
        workflow.updated_at = Some(now);
        workflow.total_cost_usd = 7.0;
        state.workflows.insert("wf-1".to_string(), workflow);
        super::super::save_cost_state(&project_root, &state).unwrap();

        // Daemon (another process) recorded enforcement DISABLED, while this
        // reader's env leaves it enabled. The persisted status must win, so a
        // stale latch is not reported as paused.
        let _enabled_env = EnvVarGuard::set(super::super::DISABLE_BUDGET_ENFORCEMENT_ENV, None);
        super::super::save_budget_enforcement_status(&project_root, false).unwrap();
        assert!(!daily_cap_report(&project_root).dispatch_paused, "persisted disabled wins over enabled env");

        // Daemon recorded enforcement ENABLED → the latch is honored even if
        // this reader's env disables it.
        let _disabled_env = EnvVarGuard::set(super::super::DISABLE_BUDGET_ENFORCEMENT_ENV, Some("1"));
        super::super::save_budget_enforcement_status(&project_root, true).unwrap();
        assert!(daily_cap_report(&project_root).dispatch_paused, "persisted enabled wins over disabled env");
    }

    #[test]
    fn budget_report_reports_daily_cap_and_no_workflow_caps_by_default() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override, project_root) = arrange(&tmp);
        write_pm_config_cap(&project_root, 12.0);

        let report = build_budget_report(&project_root);
        assert_eq!(report.schema, BUDGET_REPORT_SCHEMA);
        assert_eq!(report.daily_cap.max_daily_usd, Some(12.0));
        assert!(!report.daily_cap.dispatch_paused);
        assert!(report.workflow_caps.is_empty(), "no workflow YAML caps declared");
    }
}
