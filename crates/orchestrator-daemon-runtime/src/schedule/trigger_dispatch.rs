use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use chrono::{DateTime, Utc};
use tracing::warn;

use super::TriggerDispatchOutcome;
use crate::SubjectDispatch;

pub struct TriggerDispatch;

impl TriggerDispatch {
    /// Process all due file-watcher triggers for `project_root` at `now`.
    ///
    /// For each enabled `file_watcher` trigger whose watched paths have
    /// been modified since the last dispatch — and whose debounce window
    /// has elapsed — `spawn_pipeline` is called and trigger state is
    /// persisted.
    pub fn process_due_triggers<PipelineSpawner>(
        project_root: &str,
        now: DateTime<Utc>,
        mut spawn_pipeline: PipelineSpawner,
    ) -> Vec<TriggerDispatchOutcome>
    where
        PipelineSpawner: FnMut(&str, &SubjectDispatch) -> Result<()>,
    {
        let config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
        let triggers: Vec<&orchestrator_core::workflow_config::WorkflowTrigger> = config
            .config
            .triggers
            .iter()
            .filter(|t| t.enabled && t.trigger_type == orchestrator_core::workflow_config::TriggerType::FileWatcher)
            .collect();

        if triggers.is_empty() {
            return Vec::new();
        }

        let mut state = orchestrator_core::load_trigger_state(std::path::Path::new(project_root)).unwrap_or_default();

        let mut outcomes = Vec::new();
        for trigger in triggers {
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

            if last_known_mtime == 0 {
                // First tick: seed the mtime baseline without dispatching.
                // This prevents a spurious dispatch on daemon startup for files
                // that already exist.
                run_state.extra = Some(serde_json::json!({ "last_mtime_secs": current_max_mtime }));
            } else if current_max_mtime > last_known_mtime {
                // A file has been modified since last check — fire the trigger.
                let status = dispatch_trigger(&trigger.id, trigger, now, "file-watcher", &mut spawn_pipeline);

                // Update state.
                run_state.last_dispatched = Some(now);
                run_state.last_status = status.clone();
                run_state.dispatch_count += 1;
                run_state.extra = Some(serde_json::json!({ "last_mtime_secs": current_max_mtime }));

                outcomes.push(TriggerDispatchOutcome { trigger_id: trigger.id.clone(), status });
            }
        }

        // Persist updated state (best-effort).
        if !outcomes.is_empty() || state.triggers.values().any(|s| s.extra.is_some()) {
            let _ = orchestrator_core::save_trigger_state(std::path::Path::new(project_root), &state);
        }

        outcomes
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
            format!("Triggered by file-watcher '{trigger_id}'"),
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

    fn write_trigger_config(project_root: &std::path::Path, trigger_id: &str, paths: &[&str]) {
        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "auto-test".to_string(),
            name: "Auto Test".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            post_success: None,
            variables: Vec::new(),
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
        orchestrator_core::write_workflow_config(project_root, &config).expect("workflow config should be written");
    }

    #[test]
    fn process_due_triggers_fires_when_file_is_newer_than_baseline() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        // Create a file to watch.
        let watched_file = project_root.join("watched.rs");
        std::fs::write(&watched_file, "fn main() {}").expect("write file");

        write_trigger_config(project_root, "on-change", &[watched_file.to_string_lossy().as_ref()]);

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
            post_success: None,
            variables: Vec::new(),
        });
        config.triggers.push(orchestrator_core::workflow_config::WorkflowTrigger {
            id: "disabled-watcher".to_string(),
            trigger_type: orchestrator_core::workflow_config::TriggerType::FileWatcher,
            workflow_ref: Some("auto-test".to_string()),
            enabled: false,
            config: json!({ "paths": [watched_file.to_string_lossy().as_ref()], "debounce_secs": 0 }),
            input: None,
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("write config");

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
        write_trigger_config(project_root, "debounced", &[watched_file.to_string_lossy().as_ref()]);

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
}
