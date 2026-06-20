use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use anyhow::Result;
use orchestrator_core::{
    active_workflow_runner_ids, dispatch_workflow_event, load_agent_runtime_config_or_default, services::ServiceHub,
    WorkflowEvent, WorkflowMachineState, WorkflowStatus,
};
use std::collections::HashSet;
use std::path::Path;
use tracing::{error, warn};

/// Grace period after a workflow's `started_at` before the orphan
/// reconciler is allowed to cancel it. Async dispatch paths (control-wire
/// `workflow/run`, CLI `workflow run` without `--sync`) create a Running
/// workflow record before any executor has a chance to register a pid
/// file. Cancelling those within the same tick wipes the user's intent
/// before the scheduler picks them up.
pub(crate) const ORPHAN_RECONCILIATION_GRACE_SECS: i64 = 90;

pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to list workflows for orphan recovery"
            );
            return 0;
        }
    };
    let externally_active_workflows = match active_workflow_runner_ids(Path::new(project_root)) {
        Ok(ids) => ids,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to read active workflow runner ids"
            );
            HashSet::new()
        }
    };
    let now = chrono::Utc::now();

    let mut recovered = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == WorkflowMachineState::MergeConflict {
            continue;
        }
        if workflow_is_waiting_on_manual_phase(project_root, &workflow) {
            continue;
        }
        if active_subject_ids.contains(&workflow.id)
            || externally_active_workflows.contains(&workflow.id)
            || active_subject_ids.contains(workflow.subject.id())
        {
            continue;
        }
        if (now - workflow.started_at).num_seconds() < ORPHAN_RECONCILIATION_GRACE_SECS {
            continue;
        }

        warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %workflow.id,
            subject_id = %workflow.subject.id(),
            task_id = %workflow.task_id,
            "recovering orphaned running workflow"
        );
        let cancelled = cancel_orphaned_running_workflow(hub.clone(), project_root, &workflow).await;
        if cancelled {
            recovered = recovered.saturating_add(1);
        } else {
            error!(
                actor = protocol::ACTOR_DAEMON,
                workflow_id = %workflow.id,
                "failed to cancel orphaned workflow"
            );
        }
    }

    recovered
}

