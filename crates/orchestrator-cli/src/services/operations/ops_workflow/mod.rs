mod config;
mod control_routing;
pub(crate) mod execute;
mod phases;
mod prompt;

pub(crate) use control_routing::build_workflow_routing;

use std::path::Path;
use std::sync::Arc;

use super::ops_common::project_state_dir;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    dispatch_workflow_event, ensure_workflow_config_compiled, load_workflow_config, services::ServiceHub,
    ListPageRequest, OrchestratorTask, WorkflowEvent, WorkflowFilter, WorkflowQuery, WorkflowResumeManager,
    WorkflowRunInput, STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF,
};
use protocol::SubjectDispatchExt;
use serde_json::Value;
use uuid::Uuid;

use self::execute::WorkflowExecuteArgs;
use crate::{
    dry_run_envelope, ensure_destructive_confirmation, parse_workflow_query_sort_opt, parse_workflow_status_opt,
    print_value, WorkflowAgentRuntimeCommand, WorkflowCheckpointCommand, WorkflowCommand, WorkflowConfigCommand,
    WorkflowDefinitionsCommand, WorkflowPhaseCommand, WorkflowPhasesCommand, WorkflowPromptCommand,
    WorkflowStateMachineCommand,
};

#[allow(clippy::too_many_arguments)]
async fn resolve_workflow_run_dispatch(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task_id: Option<String>,
    requirement_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    workflow_ref: Option<String>,
    vars: std::collections::HashMap<String, String>,
) -> Result<protocol::SubjectDispatch> {
    match (task_id, requirement_id, title) {
        (Some(tid), None, None) => {
            // v0.4.12+: task data may live in an installed `subject_backend` plugin
            // rather than the in-tree task store. Try the in-tree store first (so
            // legacy projects keep working); if that misses, route through the
            // subject_resolver so the plugin fallback path engages. Either way we
            // need the resolved task id (and `is_frontend_related()` if available)
            // to pick a workflow ref.
            let (resolved_id, default_ref) = match hub.tasks().get(&tid).await {
                Ok(task) => (task.id.clone(), default_workflow_ref_for_task(&task, project_root)),
                Err(in_tree_err) => {
                    let subject = protocol::orchestrator::SubjectRef::task(tid.clone());
                    match hub.subject_resolver().resolve_subject_context(&subject, None, None).await {
                        Ok(ctx) => (ctx.subject_id, default_project_workflow_ref(project_root)),
                        Err(plugin_err) => {
                            return Err(anyhow!(
                                "task '{tid}' not found (in-tree: {in_tree_err}; plugin: {plugin_err})"
                            ));
                        }
                    }
                }
            };
            Ok(protocol::SubjectDispatch::for_task_with_metadata(
                resolved_id,
                workflow_ref.unwrap_or(default_ref),
                "manual-cli-run",
                Utc::now(),
            ))
            .map(|dispatch| dispatch.with_vars(vars))
        }
        (None, Some(rid), None) => {
            // Mirror the task path: try in-tree, fall back to plugin-backed
            // subject resolver so requirement-kind plugins (e.g. linear) work.
            let resolved_id = match hub.planning().get_requirement(&rid).await {
                Ok(_) => rid,
                Err(in_tree_err) => {
                    let subject = protocol::orchestrator::SubjectRef::requirement(rid.clone());
                    match hub.subject_resolver().resolve_subject_context(&subject, None, None).await {
                        Ok(ctx) => ctx.subject_id,
                        Err(plugin_err) => {
                            return Err(anyhow!(
                                "requirement '{rid}' not found (in-tree: {in_tree_err}; plugin: {plugin_err})"
                            ));
                        }
                    }
                }
            };
            Ok(protocol::SubjectDispatch::for_requirement(
                resolved_id,
                workflow_ref.unwrap_or(resolve_requirement_workflow_ref(project_root)?),
                "manual-cli-run",
            ))
            .map(|dispatch| dispatch.with_vars(vars))
        }
        (None, None, Some(t)) => Ok(protocol::SubjectDispatch::for_custom(
            t,
            description.unwrap_or_default(),
            workflow_ref.unwrap_or_else(|| default_project_workflow_ref(project_root)),
            None,
            "manual-cli-run",
        )
        .with_vars(vars)),
        (None, None, None) => Err(anyhow!("one of --task-id, --requirement-id, or --title must be provided")),
        _ => Err(anyhow!("--task-id, --requirement-id, and --title are mutually exclusive")),
    }
}

