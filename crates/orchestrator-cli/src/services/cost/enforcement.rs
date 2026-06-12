//! Daemon-side budget-cap enforcement.
//!
//! The manual `animus cost ...` path ([`super::scanner::enforce_caps`])
//! only appends breach records to the scoped fleet log
//! (`~/.animus/<repo-scope>/decisions.jsonl`) — records active workflow
//! runners never replay. This module is the housekeeping-sweep
//! counterpart that closes the loop: it
//!
//! 1. refreshes the cost rollup (reusing the persisted
//!    `cost-state.v1.json` history so completed runs are not re-read),
//! 2. evaluates declared workflow/phase caps,
//! 3. applies the declared `on_exceed` action: `pause` goes through the
//!    same [`orchestrator_core::dispatch_workflow_event`] pause path
//!    `animus workflow pause` uses (which annotates the task); `fail`
//!    fails the current phase terminally (the breaching runner may
//!    already have exited, so the daemon cannot rely on a replay);
//!    `warn` records + notifies only,
//! 4. writes each NEW breach into the breaching run's
//!    `runs/<run_id>/decisions.jsonl` (the log `animus output decisions`
//!    reads and the workflow runner replays),
//! 5. appends the scoped fleet record, and
//! 6. returns one [`BudgetBreachEvent`] per enforced breach so the daemon
//!    run host can emit a `workflow-budget-breach` notifier event.
//!
//! De-duplication: the per-run decision record doubles as the enforcement
//! marker. A breach whose record is already present in the resolved run's
//! `decisions.jsonl` was enforced on an earlier sweep (or by an earlier
//! phase attempt) and is skipped entirely — no duplicate pause, no
//! notification storm. Because the cost totals only grow, a breach stays
//! breached; one marker therefore suppresses all later sweeps for the
//! same phase attempt.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_runtime_shared::recording::{append_decision_event, DecisionEvent};
use anyhow::{Context, Result};
use orchestrator_core::services::ServiceHub;
use orchestrator_core::{dispatch_workflow_event, WorkflowEvent, WorkflowStatus};
use orchestrator_daemon_runtime::BudgetBreachEvent;

use super::aggregator::{BudgetExceededRecord, BudgetLimitKind, BUDGET_EXCEEDED_SCHEMA_ID};
use super::persistence::{append_decision_record, scoped_root};

const ON_EXCEED_WARN: &str = "warn";
const ON_EXCEED_FAIL: &str = "fail";
const ACTION_PAUSED: &str = "paused";
const ACTION_FAILED: &str = "failed";
const ACTION_RECORDED: &str = "recorded";

/// Run the full daemon-side enforcement sweep for one project. Returns
/// one event per breach enforced THIS sweep (already-enforced breaches
/// are skipped). Non-fatal problems are routed through `warn`.
pub(crate) async fn run_budget_enforcement(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    warn: &mut dyn FnMut(String),
) -> Result<Vec<BudgetBreachEvent>> {
    let project_path = Path::new(project_root);
    let state = super::refresh_cost_state(project_path, &mut *warn)?;
    let breaches = super::scanner::evaluate_caps(project_path, &state)?;
    apply_breach_actions(hub, project_root, breaches, warn).await
}

