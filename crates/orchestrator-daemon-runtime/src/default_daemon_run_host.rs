use anyhow::Result;
use chrono::Utc;
use orchestrator_notifications::{DaemonNotificationRuntime, NotificationLifecycleEvent};
use serde_json::json;

use crate::{DaemonEventLog, DaemonRunEvent, DaemonRunHooks, ProjectTickSummary};

pub struct DefaultDaemonRunHost {
    seq: u64,
    json: bool,
    notification_runtime: Option<DaemonNotificationRuntime>,
    startup_notification_error: Option<String>,
}

impl DefaultDaemonRunHost {
    pub fn new(project_root: &str, json: bool) -> Self {
        match DaemonNotificationRuntime::new(project_root) {
            Ok(runtime) => Self {
                seq: 0,
                json,
                notification_runtime: Some(runtime),
                startup_notification_error: None,
            },
            Err(error) => Self {
                seq: 0,
                json,
                notification_runtime: None,
                startup_notification_error: Some(error.to_string()),
            },
        }
    }

    fn emit_notification_lifecycle_events(
        &mut self,
        events: Vec<NotificationLifecycleEvent>,
    ) -> Result<()> {
        for event in events {
            let record = DaemonEventLog::next_event(
                &mut self.seq,
                &event.event_type,
                event.project_root,
                event.data,
            );
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
            let project = record
                .project_root
                .as_deref()
                .map(|value| format!(" [{value}]"))
                .unwrap_or_default();
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

        if let Some(runtime) = self.notification_runtime.as_mut() {
            match runtime.enqueue_for_event(&record) {
                Ok(lifecycle_events) => {
                    self.emit_notification_lifecycle_events(lifecycle_events)?
                }
                Err(error) => self.emit_notification_runtime_error(
                    record.project_root.clone(),
                    "enqueue",
                    error.to_string().as_str(),
                )?,
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
                "reconciled_stale_tasks": summary.reconciled_stale_tasks,
                "executed_workflow_phases": summary.executed_workflow_phases,
                "failed_workflow_phases": summary.failed_workflow_phases,
            }),
        )?;

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
        match event {
            DaemonRunEvent::Startup {
                project_root,
                daemon_pid,
            } => {
                eprintln!(
                    "{}",
                    json!({
                        "level": "info",
                        "event": "daemon_startup",
                        "timestamp": Utc::now().to_rfc3339(),
                        "pid": daemon_pid,
                        "project_root": project_root,
                    })
                );
                if let Some(error) = self.startup_notification_error.clone() {
                    self.emit_notification_runtime_error(
                        Some(project_root),
                        "startup",
                        error.as_str(),
                    )?;
                }
                Ok(())
            }
            DaemonRunEvent::Status {
                project_root,
                status,
            } => self.emit_daemon_event_with_notifications(
                "status",
                Some(project_root),
                json!({ "status": status }),
            ),
            DaemonRunEvent::StartupCleanup { project_root } => self
                .emit_daemon_event_with_notifications(
                    "recovery",
                    Some(project_root),
                    json!({
                        "startup_cleanup": true,
                    }),
                ),
            DaemonRunEvent::OrphanDetection {
                project_root,
                orphaned_workflows_recovered,
            } => self.emit_daemon_event_with_notifications(
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
            DaemonRunEvent::YamlCompileFailed {
                project_root,
                error,
            } => self.emit_daemon_event_with_notifications(
                "yaml-compile",
                Some(project_root),
                json!({
                    "compiled": false,
                    "error": error,
                }),
            ),
            DaemonRunEvent::TickSummary { summary } => {
                self.emit_project_tick_summary_events(&summary)
            }
            DaemonRunEvent::TickError {
                project_root,
                message,
            } => self.emit_daemon_event_with_notifications(
                "log",
                Some(project_root),
                json!({
                    "level": "error",
                    "message": message,
                }),
            ),
            DaemonRunEvent::GracefulShutdown {
                project_root,
                timeout_secs,
            } => self.emit_daemon_event_with_notifications(
                "graceful-shutdown",
                Some(project_root),
                json!({
                    "timeout_secs": timeout_secs,
                }),
            ),
            DaemonRunEvent::Draining {
                project_root,
                trigger,
            } => self.emit_daemon_event_with_notifications(
                "daemon-draining",
                Some(project_root),
                json!({
                    "trigger": trigger,
                }),
            ),
            DaemonRunEvent::NotificationRuntimeError {
                project_root,
                stage,
                message,
            } => {
                self.emit_notification_runtime_error(project_root, stage.as_str(), message.as_str())
            }
            DaemonRunEvent::Shutdown {
                project_root,
                daemon_pid,
            } => {
                eprintln!(
                    "{}",
                    json!({
                        "level": "info",
                        "event": "daemon_shutdown",
                        "timestamp": Utc::now().to_rfc3339(),
                        "pid": daemon_pid,
                        "project_root": project_root,
                    })
                );
                Ok(())
            }
        }
    }

    async fn flush_notifications(&mut self, project_root: &str) -> Result<()> {
        let Some(runtime) = self.notification_runtime.as_mut() else {
            return Ok(());
        };

        match runtime.flush_due_deliveries().await {
            Ok(lifecycle_events) => self.emit_notification_lifecycle_events(lifecycle_events),
            Err(error) => {
                Err(error.context(format!("failed to flush notifications for {project_root}")))
            }
        }
    }
}