async fn resolve_workflow_run_dispatch_from_input(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: WorkflowRunInput,
) -> Result<protocol::SubjectDispatch> {
    let WorkflowRunInput {
        subject,
        workflow_ref,
        input,
        vars,
        task_id: flat_task_id,
        requirement_id: flat_requirement_id,
        ..
    } = input;
    let effective_task_id = subject
        .task_id()
        .filter(|id| !id.is_empty())
        .map(|s| s.to_string())
        .or_else(|| (!flat_task_id.is_empty()).then_some(flat_task_id));
    let effective_requirement_id =
        subject.requirement_id().filter(|id| !id.is_empty()).map(|s| s.to_string()).or(flat_requirement_id);
    if let Some(id) = effective_task_id {
        // See `resolve_workflow_run_dispatch`: try in-tree task store first, then
        // fall back to subject_resolver so plugin-owned tasks dispatch correctly.
        let (resolved_id, default_ref) = match hub.tasks().get(&id).await {
            Ok(task) => (task.id.clone(), default_workflow_ref_for_task(&task, project_root)),
            Err(in_tree_err) => {
                let subject = protocol::orchestrator::SubjectRef::task(id.clone());
                match hub.subject_resolver().resolve_subject_context(&subject, None, None).await {
                    Ok(ctx) => (ctx.subject_id, default_project_workflow_ref(project_root)),
                    Err(plugin_err) => {
                        return Err(anyhow!("task '{id}' not found (in-tree: {in_tree_err}; plugin: {plugin_err})"));
                    }
                }
            }
        };
        Ok(protocol::SubjectDispatch::for_task_with_metadata(
            resolved_id,
            workflow_ref.unwrap_or(default_ref),
            "manual-cli-run",
            Utc::now(),
        )
        .with_input(input))
        .map(|dispatch| dispatch.with_vars(vars))
    } else if let Some(id) = effective_requirement_id {
        let resolved_id = match hub.planning().get_requirement(&id).await {
            Ok(_) => id,
            Err(in_tree_err) => {
                let subject = protocol::orchestrator::SubjectRef::requirement(id.clone());
                match hub.subject_resolver().resolve_subject_context(&subject, None, None).await {
                    Ok(ctx) => ctx.subject_id,
                    Err(plugin_err) => {
                        return Err(anyhow!(
                            "requirement '{id}' not found (in-tree: {in_tree_err}; plugin: {plugin_err})"
                        ));
                    }
                }
            }
        };
        Ok(protocol::SubjectDispatch::for_requirement(
            resolved_id,
            workflow_ref.unwrap_or(resolve_requirement_workflow_ref(project_root)?),
            "manual-cli-run",
        )
        .with_input(input))
        .map(|dispatch| dispatch.with_vars(vars))
    } else {
        Ok(protocol::SubjectDispatch::for_custom(
            subject.title.unwrap_or_else(|| subject.id.clone()),
            subject.description.unwrap_or_default(),
            workflow_ref.unwrap_or_else(|| default_project_workflow_ref(project_root)),
            input,
            "manual-cli-run",
        ))
        .map(|dispatch| dispatch.with_vars(vars))
    }
}

fn upgrade_legacy_workflow_run_input(raw: &str) -> Result<Option<WorkflowRunInput>> {
    let value = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.contains_key("subject") {
        return Ok(None);
    }

    let task_id = object
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let requirement_id = object
        .get("requirement_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    if task_id.is_none() && requirement_id.is_none() && title.is_none() {
        return Ok(None);
    }

    let workflow_ref = object.get("workflow_ref").and_then(Value::as_str).map(ToOwned::to_owned);
    let input = match object.get("input") {
        Some(value) => Some(value.clone()),
        None => match object.get("input_json") {
            Some(Value::String(raw_input)) => Some(
                serde_json::from_str(raw_input)
                    .with_context(|| "invalid nested input_json payload for workflow run")?,
            ),
            Some(value) => Some(value.clone()),
            None => None,
        },
    };

    let run_input = match (task_id, requirement_id, title) {
        (Some(task_id), None, None) => WorkflowRunInput::for_task(task_id, workflow_ref),
        (None, Some(requirement_id), None) => WorkflowRunInput::for_requirement(requirement_id, workflow_ref),
        (None, None, Some(title)) => WorkflowRunInput::for_custom(
            title,
            object.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
            workflow_ref,
        ),
        (None, None, None) => return Ok(None),
        _ => {
            return Err(anyhow!(
                "legacy workflow run payload fields task_id, requirement_id, and title are mutually exclusive"
            ));
        }
    };

    Ok(Some(run_input.with_input(input)))
}

