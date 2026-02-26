use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use orchestrator_core::config::resolve_project_root;
use orchestrator_core::{FileServiceHub, RuntimeConfig};
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
                Command::Task { command } => {
                    services::runtime::handle_task(command, hub.clone(), cli.json).await
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
                    services::operations::handle_execute(
                        command,
                        hub.clone(),
                        &project_root,
                        cli.json,
                    )
                    .await
                }
                Command::Planning { command } => {
                    services::operations::handle_planning(
                        command,
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
                Command::TaskControl { command } => {
                    services::operations::handle_task_control(command, hub.clone(), cli.json).await
                }
                Command::Git { command } => {
                    services::operations::handle_git(command, &project_root, cli.json).await
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
