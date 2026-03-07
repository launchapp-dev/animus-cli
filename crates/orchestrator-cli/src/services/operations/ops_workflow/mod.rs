mod config;
mod execute;
mod phases;

use std::path::Path;
use std::sync::Arc;

use super::ops_common::project_state_dir;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{services::ServiceHub, WorkflowResumeManager, WorkflowRunInput};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    dry_run_envelope, ensure_destructive_confirmation, parse_input_json_or, print_value,
    WorkflowAgentRuntimeCommand, WorkflowCheckpointCommand, WorkflowCommand,
    WorkflowConfigCommand, WorkflowPhaseCommand, WorkflowPhasesCommand,
    WorkflowPipelinesCommand, WorkflowStateMachineCommand,
};

fn resolve_workflow_run_input(
    task_id: Option<String>,
    requirement_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    pipeline_id: Option<String>,
) -> Result<WorkflowRunInput> {
    match (task_id, requirement_id, title) {
        (Some(tid), None, None) => Ok(WorkflowRunInput::for_task(tid, pipeline_id)),
        (None, Some(rid), None) => Ok(WorkflowRunInput::for_requirement(rid, pipeline_id)),
        (None, None, Some(t)) => {
            Ok(WorkflowRunInput::for_custom(t, description.unwrap_or_default(), pipeline_id))
        }
        (None, None, None) => Err(anyhow!(
            "one of --task-id, --requirement-id, or --title must be provided"
        )),
        _ => Err(anyhow!(
            "--task-id, --requirement-id, and --title are mutually exclusive"
        )),
    }
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
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

pub(crate) async fn handle_workflow(
    command: WorkflowCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let workflows = hub.workflows();

    match command {
        WorkflowCommand::List => print_value(workflows.list().await?, json),
        WorkflowCommand::Get(args) => print_value(workflows.get(&args.id).await?, json),
        WorkflowCommand::Decisions(args) => print_value(workflows.decisions(&args.id).await?, json),
        WorkflowCommand::Checkpoints { command } => match command {
            WorkflowCheckpointCommand::List(args) => {
                print_value(workflows.list_checkpoints(&args.id).await?, json)
            }
            WorkflowCheckpointCommand::Get(args) => print_value(
                workflows.get_checkpoint(&args.id, args.checkpoint).await?,
                json,
            ),
            WorkflowCheckpointCommand::Prune(args) => {
                let manager = orchestrator_core::WorkflowStateManager::new(project_root);
                let pruned = manager.prune_checkpoints(
                    &args.id,
                    args.keep_last_per_phase,
                    args.max_age_hours,
                    args.dry_run,
                )?;
                print_value(pruned, json)
            }
        },
        WorkflowCommand::Run(args) => {
            let input = parse_input_json_or(args.input_json, || {
                resolve_workflow_run_input(
                    args.task_id,
                    args.requirement_id,
                    args.title,
                    args.description,
                    args.pipeline_id,
                )
            })?;
            print_value(workflows.run(input).await?, json)
        }
        WorkflowCommand::Execute(args) => {
            execute::handle_workflow_execute(args, hub, project_root, json).await?;
            Ok(())
        }
        WorkflowCommand::Resume(args) => print_value(workflows.resume(&args.id).await?, json),
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
                            "rerun 'ao workflow pause --id {} --confirm {}' to apply",
                            workflow_id, workflow_id
                        ),
                    ),
                    json,
                );
            }
            ensure_destructive_confirmation(
                args.confirm.as_deref(),
                &args.id,
                "workflow pause",
                "--id",
            )?;
            print_value(workflows.pause(&args.id).await?, json)
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
                            "rerun 'ao workflow cancel --id {} --confirm {}' to apply",
                            workflow_id, workflow_id
                        ),
                    ),
                    json,
                );
            }
            ensure_destructive_confirmation(
                args.confirm.as_deref(),
                &args.id,
                "workflow cancel",
                "--id",
            )?;
            print_value(workflows.cancel(&args.id).await?, json)
        }
        WorkflowCommand::Phase { command } => match command {
            WorkflowPhaseCommand::Approve(args) => print_value(
                phases::approve_manual_phase(
                    hub.clone(),
                    project_root,
                    &args.id,
                    &args.phase,
                    &args.note,
                )
                .await?,
                json,
            ),
        },
        WorkflowCommand::Phases { command } => match command {
            WorkflowPhasesCommand::List => {
                print_value(phases::list_phase_payload(project_root)?, json)
            }
            WorkflowPhasesCommand::Get(args) => {
                print_value(phases::phase_payload(project_root, &args.phase)?, json)
            }
            WorkflowPhasesCommand::Upsert(args) => {
                let definition: orchestrator_core::PhaseExecutionDefinition =
                    serde_json::from_str(&args.input_json).with_context(|| {
                        "invalid --input-json payload for workflow phases upsert; run 'ao workflow phases upsert --help' for schema"
                    })?;
                print_value(
                    phases::upsert_phase_definition(project_root, &args.phase, definition)?,
                    json,
                )
            }
            WorkflowPhasesCommand::Remove(args) => {
                if args.dry_run {
                    return print_value(
                        phases::preview_phase_removal(project_root, &args.phase)?,
                        json,
                    );
                }
                ensure_destructive_confirmation(
                    args.confirm.as_deref(),
                    &args.phase,
                    "workflow phases remove",
                    "--phase",
                )?;
                print_value(
                    phases::remove_phase_definition(project_root, &args.phase)?,
                    json,
                )
            }
        },
        WorkflowCommand::Pipelines { command } => match command {
            WorkflowPipelinesCommand::List => {
                let wf_config =
                    orchestrator_core::load_workflow_config(Path::new(project_root))?;
                print_value(wf_config.pipelines, json)
            }
            WorkflowPipelinesCommand::Upsert(args) => {
                let pipeline: orchestrator_core::PipelineDefinition =
                    serde_json::from_str(&args.input_json).with_context(|| {
                        "invalid --input-json payload for workflow pipelines upsert; run 'ao workflow pipelines upsert --help' for schema"
                    })?;
                print_value(phases::upsert_pipeline(project_root, pipeline)?, json)
            }
        },
        WorkflowCommand::Config { command } => match command {
            WorkflowConfigCommand::Get => {
                print_value(config::get_workflow_config_payload(project_root), json)
            }
            WorkflowConfigCommand::Validate => {
                print_value(config::validate_workflow_config_payload(project_root), json)
            }
            WorkflowConfigCommand::MigrateV2 => {
                print_value(config::migrate_v1_to_v2(project_root)?, json)
            }
            WorkflowConfigCommand::Compile => {
                print_value(config::compile_yaml_workflows_payload(project_root)?, json)
            }
        },
        WorkflowCommand::StateMachine { command } => match command {
            WorkflowStateMachineCommand::Get => {
                print_value(config::get_state_machine_payload(project_root)?, json)
            }
            WorkflowStateMachineCommand::Validate => {
                print_value(config::validate_state_machine_payload(project_root), json)
            }
            WorkflowStateMachineCommand::Set(args) => print_value(
                config::set_state_machine_payload(project_root, &args.input_json)?,
                json,
            ),
        },
        WorkflowCommand::AgentRuntime { command } => match command {
            WorkflowAgentRuntimeCommand::Get => {
                print_value(config::get_agent_runtime_payload(project_root), json)
            }
            WorkflowAgentRuntimeCommand::Validate => {
                print_value(config::validate_agent_runtime_payload(project_root), json)
            }
            WorkflowAgentRuntimeCommand::Set(args) => print_value(
                config::set_agent_runtime_payload(project_root, &args.input_json)?,
                json,
            ),
        },
        WorkflowCommand::UpdatePipeline(args) => {
            let pipeline = parse_input_json_or(args.input_json, || {
                Ok(orchestrator_core::PipelineDefinition {
                    id: args.id,
                    name: args.name,
                    description: args.description.unwrap_or_default(),
                    phases: args
                        .phases
                        .into_iter()
                        .map(orchestrator_core::PipelinePhaseEntry::Simple)
                        .collect(),
                    post_success: None,
                    variables: Vec::new(),
                })
            })?;
            print_value(phases::upsert_pipeline(project_root, pipeline)?, json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::config::*;

    #[test]
    fn set_state_machine_payload_reports_actionable_json_error() {
        let error = set_state_machine_payload("/tmp/unused", "{invalid")
            .expect_err("invalid payload should fail");
        let message = error.to_string();
        assert!(message.contains("invalid --input-json payload"));
        assert!(message.contains("workflow state-machine set --help"));
    }

    #[test]
    fn set_agent_runtime_payload_reports_actionable_json_error() {
        let error = set_agent_runtime_payload("/tmp/unused", "{invalid")
            .expect_err("invalid payload should fail");
        let message = error.to_string();
        assert!(message.contains("invalid --input-json payload"));
        assert!(message.contains("workflow agent-runtime set --help"));
    }
}
