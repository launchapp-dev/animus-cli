use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use animus_workflow_runner_protocol as workflow_proto;
use orchestrator_core::{
    dispatch_workflow_event, register_workflow_runner_pid, services::ServiceHub, unregister_workflow_runner_pid,
    OrchestratorWorkflow, WorkflowEvent, WorkflowRunInput,
};

use crate::services::plugin_clients;

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

pub(crate) struct WorkflowRunnerPidGuard {
    project_root: String,
    workflow_id: String,
}

impl WorkflowRunnerPidGuard {
    pub(crate) fn register(project_root: &str, workflow_id: &str) -> Result<Self> {
        register_workflow_runner_pid(Path::new(project_root), workflow_id, std::process::id())?;
        Ok(Self { project_root: project_root.to_string(), workflow_id: workflow_id.to_string() })
    }
}

impl Drop for WorkflowRunnerPidGuard {
    fn drop(&mut self) {
        let _ = unregister_workflow_runner_pid(Path::new(&self.project_root), &self.workflow_id);
    }
}

fn missing_workflow_runner_error() -> anyhow::Error {
    anyhow!(
        "no workflow_runner plugin installed - run `animus plugin install-defaults` (or install \
         `launchapp-dev/animus-workflow-runner-default`) before running workflows"
    )
}

pub(crate) fn ensure_workflow_runner_plugin(project_root: &Path) -> Result<()> {
    let roles = plugin_clients::probe_active_plugin_roles(project_root)?;
    if roles.workflow_runner {
        Ok(())
    } else {
        Err(missing_workflow_runner_error())
    }
}

/// Build the `workflow/execute` request that re-attaches the workflow_runner
/// plugin to an already-persisted workflow record. Subject fields stay empty:
/// the persisted record is authoritative for subject, input, and vars.
pub(crate) fn workflow_execute_request_for_existing(
    workflow: &OrchestratorWorkflow,
) -> workflow_proto::WorkflowExecuteRequest {
    workflow_proto::WorkflowExecuteRequest {
        workflow_id: Some(workflow.id.clone()),
        subject_dispatch: None,
        subject_ref: None,
        task_id: None,
        requirement_id: None,
        title: None,
        description: None,
        workflow_ref: workflow.workflow_ref.clone(),
        input: workflow.input.clone(),
        vars: workflow.vars.clone(),
        model: None,
        tool: None,
        phase_timeout_secs: None,
        phase_filter: None,
        phase_routing: None,
        mcp_config: None,
    }
}

/// Per-run execution overrides forwarded to the detached runner child so
/// async `workflow run` honors the same `--model` / `--tool` /
/// `--phase-timeout-secs` flags as the `--sync` path.
#[derive(Debug, Clone, Default)]
pub(crate) struct DetachedRunnerOverrides {
    pub(crate) model: Option<String>,
    pub(crate) tool: Option<String>,
    pub(crate) phase_timeout_secs: Option<u64>,
}

