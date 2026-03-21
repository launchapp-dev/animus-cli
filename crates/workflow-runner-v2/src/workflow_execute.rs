use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::process::Command;

use orchestrator_config::{
    collect_workflow_refs, ensure_pack_execution_requirements, resolve_active_pack_for_workflow_ref,
    resolve_pack_registry, workflow_config::MergeStrategy,
};
use orchestrator_core::{
    dispatch_workflow_event, ensure_workflow_config_compiled, load_workflow_config,
    project_requirement_workflow_status,
    providers::SubjectContext,
    providers::{BuiltinGitProvider, GitProvider},
    register_workflow_runner_pid,
    services::ServiceHub,
    stop_agent_runner_process,
    unregister_workflow_runner_pid, FileServiceHub, OrchestratorTask, OrchestratorWorkflow, PhaseDecisionVerdict,
    SubjectRef, WorkflowEvent, WorkflowRunInput, WorkflowStatus, SUBJECT_KIND_CUSTOM,
};

use crate::ensure_execution_cwd::ensure_execution_cwd;
use crate::phase_executor::{run_workflow_phase, PhaseExecuteOverrides, PhaseExecutionOutcome, PhaseRunParams};
use crate::phase_output::persist_phase_output;

pub enum PhaseEvent<'a> {
    Started { phase_id: &'a str, phase_index: usize, total_phases: usize },
    Decision { phase_id: &'a str, decision: &'a orchestrator_core::PhaseDecision },
    Completed { phase_id: &'a str, duration: Duration, success: bool },
}

pub type PhaseEventCallback = Box<dyn Fn(PhaseEvent<'_>) + Send + Sync>;

pub struct WorkflowExecuteParams {
    pub project_root: String,
    pub workflow_id: Option<String>,
    pub task_id: Option<String>,
    pub requirement_id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub workflow_ref: Option<String>,
    pub input: Option<Value>,
    pub vars: HashMap<String, String>,
    pub model: Option<String>,
    pub tool: Option<String>,
    pub phase_timeout_secs: Option<u64>,
    pub phase_filter: Option<String>,
    pub on_phase_event: Option<PhaseEventCallback>,
    pub hub: Option<Arc<dyn ServiceHub>>,
    pub phase_routing: Option<protocol::PhaseRoutingConfig>,
    pub mcp_config: Option<protocol::McpRuntimeConfig>,
}

pub struct WorkflowExecuteResult {
    pub success: bool,
    pub workflow_id: String,
    pub workflow_ref: String,
    pub workflow_status: WorkflowStatus,
    pub subject_id: String,
    pub execution_cwd: String,
    pub phases_requested: Vec<String>,
    pub phases_completed: usize,
    pub phases_total: usize,
    pub total_duration: Duration,
    pub phase_results: Vec<Value>,
    pub post_success: Value,
}

#[derive(Clone, Default)]
struct WorkflowPhaseInputs {
    dispatch_input: Option<String>,
    schedule_input: Option<String>,
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

fn ensure_workflow_pack_execution_requirements(
    pack_registry: &orchestrator_config::ResolvedPackRegistry,
    workflow_config: &orchestrator_config::WorkflowConfig,
    workflow_ref: &str,
) -> Result<()> {
    let workflow_refs = collect_workflow_refs(&workflow_config.workflows, workflow_ref)
        .with_context(|| format!("failed to resolve workflow activation graph for '{}'", workflow_ref))?;
    let mut validated_pack_ids = HashSet::new();

    for referenced_workflow_ref in workflow_refs {
        let Some(entry) = resolve_active_pack_for_workflow_ref(pack_registry, &referenced_workflow_ref) else {
            continue;
        };
        if !validated_pack_ids.insert(entry.pack_id.to_ascii_lowercase()) {
            continue;
        }
        let Some(pack) = entry.loaded_manifest() else {
            continue;
        };
        ensure_pack_execution_requirements(pack).with_context(|| {
            format!(
                "workflow '{}' cannot activate pack '{}' required by workflow '{}' from {}",
                workflow_ref,
                pack.manifest.id,
                referenced_workflow_ref,
                pack.pack_root.display()
            )
        })?;
    }

    Ok(())
}

fn workflow_phase_inputs(workflow: &OrchestratorWorkflow) -> WorkflowPhaseInputs {
    let dispatch_input = workflow.input.as_ref().map(Value::to_string);
    let schedule_input = if workflow.subject.id().starts_with("schedule:") { dispatch_input.clone() } else { None };

    WorkflowPhaseInputs { dispatch_input, schedule_input }
}

pub async fn execute_workflow(mut params: WorkflowExecuteParams) -> Result<WorkflowExecuteResult> {
    let routing = params.phase_routing.take().unwrap_or_default();
    let phase_timeout_secs = params.phase_timeout_secs;

    let hub: Arc<dyn ServiceHub> = match params.hub {
        Some(ref h) => h.clone(),
        None => {
            Arc::new(FileServiceHub::new(&params.project_root).context("failed to create service hub for project")?)
        }
    };

    let mut workflow = match params.workflow_id.as_deref() {
        Some(workflow_id) => load_existing_workflow(hub.clone(), workflow_id, &params).await?,
        None => {
            let input = resolve_input(&params)?;
            let subject = input.subject().clone();
            let subject_id = subject.id().to_string();
            hub.workflows().run(input).await.or_else(|run_err| {
                if subject.kind().eq_ignore_ascii_case(SUBJECT_KIND_CUSTOM) {
                    return Err(run_err);
                }
                let all =
                    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(hub.workflows().list()))?;
                all.into_iter()
                    .find(|w| w.subject.id() == subject_id || w.task_id == subject_id)
                    .ok_or_else(|| anyhow!("no workflow found for subject '{}'", subject_id))
            })?
        }
    };
    let _runner_pid_guard = WorkflowRunnerPidGuard::register(&params.project_root, &workflow.id)
        .context("failed to register active workflow execution")?;
    let mut subject_context = resolve_execution_subject_context(
        hub.clone(),
        &workflow.subject,
        params.title.as_deref(),
        params.description.as_deref(),
    )
    .await?;
    let mut task = subject_context.task.take();

    let execution_cwd = ensure_execution_cwd(hub.clone(), &params.project_root, &workflow.subject, &subject_context)
        .await
        .context("failed to resolve execution cwd")?;

    if let Some(task_id) = task.as_ref().map(|t| t.id.clone()) {
        task = Some(
            hub.tasks()
                .get(&task_id)
                .await
                .with_context(|| format!("task '{}' not found after cwd preparation", task_id))?,
        );
    }

    if let Some(task) = task.as_ref() {
        subject_context.subject_title = task.title.clone();
        subject_context.subject_description = task.description.clone();
    }

    let phases_to_run: Vec<String> = if let Some(ref phase_filter) = params.phase_filter {
        vec![phase_filter.clone()]
    } else {
        workflow.phases.iter().map(|p| p.phase_id.clone()).collect()
    };

    if phases_to_run.is_empty() {
        return Err(anyhow!("workflow has no phases to execute"));
    }

    if let Err(err) = hub.daemon().start(Default::default()).await {
        eprintln!("warning: failed to auto-start runner for workflow execute: {err}");
    }

    let subject_id_str = workflow.subject.id().to_string();
    let subject_title = subject_context.subject_title.clone();
    let subject_description = subject_context.subject_description.clone();
    let task_complexity = task.as_ref().map(|t| t.complexity);

    ensure_workflow_config_compiled(Path::new(&params.project_root))?;
    let workflow_config = load_workflow_config(Path::new(&params.project_root))?;
    let workflow_ref = workflow.workflow_ref.clone().unwrap_or_else(|| workflow_config.default_workflow_ref.clone());
    let pack_registry = resolve_pack_registry(Path::new(&params.project_root))?;
    ensure_workflow_pack_execution_requirements(&pack_registry, &workflow_config, &workflow_ref)?;
    let phase_inputs = workflow_phase_inputs(&workflow);
    let workflow_vars = workflow.vars.clone();
    let mut rework_context: Option<String> = None;
    let mut results = Vec::new();
    let workflow_start = Instant::now();

    let emit = |event: PhaseEvent<'_>| {
        if let Some(ref cb) = params.on_phase_event {
            cb(event);
        }
    };

    if let Some(phase_filter) = params.phase_filter.clone() {
        let phase_attempt = workflow
            .phases
            .iter()
            .find(|p| p.phase_id.eq_ignore_ascii_case(&phase_filter))
            .map(|p| p.attempt)
            .unwrap_or(0);

        emit(PhaseEvent::Started { phase_id: &phase_filter, phase_index: 0, total_phases: 1 });
        let phase_start = Instant::now();

        let phase_overrides = PhaseExecuteOverrides {
            tool: params.tool.clone(),
            model: params.model.clone(),
            rework_context: rework_context.take(),
        };
        let run_result = run_workflow_phase(&PhaseRunParams {
            project_root: &params.project_root,
            execution_cwd: &execution_cwd,
            workflow_id: &workflow.id,
            workflow_ref: workflow_ref.as_str(),
            subject_id: &subject_id_str,
            subject_title: &subject_title,
            subject_description: &subject_description,
            task_complexity,
            phase_id: &phase_filter,
            phase_attempt,
            overrides: Some(&phase_overrides),
            pipeline_vars: if workflow_vars.is_empty() { None } else { Some(&workflow_vars) },
            dispatch_input: phase_inputs.dispatch_input.as_deref(),
            schedule_input: phase_inputs.schedule_input.as_deref(),
            routing: &routing,

            phase_timeout_secs,
        })
        .await;

        let phase_elapsed = phase_start.elapsed();

        match run_result {
            Ok(result) => {
                if let PhaseExecutionOutcome::Completed { phase_decision: Some(ref decision), .. } = &result.outcome {
                    emit(PhaseEvent::Decision { phase_id: &phase_filter, decision });
                }

                let phase_status = phase_result_status(&result.outcome);
                let _ = persist_phase_output(&params.project_root, &workflow.id, &phase_filter, &result.outcome);
                emit(PhaseEvent::Completed {
                    phase_id: &phase_filter,
                    duration: phase_elapsed,
                    success: phase_status != "failed",
                });
                results.push(serde_json::json!({
                    "phase_id": phase_filter,
                    "status": phase_status,
                    "duration_secs": phase_elapsed.as_secs(),
                    "outcome": result.outcome,
                    "metadata": result.metadata,
                }));

                let total_duration = workflow_start.elapsed();
                return Ok(WorkflowExecuteResult {
                    success: phase_status != "failed",
                    workflow_id: workflow.id.clone(),
                    workflow_ref,
                    workflow_status: workflow.status,
                    subject_id: subject_id_str,
                    execution_cwd,
                    phases_requested: vec![phase_filter],
                    phases_completed: usize::from(phase_status == "completed"),
                    phases_total: 1,
                    total_duration,
                    phase_results: results,
                    post_success: serde_json::json!({
                        "status": "skipped",
                        "reason": "post-success actions are not run for single-phase execution",
                    }),
                });
            }
            Err(err) => {
                emit(PhaseEvent::Completed { phase_id: &phase_filter, duration: phase_elapsed, success: false });
                results.push(serde_json::json!({
                    "phase_id": phase_filter,
                    "status": "failed",
                    "duration_secs": phase_elapsed.as_secs(),
                    "error": err.to_string(),
                }));
                let total_duration = workflow_start.elapsed();
                return Ok(WorkflowExecuteResult {
                    success: false,
                    workflow_id: workflow.id.clone(),
                    workflow_ref,
                    workflow_status: workflow.status,
                    subject_id: subject_id_str,
                    execution_cwd,
                    phases_requested: vec![phase_filter],
                    phases_completed: 0,
                    phases_total: 1,
                    total_duration,
                    phase_results: results,
                    post_success: serde_json::json!({
                        "status": "skipped",
                        "reason": "post-success actions are not run for single-phase execution",
                    }),
                });
            }
        }
    }

    let mut phases_to_run: Vec<String> = workflow.phases.iter().map(|p| p.phase_id.clone()).collect();
    if phases_to_run.is_empty() {
        return Err(anyhow!("workflow has no phases to execute"));
    }

    let mut phase_idx: usize = workflow.current_phase_index;
    let mut reported_workflow_status = workflow.status;
    while phase_idx < phases_to_run.len() && !is_terminal_workflow_status(workflow.status) {
        let phase_id = phases_to_run[phase_idx].clone();
        let phase_attempt = workflow.phases.iter().find(|p| p.phase_id == phase_id).map(|p| p.attempt).unwrap_or(0);

        emit(PhaseEvent::Started { phase_id: &phase_id, phase_index: phase_idx, total_phases: phases_to_run.len() });
        let phase_start = Instant::now();

        let phase_overrides = PhaseExecuteOverrides {
            tool: params.tool.clone(),
            model: params.model.clone(),
            rework_context: rework_context.take(),
        };
        let run_result = run_workflow_phase(&PhaseRunParams {
            project_root: &params.project_root,
            execution_cwd: &execution_cwd,
            workflow_id: &workflow.id,
            workflow_ref: workflow_ref.as_str(),
            subject_id: &subject_id_str,
            subject_title: &subject_title,
            subject_description: &subject_description,
            task_complexity,
            phase_id: &phase_id,
            phase_attempt,
            overrides: Some(&phase_overrides),
            pipeline_vars: if workflow_vars.is_empty() { None } else { Some(&workflow_vars) },
            dispatch_input: phase_inputs.dispatch_input.as_deref(),
            schedule_input: phase_inputs.schedule_input.as_deref(),
            routing: &routing,

            phase_timeout_secs,
        })
        .await;

        let phase_elapsed = phase_start.elapsed();

        match run_result {
            Ok(result) => {
                if let PhaseExecutionOutcome::Completed { phase_decision: Some(ref decision), .. } = &result.outcome {
                    emit(PhaseEvent::Decision { phase_id: &phase_id, decision });
                }

                let _ = persist_phase_output(&params.project_root, &workflow.id, &phase_id, &result.outcome);

                match &result.outcome {
                    PhaseExecutionOutcome::Completed { phase_decision, .. } => {
                        let decision = phase_decision.clone();
                        let updated = hub
                            .workflows()
                            .complete_current_phase_with_decision(&workflow.id, decision.clone())
                            .await?;
                        let next_status = updated.status;
                        let next_phase_index = updated.current_phase_index;
                        let next_phase_id = updated.current_phase.clone().or_else(|| {
                            updated.phases.get(updated.current_phase_index).map(|phase| phase.phase_id.clone())
                        });
                        let maybe_context = phase_rework_context(&result.outcome);
                        workflow = updated;
                        reported_workflow_status = next_status;
                        phases_to_run = workflow.phases.iter().map(|phase| phase.phase_id.clone()).collect();

                        let status = phase_result_status(&result.outcome).to_string();
                        let next_success = !matches!(next_status, WorkflowStatus::Failed | WorkflowStatus::Escalated);
                        emit(PhaseEvent::Completed {
                            phase_id: &phase_id,
                            duration: phase_elapsed,
                            success: next_success,
                        });
                        let mut result_value = serde_json::json!({
                            "phase_id": phase_id,
                            "status": status,
                            "duration_secs": phase_elapsed.as_secs(),
                            "workflow_status": format!("{:?}", next_status).to_ascii_lowercase(),
                            "outcome": result.outcome,
                            "metadata": result.metadata,
                        });
                        if let Some(next_phase_id) = next_phase_id {
                            result_value["next_phase_id"] = serde_json::json!(next_phase_id);
                        }
                        if matches!(decision.as_ref().map(|value| value.verdict), Some(PhaseDecisionVerdict::Skip)) {
                            result_value["close_reason"] = serde_json::json!(decision
                                .as_ref()
                                .map(|value| value.reason.clone())
                                .unwrap_or_default());
                        }
                        results.push(result_value);

                        if matches!(
                            workflow.status,
                            WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled
                        ) {
                            break;
                        }

                        rework_context = maybe_context;
                        phase_idx = next_phase_index;
                        continue;
                    }
                    PhaseExecutionOutcome::ManualPending { .. } => {
                        let outcome = dispatch_workflow_event(
                            hub.clone(),
                            &params.project_root,
                            WorkflowEvent::Pause { workflow_id: workflow.id.clone() },
                        )
                        .await?;
                        workflow = outcome
                            .workflow
                            .ok_or_else(|| anyhow!("workflow '{}' not found for manual pause", workflow.id))?;
                        reported_workflow_status = workflow.status;
                        emit(PhaseEvent::Completed { phase_id: &phase_id, duration: phase_elapsed, success: true });
                        results.push(serde_json::json!({
                            "phase_id": phase_id,
                            "status": "manual_pending",
                            "duration_secs": phase_elapsed.as_secs(),
                            "workflow_status": format!("{:?}", workflow.status).to_ascii_lowercase(),
                            "outcome": result.outcome,
                            "metadata": result.metadata,
                        }));
                        break;
                    }
                }
            }
            Err(err) => {
                workflow = hub.workflows().fail_current_phase(&workflow.id, err.to_string()).await?;
                reported_workflow_status = workflow.status;
                emit(PhaseEvent::Completed { phase_id: &phase_id, duration: phase_elapsed, success: false });
                results.push(serde_json::json!({
                    "phase_id": phase_id,
                    "status": "failed",
                    "duration_secs": phase_elapsed.as_secs(),
                    "workflow_status": format!("{:?}", workflow.status).to_ascii_lowercase(),
                    "error": err.to_string(),
                }));
                break;
            }
        }
    }

    let total_duration = workflow_start.elapsed();
    let mut post_success = serde_json::json!({
        "status": "skipped",
        "reason": "workflow did not complete all phases",
    });
    if workflow.status == WorkflowStatus::Completed {
        project_requirement_success_status(hub.clone(), &workflow.subject, &workflow_ref).await?;
        post_success = if let Some(ref t) = task {
            execute_post_success_actions(&params.project_root, t, &workflow, &workflow_config, &execution_cwd).await
        } else {
            serde_json::json!({
                "status": "skipped",
                "reason": "post-success actions require a task subject",
            })
        };

        match post_success["status"].as_str() {
            Some("conflict") => {
                let reason = post_success_failure_reason(&post_success)
                    .unwrap_or_else(|| "post-success merge conflict".to_string());
                workflow = hub.workflows().mark_merge_conflict(&workflow.id, reason).await?;
                reported_workflow_status = workflow.status;
            }
            Some("failed") => {
                let reason = post_success_failure_reason(&post_success)
                    .unwrap_or_else(|| "post-success action failed".to_string());
                workflow = hub.workflows().mark_completed_failed(&workflow.id, reason).await?;
                reported_workflow_status = workflow.status;
            }
            _ => {}
        }
    }

    Ok(WorkflowExecuteResult {
        success: workflow_exit_success(reported_workflow_status),
        workflow_id: workflow.id.clone(),
        workflow_ref,
        workflow_status: reported_workflow_status,
        subject_id: subject_id_str,
        execution_cwd,
        phases_requested: phases_to_run.clone(),
        phases_completed: workflow.phases.iter().filter(|phase| phase.completed_at.is_some()).count(),
        phases_total: phases_to_run.len(),
        total_duration,
        phase_results: results,
        post_success,
    })
}

async fn load_existing_workflow(
    hub: Arc<dyn ServiceHub>,
    workflow_id: &str,
    params: &WorkflowExecuteParams,
) -> Result<OrchestratorWorkflow> {
    let workflow =
        hub.workflows().get(workflow_id).await.with_context(|| format!("workflow '{}' not found", workflow_id))?;

    if workflow.status != WorkflowStatus::Running {
        return Err(anyhow!(
            "workflow '{}' is not runnable (status: {})",
            workflow_id,
            format!("{:?}", workflow.status).to_ascii_lowercase()
        ));
    }

    validate_existing_workflow_subject(&workflow, params)?;
    Ok(workflow)
}

fn validate_existing_workflow_subject(workflow: &OrchestratorWorkflow, params: &WorkflowExecuteParams) -> Result<()> {
    if let Some(task_id) = params.task_id.as_deref() {
        let workflow_task_id = workflow.subject.task_id().unwrap_or_else(|| workflow.task_id.as_str());
        if workflow_task_id != task_id {
            return Err(anyhow!("workflow '{}' is for task '{}' not '{}'", workflow.id, workflow_task_id, task_id));
        }
    }

    if let Some(requirement_id) = params.requirement_id.as_deref() {
        match workflow.subject.requirement_id() {
            Some(id) if id == requirement_id => {}
            Some(id) => {
                return Err(anyhow!("workflow '{}' is for requirement '{}' not '{}'", workflow.id, id, requirement_id));
            }
            None => {
                return Err(anyhow!("workflow '{}' is not a requirement workflow", workflow.id));
            }
        }
    }

    if let Some(title) = params.title.as_deref() {
        if !workflow.subject.kind().eq_ignore_ascii_case(SUBJECT_KIND_CUSTOM) {
            return Err(anyhow!("workflow '{}' is not a custom workflow", workflow.id));
        }
        let actual = workflow.subject.title.as_deref().unwrap_or_else(|| workflow.subject.id());
        if actual != title {
            return Err(anyhow!("workflow '{}' is for custom subject '{}' not '{}'", workflow.id, actual, title));
        }
    }

    Ok(())
}

fn resolve_input(params: &WorkflowExecuteParams) -> Result<WorkflowRunInput> {
    let workflow_ref = params.workflow_ref.clone();
    match (&params.task_id, &params.requirement_id, &params.title) {
        (Some(task_id), _, _) => Ok(WorkflowRunInput::for_task(task_id.clone(), workflow_ref)
            .with_input(params.input.clone())
            .with_vars(params.vars.clone())),
        (None, Some(req_id), _) => Ok(WorkflowRunInput::for_requirement(req_id.clone(), workflow_ref)
            .with_input(params.input.clone())
            .with_vars(params.vars.clone())),
        (None, None, Some(title)) => Ok(WorkflowRunInput::for_custom(
            title.clone(),
            params.description.clone().unwrap_or_default(),
            workflow_ref,
        )
        .with_input(params.input.clone())
        .with_vars(params.vars.clone())),
        _ => Err(anyhow!("one of --task-id, --requirement-id, or --title must be provided")),
    }
}

async fn resolve_execution_subject_context(
    hub: Arc<dyn ServiceHub>,
    subject: &SubjectRef,
    fallback_title: Option<&str>,
    fallback_description: Option<&str>,
) -> Result<SubjectContext> {
    hub.subject_resolver()
        .resolve_subject_context(subject, fallback_title, fallback_description)
        .await
        .with_context(|| format!("failed to resolve subject context for '{}'", subject.id()))
}

async fn project_requirement_success_status(
    hub: Arc<dyn ServiceHub>,
    subject: &SubjectRef,
    workflow_ref: &str,
) -> Result<()> {
    let Some(id) = subject.requirement_id() else {
        return Ok(());
    };

    project_requirement_workflow_status(hub, id, workflow_ref).await
}

fn phase_rework_context(outcome: &PhaseExecutionOutcome) -> Option<String> {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision: Some(decision), .. }
            if matches!(decision.verdict, PhaseDecisionVerdict::Rework) =>
        {
            Some(decision.reason.clone())
        }
        _ => None,
    }
}

