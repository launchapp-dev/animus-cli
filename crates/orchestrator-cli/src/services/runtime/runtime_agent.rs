use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{AgentCommand, RunnerScopeEnvGuard};

mod connection;
mod run;
mod status;

use run::handle_agent_run;
use status::{
    handle_agent_control, handle_agent_model_status, handle_agent_runner_status,
    handle_agent_status,
};

pub(crate) async fn handle_agent(
    command: AgentCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        AgentCommand::Run(args) => {
            let _scope_guard = RunnerScopeEnvGuard::new(args.runner_scope.as_ref());
            handle_agent_run(args, hub, project_root, json).await
        }
        AgentCommand::Control(args) => {
            let _scope_guard = RunnerScopeEnvGuard::new(args.runner_scope.as_ref());
            handle_agent_control(args, hub, project_root, json).await
        }
        AgentCommand::Status(args) => {
            let _scope_guard = RunnerScopeEnvGuard::new(args.runner_scope.as_ref());
            handle_agent_status(args, hub, project_root, json).await
        }
        AgentCommand::ModelStatus(args) => {
            let _scope_guard = RunnerScopeEnvGuard::new(args.runner_scope.as_ref());
            handle_agent_model_status(args, hub, project_root, json).await
        }
        AgentCommand::RunnerStatus(args) => {
            let _scope_guard = RunnerScopeEnvGuard::new(args.runner_scope.as_ref());
            handle_agent_runner_status(args, hub, project_root, json).await
        }
    }
}
