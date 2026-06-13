//! Cost aggregation, budget cap evaluation, and `animus cost` data
//! sources. See `aggregator.rs` for the data model, `scanner.rs` for
//! the events.jsonl reader, and `enforcement.rs` for the daemon-side
//! budget-cap sweep.

pub(crate) mod aggregator;
pub(crate) mod breach_summary;
pub(crate) mod cap_check;
pub(crate) mod daily_cap;
pub(crate) mod enforcement;
pub(crate) mod model_rates;
pub(crate) mod persistence;
pub(crate) mod scanner;

pub(crate) use aggregator::{CostState, PhaseCost, WorkflowCost};
#[allow(unused_imports)]
pub(crate) use breach_summary::{summarize_breaches, BudgetBreachSummary};
#[allow(unused_imports)]
pub(crate) use daily_cap::{read_max_daily_usd, DailyCapStatus};
pub(crate) use persistence::save_cost_state;
pub(crate) use scanner::enforce_caps;

#[allow(unused_imports)]
pub(crate) use aggregator::{
    BudgetExceededRecord, BudgetLimitField, BudgetLimitKind, HistorySummary, WorkflowCostStatus,
    BUDGET_EXCEEDED_SCHEMA_ID, COST_STATE_SCHEMA_ID,
};
#[allow(unused_imports)]
pub(crate) use cap_check::{check_caps, CapCheckInputs};
#[allow(unused_imports)]
pub(crate) use persistence::{
    append_decision_record, budget_enforcement_status_path, cost_state_path, decisions_log_path,
    load_budget_enforcement_status, load_cost_state, read_decision_records, save_budget_enforcement_status,
    BudgetEnforcementStatus,
};

/// Env kill-switch: `ANIMUS_DAEMON_DISABLE_BUDGET_ENFORCEMENT=1` skips the
/// daemon's budget-enforcement housekeeping leg (the sweep still records its
/// disabled state for `daemon health`). Requires a daemon restart to take
/// effect, like the other plugin kill-switches.
pub(crate) const DISABLE_BUDGET_ENFORCEMENT_ENV: &str = "ANIMUS_DAEMON_DISABLE_BUDGET_ENFORCEMENT";

/// `true` when the budget-enforcement leg is enabled (kill-switch unset or
/// not a truthy `1`/`true`).
pub(crate) fn budget_enforcement_enabled() -> bool {
    !matches!(std::env::var(DISABLE_BUDGET_ENFORCEMENT_ENV).ok().as_deref(), Some("1") | Some("true"))
}

use std::path::Path;

use anyhow::Result;

/// Build the freshest merged cost view: scan live run directories, fold in
/// the persisted history (archived workflows survive daemon restarts), and
/// re-persist the cache for downstream readers.
///
/// Non-fatal problems (a corrupt persisted state, a failed cache write) are
/// surfaced through `warn` so the CLI path can `eprintln!` them while the
/// daemon path routes them into its logger.
pub(crate) fn refresh_cost_state(project_path: &Path, mut warn: impl FnMut(String)) -> Result<CostState> {
    // Load the persisted state first: the archived workflow ids both seed
    // the history merge and let the scanner skip re-reading events for
    // completed runs (cheap housekeeping sweeps).
    let persisted = match persistence::load_cost_state(project_path) {
        Ok(persisted) => Some(persisted),
        Err(error) => {
            warn(format!("failed to load persisted cost state ({error}); reporting live scan only"));
            None
        }
    };
    let archived_ids: std::collections::HashSet<String> = persisted
        .as_ref()
        .map(|state| state.history.iter().map(|entry| entry.workflow_run_id.clone()).collect())
        .unwrap_or_default();
    let mut state = scanner::scan_runs_skipping(project_path, &archived_ids)?;
    if let Some(persisted) = persisted {
        // Drop scanned workflow rollups whose run id is already in
        // persisted history (belt and braces — the scanner already skips
        // them). Otherwise an in-place `events.jsonl` for a completed run
        // double-counts: once from the live scan, once from the archived
        // `HistorySummary`.
        state.workflows.retain(|run_id, _| !archived_ids.contains(run_id));
        state.history = persisted.history;
    }
    // Cache for downstream readers. A persistence failure is not fatal —
    // surface a warning but still return the live view.
    if let Err(error) = save_cost_state(project_path, &state) {
        warn(format!("failed to persist cost state cache: {error}; using in-memory view only"));
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_env_lock;
    use chrono::Utc;
    use protocol::test_utils::EnvVarGuard;
    use protocol::{AgentRunEvent, RunId, TokenUsage};
    use tempfile::TempDir;

    #[test]
    fn budget_enforcement_enabled_reads_kill_switch() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _unset = EnvVarGuard::set(DISABLE_BUDGET_ENFORCEMENT_ENV, None);
        assert!(budget_enforcement_enabled(), "unset → enabled");
        let _on = EnvVarGuard::set(DISABLE_BUDGET_ENFORCEMENT_ENV, Some("1"));
        assert!(!budget_enforcement_enabled(), "1 → disabled");
        let _true = EnvVarGuard::set(DISABLE_BUDGET_ENFORCEMENT_ENV, Some("true"));
        assert!(!budget_enforcement_enabled(), "true → disabled");
        let _other = EnvVarGuard::set(DISABLE_BUDGET_ENFORCEMENT_ENV, Some("0"));
        assert!(budget_enforcement_enabled(), "0 → enabled (only 1/true disable)");
    }

    #[test]
    fn refresh_skips_archived_runs_and_keeps_history() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let state_root = tmp.path().join("scope");
        std::fs::create_dir_all(&state_root).unwrap();
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let _guard = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(state_root.to_string_lossy().as_ref()));

        // A legacy-layout run dir whose id is already archived in history.
        let run_dir = state_root.join("runs").join("wf-archived");
        std::fs::create_dir_all(&run_dir).unwrap();
        let event = AgentRunEvent::Metadata {
            run_id: RunId("wf-archived".to_string()),
            cost: Some(1.0),
            tokens: Some(TokenUsage { input: 10, output: 10, reasoning: None, cache_read: None, cache_write: None }),
        };
        std::fs::write(run_dir.join("events.jsonl"), format!("{}\n", serde_json::to_string(&event).unwrap())).unwrap();

        let mut persisted = CostState::default();
        persisted.history.push(HistorySummary {
            workflow_run_id: "wf-archived".to_string(),
            workflow_id: "archived".to_string(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            total_tokens: 20,
            total_cost_usd: 1.0,
            final_status: WorkflowCostStatus::Completed,
        });
        save_cost_state(&project_root, &persisted).unwrap();

        let state = refresh_cost_state(&project_root, |message| panic!("unexpected warning: {message}")).unwrap();
        assert!(
            !state.workflows.contains_key("wf-archived"),
            "archived run must not resurface as a live rollup: {:?}",
            state.workflows.keys().collect::<Vec<_>>()
        );
        assert_eq!(state.history.len(), 1, "persisted history must survive the refresh");
    }
}