fn is_terminal_workflow_status(status: WorkflowStatus) -> bool {
    matches!(
        status,
        WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled
    )
}

fn workflow_exit_success(status: WorkflowStatus) -> bool {
    !matches!(status, WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled)
}

fn phase_result_status(outcome: &PhaseExecutionOutcome) -> &'static str {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision: Some(decision), .. } => match decision.verdict {
            PhaseDecisionVerdict::Advance | PhaseDecisionVerdict::Unknown => "completed",
            PhaseDecisionVerdict::Rework => "rework",
            PhaseDecisionVerdict::Fail => "failed",
            PhaseDecisionVerdict::Skip => "closed",
        },
        PhaseExecutionOutcome::Completed { phase_decision: None, .. } => "completed",
        PhaseExecutionOutcome::ManualPending { .. } => "manual_pending",
    }
}

fn post_success_failure_reason(post_success: &Value) -> Option<String> {
    post_success
        .get("error")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| post_success.get("reason").and_then(Value::as_str).map(ToOwned::to_owned))
        .or_else(|| {
            post_success.get("actions").and_then(Value::as_object).and_then(|actions| {
                actions.values().find_map(|action| {
                    if action.get("status").and_then(Value::as_str) == Some("failed")
                        || action.get("status").and_then(Value::as_str) == Some("conflict")
                    {
                        action.get("error").and_then(Value::as_str).map(ToOwned::to_owned)
                    } else {
                        None
                    }
                })
            })
        })
}

