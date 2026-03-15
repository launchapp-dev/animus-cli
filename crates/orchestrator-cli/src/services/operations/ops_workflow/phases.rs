use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use orchestrator_core::{
    dispatch_workflow_event, register_workflow_runner_pid, services::ServiceHub, unregister_workflow_runner_pid,
    WorkflowEvent,
};
use workflow_runner_v2::workflow_execute::{execute_workflow, WorkflowExecuteParams};

use super::config::{manual_approvals_path, title_case_phase_id};
use super::emit_daemon_event;
use crate::dry_run_envelope;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManualApprovalRecord {
    workflow_id: String,
    phase_id: String,
    note: String,
    approved_at: String,
    approved_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManualApprovalsStore {
    #[serde(default)]
    approvals: Vec<ManualApprovalRecord>,
}

struct WorkflowRunnerPidGuard {
    project_root: String,
    workflow_id: String,
}

impl WorkflowRunnerPidGuard {
    fn register(project_root: &str, workflow_id: &str) -> Result<Self> {
        register_workflow_runner_pid(Path::new(project_root), workflow_id, std::process::id())?;
        Ok(Self { project_root: project_root.to_string(), workflow_id: workflow_id.to_string() })
    }
}

impl Drop for WorkflowRunnerPidGuard {
    fn drop(&mut self) {
        let _ = unregister_workflow_runner_pid(Path::new(&self.project_root), &self.workflow_id);
    }
}

pub(crate) fn resumability_to_json(status: &orchestrator_core::ResumabilityStatus) -> Value {
    match status {
        orchestrator_core::ResumabilityStatus::Resumable { workflow_id, reason } => serde_json::json!({
            "kind": "resumable",
            "workflow_id": workflow_id,
            "reason": reason,
        }),
        orchestrator_core::ResumabilityStatus::Stale { workflow_id, age_hours, max_age_hours } => serde_json::json!({
            "kind": "stale",
            "workflow_id": workflow_id,
            "age_hours": age_hours,
            "max_age_hours": max_age_hours,
        }),
        orchestrator_core::ResumabilityStatus::InvalidState { workflow_id, status, reason } => serde_json::json!({
            "kind": "invalid_state",
            "workflow_id": workflow_id,
            "status": status,
            "reason": reason,
        }),
    }
}

fn read_manual_approvals(project_root: &str) -> Result<ManualApprovalsStore> {
    let path = manual_approvals_path(project_root);
    if !path.exists() {
        return Ok(ManualApprovalsStore::default());
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn write_manual_approvals(project_root: &str, store: &ManualApprovalsStore) -> Result<()> {
    orchestrator_core::write_json_pretty(&manual_approvals_path(project_root), store)
}

pub(crate) fn upsert_phase_definition(
    project_root: &str,
    phase_id: &str,
    definition: orchestrator_core::PhaseExecutionDefinition,
) -> Result<Value> {
    let mut workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if workflow.phase_catalog.keys().all(|existing| !existing.eq_ignore_ascii_case(phase_id)) {
        workflow.phase_catalog.insert(
            phase_id.to_string(),
            orchestrator_core::PhaseUiDefinition {
                label: title_case_phase_id(phase_id),
                description: String::new(),
                category: "custom".to_string(),
                icon: None,
                docs_url: None,
                tags: Vec::new(),
                visible: true,
            },
        );
    }

    let mut runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    runtime.phases.insert(phase_id.to_string(), definition.clone());

    orchestrator_core::validate_workflow_and_runtime_configs(&workflow, &runtime)?;
    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &runtime)?;
    orchestrator_core::write_workflow_config(Path::new(project_root), &workflow)?;

    Ok(serde_json::json!({
        "phase_id": phase_id,
        "phase": definition,
        "agent_runtime_hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&runtime),
    }))
}

pub(crate) fn remove_phase_definition(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if workflow
        .workflows
        .iter()
        .any(|pipeline| pipeline.phases.iter().any(|phase| phase.phase_id().eq_ignore_ascii_case(phase_id)))
    {
        return Err(anyhow!("cannot remove phase '{}' because at least one pipeline references it", phase_id));
    }

    let mut runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let normalized_phase_id = runtime
        .phases
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(phase_id))
        .cloned()
        .ok_or_else(|| anyhow!("phase '{}' does not exist", phase_id))?;
    runtime.phases.remove(&normalized_phase_id);

    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &runtime)?;
    Ok(serde_json::json!({
        "removed": normalized_phase_id,
        "agent_runtime_hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&runtime),
    }))
}

