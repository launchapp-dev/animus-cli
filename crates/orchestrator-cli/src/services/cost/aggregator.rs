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
    /// Portion of `cost_usd` that the provider reported directly (vendor
    /// `cost` field on the metadata event). Defaults to `0.0` on legacy
    /// records written before the reported/estimated split; in that case
    /// the whole `cost_usd` is treated as reported by the renderers.
    #[serde(default)]
    pub cost_usd_reported: f64,
    /// Portion of `cost_usd` that was estimated from the model rate table
    /// because the provider omitted a `cost` field. A non-zero value marks
    /// the row's spend as partly inferred, not vendor-billed.
    #[serde(default)]
    pub cost_usd_estimated: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider / tool that drove this phase, e.g. `claude`, `codex`,
    /// `gemini`. Sourced from the phase session checkpoint's `provider`
    /// field. `None` on legacy cost-state records written before
    /// attribution was captured; those roll up under an "unknown"
    /// bucket in the breakdown views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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

    /// Reported (vendor-billed) portion of this phase's spend. Legacy
    /// records carry the whole `cost_usd` in neither split field; those
    /// are treated as fully reported (the conservative, non-alarming
    /// default — they predate the estimator wiring).
    pub fn reported_usd(&self) -> f64 {
        if self.cost_usd_reported == 0.0 && self.cost_usd_estimated == 0.0 {
            self.cost_usd
        } else {
            self.cost_usd_reported
        }
    }

    /// Estimated (table-inferred) portion of this phase's spend.
    pub fn estimated_usd(&self) -> f64 {
        if self.cost_usd_reported == 0.0 && self.cost_usd_estimated == 0.0 {
            0.0
        } else {
            self.cost_usd_estimated
        }
    }
}

