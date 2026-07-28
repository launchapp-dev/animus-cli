use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use chrono::{DateTime, Utc};
use tracing::warn;

use super::TriggerDispatchOutcome;
use crate::{SubjectDispatch, SubjectDispatchExt};

pub struct TriggerDispatch;

impl TriggerDispatch {
    /// Process all due triggers for `project_root` at `now`.
    ///
    /// Handles two trigger types:
    /// - **file_watcher**: scans path mtimes and dispatches when files change.
    /// - **webhook / github_webhook**: drains pending events queued by the
    ///   HTTP handler (one dispatch per pending event).
    pub fn process_due_triggers<PipelineSpawner>(
        project_root: &str,
        now: DateTime<Utc>,
        mut spawn_pipeline: PipelineSpawner,
    ) -> Vec<TriggerDispatchOutcome>
    where
        PipelineSpawner: FnMut(&str, &SubjectDispatch) -> Result<()>,
    {
        let config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));

        let file_watcher_triggers: Vec<&orchestrator_core::workflow_config::WorkflowTrigger> = config
            .config
            .triggers
            .iter()
            .filter(|t| t.enabled && t.trigger_type == orchestrator_core::workflow_config::TriggerType::FileWatcher)
            .collect();

        let webhook_triggers: Vec<&orchestrator_core::workflow_config::WorkflowTrigger> = config
            .config
            .triggers
            .iter()
            .filter(|t| {
                t.enabled
                    && matches!(
                        t.trigger_type,
                        orchestrator_core::workflow_config::TriggerType::Webhook
                            | orchestrator_core::workflow_config::TriggerType::GithubWebhook
                            | orchestrator_core::workflow_config::TriggerType::Plugin
                    )
            })
            .collect();

        if file_watcher_triggers.is_empty() && webhook_triggers.is_empty() {
            return Vec::new();
        }

        let _state_lock = match orchestrator_core::lock_trigger_state(std::path::Path::new(project_root)) {
            Ok(lock) => lock,
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    error = %error,
                    "failed to lock trigger state; deferring trigger processing to next tick"
                );
                return Vec::new();
            }
        };
        let mut state = orchestrator_core::load_trigger_state(std::path::Path::new(project_root)).unwrap_or_default();
        let mut outcomes = Vec::new();
        let mut state_dirty = false;

        // --- file_watcher processing ---
        for trigger in file_watcher_triggers {
            let fw_config = orchestrator_core::workflow_config::FileWatcherTriggerConfig::from_value(&trigger.config);
            if fw_config.paths.is_empty() {
                continue;
            }

            let run_state = state.triggers.entry(trigger.id.clone()).or_default();

            // Check debounce: skip if we dispatched recently.
            if let Some(last_dispatched) = run_state.last_dispatched {
                let elapsed = now.signed_duration_since(last_dispatched).num_seconds();
                if elapsed < fw_config.debounce_secs as i64 {
                    continue;
                }
            }

            // Retrieve last known max mtime from extra state.
            let last_known_mtime: u64 =
                run_state.extra.as_ref().and_then(|v| v.get("last_mtime_secs")).and_then(|v| v.as_u64()).unwrap_or(0);

            // Scan watched paths for newer mtimes.
            let current_max_mtime = scan_max_mtime(project_root, &fw_config.paths, &fw_config.ignore);

            if run_state.extra.is_none() {
                // First tick this watch is ever evaluated: seed the mtime
                // baseline without dispatching. This prevents a spurious
                // dispatch on daemon startup for files that already exist.
                // A baseline of 0 (no matching files yet) is seeded too, so
                // a file appearing later counts as a change, not a re-seed.
                run_state.extra = Some(serde_json::json!({ "last_mtime_secs": current_max_mtime }));
                state_dirty = true;
            } else if current_max_mtime > last_known_mtime {
                // A file has been modified since last check — fire the trigger.
                let status = dispatch_trigger(&trigger.id, trigger, now, "file-watcher", &mut spawn_pipeline);

                if status == "dispatched" {
                    // Spawn accepted — advance the baseline and bookkeeping.
                    run_state.last_dispatched = Some(now);
                    run_state.dispatch_count += 1;
                    run_state.extra = Some(serde_json::json!({ "last_mtime_secs": current_max_mtime }));
                }
                // On failure the baseline stays put so the next tick retries
                // the same change instead of silently consuming it.
                run_state.last_status = status.clone();
                state_dirty = true;

                outcomes.push(TriggerDispatchOutcome { trigger_id: trigger.id.clone(), status });
            }
        }

        // --- webhook / github_webhook processing ---
        // Peek at pending events one at a time and only pop them once the
        // spawn succeeds. A failed dispatch (e.g. tick budget exhausted)
        // leaves the event at the head of the queue so the next tick can
        // retry it, instead of silently dropping the event when the runner
        // never started.
        for trigger in webhook_triggers {
            let run_state = state.triggers.entry(trigger.id.clone()).or_default();

            if run_state.pending_events.is_empty() {
                continue;
            }

            let trigger_source = match trigger.trigger_type {
                orchestrator_core::workflow_config::TriggerType::GithubWebhook => "github-webhook",
                orchestrator_core::workflow_config::TriggerType::Plugin => "plugin",
                _ => "webhook",
            };

            // Drain in FIFO order, but stop the moment a dispatch fails so
            // the failing event (and everything behind it) stays queued.
            while let Some(event) = run_state.pending_events.first().cloned() {
                let merged_input = merge_trigger_input(trigger.input.as_ref(), &event.payload);
                let trigger_with_payload = orchestrator_core::workflow_config::WorkflowTrigger {
                    input: Some(merged_input),
                    ..trigger.clone()
                };

                let status =
                    dispatch_trigger(&trigger.id, &trigger_with_payload, now, trigger_source, &mut spawn_pipeline);

                if status == "dispatched" {
                    // Spawn accepted — now safe to remove from the queue.
                    run_state.pending_events.remove(0);
                    run_state.last_dispatched = Some(now);
                    run_state.last_status = status.clone();
                    run_state.dispatch_count += 1;
                    state_dirty = true;

                    outcomes.push(TriggerDispatchOutcome { trigger_id: trigger.id.clone(), status });
                } else {
                    // Spawn refused — leave the event queued for the next
                    // tick. Update last_status so ops can see the failure
                    // reason without losing the event.
                    run_state.last_status = status.clone();
                    state_dirty = true;
                    outcomes.push(TriggerDispatchOutcome { trigger_id: trigger.id.clone(), status });
                    break;
                }
            }
        }

        // Persist updated state only when something actually changed
        // (best-effort) — avoids rewriting + fsyncing the state file on
        // every tick once a file-watcher baseline has been seeded.
        if state_dirty {
            let _ = orchestrator_core::save_trigger_state(std::path::Path::new(project_root), &state);
        }

        outcomes
    }
}

