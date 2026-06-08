//! In-memory cost aggregation.
//!
//! [`CostState`] is the disk-persisted root: a map of workflow_run_id
//! to a [`WorkflowCost`] with per-phase rollups, plus a tail of
//! recently completed workflows (`history`).
//!
//! The aggregator is intentionally passive — it scans `events.jsonl`
//! files produced by provider plugins (via the daemon's run host) and
//! folds the `AgentRunEvent::Metadata { cost, tokens }` events into
//! per-(workflow, phase) totals. The aggregator does not mutate state
//! held by the workflow runner; it produces a read model.

#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const COST_STATE_SCHEMA_ID: &str = "animus.cost-state.v1";
pub const BUDGET_EXCEEDED_SCHEMA_ID: &str = "animus.budget-exceeded.v1";
pub const HISTORY_RING_CAP: usize = 200;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseCost {
    #[serde(default)]
    pub tokens_input: u64,
    #[serde(default)]
    pub tokens_output: u64,
    #[serde(default)]
    pub tokens_reasoning: u64,
    #[serde(default)]
    pub tokens_cache_read: u64,
    #[serde(default)]
    pub tokens_cache_write: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Number of times the phase entered this aggregator. Rework
    /// attempts that reset the phase counter advance this number.
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    /// `true` after a rework reset; cleared on the next event fold.
    #[serde(default)]
    pub reset_pending: bool,
}

fn default_attempts() -> u32 {
    1
}