async fn execute_post_success_actions(
    project_root: &str,
    task: &OrchestratorTask,
    workflow: &OrchestratorWorkflow,
    workflow_config: &orchestrator_core::WorkflowConfig,
    execution_cwd: &str,
) -> Value {
    let workflow_ref = workflow.workflow_ref.as_deref().unwrap_or(workflow_config.default_workflow_ref.as_str());
    let workflow_def = workflow_config
        .workflows
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(workflow_ref))
        .or_else(|| workflow_config.workflows.iter().find(|p| p.id.eq_ignore_ascii_case("standard")))
        .or_else(|| {
            workflow_config.workflows.iter().find(|p| p.id.eq_ignore_ascii_case(&workflow_config.default_workflow_ref))
        })
        .cloned();

    let Some(workflow_def) = workflow_def else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "workflow configuration not found",
        });
    };

    let Some(merge_cfg) = workflow_def.post_success.and_then(|post_success| post_success.merge) else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "post_success.merge not configured",
            "workflow_ref": workflow_def.id,
        });
    };

    let Some(source_branch) = resolve_source_branch(task, execution_cwd).await else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "unable to resolve source branch",
            "workflow_ref": workflow_def.id,
        });
    };

    let git_provider = Arc::new(BuiltinGitProvider::new(project_root));
    let target_branch = merge_cfg.target_branch.clone();

    let mut action_result = serde_json::json!({
        "status": "skipped",
        "workflow_ref": workflow_def.id,
        "target_branch": target_branch,
        "strategy": merge_strategy_name(&merge_cfg.strategy),
        "create_pr": merge_cfg.create_pr,
        "auto_merge": merge_cfg.auto_merge,
        "cleanup_worktree": merge_cfg.cleanup_worktree,
        "actions": {
            "push": { "status": "skipped" },
            "create_pr": { "status": "skipped" },
            "merge": { "status": "skipped" },
            "cleanup_worktree": { "status": "skipped" },
        },
    });

    if merge_cfg.create_pr {
        if let Some(push_action) =
            perform_push_with_fallback(&*git_provider, execution_cwd, "origin", &source_branch).await
        {
            action_result["actions"]["push"] = push_action;
        }

        let title = if task.title.trim().is_empty() {
            format!("[{}] Automated update", task.id)
        } else {
            format!("[{}] {}", task.id, task.title.trim())
        };
        let body = if task.description.trim().is_empty() {
            format!("Automated update for task {}.", task.id)
        } else {
            format!("Automated update for task {}.\n\n{}", task.id, task.description.trim())
        };
        action_result["actions"]["create_pr"] =
            create_pull_request_via_gh(task, project_root, &target_branch, &source_branch, &title, &body).await;
        let pr_status = action_result["actions"]["create_pr"]["status"].clone();
        action_result["status"] = pr_status;
        action_result["source_branch"] = serde_json::json!(source_branch);
        if merge_cfg.cleanup_worktree {
            action_result["actions"]["cleanup_worktree"] =
                cleanup_worktree_with_fallback(&*git_provider, project_root, task).await;
        }
        return action_result;
    }

    if merge_cfg.auto_merge {
        action_result["actions"]["merge"] = perform_auto_merge_with_git(
            project_root,
            execution_cwd,
            &source_branch,
            &target_branch,
            &merge_cfg.strategy,
        )
        .await;
        action_result["status"] = action_result["actions"]["merge"]["status"].clone();
    }

    action_result["source_branch"] = serde_json::json!(source_branch);
    if merge_cfg.cleanup_worktree {
        action_result["actions"]["cleanup_worktree"] =
            cleanup_worktree_with_fallback(&*git_provider, project_root, task).await;
        if action_result["actions"]["cleanup_worktree"]["status"] == "completed" && action_result["status"] == "skipped"
        {
            action_result["status"] = serde_json::json!("completed");
        }
    }
    action_result
}

