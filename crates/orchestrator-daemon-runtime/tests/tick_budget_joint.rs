//! Joint budget tests for the shared `TickBudget` across schedule + trigger
//! dispatch within a single project tick.
//!
//! Background: prior to the P2 audit fix, `run_project_tick` handed the SAME
//! immutable headroom value to the schedule hook AND the trigger hook, so
//! each path independently consumed the full headroom. With pool cap N=5 and
//! 3 due schedules + 5 pending webhook events, the schedule hook would
//! commit 3 dispatches and the trigger hook would then drain all 5 events —
//! `ProcessManager::spawn_workflow_runner` would refuse the over-budget
//! spawns, leaving schedules marked as attempted and webhook events lost
//! without runners.
//!
//! The fix:
//! 1. Both hooks share a `&mut TickBudget` so the trigger hook only sees
//!    headroom the schedule hook didn't already claim.
//! 2. Failed-capacity schedules record `missed_count` and DO NOT update
//!    `last_run` — the schedule re-fires on the next tick.
//! 3. Webhook events are peeked, not popped; the spawn closure decides
//!    whether the event leaves the queue.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_daemon_runtime::{ScheduleDispatch, SubjectDispatch, TickBudget, TriggerDispatch};
use serde_json::json;
use tempfile::tempdir;

fn write_combined_config(project_root: &std::path::Path) {
    let mut config = orchestrator_core::builtin_workflow_config();
    config.workflows.push(orchestrator_core::WorkflowDefinition {
        id: "scheduled-flow".to_string(),
        name: "Scheduled".to_string(),
        description: String::new(),
        phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
        variables: Vec::new(),
        worktree: None,
        budget: None,
    });
    config.workflows.push(orchestrator_core::WorkflowDefinition {
        id: "webhook-flow".to_string(),
        name: "Webhook".to_string(),
        description: String::new(),
        phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
        variables: Vec::new(),
        worktree: None,
        budget: None,
    });

    config.schedules.push(orchestrator_core::WorkflowSchedule {
        id: "every-minute".to_string(),
        cron: "* * * * *".to_string(),
        workflow_ref: Some("scheduled-flow".to_string()),
        command: None,
        input: None,
        enabled: true,
        owner_id: None,
        claims: Vec::new(),
    });
    config.triggers.push(orchestrator_core::workflow_config::WorkflowTrigger {
        id: "on-webhook".to_string(),
        trigger_type: orchestrator_core::workflow_config::TriggerType::Webhook,
        workflow_ref: Some("webhook-flow".to_string()),
        enabled: true,
        config: json!({ "max_triggers_per_minute": 10 }),
        input: Some(json!({ "source": "webhook" })),
    });

    orchestrator_core::write_workflow_config(project_root, &config).expect("write config");
}

fn queue_pending_webhook_event(project_root: &std::path::Path, trigger_id: &str, event_id: &str) {
    let mut state = orchestrator_core::load_trigger_state(project_root).unwrap_or_default();
    let run = state.triggers.entry(trigger_id.to_string()).or_default();
    run.pending_events.push(orchestrator_core::WebhookEvent {
        event_id: event_id.to_string(),
        received_at: "2026-04-01T10:00:00Z".parse().unwrap(),
        payload: json!({ "evt": event_id }),
    });
    orchestrator_core::save_trigger_state(project_root, &state).expect("save trigger state");
}

