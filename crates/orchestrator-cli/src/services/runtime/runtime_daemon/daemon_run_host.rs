use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use orchestrator_daemon_runtime::{DaemonEventLog, DaemonRunEvent, DaemonRunHooks, ProjectTickSummary};
use orchestrator_logging::Logger;
use serde_json::json;
use tracing::info;

use super::notifier_dispatcher::{InteractionEventWatcher, NotifierLifecycleEvent, NotifierPluginDispatcher};

pub struct DefaultDaemonRunHost {
    seq: u64,
    json: bool,
    notifier_dispatcher: Option<NotifierPluginDispatcher>,
    startup_notification_error: Option<String>,
    interaction_watcher: InteractionEventWatcher,
    pub logger: Arc<Logger>,
}

impl DefaultDaemonRunHost {
    pub fn new(project_root: &str, json: bool) -> Self {
        let logger = Arc::new(Logger::for_project(Path::new(project_root)));
        // Prime the interaction watcher with pre-existing log history NOW so
        // an interaction created between daemon start and the first
        // flush_notifications tick is still treated as fresh and dispatched
        // (codex round-1 P2: priming lazily on the first flush would swallow
        // it as history).
        let mut interaction_watcher = InteractionEventWatcher::default();
        match DaemonEventLog::read_records(Some(1000), Some(project_root)) {
            Ok(records) => {
                let _ = interaction_watcher.unseen_interaction_events(records);
            }
            Err(error) => {
                tracing::warn!(error = %error, "failed to prime interaction notification watcher");
            }
        }
        match NotifierPluginDispatcher::discover(project_root) {
            Ok(dispatcher) if dispatcher.has_notifiers() => Self {
                seq: 0,
                json,
                notifier_dispatcher: Some(dispatcher),
                startup_notification_error: None,
                interaction_watcher,
                logger,
            },
            Ok(_) => Self {
                seq: 0,
                json,
                notifier_dispatcher: None,
                startup_notification_error: None,
                interaction_watcher,
                logger,
            },
            Err(error) => Self {
                seq: 0,
                json,
                notifier_dispatcher: None,
                startup_notification_error: Some(error.to_string()),
                interaction_watcher,
                logger,
            },
        }
    }

