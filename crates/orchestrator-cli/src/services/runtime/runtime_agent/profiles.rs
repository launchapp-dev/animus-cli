use std::path::Path;

use anyhow::{anyhow, Result};
use orchestrator_core::agent_runtime_config::AgentProfile;
use orchestrator_daemon_runtime::DaemonEventLog;
use serde_json::{json, Value};

use crate::{
    print_value, AgentGetArgs, AgentMemoryAppendArgs, AgentMemoryClearArgs, AgentMemoryGetArgs, AgentMessageListArgs,
    AgentMessageSendArgs,
};

fn ensure_agent_exists(project_root: &str, agent_id: &str) -> Result<()> {
    load_agent_profile(project_root, agent_id).map(|_| ())
}

fn load_agent_profile(project_root: &str, agent_id: &str) -> Result<AgentProfile> {
    let config =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))?
            .config;
    config.agent_profile(agent_id).cloned().ok_or_else(|| anyhow!("unknown agent profile '{}'", agent_id))
}

// Best-effort observability tee into the daemon event log, mirroring
// `emit_interaction_event`: the log is a plain jsonl file under the global
// Animus state dir, so emission works without a running daemon, and a failure
// never blocks the memory/message mutation that triggered it. The daemon's
// event watcher and notifier plugins fan `agent-memory-updated` /
// `agent-message-sent` records out so operators can watch cross-phase
// coordination via `animus daemon events`. Emitted exactly once per
// successful store mutation — never on the read paths — so there is no event
// storm.
pub(crate) fn emit_agent_coordination_event(event_type: &str, project_root: &str, data: Value) {
    let canonical_root = crate::services::runtime::canonicalize_lossy(project_root);
    let mut seq = 0;
    let event = DaemonEventLog::next_event(&mut seq, event_type, Some(canonical_root), data);
    let _ = DaemonEventLog::append(&event);
}

pub(super) fn handle_agent_list(project_root: &str, json_output: bool) -> Result<()> {
    let loaded =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))?;
    let agents = loaded
        .config
        .agents
        .iter()
        .map(|(id, profile)| {
            json!({
                "id": id,
                "name": profile.name,
                "description": profile.description,
                "role": profile.role,
                "model": profile.model,
                "tool": profile.tool,
                "memory_enabled": profile.memory.enabled,
                "communication_enabled": profile.communication.enabled,
                "channels": profile.communication.channels,
            })
        })
        .collect::<Vec<_>>();

    print_value(
        json!({
            "source": loaded.metadata.source,
            "path": loaded.path.display().to_string(),
            "agents": agents,
        }),
        json_output,
    )
}

pub(super) fn handle_agent_get(args: AgentGetArgs, project_root: &str, json_output: bool) -> Result<()> {
    let loaded =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))?;
    let Some(profile) = loaded.config.agent_profile(&args.id) else {
        return Err(anyhow!("unknown agent profile '{}'", args.id));
    };
    print_value(json!({ "id": args.id, "profile": profile }), json_output)
}

pub(super) fn handle_agent_memory_get(args: AgentMemoryGetArgs, project_root: &str, json_output: bool) -> Result<()> {
    ensure_agent_exists(project_root, &args.agent)?;
    let memory = animus_runtime_shared::load_agent_memory(project_root, &args.agent)?;
    print_value(memory, json_output)
}

pub(super) fn handle_agent_memory_append(
    args: AgentMemoryAppendArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let profile = load_agent_profile(project_root, &args.agent)?;
    let memory = animus_runtime_shared::append_agent_memory_capped(
        project_root,
        &args.agent,
        &args.text,
        args.source.as_deref(),
        profile.memory.max_entries,
    )?;
    emit_agent_coordination_event(
        "agent-memory-updated",
        project_root,
        json!({
            "agent_id": args.agent,
            "operation": "append",
            "entry_count": memory.entries.len(),
        }),
    );
    print_value(memory, json_output)
}

pub(super) fn handle_agent_memory_clear(
    args: AgentMemoryClearArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    ensure_agent_exists(project_root, &args.agent)?;
    let memory = animus_runtime_shared::clear_agent_memory(project_root, &args.agent)?;
    emit_agent_coordination_event(
        "agent-memory-updated",
        project_root,
        json!({
            "agent_id": args.agent,
            "operation": "clear",
            "entry_count": memory.entries.len(),
        }),
    );
    print_value(memory, json_output)
}

pub(super) fn handle_agent_message_list(
    args: AgentMessageListArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    if let Some(agent) = args.agent.as_deref() {
        ensure_agent_exists(project_root, agent)?;
    }
    let messages = animus_runtime_shared::list_agent_messages(
        project_root,
        args.channel.as_deref(),
        args.agent.as_deref(),
        args.limit,
    )?;
    print_value(json!({ "messages": messages }), json_output)
}

pub(super) fn handle_agent_message_send(
    args: AgentMessageSendArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let runtime =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))?
            .config;
    let workflow = orchestrator_core::load_workflow_config_or_default(Path::new(project_root)).config;
    let Some(sender) = runtime.agent_profile(&args.from) else {
        return Err(anyhow!("unknown sender agent profile '{}'", args.from));
    };
    let Some(channel) = workflow.agent_channels.get(&args.channel) else {
        return Err(anyhow!("unknown agent channel '{}'", args.channel));
    };
    if !sender.communication.enabled {
        return Err(anyhow!("agent '{}' communication is not enabled", args.from));
    }
    if !sender.communication.channels.iter().any(|channel| channel.eq_ignore_ascii_case(&args.channel)) {
        return Err(anyhow!("agent '{}' is not configured for channel '{}'", args.from, args.channel));
    }
    if !channel.participants.iter().any(|agent| agent.eq_ignore_ascii_case(&args.from)) {
        return Err(anyhow!("agent '{}' is not a participant in channel '{}'", args.from, args.channel));
    }
    if let Some(target) = args.to.as_deref() {
        if runtime.agent_profile(target).is_none() {
            return Err(anyhow!("unknown recipient agent profile '{}'", target));
        }
        if !channel.participants.iter().any(|agent| agent.eq_ignore_ascii_case(target)) {
            return Err(anyhow!("agent '{}' is not a participant in channel '{}'", target, args.channel));
        }
        if !sender.communication.can_message.is_empty()
            && !sender.communication.can_message.iter().any(|agent| agent.eq_ignore_ascii_case(target))
        {
            return Err(anyhow!("agent '{}' is not allowed to message '{}'", args.from, target));
        }
    }

    let message = animus_runtime_shared::send_agent_message(
        project_root,
        &args.channel,
        &args.from,
        args.to.as_deref(),
        &args.text,
        args.workflow_id.as_deref(),
        args.phase_id.as_deref(),
    )?;
    emit_agent_coordination_event(
        "agent-message-sent",
        project_root,
        json!({
            "message_id": message.id,
            "channel": message.channel,
            "from_agent": message.from_agent,
            "to_agent": message.to_agent,
            "workflow_id": message.workflow_id,
            "phase_id": message.phase_id,
        }),
    );
    print_value(message, json_output)
}
