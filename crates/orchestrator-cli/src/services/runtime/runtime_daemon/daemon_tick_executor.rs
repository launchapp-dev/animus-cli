use super::*;
use crate::services::runtime::runtime_daemon::daemon_process_manager::{
    ProcessManager, WorkflowSubjectArgs,
};

async fn dispatch_ready_tasks_via_runner(
    hub: Arc<dyn ServiceHub>,
    root: &str,
    process_manager: &mut ProcessManager,
    limit: usize,
) -> Result<ReadyTaskWorkflowStartSummary> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids = active_workflow_task_ids(&workflows);
    let candidates = hub.tasks().list_prioritized().await?;
    let mut started_workflows = Vec::new();

    for task in candidates {
        if started_workflows.len() >= limit {
            break;
        }

        if task.paused || task.cancelled {
            continue;
        }
        if task.status != TaskStatus::Ready {
            continue;
        }
        if active_task_ids.contains(&task.id) {
            continue;
        }
        if should_skip_dispatch(&task) {
            continue;
        }

        let dependency_issues = dependency_gate_issues_for_task(hub.clone(), root, &task).await;
        if !dependency_issues.is_empty() {
            let reason = dependency_blocked_reason(&dependency_issues);
            let _ = set_task_blocked_with_reason(hub.clone(), &task, reason, None).await;
            continue;
        }

        let pipeline_id = super::pipeline_for_task(&task);
        let subject = WorkflowSubjectArgs::Task {
            task_id: task.id.clone(),
        };
        match process_manager.spawn_workflow_runner(&subject, &pipeline_id, root) {
            Ok(_) => {
                let _ = hub
                    .tasks()
                    .set_status(&task.id, TaskStatus::InProgress, false)
                    .await;
                started_workflows.push(ReadyTaskWorkflowStart {
                    task_id: task.id.clone(),
                    workflow_id: task.id.clone(),
                    selection_source: TaskSelectionSource::FallbackPicker,
                });
            }
            Err(error) => {
                let reason = format!("failed to start workflow runner: {error}");
                let _ = set_task_blocked_with_reason(hub.clone(), &task, reason, None).await;
            }
        }
    }

    Ok(ReadyTaskWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    })
}

#[cfg(test)]
pub(super) struct FullProjectTickOperations<'a> {
    pub(super) hub: Arc<dyn ServiceHub>,
    pub(super) root: &'a str,
}

#[cfg(test)]
pub(super) struct FullProjectTickDriver {
    pub(super) schedule_process_manager: ProcessManager,
}

#[cfg(test)]
impl ProjectTickDriver for FullProjectTickDriver {
    type Operations<'a>
        = FullProjectTickOperations<'a>
    where
        Self: 'a;

    fn process_due_schedules(&mut self, root: &str, now: chrono::DateTime<chrono::Utc>) {
        ScheduleDispatch::process_due_schedules(&mut self.schedule_process_manager, root, now);
    }

    fn flush_git_outbox(&mut self, root: &str) {
        let _ = git_ops::flush_git_integration_outbox(root);
    }

    fn build_operations<'a>(
        &'a mut self,
        hub: Arc<dyn ServiceHub>,
        root: &'a str,
    ) -> Self::Operations<'a> {
        FullProjectTickOperations { hub, root }
    }
}

#[cfg(test)]
#[async_trait::async_trait(?Send)]
impl ProjectTickOperations for FullProjectTickOperations<'_> {
    async fn bootstrap_from_vision(
        &mut self,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()> {
        bootstrap_from_vision_if_needed(self.hub.clone(), startup_cleanup, ai_task_generation).await
    }

    async fn ensure_ai_generated_tasks(&mut self) -> Result<()> {
        let _ = ensure_tasks_for_unplanned_requirements(self.hub.clone(), self.root).await;
        Ok(())
    }

    async fn resume_interrupted(&mut self) -> Result<(usize, usize)> {
        resume_interrupted_workflows_for_project(self.hub.clone(), self.root).await
    }

    async fn recover_orphaned_running_workflows(&mut self) -> Result<()> {
        let _ = recover_orphaned_running_workflows(self.hub.clone(), self.root).await;
        Ok(())
    }

    async fn reconcile_stale_tasks(&mut self, stale_threshold_hours: u64) -> Result<usize> {
        reconcile_stale_in_progress_tasks_for_project(
            self.hub.clone(),
            self.root,
            stale_threshold_hours,
        )
        .await
    }

    async fn reconcile_dependency_tasks(&mut self) -> Result<usize> {
        reconcile_dependency_gate_tasks_for_project(self.hub.clone(), self.root).await
    }

    async fn reconcile_merge_tasks(&mut self) -> Result<usize> {
        reconcile_merge_gate_tasks_for_project(self.hub.clone(), self.root).await
    }

    async fn retry_failed_task_workflows(&mut self) -> Result<()> {
        let _ = retry_failed_task_workflows(self.hub.clone()).await;
        Ok(())
    }

    async fn promote_backlog_tasks_to_ready(&mut self) -> Result<()> {
        let _ = promote_backlog_tasks_to_ready(self.hub.clone(), self.root).await;
        Ok(())
    }

    async fn dispatch_ready_tasks(&mut self, limit: usize) -> Result<ReadyTaskWorkflowStartSummary> {
        run_ready_task_workflows_for_project(self.hub.clone(), self.root, limit).await
    }

    async fn refresh_runtime_binaries(&mut self) -> Result<()> {
        let _ = git_ops::refresh_runtime_binaries_if_main_advanced(
            self.hub.clone(),
            self.root,
            git_ops::RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        Ok(())
    }

    async fn execute_running_workflow_phases(
        &mut self,
        limit: usize,
    ) -> Result<(usize, usize, Vec<PhaseExecutionEvent>)> {
        execute_running_workflow_phases_for_project(self.hub.clone(), self.root, limit).await
    }
}

pub(super) struct SlimProjectTickOperations<'a> {
    pub(super) hub: Arc<dyn ServiceHub>,
    pub(super) root: &'a str,
    pub(super) process_manager: &'a mut ProcessManager,
}