fn detached_runner_command(
    program: &Path,
    project_root: &str,
    workflow_id: &str,
    overrides: &DetachedRunnerOverrides,
) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.arg("--project-root").arg(project_root).arg("--json").args([
        "workflow",
        "run",
        "--sync",
        "--workflow-id",
        workflow_id,
    ]);
    if let Some(model) = overrides.model.as_deref() {
        command.args(["--model", model]);
    }
    if let Some(tool) = overrides.tool.as_deref() {
        command.args(["--tool", tool]);
    }
    if let Some(timeout) = overrides.phase_timeout_secs {
        command.args(["--phase-timeout-secs", &timeout.to_string()]);
    }
    command
        .current_dir(project_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

/// Spawn a detached `animus workflow run --sync --workflow-id <id>` child
/// that drives the workflow_runner plugin to completion for an existing
/// workflow record, then register the child pid in the workflow-runner
/// registry so the orphan reconciler treats the run as live.
pub(crate) fn spawn_detached_workflow_runner(
    project_root: &str,
    workflow_id: &str,
    overrides: &DetachedRunnerOverrides,
) -> Result<u32> {
    let program = std::env::current_exe().context("failed to resolve current animus binary")?;
    spawn_detached_workflow_runner_with_program(&program, project_root, workflow_id, overrides)
}

fn spawn_detached_workflow_runner_with_program(
    program: &Path,
    project_root: &str,
    workflow_id: &str,
    overrides: &DetachedRunnerOverrides,
) -> Result<u32> {
    let mut command = detached_runner_command(program, project_root, workflow_id, overrides);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn detached workflow runner for workflow '{workflow_id}'"))?;
    let pid = child.id();
    if let Err(error) = register_workflow_runner_pid(Path::new(project_root), workflow_id, pid) {
        // Best-effort: the spawned child registers its own pid on startup,
        // so a registry write failure here only narrows the liveness window.
        tracing::warn!(workflow_id = %workflow_id, error = %error, "failed to register workflow runner pid");
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// Async `workflow run` entry point: bootstrap the workflow record, then
/// hand execution to a detached workflow_runner spawn. Post-v0.5 there is
/// no in-process executor — a bare `workflows.run(...)` would leave a
/// Running record that nothing drives until the orphan reconciler cancels
/// it.
pub(crate) async fn start_workflow_with_runner(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: WorkflowRunInput,
    overrides: DetachedRunnerOverrides,
) -> Result<OrchestratorWorkflow> {
    ensure_workflow_runner_plugin(Path::new(project_root))?;
    let workflow = hub.workflows().run(input).await?;
    // Mirror the daemon's ready-dispatch contract: a dispatched task moves
    // to InProgress. Terminal projection never auto-completes tasks, so a
    // task left Ready here would be re-dispatched by the daemon after its
    // workflow finishes. The transition happens BEFORE the spawn so a fast
    // runner's terminal projection cannot be overwritten afterwards.
    let mut task_status_before_dispatch = None;
    if !workflow.task_id.is_empty() {
        if let Ok(task) = hub.tasks().get(&workflow.task_id).await {
            if matches!(task.status, orchestrator_core::TaskStatus::Ready | orchestrator_core::TaskStatus::Backlog) {
                let _ =
                    hub.tasks().set_status(&workflow.task_id, orchestrator_core::TaskStatus::InProgress, false).await;
                task_status_before_dispatch = Some(task.status);
            }
        }
    }
    // Skip guards can complete (or fail) every phase during bootstrap; a
    // workflow that is already terminal has nothing left to execute, so
    // spawning a runner would only re-enter (or, on spawn failure, cancel)
    // a finished record. Project the terminal result instead. The task
    // intentionally stays InProgress for the skip-completed case: that is
    // the same operator-confirmation end state every successfully executed
    // run lands in (task completion is never automatic), and reverting to
    // Ready would make the daemon re-dispatch the task into another
    // instantly-skip-completed workflow on every tick.
    // TODO(codex-p2): if the product ever allows auto-completing tasks for
    // fully-skip-completed workflows, project task completion here instead
    // of leaving the confirmation to an operator.
    if !matches!(
        workflow.status,
        orchestrator_core::WorkflowStatus::Running | orchestrator_core::WorkflowStatus::Pending
    ) {
        project_terminal_workflow_result(
            hub.clone(),
            project_root,
            workflow.subject.id(),
            Some(workflow.task_id.as_str()),
            workflow.workflow_ref.as_deref(),
            Some(workflow.id.as_str()),
            workflow.status,
            workflow.failure_reason.as_deref(),
        )
        .await;
        return Ok(workflow);
    }
    if let Err(error) = spawn_detached_workflow_runner(project_root, &workflow.id, &overrides) {
        let _ = hub.workflows().cancel(&workflow.id).await;
        if let Some(previous_status) = task_status_before_dispatch {
            let _ = hub.tasks().set_status(&workflow.task_id, previous_status, false).await;
        }
        return Err(error.context(format!("failed to start workflow runner for workflow '{}'", workflow.id)));
    }
    Ok(workflow)
}

/// `workflow resume` entry point: flip the workflow back to Running and
/// hand execution to a detached workflow_runner spawn. The pid registry
/// entry is written BEFORE the status flip — resumed workflows keep their
/// original `started_at`, so without a liveness entry the orphan
/// reconciler would cancel them on the very next tick.
pub(crate) async fn resume_workflow_with_runner(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow_id: &str,
    feedback: Option<String>,
) -> Result<OrchestratorWorkflow> {
    let existing = hub.workflows().get(workflow_id).await?;
    if matches!(
        existing.status,
        orchestrator_core::WorkflowStatus::Completed | orchestrator_core::WorkflowStatus::Cancelled
    ) {
        return Err(anyhow!(
            "workflow '{}' is {} and cannot be resumed",
            workflow_id,
            format!("{:?}", existing.status).to_ascii_lowercase()
        ));
    }
    // A live runner already owns this workflow (e.g. resume retried while
    // the previous spawn is still driving it): refuse rather than spawn a
    // second concurrent runner for the same record.
    let live_runners = orchestrator_core::active_workflow_runner_ids(Path::new(project_root)).unwrap_or_default();
    if live_runners.contains(workflow_id) {
        return Err(anyhow!(
            "workflow '{}' already has a live workflow runner attached; wait for it to finish or cancel the workflow first",
            workflow_id
        ));
    }
    // Daemon-spawned runners are tracked via per-spawn agent records (not
    // the workflow-runner pid registry); check those too so a resume retry
    // against an actively daemon-driven subject cannot start a second
    // concurrent runner for the same record.
    match orchestrator_daemon_runtime::agent_record::scan_orphans_for_project(Path::new(project_root)) {
        Ok(report) => {
            let subject_id = existing.subject.id();
            let daemon_owned = report.detected.iter().any(|agent| {
                agent.subject_id == subject_id
                    || (!existing.task_id.is_empty() && agent.task_id.as_deref() == Some(existing.task_id.as_str()))
            });
            if daemon_owned {
                return Err(anyhow!(
                    "workflow '{}' subject '{}' is being driven by a live daemon-spawned runner; wait for it to finish or cancel the workflow first",
                    workflow_id,
                    subject_id
                ));
            }
        }
        Err(error) => {
            tracing::warn!(workflow_id = %workflow_id, error = %error, "failed to scan daemon runner records before resume");
        }
    }
    ensure_workflow_runner_plugin(Path::new(project_root))?;
    register_workflow_runner_pid(Path::new(project_root), workflow_id, std::process::id())?;
    let outcome = match dispatch_workflow_event(
        hub.clone(),
        project_root,
        WorkflowEvent::Resume { workflow_id: workflow_id.to_string(), feedback },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = unregister_workflow_runner_pid(Path::new(project_root), workflow_id);
            return Err(error);
        }
    };
    let Some(workflow) = outcome.workflow else {
        let _ = unregister_workflow_runner_pid(Path::new(project_root), workflow_id);
        return Err(anyhow!("workflow '{}' not found", workflow_id));
    };
    if let Err(error) = spawn_detached_workflow_runner(project_root, workflow_id, &DetachedRunnerOverrides::default()) {
        // Re-pause AND re-annotate: the Resume dispatch above already
        // cleared the task's "paused by workflow <id>" marker, so without
        // re-adding it the re-paused workflow would stall unexplained.
        if let Ok(repaused) = hub.workflows().pause(workflow_id).await {
            if repaused.status == orchestrator_core::WorkflowStatus::Paused {
                if let Some(task_id) = orchestrator_core::workflow_task_id(&repaused) {
                    let _ =
                        orchestrator_core::project_task_workflow_paused(hub.clone(), &task_id, workflow_id, None).await;
                }
            }
        }
        let _ = unregister_workflow_runner_pid(Path::new(project_root), workflow_id);
        return Err(error.context(format!("failed to start workflow runner for resumed workflow '{workflow_id}'")));
    }
    Ok(workflow)
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
    let catalog_entry =
        workflow.phase_catalog.keys().all(|existing| !existing.eq_ignore_ascii_case(phase_id)).then(|| {
            orchestrator_core::PhaseUiDefinition {
                label: title_case_phase_id(phase_id),
                description: String::new(),
                category: "custom".to_string(),
                icon: None,
                docs_url: None,
                tags: Vec::new(),
                visible: true,
            }
        });
    if let Some(entry) = catalog_entry.clone() {
        workflow.phase_catalog.insert(phase_id.to_string(), entry);
    }
    workflow.phase_definitions.insert(phase_id.to_string(), definition.clone());

    let mut runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    runtime.phases.insert(phase_id.to_string(), definition.clone());

    orchestrator_core::validate_workflow_and_runtime_configs_with_project_root(
        &workflow,
        &runtime,
        Some(Path::new(project_root)),
    )?;
    // Persist only the upserted definition into the generated overlay. The
    // compiled `workflow` / `runtime` configs above carry resolved `${VAR}` /
    // `${secret.X}` values and must never be serialized back into the
    // project tree.
    orchestrator_core::upsert_generated_workflow_phase(
        Path::new(project_root),
        phase_id,
        &definition,
        catalog_entry.as_ref(),
    )?;

    Ok(serde_json::json!({
        "phase_id": phase_id,
        "phase": definition,
        "agent_runtime_hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&runtime),
    }))
}

pub(crate) fn remove_phase_definition(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    // TODO(codex-p2): when the phase is a generated-overlay OVERRIDE of a
    // hand-authored YAML/pack definition, removing the override leaves the
    // phase defined and pipeline references valid — this check should not
    // block that case. Requires compiling sources minus the generated
    // overlays to detect the underlying definition.
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

    let removed = orchestrator_core::remove_generated_workflow_phase(Path::new(project_root), &normalized_phase_id)?;
    if !removed {
        return Err(anyhow!(
            "phase '{}' is not defined in the generated overlays (.animus/workflows/generated-workflow.yaml, generated-runtime.yaml); remove it from the workflow YAML source or pack that defines it",
            normalized_phase_id
        ));
    }
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

    let can_remove =
        orchestrator_core::generated_workflow_phase_is_defined(Path::new(project_root), &normalized_phase_id)?;

    let mut envelope = dry_run_envelope(
        "workflow.phases.remove",
        serde_json::json!({"phase_id": &normalized_phase_id}),
        "workflow.phases.remove",
        vec!["remove phase runtime definition".to_string()],
        &format!("rerun 'animus workflow phases remove --phase {} --confirm {}' to apply", phase_id, phase_id),
    );
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("can_remove".to_string(), serde_json::json!(can_remove));
        if !can_remove {
            obj.insert(
                "reason".to_string(),
                serde_json::json!(format!(
                    "phase '{}' is not defined in the generated overlays; remove it from the workflow YAML source or pack that defines it",
                    normalized_phase_id
                )),
            );
        }
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
    orchestrator_core::validate_workflow_and_runtime_configs_with_project_root(
        &workflow,
        &runtime,
        Some(Path::new(project_root)),
    )?;
    // Persist only the upserted pipeline into the generated overlay; the
    // compiled `workflow` config carries resolved secret values and must not
    // be serialized back into the project tree.
    orchestrator_core::upsert_generated_workflow_pipeline(Path::new(project_root), &pipeline)?;

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
        // v0.5.1 fold-in: continuation routes through the
        // `workflow_runner` plugin (workflow/execute with the
        // existing workflow_id). The in-tree continuation path was
        // removed; the plugin owns workflow execution after the
        // manual approval lands.
        let plugin_request = workflow_execute_request_for_existing(&updated);
        let project_root_path = Path::new(project_root);
        let continuation_outcome = plugin_clients::call_workflow_execute(project_root_path, &plugin_request).await;
        let continuation = match continuation_outcome {
            Ok(Some(result)) => result,
            Ok(None) => {
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
                return Err(anyhow!(
                    "no workflow_runner plugin installed - manual approval continuation requires                      `launchapp-dev/animus-workflow-runner-default`; install with `animus plugin install-defaults`"
                ));
            }
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
    #![allow(clippy::await_holding_lock)]

    use super::{reject_manual_phase, remove_phase_definition, upsert_phase_definition, upsert_pipeline};
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
            .args(["config", "user.name", "Animus Test"])
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

    // v0.5.1 fold-in (P2 #7): `approve_manual_phase` post-approval continuation
    // routes through the installed workflow_runner plugin. End-to-end coverage
    // for "approve + continue to next non-terminal phase" lives in the
    // `animus-workflow-runner-default` pack conformance suite. The in-tree
    // version was deleted in v0.5.1 round-2; the ignored test was dropped in
    // the v0.5.1 DELTA bundle as a surface-shrink.

    const UPSERT_SECRET_VALUE: &str = "zzz-upsert-leak-zzz";

    fn write_secret_using_workflows_yaml(temp: &TempDir) {
        let animus_dir = temp.path().join(".animus");
        std::fs::create_dir_all(&animus_dir).expect(".animus dir should be created");
        let yaml = r#"
secrets:
  api:
    env: ANIMUS_TEST_UPSERT_SECRET
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "${secret.api}"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build."
  lint:
    mode: agent
    agent: swe
    directive: "Lint."
agents:
  swe:
    description: "SWE"
    system_prompt: "Be a SWE."
workflows:
- id: flow
  phases: [build]
"#;
        std::fs::write(animus_dir.join("workflows.yaml"), yaml).expect("workflows.yaml should be written");
    }

    fn generated_overlay_path(temp: &TempDir, file_name: &str) -> std::path::PathBuf {
        temp.path().join(".animus").join("workflows").join(file_name)
    }

    /// Write a minimal `.animus/workflows.yaml` defining the `standard-workflow`
    /// pipeline so the config_source seam compiles a base that the FileServiceHub
    /// run/resume paths can resolve a phase plan from. v0.6 sources the base
    /// config from the config_source plugin (the seam), so tests that bootstrap a
    /// workflow must provide authored YAML for the seam to compile.
    fn write_standard_workflow_yaml(temp: &TempDir) {
        let animus_dir = temp.path().join(".animus");
        std::fs::create_dir_all(&animus_dir).expect(".animus dir should be created");
        let yaml = r#"
default_workflow_ref: standard-workflow
phases:
  requirements:
    mode: agent
    agent: swe
    directive: "Gather requirements."
  implementation:
    mode: agent
    agent: swe
    directive: "Implement."
  code-review:
    mode: agent
    agent: swe
    directive: "Review."
  testing:
    mode: agent
    agent: swe
    directive: "Test."
agents:
  swe:
    description: "SWE"
    system_prompt: "Be a SWE."
workflows:
- id: standard-workflow
  name: Standard Workflow
  phases: [requirements, implementation, code-review, testing]
"#;
        std::fs::write(animus_dir.join("workflows.yaml"), yaml).expect("workflows.yaml should be written");
    }

    #[test]
    fn upsert_phase_definition_keeps_resolved_secrets_out_of_generated_overlay() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _secret_guard = EnvVarGuard::set("ANIMUS_TEST_UPSERT_SECRET", Some(UPSERT_SECRET_VALUE));
        init_git_repo(&temp);
        write_secret_using_workflows_yaml(&temp);
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let project_root = temp.path().to_string_lossy().to_string();
        let definition: orchestrator_core::PhaseExecutionDefinition = serde_json::from_value(serde_json::json!({
            "mode": "agent",
            "agent_id": "swe",
            "directive": "Do the custom thing."
        }))
        .expect("definition should parse");
        upsert_phase_definition(&project_root, "custom-phase", definition).expect("upsert should succeed");

        let generated = generated_overlay_path(&temp, "generated-workflow.yaml");
        let content = std::fs::read_to_string(&generated).expect("generated overlay should exist");
        assert!(content.contains("custom-phase"), "upserted phase missing from overlay: {content}");
        assert!(!content.contains(UPSERT_SECRET_VALUE), "resolved secret leaked into generated overlay: {content}");
        assert!(!content.contains("mcp_servers"), "compiled mcp_servers must not be dumped: {content}");
        assert!(
            !generated_overlay_path(&temp, "generated-runtime.yaml").exists(),
            "upsert must not dump the merged runtime config"
        );

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        let recompiled = orchestrator_core::load_workflow_config(temp.path()).expect("recompile should succeed");
        assert!(recompiled.phase_definitions.contains_key("custom-phase"), "phase should survive recompile");
        let runtime = load_agent_runtime_config(temp.path()).expect("runtime should load");
        assert!(runtime.phases.contains_key("custom-phase"), "phase should reach the runtime config");
    }

    #[test]
    fn upsert_pipeline_keeps_resolved_secrets_out_of_generated_overlay() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _secret_guard = EnvVarGuard::set("ANIMUS_TEST_UPSERT_SECRET", Some(UPSERT_SECRET_VALUE));
        init_git_repo(&temp);
        write_secret_using_workflows_yaml(&temp);
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let project_root = temp.path().to_string_lossy().to_string();
        let pipeline: orchestrator_core::WorkflowDefinition = serde_json::from_value(serde_json::json!({
            "id": "custom-pipeline",
            "name": "Custom Pipeline",
            "description": "Authored via upsert.",
            "phases": ["build"],
            "budget": null
        }))
        .expect("pipeline should parse");
        upsert_pipeline(&project_root, pipeline).expect("upsert should succeed");

        let generated = generated_overlay_path(&temp, "generated-workflow.yaml");
        let content = std::fs::read_to_string(&generated).expect("generated overlay should exist");
        assert!(content.contains("custom-pipeline"), "upserted pipeline missing from overlay: {content}");
        assert!(!content.contains(UPSERT_SECRET_VALUE), "resolved secret leaked into generated overlay: {content}");
        assert!(!content.contains("mcp_servers"), "compiled mcp_servers must not be dumped: {content}");

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        let recompiled = orchestrator_core::load_workflow_config(temp.path()).expect("recompile should succeed");
        assert!(
            recompiled.workflows.iter().any(|workflow| workflow.id == "custom-pipeline"),
            "pipeline should survive recompile"
        );
    }

    #[test]
    fn remove_phase_definition_prunes_generated_overlay_and_rejects_yaml_sourced_phases() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _secret_guard = EnvVarGuard::set("ANIMUS_TEST_UPSERT_SECRET", Some(UPSERT_SECRET_VALUE));
        init_git_repo(&temp);
        write_secret_using_workflows_yaml(&temp);
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let project_root = temp.path().to_string_lossy().to_string();
        let definition: orchestrator_core::PhaseExecutionDefinition = serde_json::from_value(serde_json::json!({
            "mode": "agent",
            "agent_id": "swe",
            "directive": "Do the custom thing."
        }))
        .expect("definition should parse");
        upsert_phase_definition(&project_root, "custom-phase", definition).expect("upsert should succeed");

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        remove_phase_definition(&project_root, "custom-phase").expect("remove should succeed");
        let generated = generated_overlay_path(&temp, "generated-workflow.yaml");
        let content = std::fs::read_to_string(&generated).expect("generated overlay should exist");
        assert!(!content.contains("custom-phase"), "removed phase should be pruned from overlay: {content}");
        assert!(!content.contains(UPSERT_SECRET_VALUE), "resolved secret leaked into generated overlay: {content}");

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        let err = remove_phase_definition(&project_root, "lint").expect_err("yaml-sourced phase should not remove");
        assert!(
            format!("{err:#}").contains("generated overlays"),
            "error should direct the user to the YAML source: {err:#}"
        );
    }

    struct PluginIsolationGuards {
        _home: EnvVarGuard,
        _config_dir: EnvVarGuard,
        _plugin_dir: EnvVarGuard,
        _plugin_path: EnvVarGuard,
    }

    fn isolate_plugin_discovery(temp: &TempDir) -> PluginIsolationGuards {
        let home = temp.path().to_string_lossy().to_string();
        let config_dir = temp.path().join(".animus");
        let plugin_dir = config_dir.join("plugins");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir should be created");
        PluginIsolationGuards {
            _home: EnvVarGuard::set("HOME", Some(home.as_str())),
            _config_dir: EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_string_lossy().as_ref())),
            _plugin_dir: EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(plugin_dir.to_string_lossy().as_ref())),
            _plugin_path: EnvVarGuard::set("ANIMUS_PLUGIN_PATH", None),
        }
    }

    async fn create_test_task(hub: &Arc<FileServiceHub>, title: &str) -> orchestrator_core::OrchestratorTask {
        hub.tasks()
            .create(TaskCreateInput {
                title: title.to_string(),
                description: "workflow runner spawn test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created")
    }

    #[tokio::test]
    async fn start_workflow_with_runner_errors_actionably_when_plugin_missing() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _guards = isolate_plugin_discovery(&temp);
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        let task = create_test_task(&hub, "async run without plugin").await;

        let error = super::start_workflow_with_runner(
            hub.clone(),
            &project_root,
            WorkflowRunInput::for_task(task.id.clone(), None),
            super::DetachedRunnerOverrides::default(),
        )
        .await
        .expect_err("async run must fail when no workflow_runner plugin is installed");
        assert!(
            error.to_string().contains("animus plugin install-defaults"),
            "error must carry the install command: {error}"
        );

        let workflows = hub.workflows().list().await.expect("workflows should list");
        assert!(workflows.is_empty(), "no zombie Running record may be created when the runner cannot spawn");
    }

    #[tokio::test]
    async fn resume_workflow_with_runner_errors_without_plugin_and_leaves_workflow_paused() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _guards = isolate_plugin_discovery(&temp);
        init_git_repo(&temp);
        write_standard_workflow_yaml(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        let task = create_test_task(&hub, "resume without plugin").await;

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");
        let paused = hub.workflows().pause(&workflow.id).await.expect("workflow should pause");
        assert_eq!(paused.status, WorkflowStatus::Paused);

        let error = super::resume_workflow_with_runner(hub.clone(), &project_root, &workflow.id, None)
            .await
            .expect_err("resume must fail when no workflow_runner plugin is installed");
        assert!(
            error.to_string().contains("animus plugin install-defaults"),
            "error must carry the install command: {error}"
        );

        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Paused, "failed resume must not leave the workflow Running");
        let active = orchestrator_core::active_workflow_runner_ids(temp.path()).expect("registry should read");
        assert!(!active.contains(&workflow.id), "failed resume must not leak a runner pid entry");
    }

    #[cfg(unix)]
    fn install_fake_workflow_runner_plugin(temp: &TempDir) {
        use std::os::unix::fs::PermissionsExt;

        let plugin_dir = temp.path().join(".animus").join("plugins");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir should exist");
        let plugin = plugin_dir.join("animus-plugin-fake-workflow-runner");
        let script = concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--manifest\" ]; then\n",
            "cat <<'EOF'\n",
            "{\"name\":\"animus-plugin-fake-workflow-runner\",\"version\":\"0.0.1\",",
            "\"plugin_kind\":\"workflow_runner\",\"description\":\"test stub\",",
            "\"protocol_version\":\"1.1.0\",\"capabilities\":[]}\n",
            "EOF\n",
            "fi\n",
            "exit 0\n",
        );
        std::fs::write(&plugin, script).expect("fake plugin should be written");
        let mut perms = std::fs::metadata(&plugin).expect("plugin metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin, perms).expect("plugin should be executable");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn start_workflow_with_runner_spawns_runner_and_marks_ready_task_in_progress() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _guards = isolate_plugin_discovery(&temp);
        init_git_repo(&temp);
        install_fake_workflow_runner_plugin(&temp);
        write_standard_workflow_yaml(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        let task = create_test_task(&hub, "async run with plugin").await;
        hub.tasks().set_status(&task.id, TaskStatus::Ready, false).await.expect("task should be ready");

        let workflow = super::start_workflow_with_runner(
            hub.clone(),
            &project_root,
            WorkflowRunInput::for_task(task.id.clone(), None),
            super::DetachedRunnerOverrides::default(),
        )
        .await
        .expect("async run should bootstrap the record and spawn the runner");

        assert_eq!(workflow.task_id, task.id);
        assert_eq!(workflow.status, WorkflowStatus::Running);
        let reloaded = hub.tasks().get(&task.id).await.expect("task should reload");
        assert_eq!(
            reloaded.status,
            TaskStatus::InProgress,
            "dispatched task must leave Ready so the daemon does not re-dispatch it"
        );
    }

    #[tokio::test]
    async fn resume_workflow_with_runner_rejects_terminal_and_runner_owned_workflows() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _guards = isolate_plugin_discovery(&temp);
        init_git_repo(&temp);
        write_standard_workflow_yaml(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        let task = create_test_task(&hub, "resume guards").await;

        // Running workflow with a live runner attached: refuse a second
        // concurrent runner.
        let running = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");
        orchestrator_core::register_workflow_runner_pid(temp.path(), &running.id, std::process::id())
            .expect("pid should register");
        let error = super::resume_workflow_with_runner(hub.clone(), &project_root, &running.id, None)
            .await
            .expect_err("resume must refuse a workflow that already has a live runner");
        assert!(error.to_string().contains("live workflow runner"), "unexpected error: {error}");
        orchestrator_core::unregister_workflow_runner_pid(temp.path(), &running.id).expect("pid should unregister");

        // Cancelled workflow: not resumable.
        let cancelled = hub.workflows().cancel(&running.id).await.expect("workflow should cancel");
        assert_eq!(cancelled.status, WorkflowStatus::Cancelled);
        let error = super::resume_workflow_with_runner(hub.clone(), &project_root, &running.id, None)
            .await
            .expect_err("resume must refuse a cancelled workflow");
        assert!(error.to_string().contains("cannot be resumed"), "unexpected error: {error}");
    }

    #[test]
    fn detached_runner_command_reattaches_existing_workflow_via_sync_run() {
        let command = super::detached_runner_command(
            std::path::Path::new("/usr/local/bin/animus"),
            "/tmp/project",
            "wf-123",
            &super::DetachedRunnerOverrides::default(),
        );
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            vec!["--project-root", "/tmp/project", "--json", "workflow", "run", "--sync", "--workflow-id", "wf-123"]
        );

        let command = super::detached_runner_command(
            std::path::Path::new("/usr/local/bin/animus"),
            "/tmp/project",
            "wf-123",
            &super::DetachedRunnerOverrides {
                model: Some("claude-sonnet-4-6".to_string()),
                tool: Some("claude".to_string()),
                phase_timeout_secs: Some(120),
            },
        );
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            vec![
                "--project-root",
                "/tmp/project",
                "--json",
                "workflow",
                "run",
                "--sync",
                "--workflow-id",
                "wf-123",
                "--model",
                "claude-sonnet-4-6",
                "--tool",
                "claude",
                "--phase-timeout-secs",
                "120",
            ],
            "async-run execution overrides must be forwarded to the detached child"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_detached_workflow_runner_registers_child_pid_for_reconciler_liveness() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _guards = isolate_plugin_discovery(&temp);
        let project_root = temp.path().to_string_lossy().to_string();

        let stub = temp.path().join("stub-runner.sh");
        std::fs::write(&stub, "#!/bin/sh\nsleep 30\n").expect("stub should be written");
        let mut perms = std::fs::metadata(&stub).expect("stub metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("stub should be executable");

        let workflow_id = "wf-spawn-liveness";
        let pid = super::spawn_detached_workflow_runner_with_program(
            &stub,
            &project_root,
            workflow_id,
            &super::DetachedRunnerOverrides::default(),
        )
        .expect("detached spawn should succeed");

        let active = orchestrator_core::active_workflow_runner_ids(temp.path()).expect("registry should read");
        assert!(
            active.contains(workflow_id),
            "spawned runner must be registered as live so the orphan reconciler skips the workflow"
        );

        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        let _ = orchestrator_core::unregister_workflow_runner_pid(temp.path(), workflow_id);
    }

    #[tokio::test]
    async fn reject_manual_phase_fails_workflow() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        write_standard_workflow_yaml(&temp);
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
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
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

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