pub(crate) fn preview_phase_removal(project_root: &str, phase_id: &str) -> Result<Value> {
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let normalized_phase_id = runtime
        .phases
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(phase_id))
        .cloned()
        .ok_or_else(|| anyhow!("phase '{}' does not exist", phase_id))?;

    let mut envelope = dry_run_envelope(
        "workflow.phases.remove",
        serde_json::json!({"phase_id": &normalized_phase_id}),
        "workflow.phases.remove",
        vec!["remove phase runtime definition".to_string()],
        &format!("rerun 'ao workflow phases remove --phase {} --confirm {}' to apply", phase_id, phase_id),
    );
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("can_remove".to_string(), serde_json::json!(true));
    }
    Ok(envelope)
}

pub(crate) fn upsert_pipeline(project_root: &str, pipeline: orchestrator_core::WorkflowDefinition) -> Result<Value> {
    let mut workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if let Some(existing) =
        workflow.workflows.iter_mut().find(|existing| existing.id.eq_ignore_ascii_case(pipeline.id.as_str()))
    {
        *existing = pipeline.clone();
    } else {
        workflow.workflows.push(pipeline.clone());
    }

    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    orchestrator_core::validate_workflow_and_runtime_configs(&workflow, &runtime)?;
    orchestrator_core::write_workflow_config(Path::new(project_root), &workflow)?;

    Ok(serde_json::json!({
        "pipeline": pipeline,
        "workflow_config_hash": orchestrator_core::workflow_config_hash(&workflow),
    }))
}

pub(crate) fn phase_payload(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;

    let ui =
        workflow.phase_catalog.iter().find(|(id, _)| id.eq_ignore_ascii_case(phase_id)).map(|(_, value)| value.clone());
    let runtime_definition =
        runtime.phases.iter().find(|(id, _)| id.eq_ignore_ascii_case(phase_id)).map(|(_, value)| value.clone());

    Ok(serde_json::json!({
        "phase_id": phase_id,
        "ui": ui,
        "runtime": runtime_definition,
    }))
}

pub(crate) fn list_phase_payload(project_root: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;

    let mut phases = Vec::new();
    for (phase_id, ui) in &workflow.phase_catalog {
        let runtime_definition = runtime
            .phases
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(phase_id.as_str()))
            .map(|(_, value)| value.clone());
        phases.push(serde_json::json!({
            "phase_id": phase_id,
            "ui": ui,
            "runtime": runtime_definition,
        }));
    }

    Ok(serde_json::json!({
        "phases": phases,
    }))
}