    fn log_event(&self, event: &DaemonRunEvent) {
        match event {
            DaemonRunEvent::Startup { daemon_pid, .. } => {
                self.logger.info("daemon", "daemon started").meta(json!({ "pid": daemon_pid })).emit();
            }
            DaemonRunEvent::Shutdown { daemon_pid, .. } => {
                self.logger.info("daemon", "daemon stopped").meta(json!({ "pid": daemon_pid })).emit();
            }
            DaemonRunEvent::Status { status, .. } => {
                self.logger.info("daemon", format!("status: {status}")).emit();
            }
            DaemonRunEvent::StartupCleanup { .. } => {
                self.logger.info("reconciliation", "startup cleanup").emit();
            }
            DaemonRunEvent::OrphanDetection { orphaned_workflows_recovered, .. } => {
                self.logger
                    .warn("reconciliation", format!("recovered {orphaned_workflows_recovered} orphaned workflows"))
                    .emit();
            }
            DaemonRunEvent::YamlCompileSucceeded { source_files, phase_definitions, agent_profiles, .. } => {
                self.logger
                    .info(
                        "config",
                        format!(
                            "compiled {source_files} YAML files: {phase_definitions} phases, {agent_profiles} agents"
                        ),
                    )
                    .emit();
            }
            DaemonRunEvent::YamlCompileFailed { error, .. } => {
                self.logger.error("config", "YAML compilation failed").err(error).emit();
            }
            DaemonRunEvent::TickSummary { .. } => {}
            DaemonRunEvent::TickError { message, .. } => {
                self.logger.error("daemon", "tick error").err(message).emit();
            }
            DaemonRunEvent::GracefulShutdown { timeout_secs, .. } => {
                self.logger.info("daemon", format!("graceful shutdown (timeout={timeout_secs:?}s)")).emit();
            }
            DaemonRunEvent::Draining { trigger, .. } => {
                self.logger.info("daemon", format!("draining: {trigger}")).emit();
            }
            DaemonRunEvent::NotificationRuntimeError { stage, message, .. } => {
                self.logger.error("notification", format!("notification error at {stage}")).err(message).emit();
            }
            DaemonRunEvent::ConfigReloaded { setting, .. } => {
                self.logger.info("config", format!("hot-reloaded: {setting}")).emit();
            }
            DaemonRunEvent::PluginsDiscovered { plugins, .. } => {
                let count = plugins.len();
                if count == 0 {
                    self.logger.info("plugins", "no Animus plugins discovered").emit();
                } else {
                    let names: Vec<String> = plugins.iter().map(|p| format!("{}@{}", p.name, p.version)).collect();
                    self.logger.info("plugins", format!("discovered {count} plugin(s): {}", names.join(", "))).emit();
                }
            }
            DaemonRunEvent::PluginsDiscoveryFailed { error, .. } => {
                self.logger.warn("plugins", "plugin discovery failed").err(error).emit();
            }
            DaemonRunEvent::TriggerPluginsStarted { plugin_count, .. } => {
                self.logger.info("triggers", format!("trigger plugins started: {plugin_count}")).emit();
            }
            DaemonRunEvent::TriggerPluginStartFailed { plugin_name, error, .. } => {
                self.logger.warn("triggers", format!("trigger plugin {plugin_name} failed to start")).err(error).emit();
            }
            DaemonRunEvent::TriggerPluginEvent { plugin_name, event_id, trigger_id, routed, .. } => {
                let label = trigger_id.as_deref().unwrap_or("<unrouted>");
                self.logger
                    .info(
                        "triggers",
                        format!("trigger event {event_id} from {plugin_name} -> {label} (routed={routed})"),
                    )
                    .emit();
            }
            DaemonRunEvent::TriggerPluginRestart { plugin_name, attempt, delay_ms, .. } => {
                self.logger
                    .warn(
                        "triggers",
                        format!("restarting trigger plugin {plugin_name} (attempt {attempt}, delay {delay_ms}ms)"),
                    )
                    .emit();
            }
            DaemonRunEvent::TriggerPluginCrashed { plugin_name, attempts, error, .. } => {
                self.logger
                    .error("triggers", format!("trigger plugin {plugin_name} crashed after {attempts} attempts"))
                    .err(error)
                    .emit();
            }
            DaemonRunEvent::LogStorageDispatchResolved {
                plugin_name,
                candidate_count,
                disable_env_set,
                warnings,
                ..
            } => {
                let summary = match plugin_name {
                    Some(name) => format!("log storage routed through plugin {name}"),
                    None => "log storage using in-tree fallback (events.jsonl)".to_string(),
                };
                self.logger
                    .info("plugins", summary)
                    .meta(json!({
                        "candidate_count": candidate_count,
                        "disable_env_set": disable_env_set,
                        "warnings": warnings,
                    }))
                    .emit();
                for warning in warnings {
                    self.logger.warn("plugins", warning.clone()).emit();
                }
            }
            DaemonRunEvent::SubjectRouterResolved { plugin_count, kinds, disable_env_set, warnings, .. } => {
                let summary = if *plugin_count == 0 {
                    "subject router empty (no subject_backend plugins active)".to_string()
                } else {
                    format!("subject router resolved: {plugin_count} plugin(s), kinds={:?}", kinds)
                };
                self.logger
                    .info("plugins", summary)
                    .meta(json!({
                        "plugin_count": plugin_count,
                        "kinds": kinds,
                        "disable_env_set": disable_env_set,
                        "warnings": warnings,
                    }))
                    .emit();
                for warning in warnings {
                    self.logger.warn("plugins", warning.clone()).emit();
                }
            }
            DaemonRunEvent::ControlServerResolved { socket_path, disable_env_set, warnings, .. } => {
                let summary = if *disable_env_set {
                    format!("control server disabled via env (would have bound {})", socket_path.display())
                } else if warnings.is_empty() {
                    format!("control server bound at {}", socket_path.display())
                } else {
                    format!("control server not started (failures noted); intended path {}", socket_path.display())
                };
                self.logger
                    .info("control", summary)
                    .meta(json!({
                        "socket_path": socket_path.display().to_string(),
                        "disable_env_set": disable_env_set,
                        "warnings": warnings,
                    }))
                    .emit();
                for warning in warnings {
                    self.logger.warn("control", warning.clone()).emit();
                }
            }
            DaemonRunEvent::PluginPreflight { satisfied, auto_installed, missing, skipped, auto_install, .. } => {
                let summary = if *skipped {
                    "plugin preflight skipped via --skip-preflight".to_string()
                } else if missing.is_empty() {
                    format!("plugin preflight satisfied ({} role(s))", satisfied.len())
                } else {
                    format!("plugin preflight FAILED: {} role(s) missing", missing.len())
                };
                self.logger
                    .info("plugins", summary)
                    .meta(json!({
                        "satisfied": satisfied,
                        "auto_installed": auto_installed,
                        "missing": missing,
                        "skipped": skipped,
                        "auto_install": auto_install,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentScan {
                detected_count,
                cleaned_count,
                unparseable_count,
                unix_scan_supported,
                ..
            } => {
                if !unix_scan_supported {
                    self.logger
                        .warn("reconciliation", "orphan agent scan skipped (not implemented on this platform)")
                        .emit();
                } else {
                    let summary = format!(
                        "orphan agent scan: detected={detected_count} cleaned={cleaned_count} unparseable={unparseable_count}"
                    );
                    self.logger
                        .info("reconciliation", summary)
                        .meta(json!({
                            "detected_count": detected_count,
                            "cleaned_count": cleaned_count,
                            "unparseable_count": unparseable_count,
                        }))
                        .emit();
                }
            }
            DaemonRunEvent::OrphanAgentDetected {
                agent_session_id,
                pid,
                subject_id,
                subject_kind,
                workflow_ref,
                task_id,
                command_line,
                started_at,
                record_path,
                ..
            } => {
                self.logger
                    .warn("reconciliation", format!("orphan agent detected: session={agent_session_id} pid={pid}"))
                    .meta(json!({
                        "kind": "orphan_agent_detected",
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "subject_id": subject_id,
                        "subject_kind": subject_kind,
                        "workflow_ref": workflow_ref,
                        "task_id": task_id,
                        "command_line": command_line,
                        "started_at": started_at,
                        "record_path": record_path,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentCleanup { agent_session_id, pid, record_path, .. } => {
                self.logger
                    .info("reconciliation", format!("orphan agent cleanup: session={agent_session_id} pid={pid}"))
                    .meta(json!({
                        "kind": "orphan_agent_cleanup",
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "record_path": record_path,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentRecordUnparseable { record_path, error, .. } => {
                self.logger
                    .warn("reconciliation", format!("orphan agent record unparseable: {record_path}"))
                    .meta(json!({
                        "kind": "orphan_agent_record_unparseable",
                        "record_path": record_path,
                        "error": error,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentReattached { agent_session_id, pid, socket_path, .. } => {
                self.logger
                    .info("reconciliation", format!("orphan agent reattached: session={agent_session_id} pid={pid}"))
                    .meta(json!({
                        "kind": "orphan_agent_reattached",
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "socket_path": socket_path,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentReattachFailed { agent_session_id, pid, socket_path, error, .. } => {
                self.logger
                    .warn(
                        "reconciliation",
                        format!("orphan agent reattach failed: session={agent_session_id} pid={pid}"),
                    )
                    .meta(json!({
                        "kind": "orphan_agent_reattach_failed",
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "socket_path": socket_path,
                        "error": error,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentGapReplayed { agent_session_id, emitted, next_offset, partial_tail, .. } => {
                self.logger
                    .info(
                        "reconciliation",
                        format!("orphan agent gap replayed: session={agent_session_id} emitted={emitted}"),
                    )
                    .meta(json!({
                        "kind": "orphan_agent_gap_replayed",
                        "agent_session_id": agent_session_id,
                        "emitted": emitted,
                        "next_offset": next_offset,
                        "partial_tail": partial_tail,
                    }))
                    .emit();
            }
            DaemonRunEvent::OrphanAgentGapReplayFailed { agent_session_id, error, .. } => {
                self.logger
                    .warn("reconciliation", format!("orphan agent gap replay failed: session={agent_session_id}"))
                    .meta(json!({
                        "kind": "orphan_agent_gap_replay_failed",
                        "agent_session_id": agent_session_id,
                        "error": error,
                    }))
                    .emit();
            }
            DaemonRunEvent::WorkflowConfigReloaded {
                phase_definitions,
                workflows,
                agent_profiles,
                source_files,
                config_hash,
                ..
            } => {
                self.logger
                    .info(
                        "config",
                        format!(
                            "workflow config reloaded — {phase_definitions} phase definitions, {workflows} workflows"
                        ),
                    )
                    .meta(json!({
                        "phase_definitions": phase_definitions,
                        "workflows": workflows,
                        "agent_profiles": agent_profiles,
                        "source_files": source_files,
                        "config_hash": config_hash,
                    }))
                    .emit();
            }
            DaemonRunEvent::WorkflowConfigReloadFailed { errors, .. } => {
                self.logger
                    .warn("config", "workflow config reload failed; prior config remains active")
                    .meta(json!({"errors": errors}))
                    .emit();
            }
        }
    }

    fn emit_notification_lifecycle_events(&mut self, events: Vec<NotifierLifecycleEvent>) -> Result<()> {
        for event in events {
            let record = DaemonEventLog::next_event(&mut self.seq, &event.event_type, event.project_root, event.data);
            self.emit_record(&record)?;
        }
        Ok(())
    }

    fn emit_notification_runtime_error(
        &mut self,
        project_root: Option<String>,
        stage: &str,
        error: &str,
    ) -> Result<()> {
        let record = DaemonEventLog::next_event(
            &mut self.seq,
            "notification-runtime-error",
            project_root,
            json!({
                "stage": stage,
                "message": error,
            }),
        );
        self.emit_record(&record)
    }

    fn emit_record(&self, record: &protocol::DaemonEventRecord) -> Result<()> {
        DaemonEventLog::append(record)?;
        if self.json {
            println!("{}", serde_json::to_string(record)?);
        } else {
            let project = record.project_root.as_deref().map(|value| format!(" [{value}]")).unwrap_or_default();
            println!("{}{} {}", record.event_type, project, record.timestamp);
        }
        Ok(())
    }

    fn emit_daemon_event_with_notifications(
        &mut self,
        event_type: &str,
        project_root: Option<String>,
        data: serde_json::Value,
    ) -> Result<()> {
        let record = DaemonEventLog::next_event(&mut self.seq, event_type, project_root, data);
        self.emit_record(&record)?;

        if let Some(dispatcher) = self.notifier_dispatcher.as_ref() {
            dispatcher.dispatch(record.clone());
            let lifecycle = dispatcher.drain_lifecycle_events();
            if !lifecycle.is_empty() {
                self.emit_notification_lifecycle_events(lifecycle)?;
            }
        }
        Ok(())
    }

    fn emit_project_tick_summary_events(&mut self, summary: &ProjectTickSummary) -> Result<()> {
        self.emit_daemon_event_with_notifications(
            "health",
            Some(summary.project_root.clone()),
            summary.health.clone(),
        )?;
        self.emit_daemon_event_with_notifications(
            "queue",
            Some(summary.project_root.clone()),
            json!({
                "tasks_total": summary.tasks_total,
                "tasks_ready": summary.tasks_ready,
                "tasks_in_progress": summary.tasks_in_progress,
                "tasks_blocked": summary.tasks_blocked,
                "tasks_done": summary.tasks_done,
                "stale_in_progress_count": summary.stale_in_progress_count,
                "stale_in_progress_threshold_hours": summary.stale_in_progress_threshold_hours,
                "stale_in_progress_task_ids": summary.stale_in_progress_task_ids,
                "workflows_running": summary.workflows_running,
                "workflows_completed": summary.workflows_completed,
                "workflows_failed": summary.workflows_failed,
                "started_ready_workflows": summary.started_ready_workflows,
                "executed_workflow_phases": summary.executed_workflow_phases,
                "failed_workflow_phases": summary.failed_workflow_phases,
            }),
        )?;
        self.emit_daemon_event_with_notifications(
            "workflow",
            Some(summary.project_root.clone()),
            json!({
                "resumed_workflows": summary.resumed_workflows,
                "cleaned_stale_workflows": summary.cleaned_stale_workflows,
                "reconciled_workflows": summary.reconciled_workflows,
                "executed_workflow_phases": summary.executed_workflow_phases,
                "failed_workflow_phases": summary.failed_workflow_phases,
            }),
        )?;

        for failure in &summary.workflow_failures {
            self.emit_daemon_event_with_notifications(
                "workflow-failed",
                Some(summary.project_root.clone()),
                json!({
                    "workflow_id": failure.workflow_id,
                    "workflow_ref": failure.workflow_ref,
                    "subject_id": failure.subject_id,
                    "task_id": failure.task_id,
                    "failure_reason": failure.failure_reason,
                }),
            )?;
        }

        // Budget breaches are de-duplicated at enforcement time (the
        // per-run decision record is the marker), so each breach reaches
        // notifiers exactly once even though the housekeeping sweep
        // re-evaluates caps every heartbeat.
        for breach in &summary.budget_breaches {
            self.emit_daemon_event_with_notifications(
                "workflow-budget-breach",
                Some(summary.project_root.clone()),
                json!({
                    "workflow_run_id": breach.workflow_run_id,
                    "workflow_id": breach.workflow_id,
                    "phase_id": breach.phase_id,
                    "limit_kind": breach.limit_kind,
                    "limit_field": breach.limit_field,
                    "actual": breach.actual,
                    "budget": breach.budget,
                    "on_exceed": breach.on_exceed,
                    "action": breach.action,
                    "observed_at": breach.observed_at,
                }),
            )?;
        }

        for task_change in &summary.task_state_changes {
            let mut data = json!({
                "task_id": task_change.task_id,
                "from_status": task_change.from_status,
                "to_status": task_change.to_status,
                "changed_at": task_change.changed_at,
            });
            if let Some(selection_source) = task_change.selection_source {
                data["selection_source"] = json!(selection_source.as_str());
            }
            if let Some(blocked_reason) = task_change.blocked_reason.as_deref() {
                data["blocked_reason"] = json!(blocked_reason);
            }
            self.emit_daemon_event_with_notifications("task-state-change", Some(summary.project_root.clone()), data)?;

            // Dedicated event type so notifier subscriptions can filter on
            // "task-blocked" directly. Task state changes are diffed against
            // the pre-tick snapshot, so a given blocked transition fires
            // exactly once even though blocked-state reconciliation re-runs
            // on every tick.
            if task_change.to_status == orchestrator_core::TaskStatus::Blocked.to_string() {
                self.emit_daemon_event_with_notifications(
                    "task-blocked",
                    Some(summary.project_root.clone()),
                    json!({
                        "task_id": task_change.task_id,
                        "from_status": task_change.from_status,
                        "blocked_reason": task_change.blocked_reason,
                        "changed_at": task_change.changed_at,
                    }),
                )?;
            }
        }

        for phase_event in &summary.phase_execution_events {
            self.emit_daemon_event_with_notifications(
                &phase_event.event_type,
                Some(phase_event.project_root.clone()),
                json!({
                    "workflow_id": phase_event.workflow_id,
                    "task_id": phase_event.task_id,
                    "phase_id": phase_event.phase_id,
                    "phase_mode": phase_event.phase_mode,
                    "metadata": phase_event.metadata,
                    "payload": phase_event.payload,
                }),
            )?;
        }

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl DaemonRunHooks for DefaultDaemonRunHost {
    fn handle_event(&mut self, event: DaemonRunEvent) -> Result<()> {
        self.log_event(&event);
        match event {
            DaemonRunEvent::Startup { project_root, daemon_pid } => {
                info!(
                    event = "daemon_startup",
                    pid = daemon_pid,
                    project_root = %project_root,
                    "daemon starting"
                );
                if let Some(error) = self.startup_notification_error.clone() {
                    self.emit_notification_runtime_error(Some(project_root), "startup", error.as_str())?;
                }
                Ok(())
            }
            DaemonRunEvent::Status { project_root, status } => {
                self.emit_daemon_event_with_notifications("status", Some(project_root), json!({ "status": status }))
            }
            DaemonRunEvent::StartupCleanup { project_root } => self.emit_daemon_event_with_notifications(
                "recovery",
                Some(project_root),
                json!({
                    "startup_cleanup": true,
                }),
            ),
            DaemonRunEvent::OrphanDetection { project_root, orphaned_workflows_recovered } => self
                .emit_daemon_event_with_notifications(
                    "orphan-detection",
                    Some(project_root),
                    json!({
                        "orphaned_workflows_recovered": orphaned_workflows_recovered,
                        "recovery_action": "blocked",
                        "blocked_reason": "orphaned_after_daemon_restart",
                    }),
                ),
            DaemonRunEvent::YamlCompileSucceeded {
                project_root,
                source_files,
                output_path,
                phase_definitions,
                agent_profiles,
            } => self.emit_daemon_event_with_notifications(
                "yaml-compile",
                Some(project_root),
                json!({
                    "compiled": true,
                    "source_files": source_files,
                    "output_path": output_path,
                    "phase_definitions": phase_definitions,
                    "agent_profiles": agent_profiles,
                }),
            ),
            DaemonRunEvent::YamlCompileFailed { project_root, error } => self.emit_daemon_event_with_notifications(
                "yaml-compile",
                Some(project_root),
                json!({
                    "compiled": false,
                    "error": error,
                }),
            ),
            DaemonRunEvent::TickSummary { summary } => self.emit_project_tick_summary_events(&summary),
            DaemonRunEvent::TickError { project_root, message } => self.emit_daemon_event_with_notifications(
                "log",
                Some(project_root),
                json!({
                    "level": "error",
                    "message": message,
                }),
            ),
            DaemonRunEvent::GracefulShutdown { project_root, timeout_secs } => self
                .emit_daemon_event_with_notifications(
                    "graceful-shutdown",
                    Some(project_root),
                    json!({
                        "timeout_secs": timeout_secs,
                    }),
                ),
            DaemonRunEvent::Draining { project_root, trigger } => self.emit_daemon_event_with_notifications(
                "daemon-draining",
                Some(project_root),
                json!({
                    "trigger": trigger,
                }),
            ),
            DaemonRunEvent::NotificationRuntimeError { project_root, stage, message } => {
                self.emit_notification_runtime_error(project_root, stage.as_str(), message.as_str())
            }
            DaemonRunEvent::ConfigReloaded { project_root, setting } => self.emit_daemon_event_with_notifications(
                "config-reload",
                Some(project_root),
                json!({
                    "setting": setting,
                }),
            ),
            DaemonRunEvent::Shutdown { project_root, daemon_pid } => {
                info!(
                    event = "daemon_shutdown",
                    pid = daemon_pid,
                    project_root = %project_root,
                    "daemon stopping"
                );
                Ok(())
            }
            DaemonRunEvent::PluginsDiscovered { project_root, plugins } => {
                let payload = json!({
                    "count": plugins.len(),
                    "plugins": plugins.iter().map(|p| json!({
                        "name": p.name,
                        "version": p.version,
                        "plugin_kind": p.plugin_kind,
                        "source": p.source,
                        "path": p.path,
                    })).collect::<Vec<_>>(),
                });
                self.emit_daemon_event_with_notifications("plugins-discovered", Some(project_root), payload)
            }
            DaemonRunEvent::PluginsDiscoveryFailed { project_root, error } => self
                .emit_daemon_event_with_notifications(
                    "plugins-discovery-failed",
                    Some(project_root),
                    json!({ "error": error }),
                ),
            DaemonRunEvent::TriggerPluginsStarted { project_root, plugin_count } => self
                .emit_daemon_event_with_notifications(
                    "trigger-plugins-started",
                    Some(project_root),
                    json!({ "plugin_count": plugin_count }),
                ),
            DaemonRunEvent::TriggerPluginStartFailed { project_root, plugin_name, error } => self
                .emit_daemon_event_with_notifications(
                    "trigger-plugin-start-failed",
                    Some(project_root),
                    json!({ "plugin": plugin_name, "error": error }),
                ),
            DaemonRunEvent::TriggerPluginEvent { project_root, plugin_name, event_id, trigger_id, routed } => self
                .emit_daemon_event_with_notifications(
                    "trigger-plugin-event",
                    Some(project_root),
                    json!({
                        "plugin": plugin_name,
                        "event_id": event_id,
                        "trigger_id": trigger_id,
                        "routed": routed,
                    }),
                ),
            DaemonRunEvent::TriggerPluginRestart { project_root, plugin_name, attempt, delay_ms } => self
                .emit_daemon_event_with_notifications(
                    "trigger-plugin-restart",
                    Some(project_root),
                    json!({
                        "plugin": plugin_name,
                        "attempt": attempt,
                        "delay_ms": delay_ms,
                    }),
                ),
            DaemonRunEvent::TriggerPluginCrashed { project_root, plugin_name, attempts, error } => self
                .emit_daemon_event_with_notifications(
                    "trigger-plugin-crashed",
                    Some(project_root),
                    json!({
                        "plugin": plugin_name,
                        "attempts": attempts,
                        "error": error,
                    }),
                ),
            DaemonRunEvent::LogStorageDispatchResolved {
                project_root,
                plugin_name,
                candidate_count,
                disable_env_set,
                warnings,
            } => self.emit_daemon_event_with_notifications(
                "log-storage-dispatch-resolved",
                Some(project_root),
                json!({
                    "plugin": plugin_name,
                    "candidate_count": candidate_count,
                    "disable_env_set": disable_env_set,
                    "warnings": warnings,
                }),
            ),
            DaemonRunEvent::SubjectRouterResolved { project_root, plugin_count, kinds, disable_env_set, warnings } => {
                self.emit_daemon_event_with_notifications(
                    "subject-router-resolved",
                    Some(project_root),
                    json!({
                        "plugin_count": plugin_count,
                        "kinds": kinds,
                        "disable_env_set": disable_env_set,
                        "warnings": warnings,
                    }),
                )
            }
            DaemonRunEvent::ControlServerResolved { project_root, socket_path, disable_env_set, warnings } => self
                .emit_daemon_event_with_notifications(
                    "control-server-resolved",
                    Some(project_root),
                    json!({
                        "socket_path": socket_path.display().to_string(),
                        "disable_env_set": disable_env_set,
                        "warnings": warnings,
                    }),
                ),
            DaemonRunEvent::PluginPreflight {
                project_root,
                satisfied,
                auto_installed,
                missing,
                skipped,
                auto_install,
            } => self.emit_daemon_event_with_notifications(
                "plugin-preflight",
                Some(project_root),
                json!({
                    "satisfied": satisfied,
                    "auto_installed": auto_installed,
                    "missing": missing,
                    "skipped": skipped,
                    "auto_install": auto_install,
                }),
            ),
            DaemonRunEvent::OrphanAgentScan {
                project_root,
                detected_count,
                cleaned_count,
                unparseable_count,
                unix_scan_supported,
            } => self.emit_daemon_event_with_notifications(
                "orphan-agent-scan",
                Some(project_root),
                json!({
                    "detected_count": detected_count,
                    "cleaned_count": cleaned_count,
                    "unparseable_count": unparseable_count,
                    "unix_scan_supported": unix_scan_supported,
                }),
            ),
            DaemonRunEvent::OrphanAgentDetected {
                project_root,
                agent_session_id,
                pid,
                subject_id,
                subject_kind,
                workflow_ref,
                task_id,
                command_line,
                started_at,
                record_path,
            } => self.emit_daemon_event_with_notifications(
                "orphan-agent-detected",
                Some(project_root),
                json!({
                    "agent_session_id": agent_session_id,
                    "pid": pid,
                    "subject_id": subject_id,
                    "subject_kind": subject_kind,
                    "workflow_ref": workflow_ref,
                    "task_id": task_id,
                    "command_line": command_line,
                    "started_at": started_at,
                    "record_path": record_path,
                }),
            ),
            DaemonRunEvent::OrphanAgentCleanup { project_root, agent_session_id, pid, record_path } => self
                .emit_daemon_event_with_notifications(
                    "orphan-agent-cleanup",
                    Some(project_root),
                    json!({
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "record_path": record_path,
                    }),
                ),
            DaemonRunEvent::OrphanAgentRecordUnparseable { project_root, record_path, error } => self
                .emit_daemon_event_with_notifications(
                    "orphan-agent-record-unparseable",
                    Some(project_root),
                    json!({
                        "record_path": record_path,
                        "error": error,
                    }),
                ),
            DaemonRunEvent::OrphanAgentReattached { project_root, agent_session_id, pid, socket_path } => self
                .emit_daemon_event_with_notifications(
                    "orphan-agent-reattached",
                    Some(project_root),
                    json!({
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "socket_path": socket_path,
                    }),
                ),
            DaemonRunEvent::OrphanAgentReattachFailed { project_root, agent_session_id, pid, socket_path, error } => {
                self.emit_daemon_event_with_notifications(
                    "orphan-agent-reattach-failed",
                    Some(project_root),
                    json!({
                        "agent_session_id": agent_session_id,
                        "pid": pid,
                        "socket_path": socket_path,
                        "error": error,
                    }),
                )
            }
            DaemonRunEvent::OrphanAgentGapReplayed {
                project_root,
                agent_session_id,
                emitted,
                next_offset,
                partial_tail,
            } => self.emit_daemon_event_with_notifications(
                "orphan-agent-gap-replayed",
                Some(project_root),
                json!({
                    "agent_session_id": agent_session_id,
                    "emitted": emitted,
                    "next_offset": next_offset,
                    "partial_tail": partial_tail,
                }),
            ),
            DaemonRunEvent::OrphanAgentGapReplayFailed { project_root, agent_session_id, error } => self
                .emit_daemon_event_with_notifications(
                    "orphan-agent-gap-replay-failed",
                    Some(project_root),
                    json!({
                        "agent_session_id": agent_session_id,
                        "error": error,
                    }),
                ),
            DaemonRunEvent::WorkflowConfigReloaded {
                project_root,
                phase_definitions,
                workflows,
                agent_profiles,
                source_files,
                config_hash,
            } => self.emit_daemon_event_with_notifications(
                "workflow-config-reloaded",
                Some(project_root),
                json!({
                    "phase_definitions": phase_definitions,
                    "workflows": workflows,
                    "agent_profiles": agent_profiles,
                    "source_files": source_files,
                    "config_hash": config_hash,
                }),
            ),
            DaemonRunEvent::WorkflowConfigReloadFailed { project_root, errors } => self
                .emit_daemon_event_with_notifications(
                    "workflow-config-reload-failed",
                    Some(project_root),
                    json!({"errors": errors}),
                ),
        }
    }

    async fn flush_notifications(&mut self, project_root: &str) -> Result<()> {
        let Some(dispatcher) = self.notifier_dispatcher.clone() else {
            return Ok(());
        };
        // Interaction lifecycle events (interaction_created / answered /
        // expired) and agent coordination events (agent-memory-updated /
        // agent-message-sent) are appended to the daemon event log by the MCP
        // serve and CLI processes, not by the daemon, so they never pass
        // through emit_daemon_event_with_notifications. Tail the log each
        // tick and fan fresh ones out to notifier plugins (best-effort: a
        // read failure only skips this tick). The watcher's priming scan
        // swallows history so daemon start does not replay old events.
        match DaemonEventLog::read_records(Some(1000), Some(project_root)) {
            Ok(records) => {
                for record in self.interaction_watcher.unseen_interaction_events(records) {
                    dispatcher.dispatch(record);
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "failed to read daemon event log for interaction notifications");
            }
        }
        let lifecycle = dispatcher.flush().await;
        if lifecycle.is_empty() {
            return Ok(());
        }
        self.emit_notification_lifecycle_events(lifecycle)
    }

    async fn shutdown_drain_notifications(&mut self, _project_root: &str) -> Result<()> {
        let Some(dispatcher) = self.notifier_dispatcher.as_ref() else {
            return Ok(());
        };
        dispatcher.shutdown_drain().await;
        // Final drain of any lifecycle events emitted during the wait.
        let lifecycle = dispatcher.drain_lifecycle_events();
        if lifecycle.is_empty() {
            return Ok(());
        }
        self.emit_notification_lifecycle_events(lifecycle)
    }
}
