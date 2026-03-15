mod config;
pub(crate) mod execute;
mod phases;
mod prompt;

use std::path::Path;
use std::sync::Arc;

use super::ops_common::project_state_dir;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    dispatch_workflow_event, ensure_workflow_config_compiled, load_workflow_config, services::ServiceHub,
    workflow_ref_for_task, ListPageRequest, WorkflowEvent, WorkflowFilter, WorkflowQuery, WorkflowResumeManager,
    WorkflowRunInput, WorkflowSubject, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF, STANDARD_WORKFLOW_REF,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    dry_run_envelope, ensure_destructive_confirmation, parse_input_json_or, parse_workflow_query_sort_opt,
    parse_workflow_status_opt, print_value, WorkflowAgentRuntimeCommand, WorkflowCheckpointCommand, WorkflowCommand,
    WorkflowConfigCommand, WorkflowDefinitionsCommand, WorkflowExecuteArgs, WorkflowPhaseCommand,
    WorkflowPhasesCommand, WorkflowPromptCommand, WorkflowStateMachineCommand,
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
            let task = hub.tasks().get(&tid).await?;
            Ok(protocol::SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                workflow_ref.unwrap_or_else(|| workflow_ref_for_task(&task)),
                "manual-cli-run",
                Utc::now(),
            ))
            .map(|dispatch| dispatch.with_vars(vars))
        }
        (None, Some(rid), None) => {
            hub.planning().get_requirement(&rid).await?;
            Ok(protocol::SubjectDispatch::for_requirement(
                rid,
                workflow_ref.unwrap_or(resolve_requirement_workflow_ref(project_root)?),
                "manual-cli-run",
            ))
            .map(|dispatch| dispatch.with_vars(vars))
        }
        (None, None, Some(t)) => Ok(protocol::SubjectDispatch::for_custom(
            t,
            description.unwrap_or_default(),
            workflow_ref.unwrap_or_else(|| STANDARD_WORKFLOW_REF.to_string()),
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
    let WorkflowRunInput { subject, workflow_ref, input, vars, .. } = input;
    match subject {
        WorkflowSubject::Task { id } => {
            let task = hub.tasks().get(&id).await?;
            Ok(protocol::SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                workflow_ref.unwrap_or_else(|| workflow_ref_for_task(&task)),
                "manual-cli-run",
                Utc::now(),
            )
            .with_input(input))
            .map(|dispatch| dispatch.with_vars(vars))
        }
        WorkflowSubject::Requirement { id } => {
            hub.planning().get_requirement(&id).await?;
            Ok(protocol::SubjectDispatch::for_requirement(
                id,
                workflow_ref.unwrap_or(resolve_requirement_workflow_ref(project_root)?),
                "manual-cli-run",
            )
            .with_input(input))
            .map(|dispatch| dispatch.with_vars(vars))
        }
        WorkflowSubject::Custom { title, description } => Ok(protocol::SubjectDispatch::for_custom(
            title,
            description,
            workflow_ref.unwrap_or_else(|| STANDARD_WORKFLOW_REF.to_string()),
            input,
            "manual-cli-run",
        ))
        .map(|dispatch| dispatch.with_vars(vars)),
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

async fn resolve_workflow_run_dispatch_from_raw_input(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    raw: &str,
) -> Result<protocol::SubjectDispatch> {
    if let Ok(dispatch) = serde_json::from_str::<protocol::SubjectDispatch>(raw) {
        return Ok(dispatch);
    }

    if let Ok(input) = serde_json::from_str::<WorkflowRunInput>(raw) {
        return resolve_workflow_run_dispatch_from_input(hub, project_root, input).await;
    }

    if let Some(input) = upgrade_legacy_workflow_run_input(raw)
        .with_context(|| "invalid --input-json payload for workflow run; run 'ao workflow run --help' for schema")?
    {
        return resolve_workflow_run_dispatch_from_input(hub, project_root, input).await;
    }

    Err(anyhow!("invalid --input-json payload for workflow run; run 'ao workflow run --help' for schema"))
}

pub(crate) fn resolve_requirement_workflow_ref(project_root: &str) -> Result<String> {
    let root = Path::new(project_root);
    ensure_workflow_config_compiled(root)?;
    let workflow_config = load_workflow_config(root)?;
    workflow_config
        .workflows
        .iter()
        .any(|workflow| workflow.id.eq_ignore_ascii_case(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF))
        .then(|| REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string())
        .ok_or_else(|| {
            anyhow!(
                "requirement workflow '{}' is not configured for requirement subjects",
                REQUIREMENT_TASK_GENERATION_WORKFLOW_REF
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
        "schema": "ao.daemon.event.v1",
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
            let page = workflows.query(build_workflow_query(args)?).await?;
            print_value(page.items, json)
        }
        WorkflowCommand::Get(args) => print_value(workflows.get(&args.id).await?, json),
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
            let effective_workflow_ref = args.workflow_ref.or(args.pipeline);
            if args.sync {
                let execute_args = WorkflowExecuteArgs {
                    workflow_id: args.workflow_id,
                    task_id: args.task_id,
                    requirement_id: args.requirement_id,
                    title: args.title,
                    description: args.description,
                    workflow_ref: effective_workflow_ref,
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
                let dispatch = match args.input_json {
                    Some(raw) => resolve_workflow_run_dispatch_from_raw_input(hub.clone(), project_root, &raw).await?,
                    None => {
                        let vars = parse_workflow_vars(&args.vars)?;
                        resolve_workflow_run_dispatch(
                            hub.clone(),
                            project_root,
                            args.task_id,
                            args.requirement_id,
                            args.title,
                            args.description,
                            effective_workflow_ref,
                            vars,
                        )
                        .await?
                    }
                };
                print_value(workflows.run(dispatch.to_workflow_run_input()).await?, json)
            }
        }
        WorkflowCommand::Prompt { command } => match command {
            WorkflowPromptCommand::Render(args) => {
                prompt::handle_workflow_prompt_render(args, hub, project_root, json).await
            }
        },
        WorkflowCommand::Resume(args) => {
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
                        &format!("rerun 'ao workflow pause --id {} --confirm {}' to apply", workflow_id, workflow_id),
                    ),
                    json,
                );
            }
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "workflow pause", "--id")?;
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
                        &format!("rerun 'ao workflow cancel --id {} --confirm {}' to apply", workflow_id, workflow_id),
                    ),
                    json,
                );
            }
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "workflow cancel", "--id")?;
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
                        "invalid --input-json payload for workflow phases upsert; run 'ao workflow phases upsert --help' for schema"
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
                        "invalid --input-json payload for workflow definitions upsert; run 'ao workflow definitions upsert --help' for schema"
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
        WorkflowCommand::UpdateDefinition(args) => {
            let workflow = parse_input_json_or(args.input_json, || {
                Ok(orchestrator_core::WorkflowDefinition {
                    id: args.id,
                    name: args.name,
                    description: args.description.unwrap_or_default(),
                    phases: args.phases.into_iter().map(orchestrator_core::WorkflowPhaseEntry::Simple).collect(),
                    post_success: None,
                    variables: Vec::new(),
                })
            })?;
            print_value(phases::upsert_pipeline(project_root, workflow)?, json)
        }
    }
}