fn parse_workflow_vars(raw_vars: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut vars = std::collections::HashMap::new();
    for raw in raw_vars {
        let (key, value) =
            raw.split_once('=').ok_or_else(|| anyhow!("invalid --var value '{raw}'; expected KEY=VALUE"))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid --var value '{raw}'; variable name must not be empty"));
        }
        if vars.contains_key(key) {
            return Err(anyhow!("duplicate --var key '{}'", key));
        }
        vars.insert(key.to_string(), value.to_string());
    }
    Ok(vars)
}

fn default_project_workflow_ref(project_root: &str) -> String {
    orchestrator_core::load_workflow_config_or_default(Path::new(project_root)).config.default_workflow_ref
}

fn default_workflow_ref_for_task(task: &OrchestratorTask, project_root: &str) -> String {
    if task.is_frontend_related() {
        UI_UX_WORKFLOW_REF.to_string()
    } else {
        let project_default = default_project_workflow_ref(project_root);
        if project_default.trim().is_empty() {
            STANDARD_WORKFLOW_REF.to_string()
        } else {
            project_default
        }
    }
}

async fn resolve_workflow_run_dispatch_from_raw_input(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    raw: &str,
) -> Result<protocol::SubjectDispatch> {
    if let Ok(dispatch) = serde_json::from_str::<protocol::SubjectDispatch>(raw) {
        return Ok(dispatch);
    }

    if let Some(input) = upgrade_legacy_workflow_run_input(raw)
        .with_context(|| "invalid --input-json payload for workflow run; run 'animus workflow run --help' for schema")?
    {
        return resolve_workflow_run_dispatch_from_input(hub, project_root, input).await;
    }

    if let Ok(input) = serde_json::from_str::<WorkflowRunInput>(raw) {
        return resolve_workflow_run_dispatch_from_input(hub, project_root, input).await;
    }

    Err(anyhow!("invalid --input-json payload for workflow run; run 'animus workflow run --help' for schema"))
}

pub(crate) fn resolve_requirement_workflow_ref(project_root: &str) -> Result<String> {
    const REQUIREMENT_PLAN_WORKFLOW_REF: &str = "animus.requirement/plan";
    let root = Path::new(project_root);
    ensure_workflow_config_compiled(root)?;
    let workflow_config = load_workflow_config(root)?;
    workflow_config
        .workflows
        .iter()
        .any(|workflow| workflow.id.eq_ignore_ascii_case(REQUIREMENT_PLAN_WORKFLOW_REF))
        .then(|| REQUIREMENT_PLAN_WORKFLOW_REF.to_string())
        .ok_or_else(|| {
            anyhow!(
                "requirement workflow '{}' is not configured for requirement subjects (install a pack that exports it, e.g. animus.requirement)",
                REQUIREMENT_PLAN_WORKFLOW_REF
            )
        })
}

