//! Read-only scanner over `<scoped-root>/runs/<run_id>/events.jsonl`.
//!
//! Used by `animus cost` to build an up-to-date view of per-workflow
//! spend without depending on the daemon. The scanner extracts
//! `AgentRunEvent::Metadata` events and folds them into a fresh
//! [`CostState`].
//!
//! Workflow correlation: the daemon writes
//! `<scoped-root>/runs/<workflow_id>/phases/<phase_id>.session.json`
//! checkpoints (see `animus_runtime_shared::phase_session`) that
//! carry the canonical `(workflow_id, phase_id, run_id)` triple. The
//! scanner reads those first to build a `run_id → (workflow_id,
//! phase_id)` map, then folds metadata events from
//! `runs/<run_id>/events.jsonl` files that match a mapping. Run dirs
//! that follow the legacy `wf-{workflow_id}-...` naming convention
//! but have no session checkpoint fall back to the synthetic phase
//! id [`FALLBACK_PHASE_ID`] so the workflow still surfaces.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use protocol::AgentRunEvent;
use serde_json::Value;

use super::aggregator::{CostState, MetadataDelta, WorkflowCost, WorkflowCostStatus};
use super::cap_check::{check_caps, CapCheckInputs};
use super::model_rates::estimate_cost_usd;
use super::persistence::append_decision_record;

pub(crate) const WORKFLOW_RUN_PREFIX: &str = "wf-";
pub(crate) const FALLBACK_PHASE_ID: &str = "default";

#[derive(Debug, Clone)]
struct RunMapping {
    workflow_id: String,
    phase_id: String,
    run_id: String,
    /// Provider / tool recorded in the phase session checkpoint
    /// (`provider` field). `None` for legacy `wf-`-prefixed runs that
    /// have no checkpoint.
    provider: Option<String>,
}

pub fn runs_root(project_root: &Path) -> PathBuf {
    super::persistence::scoped_root(project_root).join("runs")
}

/// Walk `state.workflows`, look up each workflow's declared
/// [`BudgetConfig`] from `WorkflowConfig`, and append one decision
/// record per breach to the scoped `decisions.jsonl`. De-duplication is
/// the caller's responsibility — typically the workflow runner picks up
/// the latest record per `workflow_run_id` and acts on it. Returns
/// the breaches found. This is the manual `animus cost ...` path; the
/// daemon's housekeeping sweep uses [`super::enforcement`], which
/// de-duplicates and additionally pauses + notifies.
pub fn enforce_caps(project_root: &Path, state: &CostState) -> Result<Vec<super::aggregator::BudgetExceededRecord>> {
    let breaches = evaluate_caps(project_root, state)?;
    for record in &breaches {
        append_decision_record(project_root, record)?;
    }
    Ok(breaches)
}

/// Pure cap evaluation: walk `state.workflows`, look up each workflow's
/// declared [`BudgetConfig`](orchestrator_config::workflow_config::BudgetConfig)
/// from `WorkflowConfig`, and return one [`BudgetExceededRecord`] per cap
/// the observed totals crossed. Writes nothing.
pub fn evaluate_caps(project_root: &Path, state: &CostState) -> Result<Vec<super::aggregator::BudgetExceededRecord>> {
    use orchestrator_config::workflow_config::WorkflowDefinition;
    let workflow_config = orchestrator_core::load_workflow_config_or_default(project_root).config;
    // Map runtime workflow id (the daemon-assigned id stored in
    // session checkpoints) to the YAML `workflow_ref` so we can look
    // up the right `BudgetConfig`. Falls back to an empty map when
    // the workflow DB is not present yet — the workflow_id itself may
    // happen to be the workflow_ref (e.g. when the in-memory test
    // suite or a synthetic enforce_caps call uses workflow_ref as
    // the id), so we still try a direct lookup as a last resort.
    let workflow_ref_index = orchestrator_core::load_workflow_ref_index(project_root).unwrap_or_default();
    let mut breaches = Vec::new();
    for (run_id, workflow) in &state.workflows {
        let resolved_ref = workflow_ref_index.get(&workflow.workflow_id).cloned();
        let lookup_ref = resolved_ref.as_deref().unwrap_or(&workflow.workflow_id);
        let definition: Option<&WorkflowDefinition> =
            workflow_config.workflows.iter().find(|wf| wf.id.eq_ignore_ascii_case(lookup_ref));
        let workflow_budget = definition.and_then(|d| d.budget.as_ref());
        // For per-phase caps, walk the currently observed phases and
        // look up the rich phase entry that declared the budget. A
        // single breach per (run_id, phase) is enough — repeated
        // breaches per check would just produce duplicate
        // decision records.
        let phase_budget_for = |phase_id: &str| -> Option<&orchestrator_config::workflow_config::BudgetConfig> {
            let definition = definition?;
            for entry in &definition.phases {
                if entry.phase_id().eq_ignore_ascii_case(phase_id) {
                    return entry.budget();
                }
            }
            None
        };
        if workflow_budget.is_none() && workflow.phases.values().all(|_| true) {
            // No workflow-level cap; we still let the per-phase check run.
        }
        let observed_at = workflow.updated_at.unwrap_or(workflow.started_at);
        // First check workflow cap.
        if let Some(record) = check_caps(&CapCheckInputs {
            workflow_run_id: run_id,
            workflow_id: &workflow.workflow_id,
            phase_id: None,
            workflow_budget,
            phase_budget: None,
            workflow_tokens: workflow.total_tokens,
            workflow_cost_usd: workflow.total_cost_usd,
            phase_tokens: 0,
            phase_cost_usd: 0.0,
            observed_at,
        }) {
            breaches.push(record);
            continue;
        }
        // Then per-phase caps.
        for (phase_id, phase_cost) in &workflow.phases {
            let phase_budget = phase_budget_for(phase_id);
            if phase_budget.is_none() {
                continue;
            }
            if let Some(record) = check_caps(&CapCheckInputs {
                workflow_run_id: run_id,
                workflow_id: &workflow.workflow_id,
                phase_id: Some(phase_id),
                workflow_budget: None,
                phase_budget,
                workflow_tokens: 0,
                workflow_cost_usd: 0.0,
                phase_tokens: phase_cost.total_tokens(),
                phase_cost_usd: phase_cost.cost_usd,
                observed_at,
            }) {
                breaches.push(record);
            }
        }
    }
    Ok(breaches)
}