#[cfg(test)]
mod requirement_workflow_tests {
    use super::*;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, write_agent_runtime_config, write_workflow_config,
        WorkflowDefinition,
    };

    #[test]
    fn resolve_requirement_workflow_ref_errors_when_workflow_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_workflow_config(temp.path(), &builtin_workflow_config()).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let error = resolve_requirement_workflow_ref(temp.path().to_string_lossy().as_ref())
            .expect_err("missing requirement workflow should error");
        assert!(
            error.to_string().contains(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF),
            "error should mention missing requirement workflow"
        );
    }

    #[test]
    fn resolve_requirement_workflow_ref_detects_requirement_pipeline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = builtin_workflow_config();
        workflow_config.workflows.push(WorkflowDefinition {
            id: REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string(),
            name: "Requirement Task Generation".to_string(),
            description: String::new(),
            phases: vec!["requirements".to_string().into()],
            post_success: None,
            variables: Vec::new(),
        });
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let workflow_ref = resolve_requirement_workflow_ref(temp.path().to_string_lossy().as_ref())
            .expect("requirement workflow should resolve");
        assert_eq!(workflow_ref, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF);
    }

    #[test]
    fn apply_requirement_workflow_default_errors_when_requirement_workflow_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_workflow_config(temp.path(), &builtin_workflow_config()).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let error = resolve_requirement_workflow_ref(temp.path().to_string_lossy().as_ref())
            .expect_err("missing requirement workflow should fail closed");
        assert!(
            error.to_string().contains(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF),
            "error should mention missing requirement workflow"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::config::*;
    use super::*;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, write_agent_runtime_config, write_workflow_config,
        InMemoryServiceHub, Priority, RequirementItem, RequirementLinks, RequirementPriority, RequirementStatus,
        TaskCreateInput, TaskType, WorkflowDefinition,
    };
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
    async fn resolve_workflow_run_dispatch_uses_requirement_workflow_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = builtin_workflow_config();
        workflow_config.workflows.push(WorkflowDefinition {
            id: REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string(),
            name: "Requirement Task Generation".to_string(),
            description: "test workflow".to_string(),
            phases: vec!["requirements".to_string().into()],
            post_success: None,
            variables: Vec::new(),
        });
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let hub = Arc::new(InMemoryServiceHub::new());
        let now = chrono::Utc::now();
        hub.planning()
            .upsert_requirement(RequirementItem {
                id: "REQ-39".to_string(),
                title: "Dispatch requirement".to_string(),
                description: "requirement dispatch builder test".to_string(),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: vec!["starts workflow".to_string()],
                priority: RequirementPriority::Must,
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
            .expect("requirement should be created");

        let dispatch = resolve_workflow_run_dispatch(
            hub,
            temp.path().to_string_lossy().as_ref(),
            None,
            Some("REQ-39".to_string()),
            None,
            None,
            None,
            std::collections::HashMap::new(),
        )
        .await
        .expect("dispatch should resolve");

        assert_eq!(dispatch.subject_id(), "REQ-39");
        assert_eq!(dispatch.workflow_ref, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF);
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
