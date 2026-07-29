use std::path::Path;

use anyhow::Result;
use orchestrator_core::agent_runtime_config::AgentProfile;
use orchestrator_daemon_runtime::DaemonEventLog;
use serde_json::{json, Value};

use crate::{
    print_value, render_table, AgentGetArgs, AgentMemoryAppendArgs, AgentMemoryClearArgs, AgentMemoryGetArgs,
    AgentMessageListArgs, AgentMessageSendArgs,
};

fn ensure_agent_exists(project_root: &str, agent_id: &str) -> Result<()> {
    load_agent_profile(project_root, agent_id, None).map(|_| ())
}

fn load_agent_profile(project_root: &str, agent_id: &str, actor: Option<&animus_actor::Actor>) -> Result<AgentProfile> {
    let config = orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata_for_actor(
        Path::new(project_root),
        actor,
    )?
    .config;
    config
        .agent_profile(agent_id)
        .cloned()
        .ok_or_else(|| crate::not_found_error(format!("unknown agent profile '{agent_id}'")))
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

pub(crate) fn agent_list_application(project_root: &str, actor: Option<&animus_actor::Actor>) -> Result<Value> {
    let loaded = orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata_for_actor(
        Path::new(project_root),
        actor,
    )?;
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

    Ok(json!({
        "source": loaded.metadata.source,
        "path": loaded.path.display().to_string(),
        "agents": agents,
    }))
}

pub(crate) fn agent_get_application(
    project_root: &str,
    agent_id: &str,
    actor: Option<&animus_actor::Actor>,
) -> Result<Value> {
    let profile = load_agent_profile(project_root, agent_id, actor)?;
    Ok(json!({ "id": agent_id, "profile": profile }))
}

pub(crate) fn agent_memory_get_application(project_root: &str, agent_id: &str) -> Result<Value> {
    ensure_agent_exists(project_root, agent_id)?;
    Ok(serde_json::to_value(animus_runtime_shared::load_agent_memory(project_root, agent_id)?)?)
}

pub(crate) fn agent_memory_append_application(
    project_root: &str,
    agent_id: &str,
    text: &str,
    source: Option<&str>,
) -> Result<Value> {
    let profile = load_agent_profile(project_root, agent_id, None)?;
    let memory = animus_runtime_shared::append_agent_memory_capped(
        project_root,
        agent_id,
        text,
        source,
        profile.memory.max_entries,
    )?;
    emit_agent_coordination_event(
        "agent-memory-updated",
        project_root,
        json!({
            "agent_id": agent_id,
            "operation": "append",
            "entry_count": memory.entries.len(),
        }),
    );
    Ok(serde_json::to_value(memory)?)
}

pub(crate) fn agent_memory_clear_application(project_root: &str, agent_id: &str) -> Result<Value> {
    ensure_agent_exists(project_root, agent_id)?;
    let memory = animus_runtime_shared::clear_agent_memory(project_root, agent_id)?;
    emit_agent_coordination_event(
        "agent-memory-updated",
        project_root,
        json!({
            "agent_id": agent_id,
            "operation": "clear",
            "entry_count": memory.entries.len(),
        }),
    );
    Ok(serde_json::to_value(memory)?)
}

pub(crate) fn agent_message_list_application(
    project_root: &str,
    channel: Option<&str>,
    agent_id: Option<&str>,
    limit: Option<usize>,
) -> Result<Value> {
    if let Some(agent_id) = agent_id {
        ensure_agent_exists(project_root, agent_id)?;
    }
    let messages = animus_runtime_shared::list_agent_messages(project_root, channel, agent_id, limit)?;
    Ok(json!({ "messages": messages }))
}

pub(crate) fn agent_message_send_application(
    project_root: &str,
    channel_id: &str,
    from_agent: &str,
    to_agent: Option<&str>,
    text: &str,
    workflow_id: Option<&str>,
    phase_id: Option<&str>,
) -> Result<Value> {
    let runtime =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))?
            .config;
    let workflow = orchestrator_core::load_workflow_config_or_default(Path::new(project_root)).config;
    let Some(sender) = runtime.agent_profile(from_agent) else {
        return Err(crate::not_found_error(format!("unknown sender agent profile '{from_agent}'")));
    };
    let Some(channel) = workflow.agent_channels.get(channel_id) else {
        return Err(crate::not_found_error(format!("unknown agent channel '{channel_id}'")));
    };
    if !sender.communication.enabled {
        return Err(crate::invalid_input_error(format!("agent '{from_agent}' communication is not enabled")));
    }
    if !sender.communication.channels.iter().any(|channel| channel.eq_ignore_ascii_case(channel_id)) {
        return Err(crate::invalid_input_error(format!(
            "agent '{from_agent}' is not configured for channel '{channel_id}'"
        )));
    }
    if !channel.participants.iter().any(|agent| agent.eq_ignore_ascii_case(from_agent)) {
        return Err(crate::invalid_input_error(format!(
            "agent '{from_agent}' is not a participant in channel '{channel_id}'"
        )));
    }
    if let Some(target) = to_agent {
        if runtime.agent_profile(target).is_none() {
            return Err(crate::not_found_error(format!("unknown recipient agent profile '{target}'")));
        }
        if !channel.participants.iter().any(|agent| agent.eq_ignore_ascii_case(target)) {
            return Err(crate::invalid_input_error(format!(
                "agent '{target}' is not a participant in channel '{channel_id}'"
            )));
        }
        if !sender.communication.can_message.is_empty()
            && !sender.communication.can_message.iter().any(|agent| agent.eq_ignore_ascii_case(target))
        {
            return Err(crate::invalid_input_error(format!(
                "agent '{from_agent}' is not allowed to message '{target}'"
            )));
        }
    }

    let message = animus_runtime_shared::send_agent_message(
        project_root,
        channel_id,
        from_agent,
        to_agent,
        text,
        workflow_id,
        phase_id,
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
    Ok(serde_json::to_value(message)?)
}

