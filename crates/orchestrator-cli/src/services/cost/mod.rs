//! Cost aggregation, budget cap evaluation, and `animus cost` data
//! sources. See `aggregator.rs` for the data model and
//! `scanner.rs` for the events.jsonl reader.

pub(crate) mod aggregator;
pub(crate) mod cap_check;
pub(crate) mod model_rates;
pub(crate) mod persistence;
pub(crate) mod scanner;

pub(crate) use aggregator::{CostState, PhaseCost, WorkflowCost};
pub(crate) use persistence::save_cost_state;
pub(crate) use scanner::{enforce_caps, scan_runs};

// Surface re-exports kept for the v0.5.5 daemon-side budget cap hook
// (see `services/runtime/runtime_daemon` work landing on top of this
// change). They are not yet referenced inside this crate, but the cost
// module exposes them as the stable wire surface that other parts of
// the workspace + the workflow_runner plugin consume.
#[allow(unused_imports)]
pub(crate) use aggregator::{
    BudgetExceededRecord, BudgetLimitField, BudgetLimitKind, HistorySummary, WorkflowCostStatus,
    BUDGET_EXCEEDED_SCHEMA_ID, COST_STATE_SCHEMA_ID,
};
#[allow(unused_imports)]
pub(crate) use cap_check::{check_caps, CapCheckInputs};
#[allow(unused_imports)]
pub(crate) use persistence::{
    append_decision_record, cost_state_path, decisions_log_path, load_cost_state, read_decision_records,
};
