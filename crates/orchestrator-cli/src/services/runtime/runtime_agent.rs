use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::AgentCommand;

pub(crate) mod interactions;
mod profiles;
pub(crate) mod provider_client;
mod run;
mod status;

use interactions::{
    handle_agent_approve_hook, handle_agent_interactions_answer, handle_agent_interactions_list,
    handle_agent_interactions_show,
};
use profiles::{
    handle_agent_get, handle_agent_list, handle_agent_memory_append, handle_agent_memory_clear,
    handle_agent_memory_get, handle_agent_message_list, handle_agent_message_send,
};
use run::handle_agent_run;
use status::{handle_agent_control, handle_agent_status};

pub(crate) async fn handle_agent(
    command: AgentCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        AgentCommand::List => handle_agent_list(project_root, json),
        AgentCommand::Get(args) => handle_agent_get(args, project_root, json),
        AgentCommand::Run(args) => handle_agent_run(args, hub, project_root, json).await,
        AgentCommand::Control(args) => handle_agent_control(args, hub, project_root, json).await,
        AgentCommand::Status(args) => handle_agent_status(args, hub, project_root, json).await,
        AgentCommand::Memory { command } => match command {
            crate::AgentMemoryCommand::Get(args) => handle_agent_memory_get(args, project_root, json),
            crate::AgentMemoryCommand::Append(args) => handle_agent_memory_append(args, project_root, json),
            crate::AgentMemoryCommand::Clear(args) => handle_agent_memory_clear(args, project_root, json),
        },
        AgentCommand::Message { command } => match command {
            crate::AgentMessageCommand::Send(args) => handle_agent_message_send(args, project_root, json),
            crate::AgentMessageCommand::List(args) => handle_agent_message_list(args, project_root, json),
        },
        AgentCommand::Interactions { command } => match command {
            crate::AgentInteractionsCommand::List(args) => handle_agent_interactions_list(args, project_root, json),
            crate::AgentInteractionsCommand::Show(args) => handle_agent_interactions_show(args, project_root, json),
            crate::AgentInteractionsCommand::Answer(args) => {
                handle_agent_interactions_answer(args, project_root, json).await
            }
        },
        AgentCommand::ApproveHook(args) => handle_agent_approve_hook(args, project_root).await,
    }
}