async fn resolve_source_branch(task: &OrchestratorTask, execution_cwd: &str) -> Option<String> {
    if let Some(branch) = task.branch_name.as_deref().map(str::trim).filter(|branch| !branch.is_empty()) {
        return Some(branch.to_string());
    }

    if execution_cwd.is_empty() || !Path::new(execution_cwd).exists() {
        return None;
    }

    let output = run_git_output("git", execution_cwd, &["branch", "--show-current"]).await.ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn merge_strategy_name(strategy: &MergeStrategy) -> &'static str {
    match strategy {
        MergeStrategy::Squash => "squash",
        MergeStrategy::Merge => "merge",
        MergeStrategy::Rebase => "rebase",
    }
}

fn command_summary(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

fn looks_like_merge_conflict(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("merge conflict")
        || text.contains("conflict")
        || text.contains("automatic merge failed")
        || text.contains("merge blocked")
}

async fn run_git_output(program: &str, cwd: &str, args: &[&str]) -> Result<std::process::Output> {
    Command::new(program)
        .current_dir(cwd)
        .args(args)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ACCESS_TOKEN")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .await
        .with_context(|| format!("failed to run command {program} in {cwd}"))
}

async fn perform_push_with_fallback(
    git_provider: &dyn GitProvider,
    execution_cwd: &str,
    remote: &str,
    branch: &str,
) -> Option<Value> {
    match git_provider.push_branch(execution_cwd, remote, branch).await {
        Ok(_) => Some(serde_json::json!({
            "status": "completed",
            "method": "git-provider",
            "branch": branch,
            "remote": remote,
        })),
        Err(provider_error) => {
            let direct = run_git_output("git", execution_cwd, &["push", remote, branch]).await;
            match direct {
                Ok(output) if output.status.success() => Some(serde_json::json!({
                    "status": "completed",
                    "method": "git-direct",
                    "branch": branch,
                    "remote": remote,
                    "provider_error": provider_error.to_string(),
                })),
                Ok(output) => Some(serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "branch": branch,
                    "remote": remote,
                    "error": command_summary(&output),
                    "provider_error": provider_error.to_string(),
                })),
                Err(command_error) => Some(serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "branch": branch,
                    "remote": remote,
                    "error": command_error.to_string(),
                    "provider_error": provider_error.to_string(),
                })),
            }
        }
    }
}

async fn create_pull_request_via_gh(
    task: &OrchestratorTask,
    execution_cwd: &str,
    target_branch: &str,
    source_branch: &str,
    title: &str,
    body: &str,
) -> Value {
    let args = ["pr", "create", "--base", target_branch, "--head", source_branch, "--title", title, "--body", body];
    match run_git_output("gh", execution_cwd, &args).await {
        Ok(output) if output.status.success() => {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            serde_json::json!({
                "status": "completed",
                "method": "gh",
                "task_id": task.id,
                "source_branch": source_branch,
                "target_branch": target_branch,
                "url": if url.is_empty() { None::<String> } else { Some(url) },
            })
        }
        Ok(output) => {
            let message = command_summary(&output);
            if message.to_ascii_lowercase().contains("already exists")
                || message.to_ascii_lowercase().contains("already open")
            {
                serde_json::json!({
                    "status": "completed",
                    "method": "gh",
                    "task_id": task.id,
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "error": message,
                })
            } else {
                serde_json::json!({
                    "status": "failed",
                    "method": "gh",
                    "task_id": task.id,
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "error": message,
                })
            }
        }
        Err(error) => serde_json::json!({
            "status": "failed",
            "method": "gh",
            "task_id": task.id,
            "source_branch": source_branch,
            "target_branch": target_branch,
            "error": error.to_string(),
        }),
    }
}

async fn checkout_target_branch(git_cwd: &str, target_branch: &str) -> Result<()> {
    let checkout_output = run_git_output("git", git_cwd, &["checkout", target_branch]).await;
    match checkout_output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let primary_error = command_summary(&output);
            let fallback_ref = format!("origin/{target_branch}");
            let fallback =
                run_git_output("git", git_cwd, &["checkout", "-b", target_branch, fallback_ref.as_str()]).await;
            match fallback {
                Ok(fb_output) if fb_output.status.success() => Ok(()),
                Ok(fb_output) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {primary_error}; fallback failed: {}",
                    command_summary(&fb_output),
                ),
                Err(fb_err) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {primary_error}; fallback failed: {fb_err}",
                ),
            }
        }
        Err(error) => {
            let fallback_ref = format!("origin/{target_branch}");
            let fallback =
                run_git_output("git", git_cwd, &["checkout", "-b", target_branch, fallback_ref.as_str()]).await;
            match fallback {
                Ok(fb_output) if fb_output.status.success() => Ok(()),
                Ok(fb_output) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {error}; fallback failed: {}",
                    command_summary(&fb_output),
                ),
                Err(fb_err) => anyhow::bail!(
                    "failed to checkout target branch '{target_branch}': {error}; fallback failed: {fb_err}",
                ),
            }
        }
    }
}

fn parse_worktree_path_for_branch(raw: &str, target_branch: &str) -> Option<String> {
    let target_ref = format!("refs/heads/{target_branch}");
    let mut current_path: Option<String> = None;
    let mut current_branch: Option<String> = None;

    for line in raw.lines().chain(std::iter::once("")) {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.trim().to_string());
            continue;
        }
        if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = Some(branch.trim().to_string());
            continue;
        }
        if line.trim().is_empty() {
            if current_branch.as_deref() == Some(target_ref.as_str()) {
                return current_path;
            }
            current_path = None;
            current_branch = None;
        }
    }

    None
}

async fn resolve_target_merge_cwd(project_root: &str, target_branch: &str) -> Result<String> {
    let worktree_list = run_git_output("git", project_root, &["worktree", "list", "--porcelain"]).await?;
    if !worktree_list.status.success() {
        anyhow::bail!(
            "failed to inspect git worktrees while resolving target branch '{}': {}",
            target_branch,
            command_summary(&worktree_list)
        );
    }

    let stdout = String::from_utf8_lossy(&worktree_list.stdout);
    if let Some(path) = parse_worktree_path_for_branch(&stdout, target_branch) {
        return Ok(path);
    }

    checkout_target_branch(project_root, target_branch).await?;
    Ok(project_root.to_string())
}