/// Act on evaluated breaches: per-run decision record + scoped fleet
/// record + pause + notifier event, exactly once per breach.
pub(crate) async fn apply_breach_actions(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    breaches: Vec<BudgetExceededRecord>,
    warn: &mut dyn FnMut(String),
) -> Result<Vec<BudgetBreachEvent>> {
    let project_path = Path::new(project_root);
    let mut events = Vec::new();
    for record in breaches {
        // Anchor the breach to the run whose decision log the active
        // workflow runner replays.
        let Some(run_id) = resolve_enforcement_run_id(project_path, &record) else {
            warn(format!(
                "budget breach on workflow run '{}' has no resolvable run directory; skipping enforcement",
                record.workflow_run_id
            ));
            continue;
        };
        if dedup_run_ids(project_path, &record, &run_id)
            .iter()
            .any(|candidate| run_log_has_breach(project_path, candidate, &record))
        {
            // Enforced on an earlier sweep — never act twice. Workflow-level
            // breaches check every phase run log, not just the current
            // anchor, so a workflow cap warned about during phase A does not
            // re-notify after the workflow advances to phase B.
            continue;
        }
        // Act BEFORE writing the enforcement marker: a transient action
        // failure must leave the breach unmarked so the next sweep retries
        // it. The inverse crash window (acted, then crashed before the
        // marker landed) is benign — re-applying the action to an
        // already-paused/failed workflow is a no-op and the breach still
        // notifies exactly once.
        let mut action = ACTION_RECORDED;
        if record.on_exceed != ON_EXCEED_WARN {
            // A workflow with no live record (legacy run dirs, pruned
            // state) cannot be acted on, ever — record the breach anyway
            // instead of retrying forever. Any OTHER lookup failure is
            // treated as transient: leave the breach unmarked so the next
            // sweep retries the action.
            match hub.workflows().get(&record.workflow_id).await {
                Ok(_) => match enforce_on_exceed_action(hub.clone(), project_root, &record, warn).await {
                    Ok(taken) => action = taken,
                    Err(()) => continue,
                },
                Err(error)
                    if error
                        .downcast_ref::<protocol::ClassifiedError>()
                        .is_some_and(|classified| classified.kind() == protocol::ErrorKind::NotFound) =>
                {
                    warn(format!(
                        "budget breach on workflow '{}' has no live workflow record; recording without pause",
                        record.workflow_id
                    ));
                }
                Err(error) => {
                    warn(format!(
                        "failed to look up workflow '{}' for budget breach: {error}; retrying next sweep",
                        record.workflow_id
                    ));
                    continue;
                }
            }
        }
        // The per-run record is the enforcement marker; until it lands the
        // next sweep re-enforces (the duplicate pause is a no-op).
        if let Err(error) = write_run_breach_record(project_path, &run_id, &record) {
            warn(format!("failed to write per-run budget decision for '{run_id}': {error}; deferring enforcement"));
            continue;
        }
        // Scoped fleet record (`animus cost decisions` reads this).
        if let Err(error) = append_decision_record(project_path, &record) {
            warn(format!("failed to append scoped budget decision for '{}': {error}", record.workflow_run_id));
        }
        events.push(to_breach_event(&record, action));
    }
    Ok(events)
}