/// Clamp negative-zero (and tiny negative float drift) to a clean `0.0`
/// so trust-sensitive cost output never prints `$-0.00`. Costs are
/// non-negative by construction; any sub-cent negative is float noise.
pub fn clamp_cost(value: f64) -> f64 {
    if value <= 0.0 && value > -0.005 {
        0.0
    } else {
        value
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
        phase.cost_usd_reported += delta.cost_usd_reported;
        phase.cost_usd_estimated += delta.cost_usd_estimated;
        if let Some(model) = delta.model.as_ref() {
            phase.model = Some(model.clone());
        }
        if let Some(provider) = delta.provider.as_ref() {
            phase.provider = Some(provider.clone());
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

    /// Vendor-reported portion of the workflow's lifetime spend.
    pub fn reported_usd(&self) -> f64 {
        self.phases.values().map(PhaseCost::reported_usd).sum()
    }

    /// Table-estimated portion of the workflow's lifetime spend.
    pub fn estimated_usd(&self) -> f64 {
        self.phases.values().map(PhaseCost::estimated_usd).sum()
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
        phase.cost_usd_reported = 0.0;
        phase.cost_usd_estimated = 0.0;
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
    /// Vendor-reported portion of `cost_usd` (provider emitted a `cost`).
    pub cost_usd_reported: f64,
    /// Table-estimated portion of `cost_usd` (provider omitted `cost`).
    pub cost_usd_estimated: f64,
    pub model: Option<String>,
    pub provider: Option<String>,
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

impl BudgetExceededRecord {
    /// One-line human cause for the breach, e.g.
    /// `budget exceeded ($7.50 > $5.00 max_cost_usd)` for a workflow cap or
    /// `phase impl budget exceeded (150000 > 100000 max_tokens)` for a phase
    /// cap. Reused by the task pause annotation and the `status` /
    /// `daemon health` breach renderers so the wording stays consistent.
    pub fn breach_summary(&self) -> String {
        let (actual, budget) = match self.limit_field {
            BudgetLimitField::MaxCostUsd => (format!("${:.2}", self.actual), format!("${:.2}", self.budget)),
            BudgetLimitField::MaxTokens => (format!("{}", self.actual as u64), format!("{}", self.budget as u64)),
        };
        match self.phase_id.as_deref() {
            Some(phase_id) => {
                format!("phase {phase_id} budget exceeded ({actual} > {budget} {})", self.limit_field.as_str())
            }
            None => format!("budget exceeded ({actual} > {budget} {})", self.limit_field.as_str()),
        }
    }
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
                cost_usd_reported: 0.012,
                cost_usd_estimated: 0.0,
                model: Some("claude-sonnet-4-6".to_string()),
                provider: Some("claude".to_string()),
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
                cost_usd_reported: 0.003,
                cost_usd_estimated: 0.0,
                model: Some("claude-sonnet-4-6".to_string()),
                provider: Some("claude".to_string()),
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
                cost_usd_reported: 0.001,
                cost_usd_estimated: 0.0,
                model: Some("claude-haiku-4".to_string()),
                provider: Some("claude".to_string()),
            },
        );
        let phase_impl = wf.phases.get("impl").unwrap();
        assert_eq!(phase_impl.tokens_input, 110);
        assert_eq!(phase_impl.total_tokens(), 110 + 220 + 55);
        assert!((wf.total_cost_usd - 0.016).abs() < 1e-9);
        assert_eq!(wf.total_tokens, (110 + 220 + 55) + (50 + 30));
    }

    #[test]
    fn record_metadata_folds_provider_attribution_onto_phase() {
        let started = dt("2026-06-01T00:00:00Z");
        let mut wf = WorkflowCost::new("flow", started);
        wf.record_metadata(
            "impl",
            started,
            MetadataDelta {
                input_tokens: 10,
                output_tokens: 10,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.01,
                cost_usd_reported: 0.01,
                cost_usd_estimated: 0.0,
                model: Some("claude-sonnet-4-6".to_string()),
                provider: Some("claude".to_string()),
            },
        );
        let phase = wf.phases.get("impl").unwrap();
        assert_eq!(phase.provider.as_deref(), Some("claude"));
        assert_eq!(phase.model.as_deref(), Some("claude-sonnet-4-6"));
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
                cost_usd_reported: 0.05,
                cost_usd_estimated: 0.0,
                model: None,
                provider: None,
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
                cost_usd_reported: 0.001,
                cost_usd_estimated: 0.0,
                model: None,
                provider: None,
            },
        );
        let phase = wf.phases.get("impl").unwrap();
        assert!(!phase.reset_pending);
        assert_eq!(phase.tokens_input, 100);
    }

    #[test]
    fn clamp_cost_zeroes_negative_zero_and_drift() {
        assert_eq!(clamp_cost(-0.0), 0.0);
        assert_eq!(clamp_cost(-0.0000001), 0.0);
        assert_eq!(clamp_cost(0.0), 0.0);
        assert!((clamp_cost(1.2345) - 1.2345).abs() < 1e-12);
        assert!(!format!("${:.4}", clamp_cost(-0.0)).contains('-'));
    }

    #[test]
    fn reported_and_estimated_split_folds_per_phase() {
        let started = dt("2026-06-01T00:00:00Z");
        let mut wf = WorkflowCost::new("flow", started);
        wf.record_metadata(
            "impl",
            started,
            MetadataDelta { cost_usd: 0.20, cost_usd_reported: 0.20, ..Default::default() },
        );
        wf.record_metadata(
            "impl",
            started,
            MetadataDelta { cost_usd: 0.05, cost_usd_estimated: 0.05, ..Default::default() },
        );
        let phase = wf.phases.get("impl").unwrap();
        assert!((phase.reported_usd() - 0.20).abs() < 1e-9);
        assert!((phase.estimated_usd() - 0.05).abs() < 1e-9);
        assert!((wf.reported_usd() - 0.20).abs() < 1e-9);
        assert!((wf.estimated_usd() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn legacy_phase_without_split_counts_as_fully_reported() {
        let json = r#"{"tokens_input":10,"tokens_output":20,"cost_usd":0.50}"#;
        let phase: PhaseCost = serde_json::from_str(json).unwrap();
        assert!((phase.reported_usd() - 0.50).abs() < 1e-9, "legacy cost is treated as reported");
        assert_eq!(phase.estimated_usd(), 0.0);
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