async fn current_branch(cwd: &str) -> Result<String> {
    let output = run_git_output("git", cwd, &["branch", "--show-current"]).await?;
    if !output.status.success() {
        anyhow::bail!("failed to resolve current branch in {}: {}", cwd, command_summary(&output));
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        anyhow::bail!("git reported an empty current branch in {}", cwd);
    }

    Ok(branch)
}

async fn perform_rebase_strategy(
    source_execution_cwd: &str,
    target_execution_cwd: &str,
    source_branch: &str,
    target_branch: &str,
) -> Value {
    let current_source_branch = match current_branch(source_execution_cwd).await {
        Ok(branch) => branch,
        Err(error) => {
            return serde_json::json!({
                "status": "failed",
                "method": "git",
                "source_branch": source_branch,
                "target_branch": target_branch,
                "strategy": "rebase",
                "error": error.to_string(),
            });
        }
    };

    if current_source_branch != source_branch {
        return serde_json::json!({
            "status": "failed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": "rebase",
            "error": format!(
                "source execution cwd '{}' is on branch '{}' instead of '{}'",
                source_execution_cwd, current_source_branch, source_branch
            ),
        });
    }

    let rebase_output = run_git_output("git", source_execution_cwd, &["rebase", target_branch]).await;
    match rebase_output {
        Ok(output) if output.status.success() => {
            let ff_merge = run_git_output("git", target_execution_cwd, &["merge", "--ff-only", source_branch]).await;
            match ff_merge {
                Ok(merge_out) if merge_out.status.success() => serde_json::json!({
                    "status": "completed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": "rebase",
                }),
                Ok(merge_out) => serde_json::json!({
                    "status": "failed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": "rebase",
                    "error": format!("rebase succeeded but ff-merge failed: {}", command_summary(&merge_out)),
                }),
                Err(err) => serde_json::json!({
                    "status": "failed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": "rebase",
                    "error": format!("rebase succeeded but ff-merge failed: {err}"),
                }),
            }
        }
        Ok(output) => {
            let _ = run_git_output("git", source_execution_cwd, &["rebase", "--abort"]).await;
            let summary = command_summary(&output);
            let status = if looks_like_merge_conflict(&summary) { "conflict" } else { "failed" };
            serde_json::json!({
                "status": status,
                "method": "git",
                "source_branch": source_branch,
                "target_branch": target_branch,
                "strategy": "rebase",
                "error": summary,
            })
        }
        Err(error) => serde_json::json!({
            "status": "failed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": "rebase",
            "error": error.to_string(),
        }),
    }
}

async fn has_staged_changes(cwd: &str) -> Result<bool> {
    let output = run_git_output("git", cwd, &["diff", "--cached", "--quiet"]).await?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => anyhow::bail!("failed to inspect staged changes in {}: {}", cwd, command_summary(&output)),
    }
}

async fn perform_auto_merge_with_git(
    project_root: &str,
    source_execution_cwd: &str,
    source_branch: &str,
    target_branch: &str,
    strategy: &MergeStrategy,
) -> Value {
    let target_execution_cwd = match resolve_target_merge_cwd(project_root, target_branch).await {
        Ok(cwd) => cwd,
        Err(error) => {
            return serde_json::json!({
                "status": "failed",
                "method": "git",
                "source_branch": source_branch,
                "target_branch": target_branch,
                "strategy": merge_strategy_name(strategy),
                "error": error.to_string(),
            });
        }
    };

    if matches!(strategy, MergeStrategy::Rebase) {
        return perform_rebase_strategy(source_execution_cwd, &target_execution_cwd, source_branch, target_branch)
            .await;
    }

    let mut merge_args: Vec<&str> = vec!["merge"];
    match strategy {
        MergeStrategy::Squash => merge_args.push("--squash"),
        MergeStrategy::Merge => merge_args.push("--no-ff"),
        MergeStrategy::Rebase => unreachable!(),
    };
    merge_args.push("--no-edit");
    merge_args.push(source_branch);

    let output = run_git_output("git", &target_execution_cwd, &merge_args).await;
    match output {
        Ok(output) if output.status.success() && matches!(strategy, MergeStrategy::Squash) => {
            match has_staged_changes(&target_execution_cwd).await {
                Ok(true) => {
                    let message = format!("Squash merge branch '{source_branch}' into '{target_branch}'");
                    let commit =
                        run_git_output("git", &target_execution_cwd, &["commit", "-m", message.as_str()]).await;
                    match commit {
                        Ok(commit_output) if commit_output.status.success() => serde_json::json!({
                            "status": "completed",
                            "method": "git",
                            "source_branch": source_branch,
                            "target_branch": target_branch,
                            "strategy": merge_strategy_name(strategy),
                        }),
                        Ok(commit_output) => serde_json::json!({
                            "status": "failed",
                            "method": "git",
                            "source_branch": source_branch,
                            "target_branch": target_branch,
                            "strategy": merge_strategy_name(strategy),
                            "error": format!("squash merge staged changes but commit failed: {}", command_summary(&commit_output)),
                        }),
                        Err(error) => serde_json::json!({
                            "status": "failed",
                            "method": "git",
                            "source_branch": source_branch,
                            "target_branch": target_branch,
                            "strategy": merge_strategy_name(strategy),
                            "error": format!("squash merge staged changes but commit failed: {error}"),
                        }),
                    }
                }
                Ok(false) => serde_json::json!({
                    "status": "completed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": merge_strategy_name(strategy),
                    "result": "no-op",
                }),
                Err(error) => serde_json::json!({
                    "status": "failed",
                    "method": "git",
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "strategy": merge_strategy_name(strategy),
                    "error": error.to_string(),
                }),
            }
        }
        Ok(output) if output.status.success() => serde_json::json!({
            "status": "completed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": merge_strategy_name(strategy),
        }),
        Ok(output) => {
            let summary = command_summary(&output);
            let status = if looks_like_merge_conflict(&summary) { "conflict" } else { "failed" };
            serde_json::json!({
                "status": status,
                "method": "git",
                "source_branch": source_branch,
                "target_branch": target_branch,
                "strategy": merge_strategy_name(strategy),
                "error": summary,
            })
        }
        Err(error) => serde_json::json!({
            "status": "failed",
            "method": "git",
            "source_branch": source_branch,
            "target_branch": target_branch,
            "strategy": merge_strategy_name(strategy),
            "error": error.to_string(),
        }),
    }
}

async fn cleanup_worktree_with_fallback(
    git_provider: &dyn GitProvider,
    project_root: &str,
    task: &OrchestratorTask,
) -> Value {
    let Some(worktree_path) = task.worktree_path.as_deref().filter(|path| !path.trim().is_empty()) else {
        return serde_json::json!({
            "status": "skipped",
            "reason": "worktree path not available",
        });
    };

    // Stop the scoped agent-runner for this worktree before removing it.
    // Without this, each removed worktree leaves its agent-runner process running
    // as an orphan, causing a process leak that accumulates over time.
    let runner_stopped = match stop_agent_runner_process(Path::new(worktree_path)).await {
        Ok(true) => {
            tracing::info!(worktree_path, "Stopped scoped agent-runner before worktree removal");
            true
        }
        Ok(false) => {
            tracing::debug!(worktree_path, "No scoped agent-runner found for worktree");
            false
        }
        Err(e) => {
            tracing::warn!(worktree_path, error = %e, "Failed to stop scoped agent-runner; proceeding with worktree removal");
            false
        }
    };

    match git_provider.remove_worktree(project_root, worktree_path).await {
        Ok(()) => serde_json::json!({
            "status": "completed",
            "method": "git-provider",
            "worktree_path": worktree_path,
            "runner_stopped": runner_stopped,
        }),
        Err(provider_error) => {
            let output = run_git_output("git", project_root, &["worktree", "remove", worktree_path, "--force"]).await;
            match output {
                Ok(output) if output.status.success() => serde_json::json!({
                    "status": "completed",
                    "method": "git-direct",
                    "worktree_path": worktree_path,
                    "runner_stopped": runner_stopped,
                }),
                Ok(output) => serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "worktree_path": worktree_path,
                    "error": command_summary(&output),
                    "provider_error": provider_error.to_string(),
                    "runner_stopped": runner_stopped,
                }),
                Err(error) => serde_json::json!({
                    "status": "failed",
                    "method": "git-direct",
                    "worktree_path": worktree_path,
                    "error": error.to_string(),
                    "provider_error": provider_error.to_string(),
                    "runner_stopped": runner_stopped,
                }),
            }
        }
    }
}