pub(super) fn handle_agent_list(project_root: &str, json_output: bool) -> Result<()> {
    let payload = agent_list_application(project_root, None)?;
    let agents = payload.get("agents").and_then(Value::as_array).cloned().unwrap_or_default();

    if !json_output {
        if agents.is_empty() {
            println!("No agent profiles configured.");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = agents
            .iter()
            .map(|agent| {
                vec![
                    agent.get("id").and_then(Value::as_str).unwrap_or("--").to_string(),
                    agent.get("name").and_then(Value::as_str).unwrap_or("--").to_string(),
                    agent.get("model").and_then(Value::as_str).unwrap_or("--").to_string(),
                    agent.get("tool").and_then(Value::as_str).unwrap_or("--").to_string(),
                    agent.get("role").and_then(Value::as_str).unwrap_or("--").to_string(),
                ]
            })
            .collect();
        render_table(&["ID", "NAME", "MODEL", "TOOL", "ROLE"], &rows);
        return Ok(());
    }

    print_value(payload, json_output)
}

pub(super) fn handle_agent_get(args: AgentGetArgs, project_root: &str, json_output: bool) -> Result<()> {
    print_value(agent_get_application(project_root, &args.id, None)?, json_output)
}

pub(super) fn handle_agent_memory_get(args: AgentMemoryGetArgs, project_root: &str, json_output: bool) -> Result<()> {
    print_value(agent_memory_get_application(project_root, &args.agent)?, json_output)
}

pub(super) fn handle_agent_memory_append(
    args: AgentMemoryAppendArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    print_value(
        agent_memory_append_application(project_root, &args.agent, &args.text, args.source.as_deref())?,
        json_output,
    )
}

pub(super) fn handle_agent_memory_clear(
    args: AgentMemoryClearArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    print_value(agent_memory_clear_application(project_root, &args.agent)?, json_output)
}

pub(super) fn handle_agent_message_list(
    args: AgentMessageListArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    print_value(
        agent_message_list_application(project_root, args.channel.as_deref(), args.agent.as_deref(), args.limit)?,
        json_output,
    )
}

pub(super) fn handle_agent_message_send(
    args: AgentMessageSendArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    print_value(
        agent_message_send_application(
            project_root,
            &args.channel,
            &args.from,
            args.to.as_deref(),
            &args.text,
            args.workflow_id.as_deref(),
            args.phase_id.as_deref(),
        )?,
        json_output,
    )
}