/// Replays the schedule + trigger budget-aware spawn logic exactly as
/// `DefaultSlimProjectTickHooks` does, against a shared `TickBudget`.
fn run_dispatch_round(
    project_root: &std::path::Path,
    now: DateTime<Utc>,
    budget: &mut TickBudget,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let root = project_root.to_string_lossy().to_string();
    let dispatched_schedules: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let missed_schedules: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let dispatched_webhooks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // --- schedules ---
    if !budget.is_exhausted() {
        let dispatched_ref = dispatched_schedules.clone();
        let missed_ref = missed_schedules.clone();
        let outcomes =
            ScheduleDispatch::process_due_schedules(&root, now, |schedule_id, _dispatch: &SubjectDispatch| {
                if !budget.try_take() {
                    missed_ref.lock().unwrap().push(schedule_id.to_string());
                    return Err(anyhow::anyhow!("schedule dispatch skipped: tick budget exhausted"));
                }
                dispatched_ref.lock().unwrap().push(schedule_id.to_string());
                Ok::<(), anyhow::Error>(())
            });

        let missed: std::collections::HashSet<String> = missed_ref.lock().unwrap().iter().cloned().collect();

        // Project state writes the way the driver does:
        for outcome in outcomes {
            if missed.contains(&outcome.schedule_id) {
                orchestrator_core::project_schedule_dispatch_missed(&root, &outcome.schedule_id, &outcome.status);
            } else {
                orchestrator_core::project_schedule_dispatch_attempt(&root, &outcome.schedule_id, now, &outcome.status);
            }
        }
    }

    // --- triggers ---
    if !budget.is_exhausted() {
        let webhook_ref = dispatched_webhooks.clone();
        let _ = TriggerDispatch::process_due_triggers(&root, now, |trigger_id, _dispatch| -> Result<()> {
            if !budget.try_take() {
                return Err(anyhow::anyhow!("trigger dispatch skipped: tick budget exhausted"));
            }
            webhook_ref.lock().unwrap().push(trigger_id.to_string());
            Ok(())
        });
    }

    (
        Arc::try_unwrap(dispatched_schedules).unwrap().into_inner().unwrap(),
        Arc::try_unwrap(missed_schedules).unwrap().into_inner().unwrap(),
        Arc::try_unwrap(dispatched_webhooks).unwrap().into_inner().unwrap(),
    )
}

#[test]
fn shared_budget_cap_one_lets_schedule_win_over_trigger() {
    // Cap=1: schedule + 1 pending webhook event are both ready.
    // Ordering: schedule runs FIRST and claims the slot. Trigger sees an
    // exhausted budget and the event stays queued.
    let temp = tempdir().expect("tempdir");
    let project_root = temp.path();
    write_combined_config(project_root);
    queue_pending_webhook_event(project_root, "on-webhook", "evt-A");

    let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();
    let mut budget = TickBudget::new(Some(1));

    let (dispatched_schedules, missed_schedules, dispatched_webhooks) =
        run_dispatch_round(project_root, now, &mut budget);

    assert_eq!(dispatched_schedules, vec!["every-minute".to_string()], "schedule wins the only slot");
    assert!(missed_schedules.is_empty(), "the schedule wasn't missed — it ran");
    assert!(dispatched_webhooks.is_empty(), "trigger lost — no slots left in shared budget");
    assert_eq!(budget.remaining(), Some(0), "budget fully consumed");

    // Schedule state: last_run set, run_count=1
    let schedule_state = orchestrator_core::load_schedule_state(project_root).expect("schedule state");
    let schedule_entry = schedule_state.schedules.get("every-minute").expect("entry");
    assert_eq!(schedule_entry.run_count, 1);
    assert_eq!(schedule_entry.missed_count, 0);
    assert!(schedule_entry.last_run.is_some(), "successful dispatch updates last_run");

    // Webhook state: event still queued
    let trigger_state = orchestrator_core::load_trigger_state(project_root).expect("trigger state");
    let webhook_entry = trigger_state.triggers.get("on-webhook").expect("entry");
    assert_eq!(webhook_entry.pending_events.len(), 1, "webhook event stays queued for next tick");
    assert_eq!(webhook_entry.pending_events[0].event_id, "evt-A");
    assert_eq!(webhook_entry.dispatch_count, 0);
}