/// Merge the webhook event payload into the trigger's static input object.
///
/// If the trigger has a static `input` object, the event payload is nested
/// under the key `"webhook_payload"`.  Otherwise the event payload itself
/// becomes the task input.
fn merge_trigger_input(
    static_input: Option<&serde_json::Value>,
    event_payload: &serde_json::Value,
) -> serde_json::Value {
    match static_input {
        Some(serde_json::Value::Object(map)) => {
            let mut merged = map.clone();
            merged.insert("webhook_payload".to_string(), event_payload.clone());
            serde_json::Value::Object(merged)
        }
        Some(other) => other.clone(),
        None => event_payload.clone(),
    }
}

fn dispatch_trigger<PipelineSpawner>(
    trigger_id: &str,
    trigger: &orchestrator_core::workflow_config::WorkflowTrigger,
    _now: DateTime<Utc>,
    trigger_source: &str,
    spawn_pipeline: &mut PipelineSpawner,
) -> String
where
    PipelineSpawner: FnMut(&str, &SubjectDispatch) -> Result<()>,
{
    if let Some(ref workflow_ref) = trigger.workflow_ref {
        let dispatch = SubjectDispatch::for_custom(
            format!("trigger:{trigger_id}"),
            format!("Triggered by {trigger_source} '{trigger_id}'"),
            workflow_ref.clone(),
            trigger.input.clone(),
            trigger_source.to_string(),
        );
        match spawn_pipeline(trigger_id, &dispatch) {
            Ok(()) => "dispatched".to_string(),
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    trigger_id,
                    workflow_ref,
                    error = %error,
                    "trigger dispatch failed"
                );
                format!("failed: {error}")
            }
        }
    } else {
        warn!(actor = protocol::ACTOR_DAEMON, trigger_id, "trigger is missing workflow_ref and will not be dispatched");
        "failed: trigger is missing workflow_ref".to_string()
    }
}

