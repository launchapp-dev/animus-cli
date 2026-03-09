use super::reconciliation_test_support::reconcile_stale_in_progress_tasks_for_project;
use super::task_dispatch::run_ready_task_workflows_for_project;
use super::*;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::reconcile_manual_phase_timeouts;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use chrono::Utc;
use orchestrator_core::ServiceHub;
use orchestrator_core::{
    FileServiceHub, PhaseExecutionMode, PhaseManualDefinition, Priority, TaskCreateInput,
    TaskStatus, TaskType, WorkflowRunInput, WorkflowStatus,
};
use orchestrator_daemon_runtime::CompletedProcess;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use workflow_runner::executor::parse_merge_conflict_recovery_response;

use protocol::test_utils::EnvVarGuard;

#[tokio::test]
async fn daemon_agent_assignee_defaults_to_unknown_role_when_phase_metadata_missing() {
    let hub = orchestrator_core::InMemoryServiceHub::new();
    let task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "phase-less-workflow-assignee".to_string(),
            description: "ensure daemon assignment still resolves when workflow phase is absent"
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
        .run(WorkflowRunInput::for_task(task.id.clone(), None))
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
async fn run_ready_prefers_dispatch_queue_and_marks_selected_entry_assigned() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&fallback_task.id, TaskStatus::Ready, false)
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
        .set_status(&queue_task.id, TaskStatus::Ready, false)
        .await
        .expect("queue task should be ready");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![
                DispatchQueueEntry {
                    subject_id: None,
                    task_id: queue_task.id.clone(),
                    dispatch: None,
                    status: DispatchQueueEntryStatus::Pending,
                    workflow_id: None,
                    assigned_at: None,
                    held_at: None,
                },
                DispatchQueueEntry {
                    subject_id: None,
                    task_id: fallback_task.id.clone(),
                    dispatch: None,
                    status: DispatchQueueEntryStatus::Pending,
                    workflow_id: None,
                    assigned_at: None,
                    held_at: None,
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
    assert_eq!(started.task_id(), Some(queue_task.id.as_str()));
    assert_eq!(
        started.selection_source,
        DispatchSelectionSource::DispatchQueue
    );

    let queue_state = load_dispatch_queue_state(&project_root_str)
        .expect("queue should load")
        .expect("queue should exist");
    let queue_entry = queue_state
        .entries
        .iter()
        .find(|entry| entry.task_id == queue_task.id)
        .expect("queue task entry should remain present");
    assert_eq!(queue_entry.status, DispatchQueueEntryStatus::Assigned);
    assert_eq!(
        queue_entry.workflow_id.as_deref(),
        started.workflow_id.as_deref()
    );

    let fallback_entry = queue_state
        .entries
        .iter()
        .find(|entry| entry.task_id == fallback_task.id)
        .expect("fallback queue entry should remain present");
    assert_eq!(fallback_entry.status, DispatchQueueEntryStatus::Pending);
}

#[tokio::test]
async fn run_ready_dispatches_custom_subjects_from_queue() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
    let home = TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

    let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
    let project_root = TempDir::new().expect("project temp dir");
    let project_root_str = project_root.path().to_string_lossy().to_string();

    let dispatch = SubjectDispatch::for_custom(
        "custom-dispatch",
        "non-task workflow",
        orchestrator_core::STANDARD_WORKFLOW_REF,
        Some(json!({"scope":"non-task"})),
        "manual-queue-enqueue",
    );

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry::from_dispatch(dispatch.clone())],
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
    assert_eq!(started.task_id(), None);
    assert_eq!(started.subject_id(), "custom-dispatch");
    assert!(started.workflow_id.is_some());
    assert_eq!(
        started.selection_source,
        DispatchSelectionSource::DispatchQueue
    );

    let queue_state = load_dispatch_queue_state(&project_root_str)
        .expect("queue should load")
        .expect("queue should exist");
    let queue_entry = queue_state
        .entries
        .iter()
        .find(|entry| entry.subject_id() == "custom-dispatch")
        .expect("custom queue entry should remain present");
    assert_eq!(queue_entry.status, DispatchQueueEntryStatus::Assigned);
    assert_eq!(queue_entry.workflow_id.as_deref(), started.workflow_id.as_deref());
}