fn emit_daemon_event(project_root: &str, event_type: &str, data: Value) -> Result<()> {
    let path = protocol::Config::global_config_dir().join("daemon-events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let timestamp = Utc::now().to_rfc3339();
    let event = serde_json::json!({
        "schema": "animus.daemon.event.v1",
        "id": Uuid::new_v4().to_string(),
        "seq": 0,
        "timestamp": timestamp,
        "event_type": event_type,
        "project_root": project_root,
        "data": data,
    });
    let mut line = serde_json::to_string(&event)?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn build_workflow_query(args: crate::WorkflowListArgs) -> Result<WorkflowQuery> {
    Ok(WorkflowQuery {
        filter: WorkflowFilter {
            status: parse_workflow_status_opt(args.status.as_deref())?,
            workflow_ref: args.workflow_ref,
            task_id: args.task_id,
            phase_id: args.phase_id,
            search_text: args.search,
        },
        page: ListPageRequest { limit: args.limit, offset: args.offset },
        sort: parse_workflow_query_sort_opt(args.sort.as_deref())?.unwrap_or_default(),
    })
}

pub(crate) async fn handle_workflow(
    command: WorkflowCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let workflows = hub.workflows();

    match command {
        WorkflowCommand::List(args) => {
            // C6.5: prefer the control wire when daemon is running + json
            // mode, so the daemon's view of workflow runs is authoritative.
            // Falls back to the local in-process query when no socket is
            // available or the daemon returns NotSupported (older daemon).
            if json {
                if let Some(response) = try_workflow_list_via_control(project_root, &args).await? {
                    return print_value(response, true);
                }
            }
            let page = workflows.query(build_workflow_query(args)?).await?;
            print_value(page.items, json)
        }
        WorkflowCommand::Get(args) => {
            if json {
                if let Some(run) = try_workflow_get_via_control(project_root, &args.id).await? {
                    return print_value(run, true);
                }
            }
            print_value(workflows.get(&args.id).await?, json)
        }
        WorkflowCommand::Decisions(args) => print_value(workflows.decisions(&args.id).await?, json),
        WorkflowCommand::Checkpoints { command } => match command {
            WorkflowCheckpointCommand::List(args) => print_value(workflows.list_checkpoints(&args.id).await?, json),
            WorkflowCheckpointCommand::Get(args) => {
                print_value(workflows.get_checkpoint(&args.id, args.checkpoint).await?, json)
            }
            WorkflowCheckpointCommand::Prune(args) => {
                let manager = orchestrator_core::WorkflowStateManager::new(project_root);
                let pruned =
                    manager.prune_checkpoints(&args.id, args.keep_last_per_phase, args.max_age_hours, args.dry_run)?;
                print_value(pruned, json)
            }
        },
        WorkflowCommand::Run(args) => {
            let workflow_ref = args.pipeline.clone();
            if args.sync {
                let execute_args = WorkflowExecuteArgs {
                    workflow_id: args.workflow_id,
                    task_id: args.task_id,
                    requirement_id: args.requirement_id,
                    title: args.title,
                    description: args.description,
                    workflow_ref: workflow_ref.clone(),
                    phase: args.phase,
                    model: args.model,
                    tool: args.tool,
                    phase_timeout_secs: args.phase_timeout_secs,
                    input_json: args.input_json,
                    vars: args.vars,
                };
                execute::handle_workflow_execute(execute_args, hub, project_root, json).await?;
                Ok(())
            } else {
                // C6.5: when JSON mode + task id + daemon is running,
                // route the start through the control wire so the
                // daemon's WorkflowService owns the run lifecycle. The
                // wire response is a WorkflowRunStart (id + status +
                // started_at) which is a strict subset of the local
                // OrchestratorWorkflow shape; that's the intentional
                // trade for the cross-transport story. Requirement /
                // title / freeform paths and any --input-json path stay
                // local — the wire surface only covers `workflow/run`
                // for task subjects today.
                // Parse --var KEY=VALUE pairs up front so both the
                // control-wire path and the local path receive the same
                // validated map. Otherwise the wire path would silently
                // drop user-supplied vars when the daemon is running.
                let parsed_vars = if args.input_json.is_none() {
                    parse_workflow_vars(&args.vars)?
                } else {
                    std::collections::HashMap::new()
                };
                if json && args.input_json.is_none() && args.requirement_id.is_none() && args.title.is_none() {
                    if let Some(task_id) = args.task_id.as_ref() {
                        if let Some(start) =
                            try_workflow_run_via_control(project_root, task_id, workflow_ref.clone(), &parsed_vars)
                                .await?
                        {
                            return print_value(start, true);
                        }
                    }
                }
                let dispatch = match args.input_json {
                    Some(raw) => resolve_workflow_run_dispatch_from_raw_input(hub.clone(), project_root, &raw).await?,
                    None => {
                        resolve_workflow_run_dispatch(
                            hub.clone(),
                            project_root,
                            args.task_id,
                            args.requirement_id,
                            args.title,
                            args.description,
                            workflow_ref,
                            parsed_vars,
                        )
                        .await?
                    }
                };
                let workflow = workflows.run(dispatch.to_workflow_run_input()).await?;
                if !json {
                    eprintln!(
                        "dispatched workflow {} (status={:?}) — tail with: ao daemon events --follow, or rerun with --sync to stream phase events",
                        workflow.id, workflow.status
                    );
                }
                print_value(workflow, json)
            }
        }
        WorkflowCommand::Prompt { command } => match command {
            WorkflowPromptCommand::Render(args) => {
                prompt::handle_workflow_prompt_render(args, hub, project_root, json).await
            }
        },
        WorkflowCommand::Resume(args) => {
            if !args.force {
                let existing = workflows.get(&args.id).await?;
                if let Some(reason) = existing.failure_reason.as_deref() {
                    if reason.contains("idempotency annotation") || reason.contains("sideeffecting") {
                        return Err(anyhow!(
                            "workflow '{}' is blocked: {} — rerun with --force to override",
                            args.id,
                            reason
                        ));
                    }
                }
            }
            if json {
                if let Some(()) = try_workflow_resume_via_control(project_root, &args.id).await? {
                    let workflow = workflows.get(&args.id).await?;
                    return print_value(workflow, json);
                }
            }
            let outcome = dispatch_workflow_event(
                hub.clone(),
                project_root,
                WorkflowEvent::Resume { workflow_id: args.id.clone(), feedback: None },
            )
            .await?;
            let workflow = outcome.workflow.ok_or_else(|| anyhow!("workflow '{}' not found", args.id))?;
            print_value(workflow, json)
        }
        WorkflowCommand::ResumeStatus(args) => {
            let workflow = workflows.get(&args.id).await?;
            let manager = WorkflowResumeManager::new(project_root)?;
            let resumability = manager.validate_resumability(&workflow);
            print_value(
                serde_json::json!({
                    "workflow_id": workflow.id,
                    "status": workflow.status,
                    "machine_state": workflow.machine_state,
                    "resumability": phases::resumability_to_json(&resumability),
                }),
                json,
            )
        }
        WorkflowCommand::Pause(args) => {
            let workflow = workflows.get(&args.id).await?;
            if args.dry_run {
                let workflow_id = workflow.id.clone();
                return print_value(
                    dry_run_envelope(
                        "workflow.pause",
                        serde_json::json!({"id": &workflow_id}),
                        "workflow.pause",
                        vec!["pause workflow execution".to_string()],
                        &format!(
                            "rerun 'animus workflow pause --id {} --confirm {}' to apply",
                            workflow_id, workflow_id
                        ),
                    ),
                    json,
                );
            }
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "workflow pause", "--id")?;
            if json {
                if let Some(()) = try_workflow_pause_via_control(project_root, &args.id).await? {
                    let workflow = workflows.get(&args.id).await?;
                    return print_value(workflow, json);
                }
            }
            let outcome = dispatch_workflow_event(
                hub.clone(),
                project_root,
                WorkflowEvent::Pause { workflow_id: args.id.clone() },
            )
            .await?;
            let workflow = outcome.workflow.ok_or_else(|| anyhow!("workflow '{}' not found", args.id))?;
            print_value(workflow, json)
        }
        WorkflowCommand::Cancel(args) => {
            let workflow = workflows.get(&args.id).await?;
            if args.dry_run {
                let workflow_id = workflow.id.clone();
                return print_value(
                    dry_run_envelope(
                        "workflow.cancel",
                        serde_json::json!({"id": &workflow_id}),
                        "workflow.cancel",
                        vec!["cancel workflow execution".to_string()],
                        &format!(
                            "rerun 'animus workflow cancel --id {} --confirm {}' to apply",
                            workflow_id, workflow_id
                        ),
                    ),
                    json,
                );
            }
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "workflow cancel", "--id")?;
            if json {
                if let Some(()) = try_workflow_cancel_via_control(project_root, &args.id).await? {
                    let workflow = workflows.get(&args.id).await?;
                    return print_value(workflow, json);
                }
            }
            let outcome = dispatch_workflow_event(
                hub.clone(),
                project_root,
                WorkflowEvent::Cancel { workflow_id: args.id.clone() },
            )
            .await?;
            let workflow = outcome.workflow.ok_or_else(|| anyhow!("workflow '{}' not found", args.id))?;
            print_value(workflow, json)
        }
        WorkflowCommand::Phase { command } => match command {
            WorkflowPhaseCommand::Approve(args) => print_value(
                phases::approve_manual_phase(hub.clone(), project_root, &args.id, &args.phase, &args.note).await?,
                json,
            ),
            WorkflowPhaseCommand::Reject(args) => print_value(
                phases::reject_manual_phase(hub.clone(), project_root, &args.id, &args.phase, &args.note).await?,
                json,
            ),
        },
        WorkflowCommand::Phases { command } => match command {
            WorkflowPhasesCommand::List => print_value(phases::list_phase_payload(project_root)?, json),
            WorkflowPhasesCommand::Get(args) => print_value(phases::phase_payload(project_root, &args.phase)?, json),
            WorkflowPhasesCommand::Upsert(args) => {
                let definition: orchestrator_core::PhaseExecutionDefinition =
                    serde_json::from_str(&args.input_json).with_context(|| {
                        "invalid --input-json payload for workflow phases upsert; run 'animus workflow phases upsert --help' for schema"
                    })?;
                print_value(phases::upsert_phase_definition(project_root, &args.phase, definition)?, json)
            }
            WorkflowPhasesCommand::Remove(args) => {
                if args.dry_run {
                    return print_value(phases::preview_phase_removal(project_root, &args.phase)?, json);
                }
                ensure_destructive_confirmation(
                    args.confirm.as_deref(),
                    &args.phase,
                    "workflow phases remove",
                    "--phase",
                )?;
                print_value(phases::remove_phase_definition(project_root, &args.phase)?, json)
            }
        },
        WorkflowCommand::Definitions { command } => match command {
            WorkflowDefinitionsCommand::List => {
                let wf_config = orchestrator_core::load_workflow_config(Path::new(project_root))?;
                print_value(wf_config.workflows, json)
            }
            WorkflowDefinitionsCommand::Upsert(args) => {
                let workflow: orchestrator_core::WorkflowDefinition =
                    serde_json::from_str(&args.input_json).with_context(|| {
                        "invalid --input-json payload for workflow definitions upsert; run 'animus workflow definitions upsert --help' for schema"
                    })?;
                print_value(phases::upsert_pipeline(project_root, workflow)?, json)
            }
        },
        WorkflowCommand::Config { command } => match command {
            WorkflowConfigCommand::Get => print_value(config::get_workflow_config_payload(project_root), json),
            WorkflowConfigCommand::Validate => {
                print_value(config::validate_workflow_config_payload(project_root), json)
            }
            WorkflowConfigCommand::Compile => print_value(config::compile_yaml_workflows_payload(project_root)?, json),
        },
        WorkflowCommand::StateMachine { command } => match command {
            WorkflowStateMachineCommand::Get => print_value(config::get_state_machine_payload(project_root)?, json),
            WorkflowStateMachineCommand::Validate => {
                print_value(config::validate_state_machine_payload(project_root), json)
            }
            WorkflowStateMachineCommand::Set(args) => {
                print_value(config::set_state_machine_payload(project_root, &args.input_json)?, json)
            }
        },
        WorkflowCommand::AgentRuntime { command } => match command {
            WorkflowAgentRuntimeCommand::Get => print_value(config::get_agent_runtime_payload(project_root), json),
            WorkflowAgentRuntimeCommand::Validate => {
                print_value(config::validate_agent_runtime_payload(project_root), json)
            }
            WorkflowAgentRuntimeCommand::Set(args) => {
                print_value(config::set_agent_runtime_payload(project_root, &args.input_json)?, json)
            }
        },
    }
}

