use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use orchestrator_core::config::resolve_project_root;
use orchestrator_core::{
    FileServiceHub, RuntimeConfig, REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
    REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
};
use serde::Serialize;

mod cli_types;
mod services;
mod shared;
pub(crate) use cli_types::*;
pub(crate) use shared::*;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    let run_result = run(cli).await;
    let exit_code = match run_result {
        Ok(()) => 0,
        Err(error) => {
            emit_cli_error(&error, json);
            classify_exit_code(&error)
        }
    };
    std::process::exit(exit_code);
}

async fn run(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::Version) {
        let data = VersionInfo {
            name: env!("CARGO_PKG_NAME"),
            binary: env!("CARGO_BIN_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        };
        return print_value(data, cli.json);
    }

    let runtime_config = RuntimeConfig {
        project_root: cli.project_root.clone(),
        ..RuntimeConfig::default()
    };
    let (project_root, _) = resolve_project_root(&runtime_config);
    match cli.command {
        Command::Setup(args) => {
            services::operations::handle_setup(args, &project_root, cli.json).await
        }
        Command::Doctor(args) => {
            services::operations::handle_doctor(&project_root, args, cli.json).await
        }
        command => {
            let hub = Arc::new(FileServiceHub::new(&project_root)?);
            match command {
                Command::Daemon { command } => {
                    services::runtime::handle_daemon(command, hub.clone(), &project_root, cli.json)
                        .await
                }
                Command::Agent { command } => {
                    services::runtime::handle_agent(command, hub.clone(), &project_root, cli.json)
                        .await
                }
                Command::Project { command } => {
                    services::runtime::handle_project(command, hub.clone(), cli.json).await
                }
                Command::Queue { command } => {
                    services::operations::handle_queue(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Task { command } => {
                    services::runtime::handle_task(command, hub.clone(), &project_root, cli.json)
                        .await
                }
                Command::TaskControl { command } => {
                    eprintln!("warning: `task-control` is deprecated; use `task` instead");
                    services::runtime::handle_task(command, hub.clone(), &project_root, cli.json)
                        .await
                }
                Command::Workflow { command } => {
                    services::operations::handle_workflow(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Schedule { command } => {
                    services::operations::handle_schedule(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Vision { command } => {
                    services::operations::handle_vision(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Requirements { command } => {
                    services::operations::handle_requirements(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Architecture { command } => {
                    services::operations::handle_architecture(command, &project_root, cli.json)
                        .await
                }
                Command::Execute { command } => {
                    eprintln!("warning: `execute` is deprecated; use `workflow execute` instead");
                    let workflow_command = build_workflow_execute_command(command)?;
                    services::operations::handle_workflow(
                        workflow_command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }

                Command::Review { command } => {
                    services::operations::handle_review(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Qa { command } => {
                    services::operations::handle_qa(command, &project_root, cli.json).await
                }
                Command::History { command } => {
                    services::operations::handle_history(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Errors { command } => {
                    services::operations::handle_errors(command, &project_root, cli.json).await
                }
                Command::Git { command } => {
                    services::operations::handle_git(command, &project_root, cli.json).await
                }
                Command::Skill { command } => {
                    services::operations::handle_skill(command, &project_root, cli.json).await
                }
                Command::Model { command } => {
                    services::operations::handle_model(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Runner { command } => {
                    services::operations::handle_runner(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Status => {
                    services::operations::handle_status(hub.clone(), &project_root, cli.json).await
                }
                Command::Output { command } => {
                    services::operations::handle_output(command, &project_root, cli.json).await
                }
                Command::Mcp { command } => {
                    services::operations::handle_mcp(command, &project_root).await
                }
                Command::Web { command } => {
                    services::operations::handle_web(command, hub.clone(), &project_root, cli.json)
                        .await
                }
                Command::Tui(args) => {
                    services::tui::handle_tui(args, hub.clone(), &project_root, cli.json).await
                }
                Command::WorkflowMonitor(args) => {
                    services::tui::handle_workflow_monitor(args, hub.clone(), cli.json).await
                }
                Command::Version => {
                    unreachable!("version command handled before runtime initialization")
                }
                Command::Setup(_) | Command::Doctor(_) => {
                    unreachable!("setup/doctor commands handled before hub initialization")
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct VersionInfo {
    name: &'static str,
    binary: &'static str,
    version: &'static str,
}

fn build_workflow_execute_command(command: ExecuteCommand) -> Result<WorkflowCommand> {
    let (requirement_ids, workflow_ref, input_json, default_workflow_ref) = match command {
        ExecuteCommand::Plan(args) => (
            args.requirement_ids,
            args.workflow_ref,
            args.input_json,
            REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
        ),
        ExecuteCommand::Run(args) => (
            args.requirement_ids,
            args.workflow_ref,
            args.input_json,
            REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
        ),
    };

    let mut requirement_ids = requirement_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();

    if requirement_ids.is_empty() {
        return Err(anyhow::anyhow!(
            "missing --id value for deprecated `execute` command; pass a requirement id to delegate to `workflow execute --requirement-id`"
        ));
    }

    if requirement_ids.len() > 1 {
        return Err(anyhow::anyhow!(
            "deprecated `execute` supports only a single --id when delegating to `workflow execute --requirement-id`"
        ));
    }

    let requirement_id = requirement_ids.remove(0);

    Ok(WorkflowCommand::Execute(WorkflowExecuteArgs {
        task_id: None,
        requirement_id: Some(requirement_id),
        title: None,
        description: None,
        workflow_ref: workflow_ref.or_else(|| Some(default_workflow_ref.to_string())),
        phase: None,
        model: None,
        tool: None,
        phase_timeout_secs: None,
        input_json,
        quiet: false,
        verbose: false,
        vars: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deprecated_execute_plan_delegates_to_requirement_workflow_execute() {
        let command = ExecuteCommand::Plan(ExecuteArgs {
            requirement_ids: vec!["REQ-123".to_string()],
            workflow_ref: None,
            ai_task_generation: false,
            include_wont: false,
            input_json: None,
        });

        let workflow_command =
            build_workflow_execute_command(command).expect("execute command should build");

        match workflow_command {
            WorkflowCommand::Execute(args) => {
                assert_eq!(args.task_id, None);
                assert_eq!(args.requirement_id.as_deref(), Some("REQ-123"));
                assert_eq!(
                    args.workflow_ref.as_deref(),
                    Some(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF)
                );
            }
            other => panic!("expected workflow execute command, got {other:?}"),
        }
    }
}