impl PhaseCost {
    pub fn total_tokens(&self) -> u64 {
        self.tokens_input.saturating_add(self.tokens_output).saturating_add(self.tokens_reasoning)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCost {
    pub workflow_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub phases: BTreeMap<String, PhaseCost>,
    #[serde(default)]
    pub status: WorkflowCostStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowCostStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Paused,
}

impl WorkflowCost {
    pub fn new(workflow_id: impl Into<String>, started_at: DateTime<Utc>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            task_id: None,
            started_at,
            updated_at: Some(started_at),
            total_tokens: 0,
            total_cost_usd: 0.0,
            phases: BTreeMap::new(),
            status: WorkflowCostStatus::Running,
        }
    }

    /// Fold a metadata event into the per-phase counter. Returns the
    /// post-fold totals (workflow, phase) for downstream cap checks.
    pub fn record_metadata(
        &mut self,
        phase_id: &str,
        observed_at: DateTime<Utc>,
        delta: MetadataDelta,
    ) -> (u64, f64, u64, f64) {
        // `updated_at` is "most recent activity"; do not let
        // out-of-order folds move it backwards. `--since` filters in
        // `animus cost summary` use this field, so a backward move
        // can omit a workflow from the window.
        self.updated_at = Some(match self.updated_at {
            Some(existing) if existing > observed_at => existing,
            _ => observed_at,
        });
        let phase = self.phases.entry(phase_id.to_string()).or_default();
        if phase.reset_pending {
            phase.reset_pending = false;
        }
        phase.tokens_input = phase.tokens_input.saturating_add(delta.input_tokens);
        phase.tokens_output = phase.tokens_output.saturating_add(delta.output_tokens);
        phase.tokens_reasoning = phase.tokens_reasoning.saturating_add(delta.reasoning_tokens);
        phase.tokens_cache_read = phase.tokens_cache_read.saturating_add(delta.cache_read_tokens);
        phase.tokens_cache_write = phase.tokens_cache_write.saturating_add(delta.cache_write_tokens);
        phase.cost_usd += delta.cost_usd;
        if let Some(model) = delta.model.as_ref() {
            phase.model = Some(model.clone());
        }
        if phase.attempts == 0 {
            phase.attempts = 1;
        }

        let phase_tokens = phase.total_tokens();
        let phase_cost = phase.cost_usd;

        // Recompute workflow totals from per-phase. Cheap (BTreeMap is
        // tiny) and avoids drift if events are folded out of order.
        self.recompute_totals();
        (self.total_tokens, self.total_cost_usd, phase_tokens, phase_cost)
    }

    pub fn recompute_totals(&mut self) {
        let mut tokens: u64 = 0;
        let mut cost: f64 = 0.0;
        for phase in self.phases.values() {
            tokens = tokens.saturating_add(phase.total_tokens());
            cost += phase.cost_usd;
        }
        self.total_tokens = tokens;
        self.total_cost_usd = cost;
    }

    /// Mark a phase as about to receive new events under a fresh
    /// attempt; clears the per-phase counters and bumps `attempts`.
    pub fn reset_phase_for_rework(&mut self, phase_id: &str) {
        let phase = self.phases.entry(phase_id.to_string()).or_default();
        phase.tokens_input = 0;
        phase.tokens_output = 0;
        phase.tokens_reasoning = 0;
        phase.tokens_cache_read = 0;
        phase.tokens_cache_write = 0;
        phase.cost_usd = 0.0;
        phase.attempts = phase.attempts.saturating_add(1);
        phase.reset_pending = true;
        self.recompute_totals();
    }
}

/// One observation extracted from an `AgentRunEvent::Metadata` event.
#[derive(Debug, Clone, Default)]
pub struct MetadataDelta {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub workflow_run_id: String,
    pub workflow_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub final_status: WorkflowCostStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostState {
    pub schema: String,
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowCost>,
    #[serde(default)]
    pub history: Vec<HistorySummary>,
}

impl Default for CostState {
    fn default() -> Self {
        Self { schema: COST_STATE_SCHEMA_ID.to_string(), workflows: BTreeMap::new(), history: Vec::new() }
    }
}

impl CostState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Move an active workflow into the history ring (or drop it if
    /// unknown). Trims the ring to `HISTORY_RING_CAP`.
    pub fn archive_workflow(
        &mut self,
        workflow_run_id: &str,
        finished_at: DateTime<Utc>,
        final_status: WorkflowCostStatus,
    ) {
        if let Some(mut workflow) = self.workflows.remove(workflow_run_id) {
            workflow.status = final_status;
            let summary = HistorySummary {
                workflow_run_id: workflow_run_id.to_string(),
                workflow_id: workflow.workflow_id.clone(),
                started_at: workflow.started_at,
                finished_at,
                total_tokens: workflow.total_tokens,
                total_cost_usd: workflow.total_cost_usd,
                final_status,
            };
            self.history.push(summary);
            if self.history.len() > HISTORY_RING_CAP {
                let excess = self.history.len() - HISTORY_RING_CAP;
                self.history.drain(0..excess);
            }
        }
    }

    pub fn ensure_workflow(
        &mut self,
        workflow_run_id: &str,
        workflow_id: &str,
        started_at: DateTime<Utc>,
    ) -> &mut WorkflowCost {
        self.workflows.entry(workflow_run_id.to_string()).or_insert_with(|| WorkflowCost::new(workflow_id, started_at))
    }
}

/// Decision record emitted when a cap is crossed. Persisted to
/// `decisions.jsonl` alongside `cost-state.v1.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetExceededRecord {
    pub schema: String,
    pub workflow_run_id: String,
    pub workflow_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    pub limit_kind: BudgetLimitKind,
    pub limit_field: BudgetLimitField,
    pub actual: f64,
    pub budget: f64,
    pub on_exceed: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetLimitKind {
    Workflow,
    Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLimitField {
    MaxTokens,
    MaxCostUsd,
}

impl BudgetLimitField {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetLimitField::MaxTokens => "max_tokens",
            BudgetLimitField::MaxCostUsd => "max_cost_usd",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn record_metadata_accumulates_per_phase_and_workflow() {
        let started = dt("2026-06-01T00:00:00Z");
        let observed = dt("2026-06-01T00:01:00Z");
        let mut wf = WorkflowCost::new("flow", started);
        wf.record_metadata(
            "impl",
            observed,
            MetadataDelta {
                input_tokens: 100,
                output_tokens: 200,
                reasoning_tokens: 50,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.012,
                model: Some("claude-sonnet-4-6".to_string()),
            },
        );
        wf.record_metadata(
            "impl",
            observed,
            MetadataDelta {
                input_tokens: 10,
                output_tokens: 20,
                reasoning_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.003,
                model: Some("claude-sonnet-4-6".to_string()),
            },
        );
        wf.record_metadata(
            "review",
            observed,
            MetadataDelta {
                input_tokens: 50,
                output_tokens: 30,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.001,
                model: Some("claude-haiku-4".to_string()),
            },
        );
        let phase_impl = wf.phases.get("impl").unwrap();
        assert_eq!(phase_impl.tokens_input, 110);
        assert_eq!(phase_impl.total_tokens(), 110 + 220 + 55);
        assert!((wf.total_cost_usd - 0.016).abs() < 1e-9);
        assert_eq!(wf.total_tokens, (110 + 220 + 55) + (50 + 30));
    }

    #[test]
    fn rework_reset_clears_phase_but_bumps_attempts() {
        let started = dt("2026-06-01T00:00:00Z");
        let mut wf = WorkflowCost::new("flow", started);
        wf.record_metadata(
            "impl",
            started,
            MetadataDelta {
                input_tokens: 5_000,
                output_tokens: 5_000,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.05,
                model: None,
            },
        );
        assert_eq!(wf.total_tokens, 10_000);
        wf.reset_phase_for_rework("impl");
        assert_eq!(wf.total_tokens, 0);
        let phase = wf.phases.get("impl").unwrap();
        assert_eq!(phase.attempts, 2);
        assert!(phase.reset_pending);
        wf.record_metadata(
            "impl",
            started,
            MetadataDelta {
                input_tokens: 100,
                output_tokens: 100,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.001,
                model: None,
            },
        );
        let phase = wf.phases.get("impl").unwrap();
        assert!(!phase.reset_pending);
        assert_eq!(phase.tokens_input, 100);
    }

    #[test]
    fn archive_workflow_moves_to_history_with_cap() {
        let started = dt("2026-06-01T00:00:00Z");
        let mut state = CostState::new();
        for i in 0..(HISTORY_RING_CAP + 5) {
            let id = format!("wf-{i}");
            state.ensure_workflow(&id, "flow", started);
            state.archive_workflow(&id, started, WorkflowCostStatus::Completed);
        }
        assert_eq!(state.history.len(), HISTORY_RING_CAP);
        // The oldest entries should be dropped: first entry must not be wf-0.
        assert!(state.history.iter().all(|h| h.workflow_run_id != "wf-0"));
    }
}