/// Apply the declared `on_exceed` action to a live workflow. Returns the
/// action actually taken, or `Err(())` when the action failed transiently
/// and the breach must stay unmarked so the next sweep retries.
async fn enforce_on_exceed_action(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    record: &BudgetExceededRecord,
    warn: &mut dyn FnMut(String),
) -> std::result::Result<&'static str, ()> {
    if record.on_exceed == ON_EXCEED_FAIL {
        // `fail` is terminal by declaration. The daemon applies it directly
        // — the breaching phase's runner may already have exited, so there
        // may be no process left to replay the per-run record and escalate.
        let reason = format!(
            "budget cap exceeded: {} {} ({} > {})",
            match record.limit_kind {
                BudgetLimitKind::Workflow => "workflow",
                BudgetLimitKind::Phase => "phase",
            },
            record.limit_field.as_str(),
            record.actual,
            record.budget
        );
        // `fail_current_phase` only lands terminally on a RUNNING
        // workflow. If the workflow is paused (an operator, or an earlier
        // sweep racing this one), resume it first — through the same
        // dispatch path `animus workflow resume` uses, so the task's pause
        // annotation is cleared — and then fail it. A resume failure is
        // transient: leave the breach unmarked and retry next sweep.
        let current = match hub.workflows().get(&record.workflow_id).await {
            Ok(workflow) => workflow,
            Err(error) => {
                warn(format!(
                    "failed to look up workflow '{}' for budget fail action: {error}; retrying next sweep",
                    record.workflow_id
                ));
                return Err(());
            }
        };
        if current.status == WorkflowStatus::Paused {
            if let Err(error) = dispatch_workflow_event(
                hub.clone(),
                project_root,
                WorkflowEvent::Resume { workflow_id: record.workflow_id.clone(), feedback: None },
            )
            .await
            {
                warn(format!(
                    "failed to resume paused workflow '{}' to apply budget fail action: {error}; retrying next sweep",
                    record.workflow_id
                ));
                return Err(());
            }
        }
        match hub.workflows().fail_current_phase(&record.workflow_id, reason.clone()).await {
            Ok(workflow) if workflow.status == WorkflowStatus::Failed => {
                // Project the failure onto the task the same way the
                // daemon's completion reconciliation does.
                if let Some(task_id) = orchestrator_core::workflow_task_id(&workflow) {
                    orchestrator_core::project_task_terminal_workflow_status(
                        hub,
                        &task_id,
                        WorkflowStatus::Failed,
                        Some(reason),
                    )
                    .await;
                }
                return Ok(ACTION_FAILED);
            }
            Ok(workflow) if workflow.status == WorkflowStatus::Completed => {
                // The run finished (and was reaped) before this sweep saw
                // the breach. The declared action is still `fail`: flip the
                // completed run to failed retroactively so an over-budget
                // workflow cannot count as a success.
                match hub.workflows().mark_completed_failed(&record.workflow_id, reason.clone()).await {
                    Ok(updated) if updated.status == WorkflowStatus::Failed => {
                        if let Some(task_id) = orchestrator_core::workflow_task_id(&updated) {
                            orchestrator_core::project_task_terminal_workflow_status(
                                hub,
                                &task_id,
                                WorkflowStatus::Failed,
                                Some(reason),
                            )
                            .await;
                        }
                        return Ok(ACTION_FAILED);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn(format!(
                            "failed to mark completed workflow '{}' failed for budget breach: {error}; retrying next sweep",
                            record.workflow_id
                        ));
                        return Err(());
                    }
                }
            }
            Ok(_) => {
                // The fail did not land terminally (e.g. the workflow was
                // already paused by an operator). Fall through to the pause
                // path so the spend still stops.
            }
            Err(error) => {
                warn(format!(
                    "failed to fail workflow '{}' for budget breach: {error}; falling back to pause",
                    record.workflow_id
                ));
            }
        }
    }
    match dispatch_workflow_event(
        hub.clone(),
        project_root,
        WorkflowEvent::Pause { workflow_id: record.workflow_id.clone(), reason_detail: Some(record.breach_summary()) },
    )
    .await
    {
        Ok(outcome) => {
            if outcome.workflow.is_some_and(|workflow| workflow.status == WorkflowStatus::Paused) {
                Ok(ACTION_PAUSED)
            } else {
                // No-op pause (already terminal/paused): record only.
                Ok(ACTION_RECORDED)
            }
        }
        Err(error) => {
            warn(format!(
                "failed to pause workflow '{}' for budget breach: {error}; retrying next sweep",
                record.workflow_id
            ));
            Err(())
        }
    }
}

fn to_breach_event(record: &BudgetExceededRecord, action: &str) -> BudgetBreachEvent {
    BudgetBreachEvent {
        workflow_run_id: record.workflow_run_id.clone(),
        workflow_id: record.workflow_id.clone(),
        phase_id: record.phase_id.clone(),
        limit_kind: match record.limit_kind {
            BudgetLimitKind::Workflow => "workflow".to_string(),
            BudgetLimitKind::Phase => "phase".to_string(),
        },
        limit_field: record.limit_field.as_str().to_string(),
        actual: record.actual,
        budget: record.budget,
        on_exceed: record.on_exceed.clone(),
        action: action.to_string(),
        observed_at: record.observed_at.to_rfc3339(),
    }
}

fn run_decisions_path(project_path: &Path, run_id: &str) -> PathBuf {
    scoped_root(project_path).join("runs").join(run_id).join("decisions.jsonl")
}