/// Half-open `[start, end)` UTC window used to attribute spend to the
/// interval in which it was incurred (per-event timestamp filter), rather
/// than rolling up a run's lifetime totals because it was merely *touched*
/// inside the window.
#[derive(Debug, Clone, Copy)]
pub struct CostWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl CostWindow {
    fn contains(&self, ts: DateTime<Utc>) -> bool {
        ts >= self.start && ts < self.end
    }
}

/// Scan `<scoped-root>/runs/` for workflow runs and fold metadata
/// events into a fresh [`CostState`]. Idempotent and side-effect free.
pub fn scan_runs(project_root: &Path) -> Result<CostState> {
    scan_runs_skipping(project_root, &std::collections::HashSet::new())
}

/// Scan live run directories but fold only metadata events whose
/// timestamp falls inside `window`. The resulting [`CostState`] carries
/// in-window spend deltas, not lifetime totals — the answer to "what did
/// this window cost". Per-event timestamps come from the `events.jsonl`
/// frames the providers write. Side-effect free.
pub fn scan_runs_in_window(project_root: &Path, window: CostWindow) -> Result<CostState> {
    let mut state = CostState::default();
    let runs_dir = runs_root(project_root);
    if !runs_dir.exists() {
        return Ok(state);
    }
    let run_lookup = collect_session_mappings(&runs_dir)?;
    let entries = fs::read_dir(&runs_dir).with_context(|| format!("read runs root {}", runs_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let events_path = path.join("events.jsonl");
        if !events_path.exists() {
            continue;
        }
        let mapping = match run_lookup.get(&dir_name).cloned() {
            Some(mapping) => mapping,
            None => {
                if let Some(workflow_id) = dir_name.strip_prefix(WORKFLOW_RUN_PREFIX) {
                    if workflow_id.is_empty() {
                        continue;
                    }
                    RunMapping {
                        workflow_id: workflow_id.to_string(),
                        phase_id: FALLBACK_PHASE_ID.to_string(),
                        run_id: dir_name.clone(),
                        provider: None,
                    }
                } else {
                    continue;
                }
            }
        };
        fold_events_for_run_windowed(&mapping, &events_path, &mut state, Some(window))?;
    }
    Ok(state)
}

/// [`scan_runs`], but skip run directories whose workflow run id is in
/// `skip_run_ids`. Used by the refresh path to avoid re-folding
/// `events.jsonl` files for workflows already archived into the persisted
/// `cost-state.v1.json` history — those rollups would be dropped after the
/// merge anyway, so skipping them keeps the daemon's housekeeping sweep
/// from re-reading completed runs on every pass.
pub fn scan_runs_skipping(project_root: &Path, skip_run_ids: &std::collections::HashSet<String>) -> Result<CostState> {
    let mut state = CostState::default();
    let runs_dir = runs_root(project_root);
    if !runs_dir.exists() {
        return Ok(state);
    }
    let run_lookup = collect_session_mappings(&runs_dir)?;

    let entries = fs::read_dir(&runs_dir).with_context(|| format!("read runs root {}", runs_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let events_path = path.join("events.jsonl");
        if !events_path.exists() {
            continue;
        }
        let mapping = match run_lookup.get(&dir_name).cloned() {
            Some(mapping) => mapping,
            None => {
                if let Some(workflow_id) = dir_name.strip_prefix(WORKFLOW_RUN_PREFIX) {
                    if workflow_id.is_empty() {
                        continue;
                    }
                    RunMapping {
                        workflow_id: workflow_id.to_string(),
                        phase_id: FALLBACK_PHASE_ID.to_string(),
                        run_id: dir_name.clone(),
                        provider: None,
                    }
                } else {
                    // Not a workflow run; skip.
                    continue;
                }
            }
        };
        if skip_run_ids.contains(&mapping.workflow_id) {
            // Already archived into persisted history — the merged view
            // drops the live rollup anyway, so don't re-read its events.
            continue;
        }
        fold_events_for_run(&mapping, &events_path, &mut state)?;
    }
    Ok(state)
}

// NOTE(codex-p2): the session checkpoint file is overwritten when a
// phase is reworked, so this map carries only the *current* attempt's
// `run_id` for each `(workflow_id, phase_id)` pair. Prior-attempt
// `runs/<run_id>/events.jsonl` directories are orphaned and dropped
// from the scan. That matches the dispatch's locked semantic
// ("phase rework resets the per-phase counter"), but it also drops
// the spend from the prior attempt from the workflow rollup, which
// is a known limitation. A follow-up will tee a per-attempt sidecar
// so the scanner can sum across attempts when the workflow-level
// budget is what's authoritative.
fn collect_session_mappings(runs_dir: &Path) -> Result<HashMap<String, RunMapping>> {
    let mut out: HashMap<String, RunMapping> = HashMap::new();
    let entries = fs::read_dir(runs_dir).with_context(|| format!("read runs root {}", runs_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let phases_dir = entry.path().join("phases");
        if !phases_dir.is_dir() {
            continue;
        }
        let phase_entries = match fs::read_dir(&phases_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for phase_entry in phase_entries {
            let phase_entry = phase_entry?;
            let path = phase_entry.path();
            if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".session.json")) {
                continue;
            }
            let raw = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let value: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let workflow_id = value.get("workflow_id").and_then(Value::as_str).map(ToOwned::to_owned);
            let phase_id = value.get("phase_id").and_then(Value::as_str).map(ToOwned::to_owned);
            let run_id = value.get("run_id").and_then(Value::as_str).map(ToOwned::to_owned);
            // `provider` is the tool that drove the phase (claude / codex /
            // gemini / ...). Empty strings are treated as missing so the
            // breakdown views bucket them under "unknown" rather than "".
            let provider = value
                .get("provider")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|provider| !provider.is_empty())
                .map(ToOwned::to_owned);
            if let (Some(workflow_id), Some(phase_id), Some(run_id)) = (workflow_id, phase_id, run_id) {
                out.insert(run_id.clone(), RunMapping { workflow_id, phase_id, run_id, provider });
            }
        }
    }
    Ok(out)
}

fn fold_events_for_run(mapping: &RunMapping, events_path: &Path, state: &mut CostState) -> Result<bool> {
    fold_events_for_run_windowed(mapping, events_path, state, None)
}

/// Fold a run's events into `state`. When `window` is `Some`, only
/// metadata events whose timestamp falls inside the window contribute
/// spend — the rollup then reflects in-window deltas instead of lifetime
/// totals. `None` keeps the lifetime behavior.
fn fold_events_for_run_windowed(
    mapping: &RunMapping,
    events_path: &Path,
    state: &mut CostState,
    window: Option<CostWindow>,
) -> Result<bool> {
    let raw = fs::read_to_string(events_path).with_context(|| format!("read events log {}", events_path.display()))?;
    let mut observed = false;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut last_observed_at: Option<DateTime<Utc>> = None;
    // Last timestamp seen on any frame, carried forward to place
    // untimestamped metadata frames inside/outside the requested window.
    let mut carry_ts: Option<DateTime<Utc>> = None;
    // A `Finished` / `Error` frame here marks the per-phase RUN as
    // terminal, NOT the workflow. We only carry the status forward
    // when we can be sure the workflow itself is done; for now that
    // means we leave the workflow `Running` and let the daemon's
    // archive hook flip it via `CostState::archive_workflow` once
    // every phase has settled. The honest cost of being wrong here
    // is much higher than the cost of being slow: marking a
    // multi-phase workflow as completed mid-run would corrupt
    // `summary` / `top`.
    let mut phase_run_terminal: Option<WorkflowCostStatus> = None;
    let mut pending: BTreeMap<String, MetadataDelta> = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw_event: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let line_ts = extract_timestamp(&raw_event, "timestamp");
        if started_at.is_none() {
            started_at = line_ts;
        }
        // Carry the last-seen frame timestamp forward. `AgentRunEvent`
        // frames that carry a timestamp today are `Started` (run start);
        // `Metadata` frames carry none on the wire (see
        // `animus_runtime_shared::ipc::persist_run_event`, which
        // serializes the bare enum with no envelope timestamp). Window
        // attribution for an untimestamped metadata frame therefore
        // falls back to the most recent timestamped frame in the same
        // run — i.e. the run's `Started` time — so a run that started
        // outside the window is correctly excluded instead of always
        // counting as in-window.
        if let Some(ts) = line_ts {
            carry_ts = Some(ts);
        }
        // Pull the model id from any sidecar field we have. Different
        // provider plugins use slightly different names; we accept the
        // most common ones so the fallback rate table fires when the
        // provider does not report `cost`.
        //
        // TODO(codex-p2): the in-tree provider path
        // (`runtime_agent::provider_client::to_agent_event`) currently
        // serializes only `run_id`, `cost`, and `tokens`. Providers
        // that omit `cost` therefore lose model context here and the
        // fallback rate table never fires. Plugin authors that want
        // accurate USD attribution should attach `model_id` to the
        // Metadata frame sidecar; longer term, the runner should
        // emit a `model_id` field alongside every Metadata event.
        let event_model = raw_event
            .get("model_id")
            .or_else(|| raw_event.get("model"))
            .or_else(|| raw_event.pointer("/payload/model"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let event: AgentRunEvent = match serde_json::from_value(raw_event.clone()) {
            Ok(event) => event,
            Err(_) => continue,
        };
        match event {
            AgentRunEvent::Metadata { cost, tokens, .. } => {
                // Window attribution: skip events incurred outside the
                // requested interval so the rollup is in-window spend,
                // not lifetime totals of a merely-touched run. Metadata
                // frames carry no timestamp of their own on the wire, so
                // we place them by the most recent timestamped frame in
                // the run (`carry_ts`, typically the `Started` frame).
                // Frames we cannot place at all fall through as in-window
                // (dropping silently would understate spend).
                if let Some(window) = window {
                    if let Some(ts) = extract_timestamp(&raw_event, "timestamp").or(carry_ts) {
                        if !window.contains(ts) {
                            continue;
                        }
                    }
                }
                let mut delta = MetadataDelta::default();
                if let Some(tokens) = tokens {
                    delta.input_tokens = u64::from(tokens.input);
                    delta.output_tokens = u64::from(tokens.output);
                    delta.reasoning_tokens = tokens.reasoning.map(u64::from).unwrap_or(0);
                    delta.cache_read_tokens = tokens.cache_read.map(u64::from).unwrap_or(0);
                    delta.cache_write_tokens = tokens.cache_write.map(u64::from).unwrap_or(0);
                }
                let total_tokens =
                    delta.input_tokens.saturating_add(delta.output_tokens).saturating_add(delta.reasoning_tokens);
                if let Some(cost) = cost {
                    if cost.is_finite() {
                        delta.cost_usd = cost;
                        delta.cost_usd_reported = cost;
                    }
                } else if let Some(model_id) = event_model.as_deref() {
                    if let Some(estimated) = estimate_cost_usd(model_id, total_tokens) {
                        delta.cost_usd = estimated;
                        delta.cost_usd_estimated = estimated;
                    }
                }
                delta.model = event_model;
                delta.provider = mapping.provider.clone();
                pending.entry(mapping.phase_id.clone()).and_modify(|d| merge_delta(d, &delta)).or_insert(delta);
                observed = true;
                last_observed_at = extract_timestamp(&raw_event, "timestamp").or(last_observed_at);
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                phase_run_terminal = Some(if exit_code.unwrap_or(0) == 0 {
                    WorkflowCostStatus::Completed
                } else {
                    WorkflowCostStatus::Failed
                });
                last_observed_at = extract_timestamp(&raw_event, "timestamp").or(last_observed_at);
            }
            AgentRunEvent::Error { .. } => {
                phase_run_terminal = Some(WorkflowCostStatus::Failed);
                last_observed_at = extract_timestamp(&raw_event, "timestamp").or(last_observed_at);
            }
            _ => {}
        }
    }
    if !observed && phase_run_terminal.is_none() {
        return Ok(false);
    }
    let started = started_at.unwrap_or_else(Utc::now);
    let observed_at = last_observed_at.unwrap_or(started);
    // Aggregate under the workflow id, not the per-phase run id, so
    // a workflow with multiple phases rolls up into a single
    // `WorkflowCost`. The `runs/<workflow_id>/phases/` directory
    // structure already enforces one-active-rollup-per-workflow_id;
    // historical runs of the same workflow id get archived to
    // `history` before a new run starts.
    let workflow: &mut WorkflowCost = state.ensure_workflow(&mapping.workflow_id, &mapping.workflow_id, started);
    if workflow.started_at > started {
        workflow.started_at = started;
    }
    for (phase_id, delta) in pending {
        workflow.record_metadata(&phase_id, observed_at, delta);
    }
    // A failed phase is the only signal we trust without a full
    // workflow lifecycle event: a failed phase has stopped the
    // workflow as well, so the workflow rollup can safely flip to
    // `Failed`. A successful phase says nothing about later phases,
    // so we leave the workflow `Running` (the daemon's archive hook
    // will flip it once the workflow lifecycle completes).
    //
    // TODO(codex-p2): without an archive hook, completed
    // single-phase workflows stay `Running` forever from the
    // scanner's point of view. The fix is to teach the daemon's
    // workflow lifecycle path to call
    // `CostState::archive_workflow` on the workflow-completed
    // event; that lands in the same v0.5.5 follow-up that wires
    // budget-exceeded decisions into the runner.
    if let Some(WorkflowCostStatus::Failed) = phase_run_terminal {
        workflow.status = WorkflowCostStatus::Failed;
        workflow.updated_at = Some(observed_at);
    }
    Ok(true)
}

fn merge_delta(into: &mut MetadataDelta, from: &MetadataDelta) {
    into.input_tokens = into.input_tokens.saturating_add(from.input_tokens);
    into.output_tokens = into.output_tokens.saturating_add(from.output_tokens);
    into.reasoning_tokens = into.reasoning_tokens.saturating_add(from.reasoning_tokens);
    into.cache_read_tokens = into.cache_read_tokens.saturating_add(from.cache_read_tokens);
    into.cache_write_tokens = into.cache_write_tokens.saturating_add(from.cache_write_tokens);
    into.cost_usd += from.cost_usd;
    into.cost_usd_reported += from.cost_usd_reported;
    into.cost_usd_estimated += from.cost_usd_estimated;
    if from.model.is_some() {
        into.model = from.model.clone();
    }
    if from.provider.is_some() {
        into.provider = from.provider.clone();
    }
}

fn extract_timestamp(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    let raw = value.pointer(&format!("/{key}/0")).or_else(|| value.get(key))?;
    let text = raw.as_str()?;
    DateTime::parse_from_rfc3339(text).ok().map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_env_lock;
    use protocol::test_utils::EnvVarGuard;
    use protocol::{RunId, Timestamp, TokenUsage};
    use tempfile::TempDir;

    fn write_events(dir: &Path, events: &[AgentRunEvent]) {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("events.jsonl");
        let mut text = String::new();
        for event in events {
            text.push_str(&serde_json::to_string(event).unwrap());
            text.push('\n');
        }
        fs::write(path, text).unwrap();
    }

    fn arrange_override(tmp: &TempDir) -> (EnvVarGuard, PathBuf) {
        let state_root = tmp.path().join("scope");
        fs::create_dir_all(&state_root).unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let guard = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(state_root.to_string_lossy().as_ref()));
        (guard, project_root)
    }

    fn write_session_checkpoint(scoped: &Path, workflow_id: &str, phase_id: &str, run_id: &str) {
        let dir = scoped.join("runs").join(workflow_id).join("phases");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{phase_id}.session.json"));
        let body = serde_json::json!({
            "workflow_id": workflow_id,
            "phase_id": phase_id,
            "provider": "test",
            "run_id": run_id,
            "provider_session_id": null,
            "status": "running",
            "started_at": "2026-06-01T00:00:00Z",
        });
        fs::write(path, serde_json::to_string_pretty(&body).unwrap()).unwrap();
    }

    #[test]
    fn scan_folds_metadata_into_per_workflow_state_via_session_mapping() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "standard-workflow", "code-review", "run-deadbeef");
        let run_dir = scoped.join("runs").join("run-deadbeef");
        let event = AgentRunEvent::Metadata {
            run_id: RunId("run-deadbeef".to_string()),
            cost: Some(0.25),
            tokens: Some(TokenUsage {
                input: 1_000,
                output: 2_000,
                reasoning: Some(500),
                cache_read: None,
                cache_write: None,
            }),
        };
        write_events(&run_dir, &[event]);

        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("standard-workflow").expect("workflow tracked by workflow id");
        assert_eq!(wf.workflow_id, "standard-workflow");
        let phase = wf.phases.get("code-review").expect("phase tracked from session checkpoint");
        assert_eq!(phase.tokens_input, 1_000);
        assert_eq!(phase.total_tokens(), 1_000 + 2_000 + 500);
        assert!((phase.cost_usd - 0.25).abs() < 1e-9);
    }

    #[test]
    fn scan_captures_provider_attribution_from_session_checkpoint() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-attr");
        let run_dir = scoped.join("runs").join("run-attr");
        // Hand-construct so we can attach a sidecar `model_id` alongside
        // the provider sourced from the checkpoint (`"test"`).
        fs::create_dir_all(&run_dir).unwrap();
        let mut event_value = serde_json::to_value(&AgentRunEvent::Metadata {
            run_id: RunId("run-attr".to_string()),
            cost: Some(0.5),
            tokens: Some(TokenUsage { input: 100, output: 100, reasoning: None, cache_read: None, cache_write: None }),
        })
        .unwrap();
        event_value
            .as_object_mut()
            .unwrap()
            .insert("model_id".to_string(), Value::String("claude-sonnet-4-6".to_string()));
        fs::write(run_dir.join("events.jsonl"), format!("{}\n", serde_json::to_string(&event_value).unwrap())).unwrap();

        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("flow").expect("workflow tracked");
        let phase = wf.phases.get("impl").expect("phase tracked");
        assert_eq!(phase.provider.as_deref(), Some("test"), "provider sourced from checkpoint");
        assert_eq!(phase.model.as_deref(), Some("claude-sonnet-4-6"), "model sourced from event sidecar");
    }

    #[test]
    fn scan_leaves_provider_unset_for_legacy_runs_without_checkpoint() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        let run_dir = scoped.join("runs").join("wf-legacy-attr");
        write_events(
            &run_dir,
            &[AgentRunEvent::Metadata {
                run_id: RunId("wf-legacy-attr".to_string()),
                cost: Some(0.01),
                tokens: Some(TokenUsage {
                    input: 10,
                    output: 10,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            }],
        );
        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("legacy-attr").expect("legacy workflow tracked");
        let phase = wf.phases.get(FALLBACK_PHASE_ID).expect("fallback phase tracked");
        assert!(phase.provider.is_none(), "legacy runs without a checkpoint carry no provider attribution");
    }

    #[test]
    fn scan_uses_fallback_phase_for_legacy_wf_prefixed_runs_without_checkpoint() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        let run_dir = scoped.join("runs").join("wf-legacy");
        let event = AgentRunEvent::Metadata {
            run_id: RunId("wf-legacy".to_string()),
            cost: Some(0.01),
            tokens: Some(TokenUsage { input: 100, output: 200, reasoning: None, cache_read: None, cache_write: None }),
        };
        write_events(&run_dir, &[event]);
        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("legacy").expect("legacy workflow tracked by workflow id");
        assert_eq!(wf.workflow_id, "legacy");
        assert!(wf.phases.contains_key(FALLBACK_PHASE_ID));
    }

    #[test]
    fn scan_skips_non_workflow_runs() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        let run_dir = scoped.join("runs").join("ad-hoc-run");
        let event = AgentRunEvent::Started { run_id: RunId("ad-hoc-run".to_string()), timestamp: Timestamp::now() };
        write_events(&run_dir, &[event]);
        let state = scan_runs(&project_root).unwrap();
        assert!(state.workflows.is_empty());
    }

    #[test]
    fn scan_keeps_workflow_running_when_only_phase_finishes_with_zero_exit() {
        // A phase-run `Finished { exit_code: 0 }` says nothing about
        // whether subsequent phases will run. Only the daemon's
        // archive hook can flip a workflow to Completed.
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-completes");
        let run_dir = scoped.join("runs").join("run-completes");
        let events = vec![
            AgentRunEvent::Started { run_id: RunId("run-completes".to_string()), timestamp: Timestamp::now() },
            AgentRunEvent::Metadata {
                run_id: RunId("run-completes".to_string()),
                cost: Some(0.1),
                tokens: Some(TokenUsage {
                    input: 100,
                    output: 100,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            },
            AgentRunEvent::Finished {
                run_id: RunId("run-completes".to_string()),
                exit_code: Some(0),
                duration_ms: 100,
            },
        ];
        write_events(&run_dir, &events);
        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("flow").expect("workflow tracked by workflow id");
        assert_eq!(wf.status, WorkflowCostStatus::Running);
    }

    #[test]
    fn scan_marks_workflows_failed_when_phase_finishes_with_nonzero_exit() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-fails");
        let run_dir = scoped.join("runs").join("run-fails");
        let events = vec![
            AgentRunEvent::Metadata {
                run_id: RunId("run-fails".to_string()),
                cost: Some(0.1),
                tokens: Some(TokenUsage {
                    input: 100,
                    output: 100,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            },
            AgentRunEvent::Finished { run_id: RunId("run-fails".to_string()), exit_code: Some(42), duration_ms: 100 },
        ];
        write_events(&run_dir, &events);
        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("flow").expect("workflow tracked by workflow id");
        assert_eq!(wf.status, WorkflowCostStatus::Failed);
    }

    #[test]
    fn scan_uses_fallback_model_rate_when_provider_omits_cost_but_sets_model() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-estimated");
        let run_dir = scoped.join("runs").join("run-estimated");
        // Hand-construct the line so we can attach a sidecar `model_id`
        // that AgentRunEvent doesn't model directly.
        fs::create_dir_all(&run_dir).unwrap();
        let mut text = String::new();
        let mut event_value = serde_json::to_value(&AgentRunEvent::Metadata {
            run_id: RunId("run-estimated".to_string()),
            cost: None,
            tokens: Some(TokenUsage {
                input: 500_000,
                output: 0,
                reasoning: None,
                cache_read: None,
                cache_write: None,
            }),
        })
        .unwrap();
        event_value
            .as_object_mut()
            .unwrap()
            .insert("model_id".to_string(), Value::String("claude-sonnet-4-6".to_string()));
        text.push_str(&serde_json::to_string(&event_value).unwrap());
        text.push('\n');
        fs::write(run_dir.join("events.jsonl"), text).unwrap();

        let state = scan_runs(&project_root).unwrap();
        let wf = state.workflows.get("flow").expect("workflow tracked by workflow id");
        let phase = wf.phases.get("impl").expect("phase tracked");
        // claude-sonnet rate is $6/M for 500_000 → $3.00
        assert!((phase.cost_usd - 3.0).abs() < 1e-6, "expected $3 estimate, got {}", phase.cost_usd);
        assert_eq!(phase.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn scan_aggregates_multiple_phase_runs_under_single_workflow_id() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        // Two phases of the SAME workflow with distinct per-phase run ids.
        write_session_checkpoint(&scoped, "flow", "impl", "run-impl");
        write_session_checkpoint(&scoped, "flow", "review", "run-review");
        let impl_dir = scoped.join("runs").join("run-impl");
        let review_dir = scoped.join("runs").join("run-review");
        write_events(
            &impl_dir,
            &[AgentRunEvent::Metadata {
                run_id: RunId("run-impl".to_string()),
                cost: Some(0.10),
                tokens: Some(TokenUsage {
                    input: 100,
                    output: 200,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            }],
        );
        write_events(
            &review_dir,
            &[AgentRunEvent::Metadata {
                run_id: RunId("run-review".to_string()),
                cost: Some(0.05),
                tokens: Some(TokenUsage {
                    input: 50,
                    output: 50,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            }],
        );
        let state = scan_runs(&project_root).unwrap();
        // Both phase runs must fold into ONE workflow keyed by workflow_id.
        assert_eq!(state.workflows.len(), 1, "expected single workflow entry, got {}", state.workflows.len());
        let wf = state.workflows.get("flow").expect("workflow tracked by workflow id");
        assert_eq!(wf.phases.len(), 2);
        assert!((wf.total_cost_usd - 0.15).abs() < 1e-9);
        assert_eq!(wf.total_tokens, 300 + 100);
    }

    #[test]
    fn scan_in_window_folds_only_events_inside_the_window() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-win");
        let run_dir = scoped.join("runs").join("run-win");
        fs::create_dir_all(&run_dir).unwrap();
        // Two metadata frames with explicit timestamps: one inside the
        // window, one before it. Hand-built so the `timestamp` field is
        // present (AgentRunEvent::Metadata does not carry one).
        let frame = |ts: &str, cost: f64| {
            let mut value = serde_json::to_value(AgentRunEvent::Metadata {
                run_id: RunId("run-win".to_string()),
                cost: Some(cost),
                tokens: Some(TokenUsage {
                    input: 100,
                    output: 100,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            })
            .unwrap();
            value.as_object_mut().unwrap().insert("timestamp".to_string(), Value::String(ts.to_string()));
            serde_json::to_string(&value).unwrap()
        };
        let body = format!("{}\n{}\n", frame("2026-06-01T00:00:00Z", 1.00), frame("2026-06-10T00:00:00Z", 0.25));
        fs::write(run_dir.join("events.jsonl"), body).unwrap();

        // Window covers only the second (later) frame.
        let window = CostWindow {
            start: DateTime::parse_from_rfc3339("2026-06-05T00:00:00Z").unwrap().with_timezone(&Utc),
            end: DateTime::parse_from_rfc3339("2026-06-15T00:00:00Z").unwrap().with_timezone(&Utc),
        };
        let state = scan_runs_in_window(&project_root, window).unwrap();
        let wf = state.workflows.get("flow").expect("workflow tracked");
        assert!(
            (wf.total_cost_usd - 0.25).abs() < 1e-9,
            "only the in-window $0.25 frame counts, got {}",
            wf.total_cost_usd
        );

        // The lifetime scan still sees both frames.
        let lifetime = scan_runs(&project_root).unwrap();
        let wf = lifetime.workflows.get("flow").expect("workflow tracked");
        assert!((wf.total_cost_usd - 1.25).abs() < 1e-9, "lifetime sums both frames, got {}", wf.total_cost_usd);
    }

    #[test]
    fn scan_marks_estimated_when_provider_omits_cost() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-est");
        let run_dir = scoped.join("runs").join("run-est");
        fs::create_dir_all(&run_dir).unwrap();
        let mut value = serde_json::to_value(AgentRunEvent::Metadata {
            run_id: RunId("run-est".to_string()),
            cost: None,
            tokens: Some(TokenUsage {
                input: 500_000,
                output: 0,
                reasoning: None,
                cache_read: None,
                cache_write: None,
            }),
        })
        .unwrap();
        value.as_object_mut().unwrap().insert("model_id".to_string(), Value::String("claude-sonnet-4-6".to_string()));
        fs::write(run_dir.join("events.jsonl"), format!("{}\n", serde_json::to_string(&value).unwrap())).unwrap();

        let state = scan_runs(&project_root).unwrap();
        let phase = state.workflows.get("flow").unwrap().phases.get("impl").unwrap();
        assert!((phase.estimated_usd() - 3.0).abs() < 1e-6, "estimate marked, got {}", phase.estimated_usd());
        assert_eq!(phase.reported_usd(), 0.0, "no vendor cost was reported");
    }

    #[test]
    fn scan_marks_reported_when_provider_supplies_cost() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let scoped = super::super::persistence::scoped_root(&project_root);
        write_session_checkpoint(&scoped, "flow", "impl", "run-rep");
        let run_dir = scoped.join("runs").join("run-rep");
        write_events(
            &run_dir,
            &[AgentRunEvent::Metadata {
                run_id: RunId("run-rep".to_string()),
                cost: Some(0.42),
                tokens: Some(TokenUsage {
                    input: 10,
                    output: 10,
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }),
            }],
        );
        let state = scan_runs(&project_root).unwrap();
        let phase = state.workflows.get("flow").unwrap().phases.get("impl").unwrap();
        assert!((phase.reported_usd() - 0.42).abs() < 1e-9);
        assert_eq!(phase.estimated_usd(), 0.0);
    }

    #[test]
    fn enforce_caps_emits_decision_when_workflow_cap_is_crossed() {
        use super::super::aggregator::BudgetLimitKind;
        use chrono::Utc;
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        // The scanner consults the project's workflow YAML to find
        // budget caps. Write a minimal `.animus/workflows.yaml` that
        // declares a workflow `flow` with `max_tokens: 1000`.
        fs::create_dir_all(project_root.join(".animus")).unwrap();
        fs::write(
            project_root.join(".animus").join("workflows.yaml"),
            r#"
tools_allowlist:
  - cargo
agents:
  default:
    description: Default
    system_prompt: Default agent
phases:
  implementation:
    mode: agent
    agent_id: default
workflows:
  - id: flow
    name: Flow
    phases:
      - implementation
    budget:
      max_tokens: 1000
      on_exceed: pause
"#,
        )
        .unwrap();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&project_root);
        // Hand-build a CostState already over budget.
        let mut state = CostState::default();
        let mut wf = WorkflowCost::new("flow", Utc::now());
        wf.record_metadata(
            "implementation",
            Utc::now(),
            MetadataDelta {
                input_tokens: 5_000,
                output_tokens: 5_000,
                reasoning_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.0,
                cost_usd_reported: 0.0,
                cost_usd_estimated: 0.0,
                model: None,
                provider: None,
            },
        );
        state.workflows.insert("flow".to_string(), wf);
        let breaches = super::enforce_caps(&project_root, &state).unwrap();
        assert_eq!(breaches.len(), 1, "expected one breach, got {breaches:?}");
        let record = &breaches[0];
        assert_eq!(record.limit_kind, BudgetLimitKind::Workflow);
        assert_eq!(record.on_exceed, "pause");
        assert!((record.budget - 1_000.0).abs() < 1e-9);
        assert!(record.actual > 1_000.0);

        // Confirm the decision was appended to decisions.jsonl too.
        let read_back = super::super::persistence::read_decision_records(&project_root).unwrap();
        assert!(read_back.iter().any(|r| r.workflow_run_id == "flow"));
    }
}