pub async fn reconcile_manual_phase_timeouts(hub: Arc<dyn ServiceHub>, project_root: &str) -> Result<usize> {
    let runtime = load_agent_runtime_config_or_default(Path::new(project_root));
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to list workflows for manual phase timeout reconciliation"
            );
            return Ok(0);
        }
    };
    let mut reconciled = 0usize;
    let now = chrono::Utc::now();

    for workflow in workflows {
        if workflow.status != WorkflowStatus::Paused {
            continue;
        }

        let phase_id = workflow
            .current_phase
            .clone()
            .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
            .unwrap_or_default();
        if phase_id.is_empty() {
            continue;
        }

        let definition = match runtime.phase_execution(&phase_id) {
            Some(definition) => definition,
            None => continue,
        };
        if !matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual) {
            continue;
        }
        let manual = match definition.manual.as_ref() {
            Some(manual) => manual,
            None => continue,
        };
        let timeout_secs = match manual.timeout_secs {
            Some(timeout_secs) => timeout_secs,
            None => continue,
        };
        if timeout_secs == 0 {
            continue;
        }

        let started_at = workflow
            .phases
            .get(workflow.current_phase_index)
            .and_then(|phase| phase.started_at)
            .or(Some(workflow.started_at));
        let Some(started_at) = started_at else {
            continue;
        };
        let elapsed = now.signed_duration_since(started_at).num_seconds().max(0) as u64;
        if elapsed < timeout_secs {
            continue;
        }

        let reason = format!("manual phase '{}' timed out after {} seconds", phase_id, timeout_secs);
        let outcome = dispatch_workflow_event(
            hub.clone(),
            project_root,
            WorkflowEvent::RejectManualPhase {
                workflow_id: workflow.id.clone(),
                phase_id: phase_id.clone(),
                note: Some(reason.clone()),
            },
        )
        .await?;
        if let Some(updated) = outcome.workflow {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                updated.subject.id(),
                Some(updated.task_id.as_str()),
                updated.workflow_ref.as_deref(),
                Some(updated.id.as_str()),
                updated.status,
                updated.failure_reason.as_deref(),
            )
            .await;
        }
        reconciled = reconciled.saturating_add(1);
    }

    Ok(reconciled)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::recover_orphaned_running_workflows;
    use crate::shared::test_env_lock;
    use orchestrator_core::{
        register_workflow_runner_pid, services::ServiceHub, unregister_workflow_runner_pid, FileServiceHub, Priority,
        TaskCreateInput, TaskType, WorkflowRunInput, WorkflowStateManager, WorkflowStatus,
    };
    use protocol::test_utils::EnvVarGuard;
    use std::collections::HashSet;
    use std::process::Command as ProcessCommand;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn init_git_repo(temp: &TempDir) {
        let init = ProcessCommand::new("git")
            .args(["init", "-b", "main"])
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        assert!(init.success(), "git init should succeed");
        for args in [
            ["config", "user.email", "ao-test@example.com"].as_slice(),
            ["config", "user.name", "Animus Test"].as_slice(),
        ] {
            let status =
                ProcessCommand::new("git").args(args).current_dir(temp.path()).status().expect("git config should run");
            assert!(status.success(), "git config should succeed");
        }
        std::fs::write(temp.path().join("README.md"), "# test\n").expect("readme should be written");
        let add =
            ProcessCommand::new("git").args(["add", "README.md"]).current_dir(temp.path()).status().expect("git add");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "initial commit should succeed");
    }

    #[tokio::test]
    async fn registered_runner_pid_shields_old_running_workflow_from_orphan_cancel() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        // v0.6: the kernel sources its base workflow config from a config_source
        // plugin; in tests, stand in for it after the hub scaffolds .animus/.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "resumed workflow".to_string(),
                description: "orphan reconciler liveness test".to_string(),
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
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");

        // Simulate a resumed workflow: started_at far in the past, well
        // beyond the orphan grace, status Running.
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow.id).expect("workflow should load");
        stored.started_at = chrono::Utc::now() - chrono::Duration::hours(2);
        manager.save(&stored).expect("backdated workflow should save");

        // While a live runner pid is registered (the mechanism `workflow
        // resume` uses), the reconciler must leave the workflow alone.
        register_workflow_runner_pid(temp.path(), &workflow.id, std::process::id()).expect("pid should register");
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new()).await;
        assert_eq!(recovered, 0, "live runner pid must shield the resumed workflow");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        // Once the runner is gone, the same workflow is reconciled.
        unregister_workflow_runner_pid(temp.path(), &workflow.id).expect("pid should unregister");
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new()).await;
        assert_eq!(recovered, 1, "orphaned workflow without a live runner must be cancelled");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled);
    }

    // Workflows suspended on a pending interaction sit in Paused with no
    // live runner pid; the orphan reconciler must leave them alone (it only
    // targets Running records) until the answer path resumes them.
    #[tokio::test]
    async fn paused_workflow_is_exempt_from_orphan_recovery() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        // v0.6: the kernel sources its base workflow config from a config_source
        // plugin; in tests, stand in for it after the hub scaffolds .animus/.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "suspended workflow".to_string(),
                description: "paused exemption test".to_string(),
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
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");
        hub.workflows().pause(&workflow.id).await.expect("workflow should pause");

        // Backdate started_at well past the orphan grace so only the Paused
        // status shields it.
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow.id).expect("workflow should load");
        stored.started_at = chrono::Utc::now() - chrono::Duration::hours(2);
        manager.save(&stored).expect("backdated workflow should save");

        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new()).await;
        assert_eq!(recovered, 0, "paused workflows must be exempt from orphan recovery");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Paused);
    }
}

fn workflow_is_waiting_on_manual_phase(project_root: &str, workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    let Some(phase_id) = workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
    else {
        return false;
    };

    orchestrator_core::load_agent_runtime_config(Path::new(project_root))
        .ok()
        .and_then(|config| config.phase_execution(&phase_id).cloned())
        .map(|definition| matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual))
        .unwrap_or(false)
}