#[cfg(test)]
mod requirement_workflow_tests {
    use super::{execute_workflow, workflow_exit_success, WorkflowExecuteParams};
    use orchestrator_core::{
        load_agent_runtime_config, services::ServiceHub, write_agent_runtime_config, FileServiceHub,
        PhaseExecutionMode, PhaseManualDefinition, Priority, TaskCreateInput, TaskStatus, TaskType, WorkflowRunInput,
        WorkflowStatus,
    };
    use std::collections::HashMap;
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
    async fn execute_workflow_pauses_manual_pending_workflows() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "manual gate".to_string(),
                description: "waits for approval".to_string(),
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

        let current_phase = workflow.current_phase.clone().expect("workflow should have a current phase");
        let mut runtime = load_agent_runtime_config(temp.path()).expect("runtime config");
        let mut definition = runtime.phase_execution(&current_phase).cloned().expect("current phase should exist");
        definition.mode = PhaseExecutionMode::Manual;
        definition.agent_id = None;
        definition.command = None;
        definition.manual = Some(PhaseManualDefinition {
            instructions: "Wait for approval".to_string(),
            approval_note_required: false,
            timeout_secs: None,
        });
        runtime.phases.insert(current_phase.clone(), definition);
        write_agent_runtime_config(temp.path(), &runtime).expect("runtime config should write");