#[tokio::test]
async fn run_ready_uses_fallback_headroom_after_dispatching_queue_entries() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
    let home = TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

    let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
    let project_root = TempDir::new().expect("project temp dir");
    let project_root_str = project_root.path().to_string_lossy().to_string();

    let queue_task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "queue-first".to_string(),
            description: "queue task should consume the first slot".to_string(),
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
        .set_status(&queue_task.id, TaskStatus::Ready, false)
        .await
        .expect("queue task should be ready");

    let fallback_task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "fallback-second".to_string(),
            description: "fallback task should use the spare slot".to_string(),
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
        .set_status(&fallback_task.id, TaskStatus::Ready, false)
        .await
        .expect("fallback task should be ready");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry {
                subject_id: None,
                task_id: queue_task.id.clone(),
                dispatch: None,
                status: DispatchQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
            }],
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
    assert_eq!(
        summary.started_workflows[0].task_id(),
        Some(queue_task.id.as_str())
    );
    assert_eq!(
        summary.started_workflows[1].task_id(),
        Some(fallback_task.id.as_str())
    );
    assert_eq!(
        summary.started_workflows[0].selection_source,
        DispatchSelectionSource::DispatchQueue
    );
    assert_eq!(
        summary.started_workflows[1].selection_source,
        DispatchSelectionSource::FallbackPicker
    );

    let queue_state = load_dispatch_queue_state(&project_root_str)
        .expect("queue should load")
        .expect("queue should exist");
    let queue_entry = queue_state
        .entries
        .iter()
        .find(|entry| entry.task_id == queue_task.id)
        .expect("queue task entry should remain present");
    assert_eq!(queue_entry.status, DispatchQueueEntryStatus::Assigned);
    assert_eq!(
        queue_entry.workflow_id.as_deref(),
        summary.started_workflows[0].workflow_id.as_deref()
    );
}

#[tokio::test]
async fn run_ready_dispatches_multiple_tasks_from_dispatch_queue_before_fallback_picker() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&queue_task_one.id, TaskStatus::Ready, false)
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
        .set_status(&queue_task_two.id, TaskStatus::Ready, false)
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
        .set_status(&fallback_task.id, TaskStatus::Ready, false)
        .await
        .expect("fallback task should be ready");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![
                DispatchQueueEntry {
                    subject_id: None,
                    task_id: queue_task_one.id.clone(),
                    dispatch: None,
                    status: DispatchQueueEntryStatus::Pending,
                    workflow_id: None,
                    assigned_at: None,
                    held_at: None,
                },
                DispatchQueueEntry {
                    subject_id: None,
                    task_id: queue_task_two.id.clone(),
                    dispatch: None,
                    status: DispatchQueueEntryStatus::Pending,
                    workflow_id: None,
                    assigned_at: None,
                    held_at: None,
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
    assert_eq!(
        summary.started_workflows[0].task_id(),
        Some(queue_task_one.id.as_str())
    );
    assert_eq!(
        summary.started_workflows[1].task_id(),
        Some(queue_task_two.id.as_str())
    );
    assert_eq!(
        summary.started_workflows[0].selection_source,
        DispatchSelectionSource::DispatchQueue
    );
    assert_eq!(
        summary.started_workflows[1].selection_source,
        DispatchSelectionSource::DispatchQueue
    );
    assert!(!summary
        .started_workflows
        .iter()
        .any(|started| started.task_id() == Some(fallback_task.id.as_str())));

    let queue_state = load_dispatch_queue_state(&project_root_str)
        .expect("queue should load")
        .expect("queue should exist");
    for started in &summary.started_workflows {
        let queue_entry = queue_state
            .entries
            .iter()
            .find(|entry| started.task_id() == Some(entry.task_id.as_str()))
            .expect("started queue entry should remain present");
        assert_eq!(queue_entry.status, DispatchQueueEntryStatus::Assigned);
        assert_eq!(
            queue_entry.workflow_id.as_deref(),
            started.workflow_id.as_deref()
        );
    }
}

#[tokio::test]
async fn run_ready_falls_back_when_queue_has_no_dispatchable_entries() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&fallback_ready_task.id, TaskStatus::Ready, false)
        .await
        .expect("fallback task should be ready");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry {
                subject_id: None,
                task_id: queue_only_task.id.clone(),
                dispatch: None,
                status: DispatchQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
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
    assert_eq!(started.task_id(), Some(fallback_ready_task.id.as_str()));
    assert_eq!(
        started.selection_source,
        DispatchSelectionSource::FallbackPicker
    );
}

#[tokio::test]
async fn run_ready_falls_back_when_queue_state_is_invalid_json() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&fallback_ready_task.id, TaskStatus::Ready, false)
        .await
        .expect("fallback task should be ready");

    let queue_path = dispatch_queue_state_path(&project_root_str).expect("queue path");
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
        DispatchSelectionSource::FallbackPicker
    );
}