pub(crate) async fn approve_manual_phase(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    note: &str,
) -> Result<Value> {
    let _runner_pid_guard = WorkflowRunnerPidGuard::register(project_root, workflow_id)?;
    let approval_timestamp = Utc::now().to_rfc3339();
    let outcome = dispatch_workflow_event(
        hub.clone(),
        project_root,
        WorkflowEvent::ApproveManualPhase {
            workflow_id: workflow_id.to_string(),
            phase_id: phase_id.to_string(),
            note: Some(note.to_string()),
        },
    )
    .await?;
    let updated = outcome.workflow.ok_or_else(|| anyhow!("workflow '{}' not found", workflow_id))?;

    let mut store = read_manual_approvals(project_root)?;
    store.approvals.push(ManualApprovalRecord {
        workflow_id: workflow_id.to_string(),
        phase_id: phase_id.to_string(),
        note: note.to_string(),
        approved_at: approval_timestamp.clone(),
        approved_by: protocol::ACTOR_CLI.to_string(),
    });
    write_manual_approvals(project_root, &store)?;

    let mut continued_execution = None;
    if outcome.requires_continuation {
        let continuation = match execute_workflow(WorkflowExecuteParams {
            project_root: project_root.to_string(),
            workflow_id: Some(updated.id.clone()),
            task_id: None,
            requirement_id: None,
            title: None,
            description: None,
            workflow_ref: updated.workflow_ref.clone(),
            input: updated.input.clone(),
            vars: updated.vars.clone(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            phase_filter: None,
            on_phase_event: None,
            hub: Some(hub.clone()),
            phase_routing: None,
            mcp_config: None,
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Ok(reloaded) = hub.workflows().get(workflow_id).await {
                    project_terminal_workflow_result(
                        hub.clone(),
                        project_root,
                        reloaded.subject.id(),
                        Some(reloaded.task_id.as_str()),
                        reloaded.workflow_ref.as_deref(),
                        Some(reloaded.id.as_str()),
                        reloaded.status,
                        reloaded.failure_reason.as_deref(),
                    )
                    .await;
                }
                return Err(error.context("failed to continue workflow after manual approval"));
            }
        };

        continued_execution = Some(serde_json::json!({
            "workflow_id": continuation.workflow_id,
            "workflow_status": continuation.workflow_status,
            "phases_requested": continuation.phases_requested,
            "phase_results": continuation.phase_results,
            "post_success": continuation.post_success,
        }));
    }

    let final_workflow = hub.workflows().get(workflow_id).await?;
    project_terminal_workflow_result(
        hub.clone(),
        project_root,
        final_workflow.subject.id(),
        Some(final_workflow.task_id.as_str()),
        final_workflow.workflow_ref.as_deref(),
        Some(final_workflow.id.as_str()),
        final_workflow.status,
        final_workflow.failure_reason.as_deref(),
    )
    .await;
    emit_daemon_event(
        project_root,
        "workflow-phase-manual-approved",
        serde_json::json!({
            "workflow_id": workflow_id,
            "task_id": updated.task_id,
            "phase_id": phase_id,
            "note": note,
        }),
    )?;

    Ok(serde_json::json!({
        "workflow": final_workflow,
        "manual_approval": {
            "phase_id": phase_id,
            "note": note,
            "approved_at": approval_timestamp,
        },
        "continued_execution": continued_execution,
    }))
}

pub(crate) async fn reject_manual_phase(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    note: &str,
) -> Result<Value> {
    let outcome = dispatch_workflow_event(
        hub.clone(),
        project_root,
        WorkflowEvent::RejectManualPhase {
            workflow_id: workflow_id.to_string(),
            phase_id: phase_id.to_string(),
            note: Some(note.to_string()),
        },
    )
    .await?;
    let updated = outcome.workflow.ok_or_else(|| anyhow!("workflow '{}' not found", workflow_id))?;

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

    emit_daemon_event(
        project_root,
        "workflow-phase-manual-rejected",
        serde_json::json!({
            "workflow_id": workflow_id,
            "task_id": updated.task_id,
            "phase_id": phase_id,
            "note": note,
        }),
    )?;

    Ok(serde_json::json!({
        "workflow": updated,
        "manual_rejection": {
            "phase_id": phase_id,
            "note": note,
            "rejected_at": Utc::now().to_rfc3339(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::{approve_manual_phase, reject_manual_phase};
    use crate::shared::test_env_lock;
    use orchestrator_core::{
        load_agent_runtime_config, services::ServiceHub, write_agent_runtime_config, FileServiceHub,
        PhaseExecutionMode, PhaseManualDefinition, Priority, TaskCreateInput, TaskStatus, TaskType,
        WorkflowPhaseStatus, WorkflowRunInput, WorkflowStatus,
    };
    use protocol::test_utils::EnvVarGuard;
    use std::process::Command as ProcessCommand;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn init_git_repo(temp: &TempDir) {
        let init_main = ProcessCommand::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        if !init_main.success() {
            let init =
                ProcessCommand::new("git").arg("init").current_dir(temp.path()).status().expect("git init should run");
            assert!(init.success(), "git init should succeed");
            let rename = ProcessCommand::new("git")
                .args(["branch", "-M", "main"])
                .current_dir(temp.path())
                .status()
                .expect("git branch -M should run");
            assert!(rename.success(), "git branch -M main should succeed");
        }

        let email = ProcessCommand::new("git")
            .args(["config", "user.email", "ao-test@example.com"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.email should run");
        assert!(email.success(), "git config user.email should succeed");
        let name = ProcessCommand::new("git")
            .args(["config", "user.name", "AO Test"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.name should run");
        assert!(name.success(), "git config user.name should succeed");

        std::fs::write(temp.path().join("README.md"), "# test\n").expect("readme should be written");
        let add = ProcessCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(temp.path())
            .status()
            .expect("git add should run");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "initial commit should succeed");
    }

    #[tokio::test]
    async fn approve_manual_phase_continues_non_terminal_workflow() {
        let _lock = test_env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "manual approval".to_string(),
                description: "resume before completing".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks().set_status(&task.id, TaskStatus::InProgress, false).await.expect("task should be in progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");
        let current_phase = workflow.current_phase.clone().expect("workflow should have current phase");
        let next_phase = workflow
            .phases
            .get(workflow.current_phase_index + 1)
            .map(|phase| phase.phase_id.clone())
            .expect("workflow should have a second phase");

        let mut runtime = load_agent_runtime_config(temp.path()).expect("runtime config");
        let mut current_definition =
            runtime.phase_execution(&current_phase).cloned().expect("current phase should exist");
        current_definition.mode = PhaseExecutionMode::Manual;
        current_definition.agent_id = None;
        current_definition.command = None;
        current_definition.manual = Some(PhaseManualDefinition {
            instructions: "Approve this step".to_string(),
            approval_note_required: false,
            timeout_secs: None,
        });
        runtime.phases.insert(current_phase.clone(), current_definition);

        let mut next_definition = runtime.phase_execution(&next_phase).cloned().expect("next phase should exist");
        next_definition.mode = PhaseExecutionMode::Manual;
        next_definition.agent_id = None;
        next_definition.command = None;
        next_definition.manual = Some(PhaseManualDefinition {
            instructions: "Approve the resumed phase".to_string(),
            approval_note_required: false,
            timeout_secs: None,
        });
        runtime.phases.insert(next_phase.clone(), next_definition);
        write_agent_runtime_config(temp.path(), &runtime).expect("runtime config should write");

        let paused = hub.workflows().pause(&workflow.id).await.expect("workflow should pause");
        assert_eq!(paused.status, WorkflowStatus::Paused);

        let response = approve_manual_phase(hub.clone(), &project_root, &workflow.id, &current_phase, "approved")
            .await
            .expect("manual approval should succeed");

        let updated = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        let completed_phase = updated
            .phases
            .iter()
            .find(|phase| phase.phase_id == current_phase)
            .expect("approved phase should remain in workflow");

        assert_eq!(completed_phase.status, WorkflowPhaseStatus::Success);
        assert_eq!(updated.status, WorkflowStatus::Paused);
        assert_eq!(updated.current_phase.as_deref(), Some(next_phase.as_str()));
        assert_eq!(response["continued_execution"]["workflow_status"].as_str(), Some("paused"));
        assert_eq!(response["continued_execution"]["phase_results"][0]["phase_id"].as_str(), Some(next_phase.as_str()));
    }

    #[tokio::test]
    async fn reject_manual_phase_fails_workflow() {
        let _lock = test_env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "manual rejection".to_string(),
                description: "reject approval".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks().set_status(&task.id, TaskStatus::InProgress, false).await.expect("task should be in progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");
        let current_phase = workflow.current_phase.clone().expect("workflow should have current phase");

        let mut runtime = load_agent_runtime_config(temp.path()).expect("runtime config");
        let mut definition = runtime.phase_execution(&current_phase).cloned().expect("current phase should exist");
        definition.mode = PhaseExecutionMode::Manual;
        definition.agent_id = None;
        definition.command = None;
        definition.manual = Some(PhaseManualDefinition {
            instructions: "Approve or reject".to_string(),
            approval_note_required: false,
            timeout_secs: None,
        });
        runtime.phases.insert(current_phase.clone(), definition);
        write_agent_runtime_config(temp.path(), &runtime).expect("runtime config should write");

        let paused = hub.workflows().pause(&workflow.id).await.expect("workflow should pause");
        assert_eq!(paused.status, WorkflowStatus::Paused);

        reject_manual_phase(hub.clone(), &project_root, &workflow.id, &current_phase, "rejected")
            .await
            .expect("manual rejection should succeed");

        let updated = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        let rejected_phase = updated
            .phases
            .iter()
            .find(|phase| phase.phase_id == current_phase)
            .expect("rejected phase should remain in workflow");

        assert_eq!(rejected_phase.status, WorkflowPhaseStatus::Failed);
        assert_eq!(updated.status, WorkflowStatus::Failed);
    }
}