        let result = execute_workflow(WorkflowExecuteParams {
            project_root: project_root.clone(),
            workflow_id: None,
            task_id: Some(task.id.clone()),
            requirement_id: None,
            title: None,
            description: None,
            workflow_ref: None,
            input: None,
            vars: HashMap::new(),
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
        .expect("workflow execution should succeed");

        assert!(result.success, "manual wait should not exit as a runner failure");
        assert_eq!(result.workflow_status, WorkflowStatus::Paused);
        assert_eq!(result.phase_results[0]["status"].as_str(), Some("manual_pending"));
        assert_eq!(result.phase_results[0]["workflow_status"].as_str(), Some("paused"));

        let updated = hub.workflows().get(&result.workflow_id).await.expect("workflow should reload");
        assert_eq!(updated.status, WorkflowStatus::Paused);
    }

    #[test]
    fn cancelled_workflows_exit_unsuccessfully() {
        assert!(workflow_exit_success(WorkflowStatus::Completed));
        assert!(workflow_exit_success(WorkflowStatus::Paused));
        assert!(!workflow_exit_success(WorkflowStatus::Cancelled));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use orchestrator_core::{
        InMemoryServiceHub, RequirementItem, RequirementLinks, RequirementPriority, RequirementStatus,
        REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
    };

    #[tokio::test]
    async fn resolve_execution_subject_context_uses_requirement_metadata() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let now = Utc::now();

        hub.planning()
            .upsert_requirement(RequirementItem {
                id: "REQ-123".to_string(),
                title: "Generate linked tasks".to_string(),
                description: "Create implementation-ready tasks from this requirement.".to_string(),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: vec!["Derived tasks exist".to_string()],
                priority: RequirementPriority::Should,
                status: RequirementStatus::Refined,
                source: "test".to_string(),
                tags: Vec::new(),
                links: RequirementLinks::default(),
                comments: Vec::new(),
                relative_path: None,
                linked_task_ids: Vec::new(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("upsert requirement");

        let context = resolve_execution_subject_context(
            hub as Arc<dyn ServiceHub>,
            &SubjectRef::requirement("REQ-123".to_string()),
            None,
            None,
        )
        .await
        .expect("resolve requirement context");

        assert_eq!(context.subject_title, "Generate linked tasks");
        assert_eq!(context.subject_description, "Create implementation-ready tasks from this requirement.");
        assert!(context.task.is_none());
    }

    #[tokio::test]
    async fn project_requirement_success_status_projects_planned_for_plan_workflow() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let now = Utc::now();

        hub.planning()
            .upsert_requirement(RequirementItem {
                id: "REQ-200".to_string(),
                title: "Plan requirement".to_string(),
                description: "Requirement lifecycle parity".to_string(),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: Vec::new(),
                priority: RequirementPriority::Should,
                status: RequirementStatus::Refined,
                source: "test".to_string(),
                tags: Vec::new(),
                links: RequirementLinks::default(),
                comments: Vec::new(),
                relative_path: None,
                linked_task_ids: Vec::new(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("upsert requirement");

        project_requirement_success_status(
            hub.clone(),
            &SubjectRef::requirement("REQ-200".to_string()),
            REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
        )
        .await
        .expect("projection should succeed");

        let updated = hub.planning().get_requirement("REQ-200").await.expect("requirement should exist");
        assert_eq!(updated.status, RequirementStatus::Planned);
    }

    #[tokio::test]
    async fn project_requirement_success_status_projects_in_progress_for_run_workflow() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let now = Utc::now();

        hub.planning()
            .upsert_requirement(RequirementItem {
                id: "REQ-201".to_string(),
                title: "Run requirement".to_string(),
                description: "Requirement lifecycle parity".to_string(),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: Vec::new(),
                priority: RequirementPriority::Should,
                status: RequirementStatus::Refined,
                source: "test".to_string(),
                tags: Vec::new(),
                links: RequirementLinks::default(),
                comments: Vec::new(),
                relative_path: None,
                linked_task_ids: Vec::new(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("upsert requirement");

        project_requirement_success_status(
            hub.clone(),
            &SubjectRef::requirement("REQ-201".to_string()),
            REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
        )
        .await
        .expect("projection should succeed");

        let updated = hub.planning().get_requirement("REQ-201").await.expect("requirement should exist");
        assert_eq!(updated.status, RequirementStatus::InProgress);
    }
}

#[cfg(all(test, unix))]
mod plugin_pack_fixture_tests {
    use super::{execute_workflow, WorkflowExecuteParams};
    use orchestrator_config::{
        activate_pack_mcp_overlay, load_pack_manifest, load_pack_mcp_overlay, load_workflow_config_with_metadata,
        machine_installed_packs_dir, workflow_config::builtin_workflow_config, PackRuntimeCheckStatus,
    };
    use serde_json::Value;
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let original = env::var(key).ok();
            env::set_var(key, value);
            Self { key, original }
        }

        fn set_raw(key: &'static str, value: &str) -> Self {
            let original = env::var(key).ok();
            env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.as_deref() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).expect("fixture executable should be written");
        let mut perms = fs::metadata(path).expect("fixture executable metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("fixture executable should be chmod +x");
    }

    fn write_project_runtime_scripts(project_root: &Path) {
        fs::create_dir_all(project_root.join("scripts")).expect("scripts dir should exist");
        fs::write(project_root.join("scripts/node-fixture.js"), "console.log('node fixture');\n")
            .expect("node fixture source should be written");
        fs::write(project_root.join("scripts/python-fixture.py"), "print('python fixture')\n")
            .expect("python fixture source should be written");
    }

    fn write_runtime_pack_fixture(
        root: &Path,
        pack_id: &str,
        runtime_kind: &str,
        runtime_version: &str,
        runtime_binary_name: &str,
        phase_id: &str,
        workflow_ref: &str,
        project_script: &str,
        output_file_name: &str,
        include_mcp_overlay: bool,
    ) {
        fs::create_dir_all(root.join("workflows")).expect("pack workflows dir should exist");
        fs::create_dir_all(root.join("runtime")).expect("pack runtime dir should exist");
        fs::create_dir_all(root.join("bin")).expect("pack bin dir should exist");
        if include_mcp_overlay {
            fs::create_dir_all(root.join("mcp")).expect("pack mcp dir should exist");
        }

        let runtime_binary = root.join("bin").join(runtime_binary_name);
        write_executable(
            &runtime_binary,
            &format!(
                r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "{runtime_version}"
  exit 0
fi
script_path="$1"
output_path="$2"
subject_id="$3"
printf '{runtime_kind}:%s\n' "$script_path" > "$output_path"
printf '{{"kind":"phase_result","runtime":"{runtime_kind}","script":"%s","subject_id":"%s"}}\n' "$script_path" "$subject_id"
"#
            ),
        );

        let manifest = format!(
            r#"
schema = "ao.pack.v1"
id = "{pack_id}"
version = "0.1.0"
kind = "domain-pack"
title = "{pack_id}"
description = "{runtime_kind} subprocess fixture."

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["custom"]
default_kind = "custom"

[workflows]
root = "workflows"
exports = ["{workflow_ref}"]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[[runtime.requirements]]
runtime = "{runtime_kind}"
binary = "{runtime_binary_name}"
version = ">=0.0.1"
optional = false
reason = "Fixture pack validates external runtime probing."

{mcp_block}
[permissions]
tools = ["{runtime_binary_name}"]
{mcp_permissions}
"#,
            mcp_block = if include_mcp_overlay {
                r#"[mcp]
servers = "mcp/servers.toml"
tools = "mcp/tools.toml"
"#
            } else {
                ""
            },
            mcp_permissions =
                if include_mcp_overlay { format!(r#"mcp_namespaces = ["{pack_id}"]"#) } else { String::new() },
        );
        fs::write(root.join(orchestrator_config::PACK_MANIFEST_FILE_NAME), manifest)
            .expect("pack manifest should write");

        fs::write(
            root.join("runtime/agent-runtime.overlay.yaml"),
            format!(
                r#"
tools_allowlist:
  - {runtime_binary_name}
phases:
  {phase_id}:
    mode: command
    command:
      program: ./bin/{runtime_binary_name}
      args:
        - "{project_script}"
        - "{{{{project_root}}}}/{output_file_name}"
        - "{{{{subject_id}}}}"
      cwd_mode: project_root
      parse_json_output: true
      expected_result_kind: phase_result
"#
            ),
        )
        .expect("agent runtime overlay should write");

        fs::write(
            root.join("runtime/workflow-runtime.overlay.yaml"),
            format!(
                r#"
phase_catalog:
  {phase_id}:
    label: "{phase_id}"
    description: "{runtime_kind} fixture phase"
    category: verification
    tags: ["fixture", "{runtime_kind}"]
workflows:
  - id: {workflow_ref}
    name: "{workflow_ref}"
    phases:
      - {phase_id}
"#
            ),
        )
        .expect("workflow runtime overlay should write");

        if include_mcp_overlay {
            fs::write(
                root.join("mcp/servers.toml"),
                format!(
                    r#"[[server]]
id = "runtime"
command = "{runtime_binary_name}"
args = ["mcp-server.js"]
"#,
                ),
            )
            .expect("mcp servers should write");
            fs::write(
                root.join("mcp/tools.toml"),
                format!(
                    r#"[phase.{phase_id}]
servers = ["runtime"]
"#
                ),
            )
            .expect("mcp tools should write");
        }
    }

    fn write_delegating_pack_fixture(root: &Path, pack_id: &str, workflow_ref: &str, sub_workflow_ref: &str) {
        fs::create_dir_all(root.join("workflows")).expect("pack workflows dir should exist");
        fs::create_dir_all(root.join("runtime")).expect("pack runtime dir should exist");

        let manifest = format!(
            r#"
schema = "ao.pack.v1"
id = "{pack_id}"
version = "0.1.0"
kind = "domain-pack"
title = "{pack_id}"
description = "delegating pack fixture."

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["custom"]
default_kind = "custom"

[workflows]
root = "workflows"
exports = ["{workflow_ref}"]

[runtime]
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[[dependencies]]
id = "ao.node-fixture"
version = ">=0.1.0"
optional = false

[permissions]
tools = []
"#
        );
        fs::write(root.join(orchestrator_config::PACK_MANIFEST_FILE_NAME), manifest)
            .expect("delegating pack manifest should write");
        fs::write(
            root.join("runtime/workflow-runtime.overlay.yaml"),
            format!(
                r#"
workflows:
  - id: {workflow_ref}
    name: "{workflow_ref}"
    phases:
      - workflow_ref: {sub_workflow_ref}
"#
            ),
        )
        .expect("delegating workflow overlay should write");
    }

    fn phase_result_payload(phase_result: &Value) -> &Value {
        phase_result
            .get("outcome")
            .and_then(|outcome| outcome.get("result_payload"))
            .or_else(|| {
                phase_result
                    .get("outcome")
                    .and_then(|outcome| outcome.get("Completed"))
                    .and_then(|completed| completed.get("result_payload"))
            })
            .unwrap_or(&Value::Null)
    }

    #[tokio::test]
    async fn execute_workflow_runs_node_pack_fixture_and_namespaces_pack_mcp() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        write_project_runtime_scripts(project.path());

        let pack_root = machine_installed_packs_dir().join("ao.node-fixture").join("0.1.0");
        let path = env::var("PATH").unwrap_or_default();
        let _path_guard = EnvVarGuard::set_raw("PATH", &format!("{}:{}", pack_root.join("bin").display(), path));
        write_runtime_pack_fixture(
            &pack_root,
            "ao.node-fixture",
            "node",
            "v20.11.1",
            "node-fixture-runtime",
            "node-command",
            "ao.node-fixture/run",
            "scripts/node-fixture.js",
            "node-pack-output.txt",
            true,
        );

        let pack = load_pack_manifest(&pack_root).expect("node fixture pack should load");
        let overlay = load_pack_mcp_overlay(&pack).expect("node fixture MCP overlay should load");
        assert!(overlay.servers.contains_key("ao.node-fixture/runtime"));
        assert_eq!(
            overlay.phase_mcp_bindings.get("node-command").expect("node phase MCP binding should exist").servers,
            vec!["ao.node-fixture/runtime".to_string()]
        );

        let mut workflow = builtin_workflow_config();
        let report = activate_pack_mcp_overlay(&mut workflow, &pack).expect("node fixture MCP overlay should activate");
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].status, PackRuntimeCheckStatus::Satisfied);
        assert_eq!(
            workflow.phase_mcp_bindings.get("node-command").expect("node phase MCP binding should merge").servers,
            vec!["ao.node-fixture/runtime".to_string()]
        );

        let loaded = load_workflow_config_with_metadata(project.path()).expect("effective workflow config should load");
        assert!(
            loaded.config.workflows.iter().any(|workflow| workflow.id == "ao.node-fixture/run"),
            "node fixture workflow should be discoverable"
        );

        let result = execute_workflow(WorkflowExecuteParams {
            project_root: project.path().display().to_string(),
            workflow_id: None,
            task_id: None,
            requirement_id: None,
            title: Some("node-fixture-subject".to_string()),
            description: Some("node fixture workflow".to_string()),
            workflow_ref: Some("ao.node-fixture/run".to_string()),
            input: None,
            vars: HashMap::new(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            phase_filter: None,
            on_phase_event: None,
            hub: None,
            phase_routing: None,
            mcp_config: None,
        })
        .await
        .expect("node fixture workflow should execute");

        assert!(result.success);
        assert_eq!(result.workflow_ref, "ao.node-fixture/run");
        assert_eq!(result.execution_cwd, project.path().display().to_string());
        assert_eq!(result.phase_results[0]["status"].as_str(), Some("completed"));
        let payload = phase_result_payload(&result.phase_results[0]);
        assert_eq!(payload.get("runtime").and_then(Value::as_str), Some("node"));
        assert_eq!(payload.get("subject_id").and_then(Value::as_str), Some("node-fixture-subject"));

        let output = fs::read_to_string(project.path().join("node-pack-output.txt"))
            .expect("node fixture command output should be written");
        assert_eq!(output.trim(), "node:scripts/node-fixture.js");
    }

    #[tokio::test]
    async fn execute_workflow_defers_pack_runtime_checks_until_execution() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        write_project_runtime_scripts(project.path());

        let pack_root = machine_installed_packs_dir().join("ao.node-fixture").join("0.1.0");
        write_runtime_pack_fixture(
            &pack_root,
            "ao.node-fixture",
            "node",
            "v20.11.1",
            "node-fixture-runtime",
            "node-command",
            "ao.node-fixture/run",
            "scripts/node-fixture.js",
            "node-pack-output.txt",
            false,
        );

        let loaded = load_workflow_config_with_metadata(project.path()).expect("workflow config should load");
        assert!(
            loaded.config.workflows.iter().any(|workflow| workflow.id == "ao.node-fixture/run"),
            "fixture workflow should be discoverable before execution"
        );

        let result = execute_workflow(WorkflowExecuteParams {
            project_root: project.path().display().to_string(),
            workflow_id: None,
            task_id: None,
            requirement_id: None,
            title: Some("node-fixture-subject".to_string()),
            description: Some("node fixture workflow".to_string()),
            workflow_ref: Some("ao.node-fixture/run".to_string()),
            input: None,
            vars: HashMap::new(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            phase_filter: None,
            on_phase_event: None,
            hub: None,
            phase_routing: None,
            mcp_config: None,
        })
        .await;
        let error = match result {
            Ok(_) => panic!("execution should fail when runtime requirement is unavailable"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("cannot activate pack 'ao.node-fixture'"),
            "unexpected execution error: {error}"
        );
        assert!(
            error.chain().any(|cause| cause.to_string().contains("requires runtime 'node'")),
            "runtime requirement failure should remain in the error chain: {error:#}"
        );
    }

    #[tokio::test]
    async fn execute_workflow_checks_dependent_pack_requirements_before_phase_execution() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        write_project_runtime_scripts(project.path());

        let runtime_pack_root = machine_installed_packs_dir().join("ao.node-fixture").join("0.1.0");
        write_runtime_pack_fixture(
            &runtime_pack_root,
            "ao.node-fixture",
            "node",
            "v20.11.1",
            "node-fixture-runtime",
            "node-command",
            "ao.node-fixture/cycle",
            "scripts/node-fixture.js",
            "node-pack-output.txt",
            false,
        );
        let delegating_pack_root = machine_installed_packs_dir().join("ao.composite").join("0.1.0");
        write_delegating_pack_fixture(
            &delegating_pack_root,
            "ao.composite",
            "ao.composite/run",
            "ao.node-fixture/cycle",
        );

        let loaded = load_workflow_config_with_metadata(project.path()).expect("workflow config should load");
        assert!(
            loaded.config.workflows.iter().any(|workflow| workflow.id == "ao.composite/run"),
            "delegating workflow should be discoverable before execution"
        );
        assert!(
            loaded.config.workflows.iter().any(|workflow| workflow.id == "ao.node-fixture/cycle"),
            "dependent workflow should be discoverable before execution"
        );

        let result = execute_workflow(WorkflowExecuteParams {
            project_root: project.path().display().to_string(),
            workflow_id: None,
            task_id: None,
            requirement_id: None,
            title: Some("composite-fixture-subject".to_string()),
            description: Some("delegating fixture workflow".to_string()),
            workflow_ref: Some("ao.composite/run".to_string()),
            input: None,
            vars: HashMap::new(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            phase_filter: None,
            on_phase_event: None,
            hub: None,
            phase_routing: None,
            mcp_config: None,
        })
        .await;
        let error = match result {
            Ok(_) => panic!("execution should fail when dependent runtime requirement is unavailable"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("cannot activate pack 'ao.node-fixture'"),
            "unexpected execution error: {error}"
        );
        assert!(
            error.chain().any(|cause| cause.to_string().contains("required by workflow 'ao.node-fixture/cycle'")),
            "dependent workflow context should remain in the error chain: {error:#}"
        );
    }

    #[tokio::test]
    async fn execute_workflow_runs_python_pack_fixture_with_external_runtime() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        write_project_runtime_scripts(project.path());

        let pack_root = machine_installed_packs_dir().join("ao.python-fixture").join("0.1.0");
        let path = env::var("PATH").unwrap_or_default();
        let _path_guard = EnvVarGuard::set_raw("PATH", &format!("{}:{}", pack_root.join("bin").display(), path));
        write_runtime_pack_fixture(
            &pack_root,
            "ao.python-fixture",
            "python",
            "Python 3.11.8",
            "python-fixture-runtime",
            "python-command",
            "ao.python-fixture/run",
            "scripts/python-fixture.py",
            "python-pack-output.txt",
            false,
        );

        let loaded = load_workflow_config_with_metadata(project.path()).expect("effective workflow config should load");
        assert!(
            loaded.config.workflows.iter().any(|workflow| workflow.id == "ao.python-fixture/run"),
            "python fixture workflow should be discoverable"
        );

        let result = execute_workflow(WorkflowExecuteParams {
            project_root: project.path().display().to_string(),
            workflow_id: None,
            task_id: None,
            requirement_id: None,
            title: Some("python-fixture-subject".to_string()),
            description: Some("python fixture workflow".to_string()),
            workflow_ref: Some("ao.python-fixture/run".to_string()),
            input: None,
            vars: HashMap::new(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            phase_filter: None,
            on_phase_event: None,
            hub: None,
            phase_routing: None,
            mcp_config: None,
        })
        .await
        .expect("python fixture workflow should execute");

        assert!(result.success);
        assert_eq!(result.workflow_ref, "ao.python-fixture/run");
        assert_eq!(result.phase_results[0]["status"].as_str(), Some("completed"));
        let payload = phase_result_payload(&result.phase_results[0]);
        assert_eq!(payload.get("runtime").and_then(Value::as_str), Some("python"));
        assert_eq!(payload.get("subject_id").and_then(Value::as_str), Some("python-fixture-subject"));

        let output = fs::read_to_string(project.path().join("python-pack-output.txt"))
            .expect("python fixture command output should be written");
        assert_eq!(output.trim(), "python:scripts/python-fixture.py");
    }
}

#[cfg(test)]
mod post_success_merge_tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command as ProcessCommand;
    use tempfile::TempDir;

    fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("git {:?} failed to start in {}: {error}", args, cwd.display()))
    }

    fn run_git_ok(cwd: &Path, args: &[&str]) {
        let output = run_git(cwd, args);
        assert!(output.status.success(), "git {:?} failed in {}: {}", args, cwd.display(), command_summary(&output));
    }

    fn git_stdout(cwd: &Path, args: &[&str]) -> String {
        let output = run_git(cwd, args);
        assert!(output.status.success(), "git {:?} failed in {}: {}", args, cwd.display(), command_summary(&output));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo() -> (TempDir, PathBuf) {
        let temp = TempDir::new().expect("temp dir");
        run_git_ok(temp.path(), &["init", "--initial-branch=main"]);
        run_git_ok(temp.path(), &["config", "user.email", "ao@example.com"]);
        run_git_ok(temp.path(), &["config", "user.name", "AO"]);
        fs::write(temp.path().join("README.md"), "base\n").expect("write base file");
        run_git_ok(temp.path(), &["add", "README.md"]);
        run_git_ok(temp.path(), &["commit", "-m", "initial"]);

        let worktree_path = temp.path().join(".ao").join("worktrees").join("task-task-1");
        fs::create_dir_all(worktree_path.parent().expect("worktree parent")).expect("create worktree parent");
        run_git_ok(
            temp.path(),
            &["worktree", "add", "-b", "ao/task-1", worktree_path.to_str().expect("worktree path"), "main"],
        );

        (temp, worktree_path)
    }

    fn commit_file(cwd: &Path, file_name: &str, contents: &str, message: &str) {
        fs::write(cwd.join(file_name), contents).expect("write fixture file");
        run_git_ok(cwd, &["add", file_name]);
        run_git_ok(cwd, &["commit", "-m", message]);
    }

    #[tokio::test]
    async fn auto_merge_uses_target_branch_worktree_for_standard_merge() {
        let (repo, source_worktree) = init_repo();
        commit_file(&source_worktree, "feature.txt", "merged\n", "source change");

        let result = perform_auto_merge_with_git(
            repo.path().to_str().expect("repo path"),
            source_worktree.to_str().expect("worktree path"),
            "ao/task-1",
            "main",
            &MergeStrategy::Merge,
        )
        .await;

        assert_eq!(result["status"].as_str(), Some("completed"));
        assert_eq!(git_stdout(repo.path(), &["branch", "--show-current"]), "main");
        assert_eq!(git_stdout(&source_worktree, &["branch", "--show-current"]), "ao/task-1");
        assert_eq!(fs::read_to_string(repo.path().join("feature.txt")).expect("merged file"), "merged\n");
    }

    #[tokio::test]
    async fn auto_merge_rebase_advances_target_branch() {
        let (repo, source_worktree) = init_repo();
        commit_file(repo.path(), "main.txt", "target\n", "target change");
        commit_file(&source_worktree, "feature.txt", "rebased\n", "source change");

        let result = perform_auto_merge_with_git(
            repo.path().to_str().expect("repo path"),
            source_worktree.to_str().expect("worktree path"),
            "ao/task-1",
            "main",
            &MergeStrategy::Rebase,
        )
        .await;

        assert_eq!(result["status"].as_str(), Some("completed"));
        assert_eq!(
            git_stdout(repo.path(), &["rev-parse", "main"]),
            git_stdout(repo.path(), &["rev-parse", "ao/task-1"])
        );
        assert_eq!(git_stdout(repo.path(), &["log", "--format=%s", "-2", "main"]), "source change\ntarget change");
    }

    #[tokio::test]
    async fn auto_merge_squash_creates_commit_and_leaves_target_clean() {
        let (repo, source_worktree) = init_repo();
        let before = git_stdout(repo.path(), &["rev-parse", "main"]);
        commit_file(&source_worktree, "feature.txt", "squashed\n", "source change");

        let result = perform_auto_merge_with_git(
            repo.path().to_str().expect("repo path"),
            source_worktree.to_str().expect("worktree path"),
            "ao/task-1",
            "main",
            &MergeStrategy::Squash,
        )
        .await;

        assert_eq!(result["status"].as_str(), Some("completed"));
        assert_ne!(before, git_stdout(repo.path(), &["rev-parse", "main"]));
        assert_eq!(
            git_stdout(repo.path(), &["log", "--format=%s", "-1", "main"]),
            "Squash merge branch 'ao/task-1' into 'main'"
        );
        assert_eq!(git_stdout(repo.path(), &["status", "--porcelain", "--untracked-files=no"]), "");
        assert_eq!(fs::read_to_string(repo.path().join("feature.txt")).expect("squashed file"), "squashed\n");
    }
}