#[tokio::test]
async fn reconcile_completed_processes_removes_assigned_queue_entries_on_completion() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&task.id, TaskStatus::InProgress, false)
        .await
        .expect("task should be in-progress");

    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(task.id.clone(), None))
        .await
        .expect("workflow should start");
    let dispatch = SubjectDispatch::for_task(task.id.clone(), "standard");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry {
                subject_id: None,
                task_id: task.id.clone(),
                dispatch: Some(dispatch.clone()),
                status: DispatchQueueEntryStatus::Assigned,
                workflow_id: Some(workflow.id.clone()),
                assigned_at: Some(Utc::now().to_rfc3339()),
                held_at: None,
            }],
        },
    )
    .expect("queue state should be written");

    reconcile_completed_processes(
        hub.clone() as Arc<dyn ServiceHub>,
        &project_root_str,
        vec![CompletedProcess {
            subject_id: dispatch.subject_id().to_string(),
            task_id: Some(task.id.clone()),
            workflow_id: Some(workflow.id.clone()),
            workflow_ref: Some(dispatch.workflow_ref.clone()),
            workflow_status: Some(WorkflowStatus::Completed),
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            events: Vec::new(),
        }],
    )
    .await;

    let updated_task = hub.tasks().get(&task.id).await.expect("task should load");
    assert_eq!(updated_task.status, TaskStatus::Done);

    let queue_state =
        load_dispatch_queue_state(&project_root_str).expect("queue should load after cleanup");
    assert!(
        queue_state.is_none()
            || queue_state
                .as_ref()
                .is_some_and(|state| state.entries.is_empty())
    );
}

#[tokio::test]
async fn reconcile_completed_processes_removes_assigned_queue_entries_on_failure() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&task.id, TaskStatus::InProgress, false)
        .await
        .expect("task should be in-progress");

    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(task.id.clone(), None))
        .await
        .expect("workflow should start");
    let dispatch = SubjectDispatch::for_task(task.id.clone(), "standard");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry {
                subject_id: None,
                task_id: task.id.clone(),
                dispatch: Some(dispatch.clone()),
                status: DispatchQueueEntryStatus::Assigned,
                workflow_id: Some(workflow.id.clone()),
                assigned_at: Some(Utc::now().to_rfc3339()),
                held_at: None,
            }],
        },
    )
    .expect("queue state should be written");

    reconcile_completed_processes(
        hub.clone() as Arc<dyn ServiceHub>,
        &project_root_str,
        vec![CompletedProcess {
            subject_id: dispatch.subject_id().to_string(),
            task_id: Some(task.id.clone()),
            workflow_id: Some(workflow.id.clone()),
            workflow_ref: Some(dispatch.workflow_ref.clone()),
            workflow_status: Some(WorkflowStatus::Failed),
            schedule_id: None,
            exit_code: Some(1),
            success: false,
            failure_reason: None,
            events: Vec::new(),
        }],
    )
    .await;

    let queue_state =
        load_dispatch_queue_state(&project_root_str).expect("queue should load after cleanup");
    assert!(
        queue_state.is_none()
            || queue_state
                .as_ref()
                .is_some_and(|state| state.entries.is_empty())
    );
}

#[tokio::test]
async fn reconcile_stale_in_progress_removes_assigned_queue_entries_for_failed_workflow() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
    let home = TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

    let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
    let project_root = TempDir::new().expect("project temp dir");
    let project_root_str = project_root.path().to_string_lossy().to_string();

    let task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "stale-failed-reconcile-queue-cleanup".to_string(),
            description: "failed stale reconciliation should remove queue assignment".to_string(),
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
        .set_status(&task.id, TaskStatus::InProgress, false)
        .await
        .expect("task should be in-progress");

    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(task.id.clone(), None))
        .await
        .expect("workflow should start");

    hub.workflows()
        .fail_current_phase(&workflow.id, "forced failure".to_string())
        .await
        .expect("workflow should fail");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry {
                subject_id: None,
                task_id: task.id.clone(),
                dispatch: None,
                status: DispatchQueueEntryStatus::Assigned,
                workflow_id: Some(workflow.id.clone()),
                assigned_at: Some(Utc::now().to_rfc3339()),
                held_at: None,
            }],
        },
    )
    .expect("queue state should be written");

    let reconciled = reconcile_stale_in_progress_tasks_for_project(
        hub.clone() as Arc<dyn ServiceHub>,
        &project_root_str,
        24,
    )
    .await
    .expect("stale reconciliation should succeed");
    assert_eq!(reconciled, 1);

    let updated_task = hub.tasks().get(&task.id).await.expect("task should load");
    assert_eq!(updated_task.status, TaskStatus::Blocked);

    let queue_state =
        load_dispatch_queue_state(&project_root_str).expect("queue should load after cleanup");
    assert!(
        queue_state.is_none()
            || queue_state
                .as_ref()
                .is_some_and(|state| state.entries.is_empty())
    );
}

