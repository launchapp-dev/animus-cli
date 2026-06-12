//! Operator-facing rollup of the scoped budget-breach log.
//!
//! `animus status` and `animus daemon health` both need a compact answer to
//! "are any budgets being blown right now, and which run is worst?" without
//! grepping `~/.animus/<repo-scope>/decisions.jsonl`. This module folds that
//! log into a [`BudgetBreachSummary`].
//!
//! ## Resolution heuristic
//!
//! The scoped breach log is append-only and never rewritten, so a breach
//! record alone cannot tell us whether the operator has since dealt with it.
//! A breach is treated as **active (unresolved)** when:
//!
//! - its `on_exceed` is `pause` AND the breaching workflow is still in the
//!   caller-supplied `paused_workflow_ids` set (the cheap "still stuck"
//!   signal — a resume/re-arm removes it), OR
//! - the caller supplied no paused-set (the `daemon health` path, which does
//!   not load workflow records): then we fall back to a 24h recency window
//!   and report "breaches in last 24h" instead of a hard active/resolved
//!   split.
//!
//! `warn` / `fail` breaches are terminal-by-declaration (the run is failed or
//! the operator was only warned), so they never count as "active" — they only
//! contribute to the 24h recency count. This keeps the dashboard focused on
//! breaches an operator can still act on by resuming or raising the cap.

use chrono::{Duration, Utc};
use serde::Serialize;
use std::collections::HashSet;

use super::aggregator::BudgetExceededRecord;

/// Window for the recency-based fallback count (`daemon health`, and the
/// "last 24h" line shown when no live paused-set is available).
pub const BREACH_RECENT_WINDOW_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize)]
pub struct BudgetBreachSummary {
    /// Total breach records ever recorded in the scoped log.
    pub total_recorded: usize,
    /// Breaches observed inside the last [`BREACH_RECENT_WINDOW_HOURS`].
    pub recent_24h: usize,
    /// Count of breaches deemed active by the resolution heuristic. `None`
    /// when the caller supplied no paused-set (use [`Self::recent_24h`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
    /// Worst active (or, in the recency fallback, worst recent) offender by
    /// USD-or-token overage ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worst_offender: Option<BreachOffender>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BreachOffender {
    pub workflow_run_id: String,
    /// Human cause, e.g. `budget exceeded ($7.50 > $5.00 max_cost_usd)`.
    pub summary: String,
}

impl BudgetBreachSummary {
    /// `true` when there is anything worth surfacing on a dashboard.
    #[cfg(test)]
    pub fn has_signal(&self) -> bool {
        self.active.unwrap_or(self.recent_24h) > 0
    }
}

/// Overage ratio used to rank offenders: how far past the cap the run went.
fn overage_ratio(record: &BudgetExceededRecord) -> f64 {
    if record.budget > 0.0 {
        record.actual / record.budget
    } else {
        f64::INFINITY
    }
}

/// Build the summary. Pass `Some(set)` of currently-paused workflow ids for
/// the active/resolved split; pass `None` to use the 24h recency fallback.
pub fn summarize_breaches(
    records: &[BudgetExceededRecord],
    paused_workflow_ids: Option<&HashSet<String>>,
) -> BudgetBreachSummary {
    let cutoff = Utc::now() - Duration::hours(BREACH_RECENT_WINDOW_HOURS);
    let recent_24h = records.iter().filter(|record| record.observed_at >= cutoff).count();

    let (active, ranking_pool): (Option<usize>, Vec<&BudgetExceededRecord>) = match paused_workflow_ids {
        Some(paused) => {
            let active_records: Vec<&BudgetExceededRecord> = records
                .iter()
                .filter(|record| record.on_exceed == "pause" && paused.contains(&record.workflow_id))
                .collect();
            (Some(active_records.len()), active_records)
        }
        None => (None, records.iter().filter(|record| record.observed_at >= cutoff).collect()),
    };

    let worst_offender = ranking_pool
        .into_iter()
        .max_by(|a, b| overage_ratio(a).partial_cmp(&overage_ratio(b)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|record| BreachOffender {
            workflow_run_id: record.workflow_run_id.clone(),
            summary: record.breach_summary(),
        });

    BudgetBreachSummary { total_recorded: records.len(), recent_24h, active, worst_offender }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cost::aggregator::{BudgetLimitField, BudgetLimitKind, BUDGET_EXCEEDED_SCHEMA_ID};
    use chrono::DateTime;

    fn record(
        run_id: &str,
        workflow_id: &str,
        on_exceed: &str,
        actual: f64,
        budget: f64,
        observed_at: DateTime<Utc>,
    ) -> BudgetExceededRecord {
        BudgetExceededRecord {
            schema: BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
            workflow_run_id: run_id.to_string(),
            workflow_id: workflow_id.to_string(),
            phase_id: None,
            limit_kind: BudgetLimitKind::Workflow,
            limit_field: BudgetLimitField::MaxCostUsd,
            actual,
            budget,
            on_exceed: on_exceed.to_string(),
            observed_at,
        }
    }

    #[test]
    fn active_split_uses_paused_set_and_picks_worst_overage() {
        let now = Utc::now();
        let records = vec![
            record("wf-a", "flow-a", "pause", 7.5, 5.0, now), // ratio 1.5, paused → active
            record("wf-b", "flow-b", "pause", 20.0, 5.0, now), // ratio 4.0 but NOT paused → resolved
            record("wf-c", "flow-c", "warn", 100.0, 1.0, now), // warn → never active
        ];
        let paused: HashSet<String> = HashSet::from(["flow-a".to_string()]);
        let summary = summarize_breaches(&records, Some(&paused));
        assert_eq!(summary.active, Some(1), "only the paused pause-breach is active");
        assert_eq!(summary.worst_offender.as_ref().unwrap().workflow_run_id, "wf-a");
        assert!(summary.has_signal());
    }

    #[test]
    fn recency_fallback_counts_last_24h_when_no_paused_set() {
        let now = Utc::now();
        let records = vec![
            record("wf-old", "flow-old", "pause", 7.5, 5.0, now - Duration::hours(48)),
            record("wf-new", "flow-new", "pause", 9.0, 5.0, now - Duration::hours(2)),
        ];
        let summary = summarize_breaches(&records, None);
        assert_eq!(summary.active, None, "no active/resolved split without a paused-set");
        assert_eq!(summary.recent_24h, 1, "only the 2h-old breach is inside the window");
        assert_eq!(summary.total_recorded, 2);
        assert_eq!(summary.worst_offender.as_ref().unwrap().workflow_run_id, "wf-new");
    }

    #[test]
    fn no_breaches_has_no_signal() {
        let summary = summarize_breaches(&[], Some(&HashSet::new()));
        assert!(!summary.has_signal());
        assert!(summary.worst_offender.is_none());
    }
}
