use super::*;
use crate::services::runtime::stale_in_progress_summary;

#[path = "daemon_task_dispatch.rs"]
pub(super) mod task_dispatch;

#[path = "daemon_phase_pool.rs"]
pub(super) mod phase_pool;

#[path = "daemon_reconciliation.rs"]
pub(super) mod reconciliation;

#[path = "daemon_task_lifecycle.rs"]
pub(super) mod task_lifecycle;

#[path = "daemon_bootstrap.rs"]
pub(super) mod bootstrap;

#[path = "daemon_agent_slot.rs"]
pub(super) mod agent_slot;

use task_dispatch::*;
use phase_pool::*;
use reconciliation::*;
use task_lifecycle::*;
use bootstrap::*;
use agent_slot::*;

pub(super) async fn project_tick(root: &str, args: &DaemonRunArgs) -> Result<ProjectTickSummary> {
    let root = canonicalize_lossy(root);
    let hub = Arc::new(FileServiceHub::new(&root)?);
    let _ = git_ops::flush_git_integration_outbox(&root);
    let requirements_before = hub.planning().list_requirements().await.unwrap_or_default();
    let tasks_before = hub.tasks().list().await.unwrap_or_default();
    let daemon = hub.daemon();
    let status = daemon.status().await?;
    let mut started_daemon = false;
    if !matches!(
        status,
        orchestrator_core::DaemonStatus::Running | orchestrator_core::DaemonStatus::Paused
    ) {
        daemon.start().await?;
        started_daemon = true;
    }

    bootstrap_from_vision_if_needed(hub.clone(), args.startup_cleanup, args.ai_task_generation)
        .await?;

    if args.ai_task_generation {
        let _ = ensure_tasks_for_unplanned_requirements(hub.clone(), &root).await;
    }

    let mut cleaned_stale_workflows = 0usize;
    let mut resumed_workflows = 0usize;
    if args.resume_interrupted {
        let (cleaned, resumed) =
            resume_interrupted_workflows_for_project(hub.clone(), &root).await?;
        cleaned_stale_workflows = cleaned;
        resumed_workflows = resumed;
    }

    let _recovered_orphans =
        recover_orphaned_running_workflows(hub.clone(), &root).await;

    let reconciled_stale_tasks = if args.reconcile_stale {
        reconcile_stale_in_progress_tasks_for_project(hub.clone(), &root).await?
    } else {
        0
    };
    let reconciled_dependency_tasks =
        reconcile_dependency_gate_tasks_for_project(hub.clone(), &root).await?;
    let reconciled_merge_tasks = reconcile_merge_gate_tasks_for_project(hub.clone(), &root).await?;

    let pool_draining = phase_pool::is_pool_draining(&root);

    if !pool_draining && args.auto_run_ready {
        let _ = retry_failed_task_workflows(hub.clone()).await;
        let _ = promote_backlog_tasks_to_ready(hub.clone(), &root).await;
    }

    let ready_dispatch_limit = if !pool_draining && args.auto_run_ready {
        match daemon.health().await {
            Ok(health) => ready_task_dispatch_limit(args.max_tasks_per_tick, &health),
            Err(_) => args.max_tasks_per_tick,
        }
    } else {
        0
    };
    let ready_workflow_starts = if !pool_draining && args.auto_run_ready {
        run_ready_task_workflows_for_project(hub.clone(), &root, ready_dispatch_limit).await?
    } else {
        ReadyTaskWorkflowStartSummary::default()
    };
    let _ = git_ops::refresh_runtime_binaries_if_main_advanced(
        hub.clone(),
        &root,
        git_ops::RuntimeBinaryRefreshTrigger::Tick,
    )
    .await;
    let (executed_workflow_phases, failed_workflow_phases, phase_execution_events) =
        execute_running_workflow_phases_for_project(hub.clone(), &root, args.max_tasks_per_tick)
            .await?;

    let health = serde_json::to_value(daemon.health().await?)?;
    let tasks = hub.tasks().list().await?;
    let workflows = hub.workflows().list().await.unwrap_or_default();

    let tasks_total = tasks.len();
    let tasks_ready = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog))
        .count();
    let tasks_in_progress = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .count();
    let tasks_blocked = tasks.iter().filter(|task| task.status.is_blocked()).count();
    let tasks_done = tasks
        .iter()
        .filter(|task| task.status.is_terminal())
        .count();
    let stale_in_progress =
        stale_in_progress_summary(&tasks, args.stale_threshold_hours, Utc::now());

    let workflows_running = workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused
            )
        })
        .count();
    let workflows_completed = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .count();
    let workflows_failed = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .count();
    let requirements_after = hub.planning().list_requirements().await.unwrap_or_default();
    let requirement_lifecycle_transitions =
        collect_requirement_lifecycle_transitions(&requirements_before, &requirements_after);
    let task_state_transitions = collect_task_state_transitions(
        &tasks_before,
        &tasks,
        &workflows,
        &phase_execution_events,
        &ready_workflow_starts.started_workflows,
    );

    Ok(ProjectTickSummary {
        project_root: root,
        started_daemon,
        health,
        tasks_total,
        tasks_ready,
        tasks_in_progress,
        tasks_blocked,
        tasks_done,
        stale_in_progress_count: stale_in_progress.count,
        stale_in_progress_threshold_hours: stale_in_progress.threshold_hours,
        stale_in_progress_task_ids: stale_in_progress.task_ids(),
        workflows_running,
        workflows_completed,
        workflows_failed,
        resumed_workflows,
        cleaned_stale_workflows,
        reconciled_stale_tasks: reconciled_stale_tasks
            .saturating_add(reconciled_dependency_tasks)
            .saturating_add(reconciled_merge_tasks),
        started_ready_workflows: ready_workflow_starts.started,
        executed_workflow_phases,
        failed_workflow_phases,
        phase_execution_events,
        requirement_lifecycle_transitions,
        task_state_transitions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::phase_pool::{
        clear_running_workflow_phase_pool, drain_running_workflow_phases_for_project,
        pause_running_workflow_phase_spawns, resume_running_workflow_phase_spawns,
        subscribe_phase_completion_wake,
    };
    use super::reconciliation::{
        reconcile_dependency_gate_tasks_for_project,
        reconcile_stale_in_progress_tasks_for_project,
    };
    use super::task_dispatch::run_ready_task_workflows_for_project;
    use super::bootstrap::bootstrap_from_vision_if_needed;
    use orchestrator_core::Priority;
    use orchestrator_core::ServiceHub;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn daemon_agent_assignee_defaults_to_unknown_role_when_phase_metadata_missing() {
        let hub = orchestrator_core::InMemoryServiceHub::new();
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "phase-less-workflow-assignee".to_string(),
                description:
                    "ensure daemon assignment still resolves when workflow phase is absent"
                        .to_string(),
                task_type: Some(TaskType::Feature),
                priority: None,
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let mut phase_less_workflow = workflow;
        phase_less_workflow.current_phase = None;
        phase_less_workflow.phases.clear();
        phase_less_workflow.current_phase_index = 0;

        let project_root = TempDir::new().expect("temp dir should be created");
        let project_root = project_root.path().to_string_lossy().to_string();
        let (role, model) =
            daemon_agent_assignee_for_workflow_start(&project_root, &phase_less_workflow, &task);
        let runtime_config =
            orchestrator_core::load_agent_runtime_config_or_default(Path::new(&project_root));
        let expected_role = runtime_config
            .phase_agent_id("unknown")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "unknown".to_string());

        assert_eq!(role, expected_role);
        assert!(model
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()));
    }

    #[tokio::test]
    async fn run_ready_prefers_em_queue_and_marks_selected_entry_assigned() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let fallback_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-high-priority".to_string(),
                description: "should be skipped when queue has dispatchable item".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Critical),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        let queue_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-low-priority".to_string(),
                description: "should be selected from queue first".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Low),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("queue task should be created");
        hub.tasks()
            .set_status(&queue_task.id, TaskStatus::Ready)
            .await
            .expect("queue task should be ready");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![
                    EmWorkQueueEntry {
                        task_id: queue_task.id.clone(),
                        status: EmWorkQueueEntryStatus::Pending,
                        workflow_id: None,
                        assigned_at: None,
                    },
                    EmWorkQueueEntry {
                        task_id: fallback_task.id.clone(),
                        status: EmWorkQueueEntryStatus::Pending,
                        workflow_id: None,
                        assigned_at: None,
                    },
                ],
            },
        )
        .expect("queue state should be stored");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            1,
        )
        .await
        .expect("ready runner should succeed");

        assert_eq!(summary.started, 1);
        assert_eq!(summary.started_workflows.len(), 1);
        let started = &summary.started_workflows[0];
        assert_eq!(started.task_id, queue_task.id);
        assert_eq!(started.selection_source, TaskSelectionSource::EmQueue);

        let queue_state = load_em_work_queue_state(&project_root_str)
            .expect("queue should load")
            .expect("queue should exist");
        let queue_entry = queue_state
            .entries
            .iter()
            .find(|entry| entry.task_id == queue_task.id)
            .expect("queue task entry should remain present");
        assert_eq!(queue_entry.status, EmWorkQueueEntryStatus::Assigned);
        assert_eq!(
            queue_entry.workflow_id.as_deref(),
            Some(started.workflow_id.as_str())
        );

        let fallback_entry = queue_state
            .entries
            .iter()
            .find(|entry| entry.task_id == fallback_task.id)
            .expect("fallback queue entry should remain present");
        assert_eq!(fallback_entry.status, EmWorkQueueEntryStatus::Pending);
    }

    #[tokio::test]
    async fn run_ready_dispatches_multiple_tasks_from_em_queue_before_fallback_picker() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let queue_task_one = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-one".to_string(),
                description: "first queue entry should start first".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Low),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("first queue task should be created");
        hub.tasks()
            .set_status(&queue_task_one.id, TaskStatus::Ready)
            .await
            .expect("first queue task should be ready");

        let queue_task_two = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-two".to_string(),
                description: "second queue entry should start second".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Low),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("second queue task should be created");
        hub.tasks()
            .set_status(&queue_task_two.id, TaskStatus::Ready)
            .await
            .expect("second queue task should be ready");

        let fallback_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-not-selected".to_string(),
                description: "fallback picker should not run when queue yields tasks".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Critical),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![
                    EmWorkQueueEntry {
                        task_id: queue_task_one.id.clone(),
                        status: EmWorkQueueEntryStatus::Pending,
                        workflow_id: None,
                        assigned_at: None,
                    },
                    EmWorkQueueEntry {
                        task_id: queue_task_two.id.clone(),
                        status: EmWorkQueueEntryStatus::Pending,
                        workflow_id: None,
                        assigned_at: None,
                    },
                ],
            },
        )
        .expect("queue state should be stored");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            2,
        )
        .await
        .expect("ready runner should succeed");

        assert_eq!(summary.started, 2);
        assert_eq!(summary.started_workflows.len(), 2);
        assert_eq!(summary.started_workflows[0].task_id, queue_task_one.id);
        assert_eq!(summary.started_workflows[1].task_id, queue_task_two.id);
        assert_eq!(
            summary.started_workflows[0].selection_source,
            TaskSelectionSource::EmQueue
        );
        assert_eq!(
            summary.started_workflows[1].selection_source,
            TaskSelectionSource::EmQueue
        );
        assert!(!summary
            .started_workflows
            .iter()
            .any(|started| started.task_id == fallback_task.id));

        let queue_state = load_em_work_queue_state(&project_root_str)
            .expect("queue should load")
            .expect("queue should exist");
        for started in &summary.started_workflows {
            let queue_entry = queue_state
                .entries
                .iter()
                .find(|entry| entry.task_id == started.task_id)
                .expect("started queue entry should remain present");
            assert_eq!(queue_entry.status, EmWorkQueueEntryStatus::Assigned);
            assert_eq!(
                queue_entry.workflow_id.as_deref(),
                Some(started.workflow_id.as_str())
            );
        }
    }

    #[tokio::test]
    async fn run_ready_falls_back_when_queue_has_no_dispatchable_entries() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let queue_only_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-backlog".to_string(),
                description: "queue entry points at non-ready task".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("queue task should be created");

        let fallback_ready_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-ready".to_string(),
                description: "ready task should start via fallback picker".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_ready_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: queue_only_task.id.clone(),
                    status: EmWorkQueueEntryStatus::Pending,
                    workflow_id: None,
                    assigned_at: None,
                }],
            },
        )
        .expect("queue state should be stored");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            1,
        )
        .await
        .expect("ready runner should succeed");
        assert_eq!(summary.started, 1);
        assert_eq!(summary.started_workflows.len(), 1);
        let started = &summary.started_workflows[0];
        assert_eq!(started.task_id, fallback_ready_task.id);
        assert_eq!(
            started.selection_source,
            TaskSelectionSource::FallbackPicker
        );
    }

    #[tokio::test]
    async fn run_ready_falls_back_when_queue_state_is_invalid_json() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let fallback_ready_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-ready-invalid-queue".to_string(),
                description: "ready task should still run when queue decode fails".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_ready_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        let queue_path = em_work_queue_state_path(&project_root_str).expect("queue path");
        if let Some(parent) = queue_path.parent() {
            fs::create_dir_all(parent).expect("queue parent should be created");
        }
        fs::write(&queue_path, "{ invalid json").expect("invalid queue payload should be written");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            1,
        )
        .await
        .expect("ready runner should continue when queue state is invalid");
        assert_eq!(summary.started, 1);
        assert_eq!(summary.started_workflows.len(), 1);
        assert_eq!(
            summary.started_workflows[0].selection_source,
            TaskSelectionSource::FallbackPicker
        );
    }

    #[tokio::test]
    async fn sync_task_status_for_workflow_result_removes_assigned_queue_entries_on_completion() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-terminal-cleanup-completed".to_string(),
                description: "assigned queue entry should be removed after completion".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should be in-progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: task.id.clone(),
                    status: EmWorkQueueEntryStatus::Assigned,
                    workflow_id: Some(workflow.id.clone()),
                    assigned_at: Some(Utc::now().to_rfc3339()),
                }],
            },
        )
        .expect("queue state should be written");

        sync_task_status_for_workflow_result(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            task.id.as_str(),
            WorkflowStatus::Completed,
            Some(workflow.id.as_str()),
        )
        .await;

        let updated_task = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(updated_task.status, TaskStatus::Done);

        let queue_state =
            load_em_work_queue_state(&project_root_str).expect("queue should load after cleanup");
        assert!(
            queue_state.is_none()
                || queue_state
                    .as_ref()
                    .is_some_and(|state| state.entries.is_empty())
        );
    }

    #[tokio::test]
    async fn sync_task_status_for_workflow_result_removes_assigned_queue_entries_on_failure() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-terminal-cleanup".to_string(),
                description: "assigned queue entry should be removed after failure".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should be in-progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: task.id.clone(),
                    status: EmWorkQueueEntryStatus::Assigned,
                    workflow_id: Some(workflow.id.clone()),
                    assigned_at: Some(Utc::now().to_rfc3339()),
                }],
            },
        )
        .expect("queue state should be written");

        sync_task_status_for_workflow_result(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            task.id.as_str(),
            WorkflowStatus::Failed,
            Some(workflow.id.as_str()),
        )
        .await;

        let queue_state =
            load_em_work_queue_state(&project_root_str).expect("queue should load after cleanup");
        assert!(
            queue_state.is_none()
                || queue_state
                    .as_ref()
                    .is_some_and(|state| state.entries.is_empty())
        );
    }

    #[tokio::test]
    async fn reconcile_stale_in_progress_removes_assigned_queue_entries_for_failed_workflow() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "stale-failed-reconcile-queue-cleanup".to_string(),
                description: "failed stale reconciliation should remove queue assignment"
                    .to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should be in-progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        hub.workflows()
            .fail_current_phase(&workflow.id, "forced failure".to_string())
            .await
            .expect("workflow should fail");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: task.id.clone(),
                    status: EmWorkQueueEntryStatus::Assigned,
                    workflow_id: Some(workflow.id.clone()),
                    assigned_at: Some(Utc::now().to_rfc3339()),
                }],
            },
        )
        .expect("queue state should be written");

        let reconciled = reconcile_stale_in_progress_tasks_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
        )
        .await
        .expect("stale reconciliation should succeed");
        assert_eq!(reconciled, 1);

        let updated_task = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(updated_task.status, TaskStatus::Blocked);

        let queue_state =
            load_em_work_queue_state(&project_root_str).expect("queue should load after cleanup");
        assert!(
            queue_state.is_none()
                || queue_state
                    .as_ref()
                    .is_some_and(|state| state.entries.is_empty())
        );
    }

    #[tokio::test]
    async fn reconcile_stale_in_progress_removes_assigned_queue_entries_for_cancelled_workflow() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "stale-cancelled-reconcile-queue-cleanup".to_string(),
                description: "cancelled stale reconciliation should remove queue assignment"
                    .to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should be in-progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        hub.workflows()
            .cancel(&workflow.id)
            .await
            .expect("workflow should cancel");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: task.id.clone(),
                    status: EmWorkQueueEntryStatus::Assigned,
                    workflow_id: Some(workflow.id.clone()),
                    assigned_at: Some(Utc::now().to_rfc3339()),
                }],
            },
        )
        .expect("queue state should be written");

        let reconciled = reconcile_stale_in_progress_tasks_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
        )
        .await
        .expect("stale reconciliation should succeed");
        assert_eq!(reconciled, 1);

        let updated_task = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(updated_task.status, TaskStatus::Cancelled);

        let queue_state =
            load_em_work_queue_state(&project_root_str).expect("queue should load after cleanup");
        assert!(
            queue_state.is_none()
                || queue_state
                    .as_ref()
                    .is_some_and(|state| state.entries.is_empty())
        );
    }

    #[test]
    fn ready_task_dispatch_limit_honors_available_agent_capacity() {
        let uncapped = orchestrator_core::DaemonHealth {
            healthy: true,
            status: orchestrator_core::DaemonStatus::Running,
            runner_connected: true,
            runner_pid: None,
            active_agents: 1,
            max_agents: None,
            project_root: None,
            daemon_pid: None,
            process_alive: None,
            pool_size: None,
            pool_utilization_percent: None,
            queued_tasks: None,
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
        };
        assert_eq!(ready_task_dispatch_limit(4, &uncapped), 4);

        let capped = orchestrator_core::DaemonHealth {
            healthy: true,
            status: orchestrator_core::DaemonStatus::Running,
            runner_connected: true,
            runner_pid: None,
            active_agents: 3,
            max_agents: Some(5),
            project_root: None,
            daemon_pid: None,
            process_alive: None,
            pool_size: None,
            pool_utilization_percent: None,
            queued_tasks: None,
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
        };
        assert_eq!(ready_task_dispatch_limit(10, &capped), 2);
        assert_eq!(ready_task_dispatch_limit(1, &capped), 1);

        let saturated = orchestrator_core::DaemonHealth {
            healthy: true,
            status: orchestrator_core::DaemonStatus::Running,
            runner_connected: true,
            runner_pid: None,
            active_agents: 5,
            max_agents: Some(5),
            project_root: None,
            daemon_pid: None,
            process_alive: None,
            pool_size: None,
            pool_utilization_percent: None,
            queued_tasks: None,
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
        };
        assert_eq!(ready_task_dispatch_limit(3, &saturated), 0);
    }

    #[tokio::test]
    async fn execute_running_workflow_phases_processes_completions_when_spawn_limit_is_zero() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "completion-processing-zero-spawn-limit".to_string(),
                description: "completion queue should still be drained".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");
        let phase_id = workflow
            .current_phase
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            state.in_flight_workflow_ids.insert(workflow.id.clone());
            state
                .completion_tx
                .send(ReactivePhaseCompletion {
                    workflow: workflow.clone(),
                    task: task.clone(),
                    phase_id: phase_id.clone(),
                    spawned_at: std::time::Instant::now(),
                    run_result: Ok(PhaseExecutionRunResult {
                        outcome: PhaseExecutionOutcome::ManualPending {
                            instructions: "manual approval required".to_string(),
                            approval_note_required: false,
                        },
                        metadata: PhaseExecutionMetadata {
                            phase_id,
                            phase_mode: "manual".to_string(),
                            phase_definition_hash: "test".to_string(),
                            agent_runtime_config_hash: "test".to_string(),
                            agent_runtime_schema:
                                orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID
                                    .to_string(),
                            agent_runtime_version:
                                orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
                            agent_runtime_source: "test".to_string(),
                            agent_id: None,
                            agent_profile_hash: None,
                            selected_tool: None,
                            selected_model: None,
                        },
                        signals: Vec::new(),
                    }),
                })
                .expect("completion should enqueue");
        });

        let (executed, failed, events) = execute_running_workflow_phases_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            0,
        )
        .await
        .expect("completion processing should succeed");

        assert_eq!(executed, 0);
        assert_eq!(failed, 0);
        assert!(events.is_empty());
        assert!(
            !phase_pool::has_running_workflow_phase_pool_activity(&project_root_str),
            "in-flight marker should be cleared after completion processing"
        );

        clear_running_workflow_phase_pool(&project_root_str);
    }

    #[test]
    fn parse_merge_conflict_recovery_response_parses_json_line_output() {
        let transcript = r#"
thinking...
{"kind":"merge_conflict_resolution_result","status":"resolved","commit_message":"Resolve merge conflict","reason":""}
"#;
        let parsed = parse_merge_conflict_recovery_response(transcript)
            .expect("response should parse from transcript JSON line");
        assert_eq!(parsed.kind, "merge_conflict_resolution_result");
        assert_eq!(parsed.status, "resolved");
        assert_eq!(parsed.commit_message, "Resolve merge conflict");
    }

    #[test]
    fn parse_merge_conflict_recovery_response_rejects_non_json_output() {
        assert!(
            parse_merge_conflict_recovery_response("merge fixed, please continue").is_none(),
            "non-json output should not parse as recovery response"
        );
    }

    #[test]
    fn parse_merge_conflict_recovery_response_uses_latest_valid_payload() {
        let transcript = r#"
{"kind":"merge_conflict_resolution_result","status":"resolved|failed","commit_message":"placeholder","reason":""}
{"kind":"merge_conflict_resolution_result","status":"resolved","commit_message":"Resolve real conflict","reason":""}
"#;
        let parsed = parse_merge_conflict_recovery_response(transcript)
            .expect("response should parse from latest valid JSON line");
        assert_eq!(parsed.status, "resolved");
        assert_eq!(parsed.commit_message, "Resolve real conflict");
    }

    #[test]
    fn parse_merge_conflict_recovery_response_rejects_wrong_kind() {
        let transcript = r#"
{"kind":"phase_result","status":"resolved","commit_message":"not merge conflict result","reason":""}
"#;
        assert!(
            parse_merge_conflict_recovery_response(transcript).is_none(),
            "wrong kind should not parse as merge conflict recovery response"
        );
    }

    #[test]
    fn parse_merge_conflict_recovery_response_rejects_invalid_status_only_payload() {
        let transcript = r#"
{"kind":"merge_conflict_resolution_result","status":"resolved|failed","commit_message":"placeholder","reason":""}
"#;
        assert!(
            parse_merge_conflict_recovery_response(transcript).is_none(),
            "status placeholders should not be treated as valid recovery responses"
        );
    }

    #[test]
    fn pool_concurrency_limits_to_max_phases_per_tick() {
        let project_root = "test-pool-concurrency-limits";
        let pool_size = 3;
        clear_running_workflow_phase_pool(project_root);

        for i in 0..pool_size {
            with_reactive_phase_pool_state_mut(project_root, |state| {
                state
                    .in_flight_workflow_ids
                    .insert(format!("concurrency-wf-{}", i));
            });
        }

        let active_count = with_reactive_phase_pool_state_mut(project_root, |state| {
            state.in_flight_workflow_ids.len()
        });
        assert_eq!(
            active_count, pool_size,
            "pool should track exactly pool_size in-flight workflows"
        );
        assert!(
            phase_pool::has_running_workflow_phase_pool_activity(project_root),
            "pool should report activity when workflows are in-flight"
        );

        clear_running_workflow_phase_pool(project_root);
    }

    #[tokio::test]
    async fn pool_blocks_spawn_when_full() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();
        let pool_size = 2;

        for i in 0..pool_size {
            let task = hub
                .tasks()
                .create(TaskCreateInput {
                    title: format!("full-pool-task-{}", i),
                    description: "test task".to_string(),
                    task_type: Some(TaskType::Feature),
                    priority: Some(Priority::Medium),
                    created_by: Some("test".to_string()),
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: Vec::new(),
                })
                .await
                .expect("task should be created");
            let workflow = hub
                .workflows()
                .run(WorkflowRunInput {
                    task_id: task.id.clone(),
                    pipeline_id: None,
                })
                .await
                .expect("workflow should start");

            with_reactive_phase_pool_state_mut(&project_root_str, |state| {
                state.in_flight_workflow_ids.insert(workflow.id.clone());
            });
        }

        let extra_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "extra-task-should-wait".to_string(),
                description: "should not spawn when pool full".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let _extra_workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: extra_task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let (executed, failed, _) = execute_running_workflow_phases_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            pool_size,
        )
        .await
        .expect("execution should succeed");

        assert_eq!(executed, 0, "should not spawn when full");
        assert_eq!(failed, 0, "should have no failures");

        clear_running_workflow_phase_pool(&project_root_str);
    }

    #[tokio::test]
    async fn immediate_backfill_on_completion() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();
        let pool_size = 2;

        let task1 = hub
            .tasks()
            .create(TaskCreateInput {
                title: "backfill-task-1".to_string(),
                description: "first task".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow1 = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task1.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let task2 = hub
            .tasks()
            .create(TaskCreateInput {
                title: "backfill-task-2".to_string(),
                description: "second task".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow2 = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task2.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let task3 = hub
            .tasks()
            .create(TaskCreateInput {
                title: "backfill-task-3".to_string(),
                description: "third task - should backfill".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let _workflow3 = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task3.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            state.in_flight_workflow_ids.insert(workflow1.id.clone());
            state.in_flight_workflow_ids.insert(workflow2.id.clone());
        });

        let phase_id = workflow1
            .current_phase
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            let _ = state.completion_tx.send(ReactivePhaseCompletion {
                workflow: workflow1.clone(),
                task: task1.clone(),
                phase_id: phase_id.clone(),
                spawned_at: std::time::Instant::now(),
                run_result: Ok(PhaseExecutionRunResult {
                    outcome: PhaseExecutionOutcome::Completed {
                        commit_message: None,
                        phase_decision: None,
                    },
                    metadata: PhaseExecutionMetadata {
                        phase_id: phase_id.clone(),
                        phase_mode: "agent".to_string(),
                        phase_definition_hash: "test".to_string(),
                        agent_runtime_config_hash: "test".to_string(),
                        agent_runtime_schema: orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID.to_string(),
                        agent_runtime_version: orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
                        agent_runtime_source: "test".to_string(),
                        agent_id: None,
                        agent_profile_hash: None,
                        selected_tool: None,
                        selected_model: None,
                    },
                    signals: Vec::new(),
                }),
            });
        });

        let (executed, _failed, _) = execute_running_workflow_phases_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            pool_size,
        )
        .await
        .expect("execution should succeed");

        assert!(
            executed >= 1,
            "should process completion and backfill pool slot (got {} processed completions)",
            executed
        );

        clear_running_workflow_phase_pool(&project_root_str);
    }

    #[tokio::test]
    async fn priority_ordering_high_first() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let low_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "low-priority-task".to_string(),
                description: "low priority".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Low),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&low_task.id, TaskStatus::Ready)
            .await
            .expect("task should be ready");

        let high_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "high-priority-task".to_string(),
                description: "high priority".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&high_task.id, TaskStatus::Ready)
            .await
            .expect("task should be ready");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            2,
        )
        .await
        .expect("ready runner should succeed");

        assert_eq!(summary.started, 2, "should start both tasks");
        assert_eq!(
            summary.started_workflows[0].task_id, high_task.id,
            "high priority should start first"
        );
    }

    #[tokio::test]
    async fn graceful_drain_prevents_new_spawns() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "drain-test-task".to_string(),
                description: "task for drain test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let _workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        pause_running_workflow_phase_spawns(&project_root_str);

        let allow_spawns = with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            state.allow_spawns
        });
        assert!(
            !allow_spawns,
            "spawns should be blocked after pause"
        );

        resume_running_workflow_phase_spawns(&project_root_str);

        let allow_spawns = with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            state.allow_spawns
        });
        assert!(allow_spawns, "spawns should be allowed after resume");

        clear_running_workflow_phase_pool(&project_root_str);
    }

    #[tokio::test]
    async fn graceful_drain_completes_running() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "drain-running-task".to_string(),
                description: "running task for drain".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            state.in_flight_workflow_ids.insert(workflow.id.clone());
        });

        let has_before = phase_pool::has_running_workflow_phase_pool_activity(&project_root_str);
        assert!(has_before, "should have running activity before drain");

        let phase_id = workflow
            .current_phase
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        with_reactive_phase_pool_state_mut(&project_root_str, |state| {
            let _ = state.completion_tx.send(ReactivePhaseCompletion {
                workflow: workflow.clone(),
                task: task.clone(),
                phase_id: phase_id.clone(),
                spawned_at: std::time::Instant::now(),
                run_result: Ok(PhaseExecutionRunResult {
                    outcome: PhaseExecutionOutcome::Completed {
                        commit_message: None,
                        phase_decision: None,
                    },
                    metadata: PhaseExecutionMetadata {
                        phase_id,
                        phase_mode: "agent".to_string(),
                        phase_definition_hash: "test".to_string(),
                        agent_runtime_config_hash: "test".to_string(),
                        agent_runtime_schema: orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID.to_string(),
                        agent_runtime_version: orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
                        agent_runtime_source: "test".to_string(),
                        agent_id: None,
                        agent_profile_hash: None,
                        selected_tool: None,
                        selected_model: None,
                    },
                    signals: Vec::new(),
                }),
            });
        });

        drain_running_workflow_phases_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            5,
        )
        .await
        .expect("drain should succeed");

        let has_after = phase_pool::has_running_workflow_phase_pool_activity(&project_root_str);
        assert!(
            !has_after,
            "should have no running activity after drain completes"
        );
    }

    #[test]
    fn pool_metrics_active_count() {
        let project_root = "test-metrics-project";
        clear_running_workflow_phase_pool(project_root);

        with_reactive_phase_pool_state_mut(project_root, |state| {
            state.in_flight_workflow_ids.insert("wf-1".to_string());
            state.in_flight_workflow_ids.insert("wf-2".to_string());
            state.in_flight_workflow_ids.insert("wf-3".to_string());
        });

        let has_activity = phase_pool::has_running_workflow_phase_pool_activity(project_root);
        assert!(has_activity, "should detect active workflows");

        let active_count = with_reactive_phase_pool_state_mut(project_root, |state| {
            state.in_flight_workflow_ids.len()
        });
        assert_eq!(active_count, 3, "should track 3 in-flight workflows");

        clear_running_workflow_phase_pool(project_root);
    }

    #[test]
    fn pool_metrics_utilization() {
        let project_root = "test-utilization-project";
        let pool_size = 5;
        clear_running_workflow_phase_pool(project_root);

        with_reactive_phase_pool_state_mut(project_root, |state| {
            state.in_flight_workflow_ids.insert("wf-1".to_string());
            state.in_flight_workflow_ids.insert("wf-2".to_string());
            state.in_flight_workflow_ids.insert("wf-3".to_string());
        });

        let active_count = with_reactive_phase_pool_state_mut(project_root, |state| {
            state.in_flight_workflow_ids.len()
        });
        let utilization = active_count as f64 / pool_size as f64;
        assert!(
            (utilization - 0.6).abs() < 0.01,
            "utilization should be 0.6 (3/5)"
        );

        clear_running_workflow_phase_pool(project_root);
    }
}