#[test]
fn shared_budget_cap_zero_blocks_everything() {
    // Cap=0: NO schedule should record last_run, NO event should drain.
    let temp = tempdir().expect("tempdir");
    let project_root = temp.path();
    write_combined_config(project_root);
    queue_pending_webhook_event(project_root, "on-webhook", "evt-A");
    queue_pending_webhook_event(project_root, "on-webhook", "evt-B");

    let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();
    let mut budget = TickBudget::new(Some(0));

    let (dispatched_schedules, _missed_schedules, dispatched_webhooks) =
        run_dispatch_round(project_root, now, &mut budget);

    assert!(dispatched_schedules.is_empty(), "exhausted budget skips schedules entirely");
    assert!(dispatched_webhooks.is_empty(), "exhausted budget skips triggers entirely");
    assert_eq!(budget.remaining(), Some(0));

    // Schedule state must be untouched (no entry created, since the hook
    // short-circuited before calling process_due_schedules).
    let schedule_state = orchestrator_core::load_schedule_state(project_root).expect("schedule state");
    assert!(
        schedule_state.schedules.get("every-minute").map(|e| e.last_run).flatten().is_none(),
        "no schedule attempt → no last_run"
    );

    // Webhook events must all still be queued.
    let trigger_state = orchestrator_core::load_trigger_state(project_root).expect("trigger state");
    let webhook_entry = trigger_state.triggers.get("on-webhook").expect("entry");
    assert_eq!(webhook_entry.pending_events.len(), 2, "both events must remain queued");
    assert_eq!(webhook_entry.dispatch_count, 0);
}

#[test]
fn shared_budget_cap_ten_runs_both_paths() {
    // Cap=10: 1 schedule + 1 webhook event both have headroom; both run.
    let temp = tempdir().expect("tempdir");
    let project_root = temp.path();
    write_combined_config(project_root);
    queue_pending_webhook_event(project_root, "on-webhook", "evt-A");

    let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();
    let mut budget = TickBudget::new(Some(10));

    let (dispatched_schedules, missed_schedules, dispatched_webhooks) =
        run_dispatch_round(project_root, now, &mut budget);

    assert_eq!(dispatched_schedules, vec!["every-minute".to_string()]);
    assert!(missed_schedules.is_empty());
    assert_eq!(dispatched_webhooks, vec!["on-webhook".to_string()]);
    assert_eq!(budget.remaining(), Some(8), "10 - 1 schedule - 1 trigger");

    let schedule_state = orchestrator_core::load_schedule_state(project_root).expect("schedule state");
    let schedule_entry = schedule_state.schedules.get("every-minute").expect("entry");
    assert_eq!(schedule_entry.run_count, 1);
    assert_eq!(schedule_entry.missed_count, 0);

    let trigger_state = orchestrator_core::load_trigger_state(project_root).expect("trigger state");
    let webhook_entry = trigger_state.triggers.get("on-webhook").expect("entry");
    assert!(webhook_entry.pending_events.is_empty(), "queue drained on success");
    assert_eq!(webhook_entry.dispatch_count, 1);
}

#[test]
fn missed_schedule_refires_on_next_tick_within_same_cron_minute() {
    // Regression: prior to the fix, a failed-capacity schedule still set
    // last_run = now, which caused `evaluate_schedules` to skip it on the
    // very next tick within the same minute. With the fix, missed
    // dispatches leave last_run untouched, so the schedule fires again as
    // soon as capacity frees up.
    let temp = tempdir().expect("tempdir");
    let project_root = temp.path();
    write_combined_config(project_root);

    let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();

    // Tick 1: budget=0 → schedule misses.
    let mut budget = TickBudget::new(Some(0));
    let (_, _, _) = run_dispatch_round(project_root, now, &mut budget);

    let schedule_state = orchestrator_core::load_schedule_state(project_root).expect("schedule state");
    let entry_after_miss = schedule_state.schedules.get("every-minute");
    // Either no entry yet, or entry with last_run=None.
    assert!(
        entry_after_miss.map(|e| e.last_run).flatten().is_none(),
        "missed dispatch must NOT set last_run — otherwise the next tick suppresses the schedule"
    );

    // Tick 2 (same cron minute): budget=1 → schedule fires.
    let mut budget = TickBudget::new(Some(1));
    let (dispatched_schedules, _, _) = run_dispatch_round(project_root, now, &mut budget);
    assert_eq!(
        dispatched_schedules,
        vec!["every-minute".to_string()],
        "schedule must re-fire within the same cron minute because last_run was never set"
    );

    let schedule_state = orchestrator_core::load_schedule_state(project_root).expect("schedule state");
    let entry_after_run = schedule_state.schedules.get("every-minute").expect("entry");
    assert_eq!(entry_after_run.run_count, 1);
    assert!(entry_after_run.last_run.is_some());
}