// =====================================================================
// C6.5 — control-wire routing helpers for workflow/*
// =====================================================================
//
// Each helper opens the control socket (returns Ok(None) when the daemon
// isn't running so the caller falls back to the local in-process path),
// issues the corresponding JSON-RPC call, and returns the wire-shaped
// response. When the daemon advertises the surface but the specific
// method is unavailable (older daemon, mid-rollout) we treat that the
// same as "socket missing" and degrade to local.

async fn try_workflow_list_via_control(
    project_root: &str,
    args: &crate::WorkflowListArgs,
) -> Result<Option<animus_control_protocol::types::WorkflowListResponse>> {
    use animus_control_protocol::types::WorkflowListRequest as WireRequest;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    let status = match args.status.as_deref() {
        Some(raw) => parse_wire_workflow_status(raw)?,
        None => None,
    };
    let limit = args.limit.map(|l| u32::try_from(l).unwrap_or(u32::MAX));
    let cursor = if args.offset == 0 { None } else { Some(args.offset.to_string()) };
    let request = WireRequest { status, cursor, limit };
    match client.workflow_list(request).await {
        Ok(response) => Ok(Some(response)),
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "workflow/list wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

async fn try_workflow_get_via_control(
    project_root: &str,
    id: &str,
) -> Result<Option<animus_control_protocol::types::WorkflowRun>> {
    use animus_control_protocol::types::WorkflowGetRequest as WireRequest;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    match client.workflow_get(WireRequest { id: id.to_string() }).await {
        Ok(response) => Ok(Some(response)),
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "workflow/get wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

async fn try_workflow_run_via_control(
    project_root: &str,
    task_id: &str,
    definition: Option<String>,
    vars: &std::collections::HashMap<String, String>,
) -> Result<Option<animus_control_protocol::types::WorkflowRunStart>> {
    use animus_control_protocol::types::WorkflowRunRequest as WireRequest;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    // Plumb --var KEY=VALUE pairs through the control wire via the
    // `params` map under the well-known `vars` key so they reach
    // `WorkflowRunInput::with_vars` on the daemon side. See the
    // matching extraction in `ops_workflow::control_routing::workflow_run`.
    let mut params: std::collections::BTreeMap<String, serde_json::Value> = std::collections::BTreeMap::new();
    if !vars.is_empty() {
        let vars_obj: serde_json::Map<String, serde_json::Value> =
            vars.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
        params.insert("vars".to_string(), serde_json::Value::Object(vars_obj));
    }
    let request = WireRequest { task_id: task_id.to_string(), definition, params };
    match client.workflow_run(request).await {
        Ok(response) => Ok(Some(response)),
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "workflow/run wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

async fn try_workflow_pause_via_control(project_root: &str, id: &str) -> Result<Option<()>> {
    use animus_control_protocol::types::WorkflowPauseRequest as WireRequest;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    match client.workflow_pause(WireRequest { id: id.to_string() }).await {
        Ok(_) => Ok(Some(())),
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "workflow/pause wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

async fn try_workflow_resume_via_control(project_root: &str, id: &str) -> Result<Option<()>> {
    use animus_control_protocol::types::WorkflowResumeRequest as WireRequest;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    match client.workflow_resume(WireRequest { id: id.to_string(), feedback: None }).await {
        Ok(_) => Ok(Some(())),
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "workflow/resume wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

async fn try_workflow_cancel_via_control(project_root: &str, id: &str) -> Result<Option<()>> {
    use animus_control_protocol::types::WorkflowCancelRequest as WireRequest;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    match client.workflow_cancel(WireRequest { id: id.to_string(), reason: None }).await {
        Ok(_) => Ok(Some(())),
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "workflow/cancel wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Parse the CLI `--status` argument into the wire-side enum, returning
/// `None` for the (already-handled) absent case. The CLI accepts core
/// snake_case names; the wire uses kebab-case. We accept both for
/// resilience and surface unknown values as anyhow errors with the
/// canonical set in the message.
fn parse_wire_workflow_status(raw: &str) -> Result<Option<animus_control_protocol::types::WorkflowStatus>> {
    use animus_control_protocol::types::WorkflowStatus;
    let normalized = raw.trim().to_ascii_lowercase().replace('_', "-");
    let value = match normalized.as_str() {
        "" => return Ok(None),
        "pending" => WorkflowStatus::Pending,
        "running" => WorkflowStatus::Running,
        "paused" => WorkflowStatus::Paused,
        "completed" => WorkflowStatus::Completed,
        "failed" | "escalated" => WorkflowStatus::Failed,
        "cancelled" | "canceled" => WorkflowStatus::Cancelled,
        other => {
            return Err(anyhow!(
                "unknown workflow status '{other}'; expected one of pending, running, paused, completed, failed, cancelled"
            ));
        }
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::config::*;
    use super::*;
    use orchestrator_core::{InMemoryServiceHub, Priority, TaskCreateInput, TaskType};
    use std::sync::Arc;

    #[test]
    fn set_state_machine_payload_reports_actionable_json_error() {
        let error = set_state_machine_payload("/tmp/unused", "{invalid").expect_err("invalid payload should fail");
        let message = error.to_string();
        assert!(message.contains("invalid --input-json payload"));
        assert!(message.contains("workflow state-machine set --help"));
    }

    #[test]
    fn set_agent_runtime_payload_reports_actionable_json_error() {
        let error = set_agent_runtime_payload("/tmp/unused", "{invalid").expect_err("invalid payload should fail");
        let message = error.to_string();
        assert!(message.contains("invalid --input-json payload"));
        assert!(message.contains("workflow agent-runtime set --help"));
    }

    #[tokio::test]
    async fn resolve_workflow_run_dispatch_builds_task_dispatch_with_concrete_workflow_ref() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "dispatch me".to_string(),
                description: "task dispatch builder test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let dispatch = resolve_workflow_run_dispatch(
            hub,
            "/tmp/unused",
            Some(task.id.clone()),
            None,
            None,
            None,
            None,
            std::collections::HashMap::new(),
        )
        .await
        .expect("dispatch should resolve");

        assert_eq!(dispatch.subject_id(), task.id);
        assert_eq!(dispatch.workflow_ref, orchestrator_core::workflow_ref_for_task(&task));
        assert_eq!(dispatch.trigger_source, "manual-cli-run");
    }

    #[tokio::test]
    async fn resolve_workflow_run_dispatch_from_input_accepts_legacy_workflow_run_input() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "legacy input".to_string(),
                description: "legacy workflow run input should still work".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let dispatch = resolve_workflow_run_dispatch_from_input(
            hub,
            "/tmp/unused",
            WorkflowRunInput::for_task(task.id.clone(), None),
        )
        .await
        .expect("legacy input should resolve");

        assert_eq!(dispatch.subject_id(), task.id);
        assert_eq!(dispatch.workflow_ref, orchestrator_core::workflow_ref_for_task(&task));
    }

    #[tokio::test]
    async fn resolve_workflow_run_dispatch_from_input_preserves_subject_input() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "dispatch input".to_string(),
                description: "workflow run input should preserve dispatch input".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let dispatch = resolve_workflow_run_dispatch_from_input(
            hub,
            "/tmp/unused",
            WorkflowRunInput::for_task(task.id, None).with_input(Some(serde_json::json!({"scope":"req-39"}))),
        )
        .await
        .expect("dispatch should resolve");

        assert_eq!(dispatch.input, Some(serde_json::json!({"scope":"req-39"})));
    }

    #[tokio::test]
    async fn resolve_workflow_run_dispatch_from_input_preserves_vars() {
        let hub = Arc::new(InMemoryServiceHub::new());

        let dispatch = resolve_workflow_run_dispatch_from_input(
            hub,
            "/tmp/unused",
            WorkflowRunInput::for_custom("prompt preview".to_string(), "inspect vars".to_string(), None)
                .with_vars(std::collections::HashMap::from([("release_name".to_string(), "Mercury".to_string())])),
        )
        .await
        .expect("dispatch should resolve");

        assert_eq!(dispatch.vars.get("release_name").map(String::as_str), Some("Mercury"));
    }

    #[tokio::test]
    async fn resolve_workflow_run_dispatch_from_raw_input_accepts_legacy_task_payload() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "legacy raw input".to_string(),
                description: "legacy workflow run payload should be upgraded".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let raw = format!("{{\"task_id\":\"{}\",\"input_json\":\"{{\\\"k\\\":\\\"v\\\"}}\"}}", task.id);
        let dispatch = resolve_workflow_run_dispatch_from_raw_input(hub, "/tmp/unused", &raw)
            .await
            .expect("legacy raw payload should resolve");

        assert_eq!(dispatch.subject_id(), task.id);
        assert_eq!(dispatch.input, Some(serde_json::json!({"k":"v"})));
    }

    #[test]
    fn parse_workflow_vars_rejects_invalid_pairs_and_duplicates() {
        let invalid = parse_workflow_vars(&["missing-separator".to_string()]).expect_err("missing '=' should fail");
        assert!(invalid.to_string().contains("expected KEY=VALUE"));

        let duplicate = parse_workflow_vars(&["release_name=Mercury".to_string(), "release_name=Gemini".to_string()])
            .expect_err("duplicate keys should fail");
        assert!(duplicate.to_string().contains("duplicate --var key"));
    }
}