#[tokio::test]
async fn reconcile_stale_in_progress_removes_assigned_queue_entries_for_cancelled_workflow() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
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
        .set_status(&task.id, TaskStatus::InProgress, false)
        .await
        .expect("task should be in-progress");

    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(task.id.clone(), None))
        .await
        .expect("workflow should start");

    hub.workflows()
        .cancel(&workflow.id)
        .await
        .expect("workflow should cancel");

    save_dispatch_queue_state(
        &project_root_str,
        &DispatchQueueState {
            entries: vec![DispatchQueueEntry {
                subject_id: None,
                task_id: task.id.clone(),
                dispatch: None,
                status: DispatchQueueEntryStatus::Assigned,
                workflow_id: Some(workflow.id.clone()),
                assigned_at: Some(Utc::now().to_rfc3339()),
                held_at: None,
            }],
        },
    )
    .expect("queue state should be written");

    let reconciled = reconcile_stale_in_progress_tasks_for_project(
        hub.clone() as Arc<dyn ServiceHub>,
        &project_root_str,
        24,
    )
    .await
    .expect("stale reconciliation should succeed");
    assert_eq!(reconciled, 1);

    let updated_task = hub.tasks().get(&task.id).await.expect("task should load");
    assert_eq!(updated_task.status, TaskStatus::Cancelled);

    let queue_state =
        load_dispatch_queue_state(&project_root_str).expect("queue should load after cleanup");
    assert!(
        queue_state.is_none()
            || queue_state
                .as_ref()
                .is_some_and(|state| state.entries.is_empty())
    );
}

#[test]
fn ready_dispatch_limit_honors_available_agent_capacity() {
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
    assert_eq!(
        orchestrator_daemon_runtime::ready_dispatch_limit(4, &uncapped),
        4
    );

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
    assert_eq!(
        orchestrator_daemon_runtime::ready_dispatch_limit(10, &capped),
        2
    );
    assert_eq!(
        orchestrator_daemon_runtime::ready_dispatch_limit(1, &capped),
        1
    );

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
    assert_eq!(
        orchestrator_daemon_runtime::ready_dispatch_limit(3, &saturated),
        0
    );
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
        .set_status(&low_task.id, TaskStatus::Ready, false)
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
        .set_status(&high_task.id, TaskStatus::Ready, false)
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
        summary.started_workflows[0].task_id(),
        Some(high_task.id.as_str()),
        "high priority should start first"
    );
}

#[tokio::test]
async fn reconcile_manual_phase_timeouts_fails_workflow() {
    let temp = TempDir::new().expect("temp dir should be created");
    let project_root = temp.path().to_string_lossy().to_string();
    let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

    let task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "manual timeout".to_string(),
            description: "manual phase timeout".to_string(),
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
        .set_status(&task.id, TaskStatus::InProgress, false)
        .await
        .expect("task should be in progress");

    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(task.id.clone(), None))
        .await
        .expect("workflow should start");
    let current_phase = workflow
        .current_phase
        .clone()
        .expect("workflow should have current phase");

    let mut runtime = orchestrator_core::load_agent_runtime_config(temp.path())
        .expect("runtime config should load");
    let mut definition = runtime
        .phase_execution(&current_phase)
        .cloned()
        .expect("current phase should exist");
    definition.mode = PhaseExecutionMode::Manual;
    definition.agent_id = None;
    definition.command = None;
    definition.manual = Some(PhaseManualDefinition {
        instructions: "Approve or reject".to_string(),
        approval_note_required: false,
        timeout_secs: Some(1),
    });
    runtime.phases.insert(current_phase.clone(), definition);
    orchestrator_core::write_agent_runtime_config(temp.path(), &runtime)
        .expect("runtime config should write");

    let paused = hub
        .workflows()
        .pause(&workflow.id)
        .await
        .expect("workflow should pause");
    assert_eq!(paused.status, WorkflowStatus::Paused);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let reconciled =
        reconcile_manual_phase_timeouts(hub.clone() as Arc<dyn ServiceHub>, &project_root)
            .await
            .expect("manual timeout reconciliation should succeed");
    assert_eq!(reconciled, 1);

    let updated = hub
        .workflows()
        .get(&workflow.id)
        .await
        .expect("workflow should reload");
    assert_eq!(updated.status, WorkflowStatus::Failed);
}
