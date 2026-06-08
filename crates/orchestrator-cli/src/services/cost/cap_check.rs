//! Pure functions that translate post-fold workflow + phase totals
//! into [`BudgetExceededRecord`] decisions. The aggregator calls
//! [`check_caps`] after each metadata fold and routes the result to
//! the workflow runner via `persistence::append_decision_record`.

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use orchestrator_config::workflow_config::BudgetConfig;

use super::aggregator::{BudgetExceededRecord, BudgetLimitField, BudgetLimitKind, BUDGET_EXCEEDED_SCHEMA_ID};

/// Inputs the aggregator hands to [`check_caps`] after each fold.
pub struct CapCheckInputs<'a> {
    pub workflow_run_id: &'a str,
    pub workflow_id: &'a str,
    pub phase_id: Option<&'a str>,
    pub workflow_budget: Option<&'a BudgetConfig>,
    pub phase_budget: Option<&'a BudgetConfig>,
    pub workflow_tokens: u64,
    pub workflow_cost_usd: f64,
    pub phase_tokens: u64,
    pub phase_cost_usd: f64,
    pub observed_at: DateTime<Utc>,
}

/// Returns any cap that the post-fold totals crossed.
///
/// Workflow caps are checked first because the dispatch's locked
/// semantic decision says workflow caps subsume phase caps. The first
/// breach we find is returned — emitting all breaches at once would
/// just produce duplicate pause requests.
pub fn check_caps(inputs: &CapCheckInputs<'_>) -> Option<BudgetExceededRecord> {
    if let Some(budget) = inputs.workflow_budget {
        if let Some(record) = breach_from(
            budget,
            inputs.workflow_run_id,
            inputs.workflow_id,
            None,
            BudgetLimitKind::Workflow,
            inputs.workflow_tokens,
            inputs.workflow_cost_usd,
            inputs.observed_at,
        ) {
            return Some(record);
        }
    }
    if let (Some(budget), Some(phase_id)) = (inputs.phase_budget, inputs.phase_id) {
        if let Some(record) = breach_from(
            budget,
            inputs.workflow_run_id,
            inputs.workflow_id,
            Some(phase_id),
            BudgetLimitKind::Phase,
            inputs.phase_tokens,
            inputs.phase_cost_usd,
            inputs.observed_at,
        ) {
            return Some(record);
        }
    }
    None
}

fn breach_from(
    budget: &BudgetConfig,
    workflow_run_id: &str,
    workflow_id: &str,
    phase_id: Option<&str>,
    limit_kind: BudgetLimitKind,
    tokens: u64,
    cost_usd: f64,
    observed_at: DateTime<Utc>,
) -> Option<BudgetExceededRecord> {
    if let Some(max_tokens) = budget.max_tokens {
        if tokens > max_tokens {
            return Some(BudgetExceededRecord {
                schema: BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
                workflow_run_id: workflow_run_id.to_string(),
                workflow_id: workflow_id.to_string(),
                phase_id: phase_id.map(str::to_string),
                limit_kind,
                limit_field: BudgetLimitField::MaxTokens,
                actual: tokens as f64,
                budget: max_tokens as f64,
                on_exceed: budget.on_exceed.as_str().to_string(),
                observed_at,
            });
        }
    }
    if let Some(max_cost_usd) = budget.max_cost_usd {
        if cost_usd > max_cost_usd {
            return Some(BudgetExceededRecord {
                schema: BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
                workflow_run_id: workflow_run_id.to_string(),
                workflow_id: workflow_id.to_string(),
                phase_id: phase_id.map(str::to_string),
                limit_kind,
                limit_field: BudgetLimitField::MaxCostUsd,
                actual: cost_usd,
                budget: max_cost_usd,
                on_exceed: budget.on_exceed.as_str().to_string(),
                observed_at,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_config::workflow_config::BudgetOnExceed;

    fn now() -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 6, 1, 12, 0, 0).unwrap()
    }

    #[test]
    fn workflow_cap_breach_wins_over_phase_cap() {
        let workflow_budget =
            BudgetConfig { max_tokens: Some(1_000), max_cost_usd: None, on_exceed: BudgetOnExceed::Pause };
        let phase_budget =
            BudgetConfig { max_tokens: Some(10_000), max_cost_usd: None, on_exceed: BudgetOnExceed::Fail };
        let record = check_caps(&CapCheckInputs {
            workflow_run_id: "wf-1",
            workflow_id: "flow",
            phase_id: Some("impl"),
            workflow_budget: Some(&workflow_budget),
            phase_budget: Some(&phase_budget),
            workflow_tokens: 2_000,
            workflow_cost_usd: 0.0,
            phase_tokens: 2_000,
            phase_cost_usd: 0.0,
            observed_at: now(),
        })
        .expect("should breach workflow cap");
        assert_eq!(record.limit_kind, BudgetLimitKind::Workflow);
        assert_eq!(record.on_exceed, "pause");
    }

    #[test]
    fn phase_cost_breach_emits_record() {
        let phase_budget = BudgetConfig { max_tokens: None, max_cost_usd: Some(1.0), on_exceed: BudgetOnExceed::Fail };
        let record = check_caps(&CapCheckInputs {
            workflow_run_id: "wf-2",
            workflow_id: "flow",
            phase_id: Some("review"),
            workflow_budget: None,
            phase_budget: Some(&phase_budget),
            workflow_tokens: 0,
            workflow_cost_usd: 1.5,
            phase_tokens: 0,
            phase_cost_usd: 1.5,
            observed_at: now(),
        })
        .expect("should breach phase cap");
        assert_eq!(record.limit_kind, BudgetLimitKind::Phase);
        assert_eq!(record.limit_field, BudgetLimitField::MaxCostUsd);
        assert!((record.actual - 1.5).abs() < 1e-9);
    }

    #[test]
    fn no_breach_returns_none() {
        let budget =
            BudgetConfig { max_tokens: Some(10_000), max_cost_usd: Some(5.0), on_exceed: BudgetOnExceed::Pause };
        let record = check_caps(&CapCheckInputs {
            workflow_run_id: "wf-3",
            workflow_id: "flow",
            phase_id: None,
            workflow_budget: Some(&budget),
            phase_budget: None,
            workflow_tokens: 500,
            workflow_cost_usd: 0.1,
            phase_tokens: 0,
            phase_cost_usd: 0.0,
            observed_at: now(),
        });
        assert!(record.is_none());
    }
}