pub(super) struct SlimProjectTickDriver<'a> {
    pub(super) process_manager: &'a mut ProcessManager,
}

impl ProjectTickDriver for SlimProjectTickDriver<'_> {
    type Operations<'a>
        = SlimProjectTickOperations<'a>
    where
        Self: 'a;

    fn process_due_schedules(&mut self, root: &str, now: chrono::DateTime<chrono::Utc>) {
        ScheduleDispatch::process_due_schedules(self.process_manager, root, now);
    }

    fn flush_git_outbox(&mut self, root: &str) {
        let _ = git_ops::flush_git_integration_outbox(root);
    }

    fn emit_notice(&mut self, message: &str) {
        eprintln!("{}", message);
    }

    fn build_operations<'a>(
        &'a mut self,
        hub: Arc<dyn ServiceHub>,
        root: &'a str,
    ) -> Self::Operations<'a> {
        SlimProjectTickOperations {
            hub,
            root,
            process_manager: self.process_manager,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ProjectTickOperations for SlimProjectTickOperations<'_> {
    async fn bootstrap_from_vision(
        &mut self,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()> {
        bootstrap_from_vision_if_needed(self.hub.clone(), startup_cleanup, ai_task_generation).await
    }

    async fn ensure_ai_generated_tasks(&mut self) -> Result<()> {
        let _ = ensure_tasks_for_unplanned_requirements(self.hub.clone(), self.root).await;
        Ok(())
    }

    async fn resume_interrupted(&mut self) -> Result<(usize, usize)> {
        resume_interrupted_workflows_for_project(self.hub.clone(), self.root).await
    }

    async fn recover_orphaned_running_workflows(&mut self) -> Result<()> {
        let _ = recover_orphaned_running_workflows(self.hub.clone(), self.root).await;
        Ok(())
    }

    async fn reconcile_stale_tasks(&mut self, stale_threshold_hours: u64) -> Result<usize> {
        reconcile_stale_in_progress_tasks_for_project(
            self.hub.clone(),
            self.root,
            stale_threshold_hours,
        )
        .await
    }

    async fn reconcile_dependency_tasks(&mut self) -> Result<usize> {
        reconcile_dependency_gate_tasks_for_project(self.hub.clone(), self.root).await
    }

    async fn reconcile_merge_tasks(&mut self) -> Result<usize> {
        reconcile_merge_gate_tasks_for_project(self.hub.clone(), self.root).await
    }

    async fn reconcile_completed_processes(&mut self) -> Result<(usize, usize)> {
        let completed_processes = self.process_manager.check_running();
        Ok(CompletionReconciler::reconcile(self.hub.clone(), self.root, completed_processes).await)
    }

    async fn retry_failed_task_workflows(&mut self) -> Result<()> {
        let _ = retry_failed_task_workflows(self.hub.clone()).await;
        Ok(())
    }

    async fn promote_backlog_tasks_to_ready(&mut self) -> Result<()> {
        let _ = promote_backlog_tasks_to_ready(self.hub.clone(), self.root).await;
        Ok(())
    }

    async fn dispatch_ready_tasks(&mut self, limit: usize) -> Result<ReadyTaskWorkflowStartSummary> {
        dispatch_ready_tasks_via_runner(self.hub.clone(), self.root, self.process_manager, limit)
            .await
    }

    async fn refresh_runtime_binaries(&mut self) -> Result<()> {
        let _ = git_ops::refresh_runtime_binaries_if_main_advanced(
            self.hub.clone(),
            self.root,
            git_ops::RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        Ok(())
    }
}
