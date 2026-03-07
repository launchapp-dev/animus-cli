use super::*;
#[cfg(test)]
use orchestrator_daemon_runtime::{default_full_project_tick_driver, DefaultFullProjectTickDriver};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, CompletedProcess, DefaultProjectTickServices,
    DefaultSlimProjectTickDriver, ProcessManager, WorkflowSubjectArgs,
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

pub(crate) struct CliProjectTickServices;

#[async_trait::async_trait(?Send)]
impl DefaultProjectTickServices for CliProjectTickServices {
    fn flush_git_outbox(&mut self, root: &str) {
        let _ = git_ops::flush_git_integration_outbox(root);
    }

    async fn bootstrap_from_vision(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        _root: &str,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()> {
        bootstrap_from_vision_if_needed(hub, startup_cleanup, ai_task_generation).await
    }

    async fn ensure_ai_generated_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        let _ = ensure_tasks_for_unplanned_requirements(hub, root).await;
        Ok(())
    }

    async fn resume_interrupted(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<(usize, usize)> {
        resume_interrupted_workflows_for_project(hub, root).await
    }

    async fn recover_orphaned_running_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        active_subject_ids: &std::collections::HashSet<String>,
    ) -> Result<()> {
        let _ =
            recover_orphaned_running_workflows_with_active_ids(hub, root, active_subject_ids).await;
        Ok(())
    }

    async fn reconcile_stale_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        stale_threshold_hours: u64,
    ) -> Result<usize> {
        reconcile_stale_in_progress_tasks_for_project(hub, root, stale_threshold_hours).await
    }

    async fn reconcile_dependency_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        reconcile_dependency_gate_tasks_for_project(hub, root).await
    }

    async fn reconcile_merge_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        reconcile_merge_gate_tasks_for_project(hub, root).await
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)> {
        Ok(CompletionReconciler::reconcile(hub, root, completed_processes).await)
    }

    async fn retry_failed_task_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<()> {
        let _ = retry_failed_task_workflows(hub).await;
        Ok(())
    }

    async fn promote_backlog_tasks_to_ready(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        let _ = promote_backlog_tasks_to_ready(hub, root).await;
        Ok(())
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        match process_manager {
            Some(process_manager) => {
                dispatch_ready_tasks_via_runner(hub, root, process_manager, limit).await
            }
            None => run_ready_task_workflows_for_project(hub, root, limit).await,
        }
    }

    async fn refresh_runtime_binaries(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        let _ = git_ops::refresh_runtime_binaries_if_main_advanced(
            hub,
            root,
            git_ops::RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
pub(super) type FullProjectTickDriver = DefaultFullProjectTickDriver<CliProjectTickServices>;

pub(crate) type SlimProjectTickDriver<'a> =
    DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

#[cfg(test)]
pub(super) fn full_project_tick_driver() -> FullProjectTickDriver {
    default_full_project_tick_driver(CliProjectTickServices)
}

pub(crate) fn slim_project_tick_driver(
    process_manager: &mut ProcessManager,
) -> SlimProjectTickDriver<'_> {
    default_slim_project_tick_driver(CliProjectTickServices, process_manager)
}