/// Resolve which `runs/<run_id>/` directory should carry the breach
/// decision. Phase breaches anchor to that phase's current session run;
/// workflow breaches anchor to the running (else most recently started)
/// phase session. Legacy `wf-*` run dirs without session checkpoints are
/// their own run id.
fn resolve_enforcement_run_id(project_path: &Path, record: &BudgetExceededRecord) -> Option<String> {
    let workflow_dir = scoped_root(project_path).join("runs").join(&record.workflow_run_id);
    let sessions = read_phase_sessions(&workflow_dir.join("phases"));
    if let Some(phase_id) = record.phase_id.as_deref() {
        if let Some(session) = sessions.iter().find(|session| session.phase_id.eq_ignore_ascii_case(phase_id)) {
            return Some(session.run_id.clone());
        }
    } else if !sessions.is_empty() {
        let chosen = sessions
            .iter()
            .filter(|session| session.running)
            .max_by(|a, b| a.started_at.cmp(&b.started_at))
            .or_else(|| sessions.iter().max_by(|a, b| a.started_at.cmp(&b.started_at)));
        return chosen.map(|session| session.run_id.clone());
    }
    // Legacy layout: `runs/<workflow_run_id>/events.jsonl` directly.
    if workflow_dir.join("events.jsonl").exists() {
        return Some(record.workflow_run_id.clone());
    }
    // Legacy `wf-*` run dirs without session checkpoints: the scanner keys
    // the cost state by the dir name with the `wf-` prefix STRIPPED, so the
    // physical run dir is `runs/wf-<workflow_run_id>/`.
    let legacy_run_id = format!("{}{}", super::scanner::WORKFLOW_RUN_PREFIX, record.workflow_run_id);
    let legacy_dir = scoped_root(project_path).join("runs").join(&legacy_run_id);
    if legacy_dir.join("events.jsonl").exists() {
        return Some(legacy_run_id);
    }
    None
}

struct PhaseSession {
    phase_id: String,
    run_id: String,
    started_at: String,
    running: bool,
}

fn read_phase_sessions(phases_dir: &Path) -> Vec<PhaseSession> {
    let Ok(entries) = std::fs::read_dir(phases_dir) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let (Some(phase_id), Some(run_id)) = (
            value.get("phase_id").and_then(serde_json::Value::as_str),
            value.get("run_id").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        sessions.push(PhaseSession {
            phase_id: phase_id.to_string(),
            run_id: run_id.to_string(),
            started_at: value.get("started_at").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
            running: value.get("status").and_then(serde_json::Value::as_str) == Some("running"),
        });
    }
    sessions
}

/// Run ids whose decision logs can carry the enforcement marker for this
/// breach. Phase-level breaches live in exactly one log (that phase's
/// current attempt — rework intentionally resets the marker with the run
/// id). Workflow-level breaches may have been marked while an earlier
/// phase was the anchor, so every known phase run of the workflow is
/// checked.
fn dedup_run_ids(project_path: &Path, record: &BudgetExceededRecord, anchor_run_id: &str) -> Vec<String> {
    let mut candidates = vec![anchor_run_id.to_string()];
    if record.phase_id.is_none() {
        let workflow_dir = scoped_root(project_path).join("runs").join(&record.workflow_run_id);
        for session in read_phase_sessions(&workflow_dir.join("phases")) {
            if session.run_id != anchor_run_id {
                candidates.push(session.run_id);
            }
        }
    }
    candidates
}

/// `true` when `runs/<run_id>/decisions.jsonl` already carries a
/// budget-exceeded metadata record for the same cap — same scope AND the
/// same declared `budget` / `on_exceed`, so raising a cap (or hardening
/// `warn` to `pause`) re-arms enforcement for the new declaration.
fn run_log_has_breach(project_path: &Path, run_id: &str, record: &BudgetExceededRecord) -> bool {
    let path = run_decisions_path(project_path, run_id);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("schema").and_then(serde_json::Value::as_str) != Some(BUDGET_EXCEEDED_SCHEMA_ID) {
            continue;
        }
        let Ok(existing) = serde_json::from_value::<BudgetExceededRecord>(payload.clone()) else {
            continue;
        };
        if existing.workflow_run_id == record.workflow_run_id
            && existing.limit_kind == record.limit_kind
            && existing.phase_id == record.phase_id
            && existing.limit_field == record.limit_field
            && existing.budget == record.budget
            && existing.on_exceed == record.on_exceed
        {
            return true;
        }
    }
    false
}