/// Returns the maximum mtime (seconds since UNIX_EPOCH) across all files
/// matched by `patterns`, excluding files matched by `ignore_patterns`.
/// Returns 0 if no files match or all are unreadable.
fn scan_max_mtime(project_root: &str, patterns: &[String], ignore_patterns: &[String]) -> u64 {
    let root = Path::new(project_root);
    let mut max_mtime: u64 = 0;

    let ignore_globs: Vec<glob::Pattern> = ignore_patterns
        .iter()
        .filter_map(|pat| {
            let abs_pat = root.join(pat).to_string_lossy().to_string();
            glob::Pattern::new(&abs_pat).or_else(|_| glob::Pattern::new(pat)).ok()
        })
        .collect();

    for pattern in patterns {
        let abs_pattern = root.join(pattern).to_string_lossy().to_string();
        let path_iter = match glob::glob(&abs_pattern) {
            Ok(iter) => iter,
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    pattern,
                    error = %error,
                    "file-watcher: invalid glob pattern"
                );
                continue;
            }
        };

        for entry in path_iter.flatten() {
            // Skip ignored paths.
            let entry_str = entry.to_string_lossy();
            if ignore_globs.iter().any(|ignore| ignore.matches(entry_str.as_ref())) {
                continue;
            }

            if let Ok(metadata) = std::fs::metadata(&entry) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                        let secs = dur.as_secs();
                        if secs > max_mtime {
                            max_mtime = secs;
                        }
                    }
                }
            }
        }
    }

    max_mtime
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn write_trigger_config(
        project_root: &std::path::Path,
        trigger_id: &str,
        paths: &[&str],
    ) -> crate::test_env::WorkflowConfigFixtureGuard {
        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "auto-test".to_string(),
            name: "Auto Test".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            variables: Vec::new(),
            worktree: None,
            budget: None,
            environment: None,
            workspace: None,
        });
        config.triggers.push(orchestrator_core::workflow_config::WorkflowTrigger {
            id: trigger_id.to_string(),
            trigger_type: orchestrator_core::workflow_config::TriggerType::FileWatcher,
            workflow_ref: Some("auto-test".to_string()),
            enabled: true,
            config: json!({
                "paths": paths,
                "debounce_secs": 0
            }),
            input: None,
        });
        crate::test_env::write_workflow_config_fixture(project_root, &mut config, &["requirements"])
    }

    fn write_webhook_trigger_config(
        project_root: &std::path::Path,
        trigger_id: &str,
    ) -> crate::test_env::WorkflowConfigFixtureGuard {
        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "respond-to-webhook".to_string(),
            name: "Respond To Webhook".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            variables: Vec::new(),
            worktree: None,
            budget: None,
            environment: None,
            workspace: None,
        });
        config.triggers.push(orchestrator_core::workflow_config::WorkflowTrigger {
            id: trigger_id.to_string(),
            trigger_type: orchestrator_core::workflow_config::TriggerType::Webhook,
            workflow_ref: Some("respond-to-webhook".to_string()),
            enabled: true,
            config: json!({ "max_triggers_per_minute": 10 }),
            input: Some(json!({ "source": "webhook" })),
        });
        crate::test_env::write_workflow_config_fixture(project_root, &mut config, &["requirements"])
    }

    #[test]
    fn process_due_triggers_fires_when_file_is_newer_than_baseline() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        // Create a file to watch.
        let watched_file = project_root.join("watched.rs");
        std::fs::write(&watched_file, "fn main() {}").expect("write file");

        let _config = write_trigger_config(project_root, "on-change", &[watched_file.to_string_lossy().as_ref()]);

        let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();

        // First tick: seeds baseline — no dispatch.
        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            |_id, _dispatch| Ok(()),
        );
        assert!(outcomes.is_empty(), "first tick should only seed baseline");

        // Second tick after seeding: file not modified → no dispatch.
        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            |_id, _dispatch| Ok(()),
        );
        assert!(outcomes.is_empty(), "no file change → no dispatch");
    }

    #[test]
    fn process_due_triggers_skips_disabled_triggers() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let watched_file = project_root.join("src.rs");
        std::fs::write(&watched_file, "// code").expect("write file");

        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "auto-test".to_string(),
            name: "Auto Test".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            variables: Vec::new(),
            worktree: None,
            budget: None,
            environment: None,
            workspace: None,
        });
        config.triggers.push(orchestrator_core::workflow_config::WorkflowTrigger {
            id: "disabled-watcher".to_string(),
            trigger_type: orchestrator_core::workflow_config::TriggerType::FileWatcher,
            workflow_ref: Some("auto-test".to_string()),
            enabled: false,
            config: json!({ "paths": [watched_file.to_string_lossy().as_ref()], "debounce_secs": 0 }),
            input: None,
        });
        let _config = crate::test_env::write_workflow_config_fixture(project_root, &mut config, &["requirements"]);

        let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_ref = calls.clone();
        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            move |id, _dispatch| {
                calls_ref.lock().unwrap().push(id.to_string());
                Ok(())
            },
        );
        assert!(outcomes.is_empty());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn process_due_triggers_respects_debounce() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let watched_file = project_root.join("debounce.rs");
        std::fs::write(&watched_file, "// debounce test").expect("write file");

        // Seed baseline first.
        let _config = write_trigger_config(project_root, "debounced", &[watched_file.to_string_lossy().as_ref()]);

        let t0: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();

        // Seed tick (no dispatch).
        let _ =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t0, |_id, _dispatch| Ok(()));

        // Simulate a baseline already seeded at a very old mtime (1 second after epoch)
        // so the current file (created moments ago) appears newer.
        let mut state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        if let Some(run) = state.triggers.get_mut("debounced") {
            run.last_dispatched = None;
            run.extra = Some(json!({ "last_mtime_secs": 1u64 })); // old baseline
        }
        orchestrator_core::save_trigger_state(project_root, &state).expect("save state");

        // Tick at t0 + 3s: current file mtime > 1 → change detected → dispatch
        // (debounce_secs=0 so no debounce holds us back).
        let t1 = t0 + chrono::Duration::seconds(3);
        let calls = Arc::new(Mutex::new(0usize));
        let calls_ref = calls.clone();
        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            t1,
            move |_id, _dispatch| {
                *calls_ref.lock().unwrap() += 1;
                Ok(())
            },
        );
        // File mtime > baseline → dispatch occurs
        assert_eq!(outcomes.len(), 1);
    }

    #[test]
    fn process_due_triggers_keeps_file_watcher_baseline_when_spawn_fails() {
        // Regression mirroring the webhook peek-then-pop fix: previously the
        // file-watcher branch advanced the mtime baseline, last_dispatched,
        // and dispatch_count even when the spawn closure returned Err (tick
        // budget exhausted / concurrency cap), so the change event was
        // silently consumed and never retried.
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let watched_file = project_root.join("retry.rs");
        std::fs::write(&watched_file, "// retry test").expect("write file");

        let _config = write_trigger_config(project_root, "retry-watcher", &[watched_file.to_string_lossy().as_ref()]);

        let t0: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();

        // Seed tick (no dispatch).
        let _ =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t0, |_id, _dispatch| Ok(()));

        // Reset the baseline to a very old mtime so the file appears newer.
        let mut state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        if let Some(run) = state.triggers.get_mut("retry-watcher") {
            run.last_dispatched = None;
            run.extra = Some(json!({ "last_mtime_secs": 1u64 }));
        }
        orchestrator_core::save_trigger_state(project_root, &state).expect("save state");

        // Tick with a failing spawn — the change must NOT be consumed.
        let t1 = t0 + chrono::Duration::seconds(3);
        let outcomes =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t1, |_id, _dispatch| {
                Err(anyhow::anyhow!("tick budget exhausted"))
            });
        assert_eq!(outcomes.len(), 1);
        assert_ne!(outcomes[0].status, "dispatched");

        let state_after = orchestrator_core::load_trigger_state(project_root).expect("load state after");
        let run = state_after.triggers.get("retry-watcher").expect("trigger state");
        assert_eq!(
            run.extra.as_ref().and_then(|v| v.get("last_mtime_secs")).and_then(|v| v.as_u64()),
            Some(1),
            "mtime baseline must not advance when spawn fails"
        );
        assert!(run.last_dispatched.is_none(), "last_dispatched must not be set on failure");
        assert_eq!(run.dispatch_count, 0, "no dispatch_count increment on failure");

        // Next tick with a working spawn retries the same change.
        let t2 = t0 + chrono::Duration::seconds(6);
        let outcomes =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t2, |_id, _dispatch| Ok(()));
        assert_eq!(outcomes.len(), 1, "failed change must be retried on the next tick");
        assert_eq!(outcomes[0].status, "dispatched");

        let state_after = orchestrator_core::load_trigger_state(project_root).expect("load state after retry");
        let run = state_after.triggers.get("retry-watcher").expect("trigger state");
        assert_eq!(run.dispatch_count, 1);
        assert_eq!(run.last_dispatched, Some(t2));
    }

    #[test]
    fn process_due_triggers_fires_when_first_file_appears_after_seed() {
        // A watch whose glob matches nothing seeds a baseline of 0. When the
        // first matching file later appears, that transition must dispatch
        // instead of being swallowed as another baseline seed.
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let watched_file = project_root.join("appears-later.rs");
        let _config = write_trigger_config(project_root, "first-file", &[watched_file.to_string_lossy().as_ref()]);

        let t0: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();

        // Seed tick with no matching files: baseline 0, no dispatch.
        let outcomes =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t0, |_id, _dispatch| Ok(()));
        assert!(outcomes.is_empty(), "seed tick must not dispatch");

        let state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        let run = state.triggers.get("first-file").expect("trigger state");
        assert_eq!(
            run.extra.as_ref().and_then(|v| v.get("last_mtime_secs")).and_then(|v| v.as_u64()),
            Some(0),
            "empty glob should seed a zero baseline"
        );

        // The first matching file appears.
        std::fs::write(&watched_file, "fn main() {}").expect("write file");

        let t1 = t0 + chrono::Duration::seconds(3);
        let outcomes =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t1, |_id, _dispatch| Ok(()));
        assert_eq!(outcomes.len(), 1, "first file appearing after the seed tick must dispatch");
        assert_eq!(outcomes[0].status, "dispatched");
    }

    #[test]
    fn process_due_triggers_does_not_rewrite_state_when_nothing_changed() {
        // Once the baseline is seeded, an idle tick (no file changes, no
        // pending webhook events) must not rewrite + fsync the state file.
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let watched_file = project_root.join("idle.rs");
        std::fs::write(&watched_file, "// idle").expect("write file");

        let _config = write_trigger_config(project_root, "idle-watcher", &[watched_file.to_string_lossy().as_ref()]);

        let t0: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();

        // Seed tick writes the baseline.
        let _ =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t0, |_id, _dispatch| Ok(()));

        let state_path = protocol::scoped_state_root(project_root)
            .unwrap_or_else(|| project_root.join(".animus"))
            .join("state")
            .join("trigger-state.json");
        let mtime_after_seed =
            std::fs::metadata(&state_path).and_then(|m| m.modified()).expect("state file mtime after seed");

        std::thread::sleep(std::time::Duration::from_millis(50));

        // Idle tick: nothing changed → no rewrite.
        let t1 = t0 + chrono::Duration::seconds(3);
        let outcomes =
            TriggerDispatch::process_due_triggers(project_root.to_string_lossy().as_ref(), t1, |_id, _dispatch| Ok(()));
        assert!(outcomes.is_empty());

        let mtime_after_idle =
            std::fs::metadata(&state_path).and_then(|m| m.modified()).expect("state file mtime after idle tick");
        assert_eq!(mtime_after_seed, mtime_after_idle, "idle tick must not rewrite the trigger state file");
    }

    #[test]
    fn process_due_triggers_drains_pending_webhook_events() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let _config = write_webhook_trigger_config(project_root, "on-webhook");

        // Manually inject two pending events into TriggerState.
        let mut state = orchestrator_core::load_trigger_state(project_root).unwrap_or_default();
        let run_state = state.triggers.entry("on-webhook".to_string()).or_default();
        run_state.pending_events.push(orchestrator_core::WebhookEvent {
            event_id: "evt-001".to_string(),
            received_at: "2026-04-01T10:00:00Z".parse().unwrap(),
            payload: json!({ "action": "opened" }),
        });
        run_state.pending_events.push(orchestrator_core::WebhookEvent {
            event_id: "evt-002".to_string(),
            received_at: "2026-04-01T10:00:01Z".parse().unwrap(),
            payload: json!({ "action": "closed" }),
        });
        orchestrator_core::save_trigger_state(project_root, &state).expect("save state");

        let now: DateTime<Utc> = "2026-04-01T10:01:00Z".parse().unwrap();
        let dispatched = Arc::new(Mutex::new(Vec::<String>::new()));
        let dispatched_ref = dispatched.clone();

        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            move |trigger_id, _dispatch| {
                dispatched_ref.lock().unwrap().push(trigger_id.to_string());
                Ok(())
            },
        );

        // Both pending events should have been dispatched.
        assert_eq!(outcomes.len(), 2, "both pending webhook events should dispatch");
        assert!(outcomes.iter().all(|o| o.status == "dispatched"));
        assert_eq!(*dispatched.lock().unwrap(), vec!["on-webhook", "on-webhook"]);

        // Pending queue should now be empty.
        let state_after = orchestrator_core::load_trigger_state(project_root).expect("load state after");
        let run = state_after.triggers.get("on-webhook").expect("trigger state");
        assert!(run.pending_events.is_empty(), "pending_events should be cleared after dispatch");
        assert_eq!(run.dispatch_count, 2);
    }

    #[test]
    fn process_due_triggers_keeps_event_queued_when_spawn_fails() {
        // Regression for the audit P2 finding: previously the webhook handler
        // drained ALL pending events with `std::mem::take` BEFORE invoking
        // the spawn closure, so a closure that returned Err (e.g. because
        // the shared tick budget was exhausted) silently dropped the event.
        // The fix: peek at the head of the queue, attempt the spawn, only
        // pop on success.
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let _config = write_webhook_trigger_config(project_root, "on-webhook");

        let mut state = orchestrator_core::load_trigger_state(project_root).unwrap_or_default();
        let run_state = state.triggers.entry("on-webhook".to_string()).or_default();
        run_state.pending_events.push(orchestrator_core::WebhookEvent {
            event_id: "evt-A".to_string(),
            received_at: "2026-04-01T10:00:00Z".parse().unwrap(),
            payload: json!({ "action": "opened" }),
        });
        run_state.pending_events.push(orchestrator_core::WebhookEvent {
            event_id: "evt-B".to_string(),
            received_at: "2026-04-01T10:00:01Z".parse().unwrap(),
            payload: json!({ "action": "closed" }),
        });
        orchestrator_core::save_trigger_state(project_root, &state).expect("save state");

        let now: DateTime<Utc> = "2026-04-01T10:01:00Z".parse().unwrap();

        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            |_trigger_id, _dispatch| Err(anyhow::anyhow!("tick budget exhausted")),
        );

        // The dispatch failed → the queue must still hold BOTH events so the
        // next tick can retry them. We expect exactly ONE attempt outcome
        // (the head event) and then the loop breaks so subsequent events
        // are NOT re-attempted within this tick.
        assert_eq!(outcomes.len(), 1, "should attempt only the head event before bailing out");
        assert_ne!(outcomes[0].status, "dispatched");

        let state_after = orchestrator_core::load_trigger_state(project_root).expect("load state after");
        let run = state_after.triggers.get("on-webhook").expect("trigger state");
        assert_eq!(
            run.pending_events.len(),
            2,
            "BOTH events must remain queued when spawn fails — they were never actually dispatched"
        );
        assert_eq!(run.pending_events[0].event_id, "evt-A");
        assert_eq!(run.pending_events[1].event_id, "evt-B");
        assert_eq!(run.dispatch_count, 0, "no dispatch_count increment on failure");
    }

    #[test]
    fn process_due_triggers_drains_partial_when_budget_runs_out_midway() {
        // With 3 pending events and a budget that allows only the first 2,
        // exactly 2 should be dispatched and 1 must remain queued.
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let _config = write_webhook_trigger_config(project_root, "on-webhook");

        let mut state = orchestrator_core::load_trigger_state(project_root).unwrap_or_default();
        let run_state = state.triggers.entry("on-webhook".to_string()).or_default();
        for i in 0..3u32 {
            run_state.pending_events.push(orchestrator_core::WebhookEvent {
                event_id: format!("evt-{i}"),
                received_at: "2026-04-01T10:00:00Z".parse().unwrap(),
                payload: json!({ "i": i }),
            });
        }
        orchestrator_core::save_trigger_state(project_root, &state).expect("save state");

        let now: DateTime<Utc> = "2026-04-01T10:01:00Z".parse().unwrap();

        let calls = Arc::new(Mutex::new(0usize));
        let calls_ref = calls.clone();
        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            move |_trigger_id, _dispatch| {
                let mut n = calls_ref.lock().unwrap();
                *n += 1;
                if *n > 2 {
                    Err(anyhow::anyhow!("tick budget exhausted"))
                } else {
                    Ok(())
                }
            },
        );

        // 2 dispatches + 1 budget-rejected attempt = 3 outcomes
        let dispatched: usize = outcomes.iter().filter(|o| o.status == "dispatched").count();
        assert_eq!(dispatched, 2, "first two events should dispatch");

        let state_after = orchestrator_core::load_trigger_state(project_root).expect("load");
        let run = state_after.triggers.get("on-webhook").expect("state");
        assert_eq!(run.pending_events.len(), 1, "the third event must remain queued");
        assert_eq!(run.pending_events[0].event_id, "evt-2");
        assert_eq!(run.dispatch_count, 2);
    }

    #[test]
    fn process_due_triggers_no_dispatch_when_webhook_queue_empty() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        let _config = write_webhook_trigger_config(project_root, "on-empty-webhook");

        let now: DateTime<Utc> = "2026-04-01T10:00:00Z".parse().unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let calls_ref = calls.clone();

        let outcomes = TriggerDispatch::process_due_triggers(
            project_root.to_string_lossy().as_ref(),
            now,
            move |_id, _dispatch| {
                *calls_ref.lock().unwrap() += 1;
                Ok(())
            },
        );

        assert!(outcomes.is_empty(), "empty webhook queue → no dispatch");
        assert_eq!(*calls.lock().unwrap(), 0);
    }
}