/// Append the breach as a `DecisionEvent::Metadata` frame to the per-run
/// decision log — the same typed stream `animus output decisions` renders
/// and the workflow runner replays. Uses the recording module's
/// single-write append so a phase runner still writing the same log
/// cannot end up with an interleaved, half-written record from us.
fn write_run_breach_record(project_path: &Path, run_id: &str, record: &BudgetExceededRecord) -> Result<()> {
    let path = run_decisions_path(project_path, run_id);
    let payload = serde_json::to_value(record).context("serialize budget-exceeded record")?;
    append_decision_event(path, &DecisionEvent::metadata(payload))
}

#[cfg(test)]
mod tests {
    // Intentional: the env lock guards HOME / ANIMUS_COST_STATE_ROOT
    // mutation across the awaited enforcement calls.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::shared::test_env_lock;
    use chrono::Utc;
    use orchestrator_core::InMemoryServiceHub;
    use orchestrator_core::WorkflowRunInput;
    use protocol::test_utils::EnvVarGuard;
    use tempfile::TempDir;

    fn arrange_roots(tmp: &TempDir) -> (EnvVarGuard, EnvVarGuard, PathBuf) {
        let home_guard = EnvVarGuard::set("HOME", Some(tmp.path().to_string_lossy().as_ref()));
        let state_root = tmp.path().join("scope");
        std::fs::create_dir_all(&state_root).unwrap();
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let override_guard = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(state_root.to_string_lossy().as_ref()));
        (home_guard, override_guard, project_root)
    }

    fn write_session(project_path: &Path, workflow_run_id: &str, phase_id: &str, run_id: &str, status: &str) {
        let dir = scoped_root(project_path).join("runs").join(workflow_run_id).join("phases");
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "workflow_id": workflow_run_id,
            "phase_id": phase_id,
            "provider": "test",
            "run_id": run_id,
            "status": status,
            "started_at": "2026-06-01T00:00:00Z",
        });
        std::fs::write(dir.join(format!("{phase_id}.session.json")), body.to_string()).unwrap();
        std::fs::create_dir_all(scoped_root(project_path).join("runs").join(run_id)).unwrap();
    }

    fn breach(workflow_run_id: &str, workflow_id: &str, on_exceed: &str) -> BudgetExceededRecord {
        BudgetExceededRecord {
            schema: BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
            workflow_run_id: workflow_run_id.to_string(),
            workflow_id: workflow_id.to_string(),
            phase_id: None,
            limit_kind: BudgetLimitKind::Workflow,
            limit_field: super::super::aggregator::BudgetLimitField::MaxCostUsd,
            actual: 7.5,
            budget: 5.0,
            on_exceed: on_exceed.to_string(),
            observed_at: Utc::now(),
        }
    }

    async fn running_workflow(hub: &Arc<InMemoryServiceHub>) -> String {
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task("TASK-BUDGET".to_string(), None))
            .await
            .expect("workflow should bootstrap");
        assert_eq!(workflow.status, WorkflowStatus::Running);
        workflow.id
    }

    #[tokio::test]
    async fn breach_is_enforced_exactly_once() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        write_session(&project_root, &workflow_id, "impl", "run-budget-1", "running");
        let record = breach(&workflow_id, &workflow_id, "pause");
        let mut warnings = Vec::new();

        let events =
            apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record.clone()], &mut |message| {
                warnings.push(message)
            })
            .await
            .expect("enforcement should succeed");

        assert_eq!(events.len(), 1, "first sweep enforces the breach: {warnings:?}");
        assert_eq!(events[0].action, "paused", "on_exceed=pause must pause the workflow");
        assert_eq!(events[0].limit_field, "max_cost_usd");
        let paused = hub.workflows().get(&workflow_id).await.unwrap();
        assert_eq!(paused.status, WorkflowStatus::Paused, "pause must go through the workflow pause path");
        // Per-run decision record written where `animus output decisions` reads.
        let run_log = run_decisions_path(&project_root, "run-budget-1");
        let raw = std::fs::read_to_string(&run_log).expect("per-run decisions.jsonl written");
        assert!(raw.contains(BUDGET_EXCEEDED_SCHEMA_ID), "{raw}");
        // Scoped fleet record written for `animus cost decisions`.
        let scoped = super::super::read_decision_records(&project_root).unwrap();
        assert_eq!(scoped.len(), 1);

        // Second sweep with the same breach: no new event, no new records.
        let events = apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record], &mut |message| {
            warnings.push(message)
        })
        .await
        .expect("second sweep should succeed");
        assert!(events.is_empty(), "already-enforced breach must not re-notify");
        assert_eq!(super::super::read_decision_records(&project_root).unwrap().len(), 1);
        assert_eq!(std::fs::read_to_string(&run_log).unwrap().lines().count(), raw.lines().count());
    }

    #[tokio::test]
    async fn warn_breach_records_and_notifies_without_pausing() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        write_session(&project_root, &workflow_id, "impl", "run-budget-2", "running");
        let record = breach(&workflow_id, &workflow_id, "warn");

        let events = apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record], &mut |_| {})
            .await
            .expect("enforcement should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "recorded");
        let workflow = hub.workflows().get(&workflow_id).await.unwrap();
        assert_eq!(workflow.status, WorkflowStatus::Running, "warn must not pause the workflow");
        assert_eq!(super::super::read_decision_records(&project_root).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn no_breach_means_no_writes() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());

        let events = run_budget_enforcement(hub as Arc<dyn ServiceHub>, &project, &mut |_| {})
            .await
            .expect("sweep without runs should succeed");

        assert!(events.is_empty());
        assert!(!super::super::decisions_log_path(&project_root).exists(), "no breach must write no records");
    }

    #[tokio::test]
    async fn workflow_breach_marked_in_earlier_phase_does_not_renotify_after_advance() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        // Phase A is the running anchor on the first sweep.
        write_session(&project_root, &workflow_id, "phase-a", "run-a", "running");
        let record = breach(&workflow_id, &workflow_id, "warn");
        let events =
            apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record.clone()], &mut |_| {})
                .await
                .unwrap();
        assert_eq!(events.len(), 1);

        // Workflow advances: phase B becomes the running anchor, but the
        // marker still lives in phase A's log.
        write_session(&project_root, &workflow_id, "phase-a", "run-a", "completed");
        write_session(&project_root, &workflow_id, "phase-b", "run-b", "running");
        let events =
            apply_breach_actions(hub as Arc<dyn ServiceHub>, &project, vec![record], &mut |_| {}).await.unwrap();
        assert!(events.is_empty(), "workflow-level breach must not re-notify after phase advance");
        assert!(!run_decisions_path(&project_root, "run-b").exists());
    }

    #[tokio::test]
    async fn raising_the_cap_rearms_enforcement_for_the_new_declaration() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        write_session(&project_root, &workflow_id, "impl", "run-rearm", "running");
        let record = breach(&workflow_id, &workflow_id, "warn");
        let events =
            apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record.clone()], &mut |_| {})
                .await
                .unwrap();
        assert_eq!(events.len(), 1);

        // Operator raises the cap; spend later crosses the NEW cap.
        let mut raised = record;
        raised.budget = 10.0;
        raised.actual = 12.0;
        let events =
            apply_breach_actions(hub as Arc<dyn ServiceHub>, &project, vec![raised], &mut |_| {}).await.unwrap();
        assert_eq!(events.len(), 1, "a breach of the raised cap is a new breach");
    }

    #[tokio::test]
    async fn breach_without_live_workflow_record_is_recorded_without_pause() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        write_session(&project_root, "wf-ghost", "impl", "run-ghost", "running");
        let record = breach("wf-ghost", "wf-ghost", "pause");
        let mut warnings = Vec::new();

        let events = apply_breach_actions(hub as Arc<dyn ServiceHub>, &project, vec![record], &mut |message| {
            warnings.push(message)
        })
        .await
        .expect("enforcement should succeed");

        assert_eq!(events.len(), 1, "breach must still be recorded: {warnings:?}");
        assert_eq!(events[0].action, "recorded");
        assert!(run_decisions_path(&project_root, "run-ghost").exists());
        assert!(warnings.iter().any(|w| w.contains("no live workflow record")), "{warnings:?}");
    }

    #[tokio::test]
    async fn fail_breach_fails_the_workflow_terminally() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        write_session(&project_root, &workflow_id, "impl", "run-fail", "running");
        let record = breach(&workflow_id, &workflow_id, "fail");

        let events = apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record], &mut |_| {})
            .await
            .expect("enforcement should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "failed", "on_exceed=fail must fail the workflow directly");
        let workflow = hub.workflows().get(&workflow_id).await.unwrap();
        assert_eq!(workflow.status, WorkflowStatus::Failed);
    }

    #[tokio::test]
    async fn fail_breach_flips_an_already_completed_workflow_to_failed() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        // Drive the workflow to Completed before the sweep notices the breach.
        loop {
            let workflow = hub.workflows().complete_current_phase(&workflow_id).await.unwrap();
            if workflow.status != WorkflowStatus::Running {
                assert_eq!(workflow.status, WorkflowStatus::Completed);
                break;
            }
        }
        write_session(&project_root, &workflow_id, "impl", "run-late-fail", "completed");
        let record = breach(&workflow_id, &workflow_id, "fail");

        let events = apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record], &mut |_| {})
            .await
            .expect("enforcement should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "failed", "completed over-budget runs must be failed retroactively");
        let workflow = hub.workflows().get(&workflow_id).await.unwrap();
        assert_eq!(workflow.status, WorkflowStatus::Failed);
    }

    #[tokio::test]
    async fn fail_breach_fails_a_paused_workflow_instead_of_leaving_it_paused() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        hub.workflows().pause(&workflow_id).await.unwrap();
        write_session(&project_root, &workflow_id, "impl", "run-paused-fail", "running");
        let record = breach(&workflow_id, &workflow_id, "fail");

        let events = apply_breach_actions(hub.clone() as Arc<dyn ServiceHub>, &project, vec![record], &mut |_| {})
            .await
            .expect("enforcement should succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "failed", "fail must land terminally even on a paused workflow");
        let workflow = hub.workflows().get(&workflow_id).await.unwrap();
        assert_eq!(workflow.status, WorkflowStatus::Failed);
    }

    #[test]
    fn legacy_wf_prefixed_runs_resolve_to_their_physical_dir() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        // Scanner keys legacy `runs/wf-legacy/` rollups as `legacy`.
        let run_dir = scoped_root(&project_root).join("runs").join("wf-legacy");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("events.jsonl"), "{}\n").unwrap();
        let record = breach("legacy", "legacy", "pause");

        let resolved = resolve_enforcement_run_id(&project_root, &record);

        assert_eq!(resolved.as_deref(), Some("wf-legacy"), "must map back to the wf-prefixed physical dir");
    }

    #[tokio::test]
    async fn phase_breach_anchors_to_that_phase_run() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, _override_guard, project_root) = arrange_roots(&tmp);
        let project = project_root.to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new());
        let workflow_id = running_workflow(&hub).await;
        write_session(&project_root, &workflow_id, "exploration", "run-explore", "completed");
        write_session(&project_root, &workflow_id, "impl", "run-impl", "running");
        let mut record = breach(&workflow_id, &workflow_id, "pause");
        record.phase_id = Some("exploration".to_string());
        record.limit_kind = BudgetLimitKind::Phase;

        let events = apply_breach_actions(hub as Arc<dyn ServiceHub>, &project, vec![record], &mut |_| {})
            .await
            .expect("enforcement should succeed");

        assert_eq!(events.len(), 1);
        assert!(run_decisions_path(&project_root, "run-explore").exists());
        assert!(!run_decisions_path(&project_root, "run-impl").exists());
    }
}
