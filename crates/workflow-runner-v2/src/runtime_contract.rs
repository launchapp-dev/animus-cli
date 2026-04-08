use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::warn;

use crate::config_context::RuntimeConfigContext;

fn merge_schema_into(base: &mut Value, overlay: &Value) -> Result<()> {
    if let Some(extra_properties) = overlay.get("properties").and_then(Value::as_object) {
        let properties = base
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("schema properties should be an object"))?;
        for (key, value) in extra_properties {
            properties.insert(key.clone(), value.clone());
        }
    }

    if let Some(extra_required) = overlay.get("required").and_then(Value::as_array) {
        let required = base
            .get_mut("required")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("schema required should be an array"))?;
        for field in extra_required {
            if !required.contains(field) {
                required.push(field.clone());
            }
        }
    }
    Ok(())
}

fn phase_field_schema(definition: &orchestrator_core::agent_runtime_config::PhaseFieldDefinition) -> Result<Value> {
    let mut schema = serde_json::json!({
        "type": definition.field_type
    });

    if !definition.enum_values.is_empty() {
        schema.as_object_mut().ok_or_else(|| anyhow!("field schema should be object"))?.insert(
            "enum".to_string(),
            Value::Array(definition.enum_values.iter().cloned().map(Value::String).collect()),
        );
    }

    if let Some(items) = definition.items.as_ref() {
        schema
            .as_object_mut()
            .ok_or_else(|| anyhow!("field schema should be object"))?
            .insert("items".to_string(), phase_field_schema(items)?);
    }

    if !definition.fields.is_empty() {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, nested) in &definition.fields {
            properties.insert(name.clone(), phase_field_schema(nested)?);
            if nested.required {
                required.push(Value::String(name.clone()));
            }
        }
        let object = schema.as_object_mut().ok_or_else(|| anyhow!("field schema should be object"))?;
        object.insert("properties".to_string(), Value::Object(properties));
        if !required.is_empty() {
            object.insert("required".to_string(), Value::Array(required));
        }
        object.insert("additionalProperties".to_string(), Value::Bool(true));
    }

    Ok(schema)
}

fn apply_contract_fields(
    schema: &mut Value,
    fields: &std::collections::BTreeMap<String, orchestrator_core::agent_runtime_config::PhaseFieldDefinition>,
    required_fields: &[String],
) -> Result<()> {
    let mut property_updates: Vec<(String, Value)> = Vec::new();
    let mut required_updates: Vec<String> = Vec::new();

    for field_name in required_fields {
        required_updates.push(field_name.clone());
        property_updates.push((field_name.clone(), serde_json::json!({})));
    }

    for (field_name, field) in fields {
        property_updates.push((field_name.clone(), phase_field_schema(field)?));
        if field.required {
            required_updates.push(field_name.clone());
        }
    }

    {
        let properties = schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("schema properties should be an object"))?;
        for (field_name, field_schema) in property_updates {
            properties.insert(field_name, field_schema);
        }
    }

    {
        let required = schema
            .get_mut("required")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("schema required should be an array"))?;
        for field_name in required_updates {
            let entry = Value::String(field_name);
            if !required.contains(&entry) {
                required.push(entry);
            }
        }
    }
    Ok(())
}

pub fn phase_output_json_schema_for(ctx: &RuntimeConfigContext, phase_id: &str) -> Result<Option<Value>> {
    let contract = ctx.phase_output_contract(phase_id).cloned();
    let explicit_schema = ctx.phase_output_json_schema(phase_id).cloned();

    match (contract, explicit_schema) {
        (None, None) => Ok(None),
        (Some(contract), explicit_schema) => {
            let mut schema = serde_json::json!({
                "type": "object",
                "required": ["kind"],
                "properties": {
                    "kind": { "const": contract.kind }
                },
                "additionalProperties": true
            });
            apply_contract_fields(&mut schema, &contract.fields, &contract.required_fields)?;
            if let Some(explicit_schema) = explicit_schema.as_ref() {
                merge_schema_into(&mut schema, explicit_schema)?;
            }
            Ok(Some(schema))
        }
        (None, Some(explicit_schema)) => Ok(Some(explicit_schema)),
    }
}

pub fn phase_decision_json_schema_for(ctx: &RuntimeConfigContext, phase_id: &str) -> Result<Option<Value>> {
    let contract = match ctx.phase_decision_contract(phase_id) {
        Some(c) => c,
        None => return Ok(None),
    };
    let allowed_risks = match contract.max_risk {
        orchestrator_core::WorkflowDecisionRisk::Low => vec!["low"],
        orchestrator_core::WorkflowDecisionRisk::Medium => vec!["low", "medium"],
        orchestrator_core::WorkflowDecisionRisk::High => vec!["low", "medium", "high"],
    };
    let evidence_kind_schema = serde_json::json!({ "type": "string" });

    // Build required fields — evidence is only required if there are required evidence types
    let mut required_fields = vec!["kind", "phase_id", "verdict", "confidence", "risk", "reason"];
    if !contract.required_evidence.is_empty() {
        required_fields.push("evidence");
    }

    let mut schema = serde_json::json!({
        "type": "object",
        "required": required_fields.iter().map(|s| Value::String(s.to_string())).collect::<Vec<_>>(),
        "properties": {
            "kind": { "const": "phase_decision" },
            "phase_id": { "const": phase_id },
            "verdict": { "enum": ["advance", "rework", "fail", "skip"] },
            "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
            "risk": { "enum": allowed_risks },
            "reason": { "type": "string", "minLength": 1 },
            "evidence": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["kind", "description"],
                    "properties": {
                        "kind": evidence_kind_schema,
                        "description": { "type": "string", "minLength": 1 },
                        "file_path": { "type": "string" },
                        "value": {}
                    },
                    "additionalProperties": true
                }
            },
            "guardrail_violations": {
                "type": "array",
                "items": { "type": "string" }
            },
            "commit_message": { "type": "string" }
        },
        "additionalProperties": true
    });

    apply_contract_fields(&mut schema, &contract.fields, &[])?;
    if let Some(extra_schema) = contract.extra_json_schema.as_ref() {
        merge_schema_into(&mut schema, extra_schema)?;
    }

    Ok(Some(schema))
}

pub fn phase_response_json_schema_for(ctx: &RuntimeConfigContext, phase_id: &str) -> Result<Option<Value>> {
    let output_schema = phase_output_json_schema_for(ctx, phase_id)?;
    let decision_schema = phase_decision_json_schema_for(ctx, phase_id)?;

    match (output_schema, decision_schema) {
        (Some(mut output_schema), Some(decision_schema)) => {
            let required_decision =
                ctx.phase_decision_contract(phase_id).map(|contract| !contract.allow_missing_decision).unwrap_or(false);
            let properties = output_schema
                .get_mut("properties")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow!("output schema properties should be an object"))?;
            properties.insert("phase_decision".to_string(), decision_schema);
            if required_decision {
                let required = output_schema.get_mut("required").and_then(Value::as_array_mut);
                if let Some(required) = required {
                    let field = Value::String("phase_decision".to_string());
                    if !required.contains(&field) {
                        required.push(field);
                    }
                } else if let Some(object) = output_schema.as_object_mut() {
                    object.insert(
                        "required".to_string(),
                        Value::Array(vec![Value::String("phase_decision".to_string())]),
                    );
                }
            }
            Ok(Some(output_schema))
        }
        (Some(output_schema), None) => Ok(Some(output_schema)),
        (None, Some(decision_schema)) => Ok(Some(decision_schema)),
        (None, None) => Ok(None),
    }
}

pub fn inject_read_only_flag(runtime_contract: &mut Value, config: &orchestrator_core::AgentRuntimeConfig) {
    let cli_name = runtime_contract.pointer("/cli/name").and_then(Value::as_str).unwrap_or("");

    if let Some(flag) = orchestrator_core::cli_tool_read_only_flag(cli_name, config) {
        if let Some(args) = runtime_contract.pointer_mut("/cli/launch/args").and_then(Value::as_array_mut) {
            let prompt_idx = args.len().saturating_sub(1);
            args.insert(prompt_idx, Value::String(flag));
        }
    }
}

pub fn apply_phase_capability_launch_flags(
    runtime_contract: &mut Value,
    caps: &protocol::PhaseCapabilities,
    config: &orchestrator_core::AgentRuntimeConfig,
) {
    if caps.is_strictly_read_only() {
        inject_read_only_flag(runtime_contract, config);
    }
}

pub fn inject_response_schema_into_launch_args(
    runtime_contract: &mut Value,
    schema: &Value,
    config: &orchestrator_core::AgentRuntimeConfig,
) {
    let cli_name = runtime_contract.pointer("/cli/name").and_then(Value::as_str).unwrap_or("");

    if let Some(flag) = orchestrator_core::cli_tool_response_schema_flag(cli_name, config) {
        if let Some(args) = runtime_contract.pointer_mut("/cli/launch/args").and_then(Value::as_array_mut) {
            let prompt_idx = args.len().saturating_sub(1);
            let schema_str = serde_json::to_string(schema).unwrap_or_default();
            args.insert(prompt_idx, Value::String(flag));
            args.insert(prompt_idx + 1, Value::String(schema_str));
        }
    }
}

pub fn inject_default_stdio_mcp(runtime_contract: &mut Value, project_root: &str) {
    inject_default_stdio_mcp_with_config(runtime_contract, project_root, &protocol::McpRuntimeConfig::default());
}

pub fn inject_default_stdio_mcp_with_config(
    runtime_contract: &mut Value,
    project_root: &str,
    mcp_config: &protocol::McpRuntimeConfig,
) {
    if runtime_contract.pointer("/mcp/stdio/command").and_then(Value::as_str).is_some_and(|v| !v.trim().is_empty()) {
        return;
    }

    if mcp_config.is_http_transport() {
        return;
    }

    let supports_mcp =
        runtime_contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool).unwrap_or(false);
    if !supports_mcp {
        return;
    }

    let command = mcp_config.stdio_command.clone().filter(|v| !v.trim().is_empty()).or_else(|| {
        let exe = std::env::current_exe().ok()?;
        let exe_dir = exe.parent()?;
        let ao_binary = exe_dir.join("ao");
        if ao_binary.exists() {
            Some(ao_binary.to_string_lossy().to_string())
        } else {
            Some(exe.to_string_lossy().to_string())
        }
    });
    let Some(command) = command else {
        return;
    };

    let args =
        mcp_config.stdio_args_json.as_deref().and_then(|v| serde_json::from_str::<Vec<String>>(v).ok()).unwrap_or_else(
            || vec!["--project-root".to_string(), project_root.to_string(), "mcp".to_string(), "serve".to_string()],
        );

    if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
        mcp.insert("stdio".to_string(), serde_json::json!({ "command": command, "args": args }));
        let has_agent_id = mcp.get("agent_id").and_then(Value::as_str).is_some_and(|v| !v.trim().is_empty());
        if !has_agent_id {
            mcp.insert("agent_id".to_string(), serde_json::json!("ao"));
        }
    }
}

pub fn inject_agent_tool_policy(runtime_contract: &mut Value, ctx: &RuntimeConfigContext, phase_id: &str) {
    let agent_id = ctx.phase_agent_id(phase_id);

    let wf_profile = agent_id.as_deref().and_then(|id| ctx.workflow_config.config.agent_profiles.get(id));

    let rt_profile = agent_id.as_deref().and_then(|id| ctx.agent_runtime_config.agent_profile(id));

    let policy = wf_profile.map(|p| &p.tool_policy).or_else(|| rt_profile.map(|p| &p.tool_policy));

    let Some(policy) = policy else {
        return;
    };
    set_mcp_tool_policy(runtime_contract, policy);
}

pub fn set_mcp_tool_policy(
    runtime_contract: &mut Value,
    policy: &orchestrator_core::agent_runtime_config::AgentToolPolicy,
) {
    if policy.allow.is_empty() && policy.deny.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
        mcp.insert(
            "tool_policy".to_string(),
            serde_json::json!({
                "allow": policy.allow,
                "deny": policy.deny,
            }),
        );
    }
}

fn primary_mcp_agent_id(runtime_contract: &Value) -> Option<&str> {
    runtime_contract.pointer("/mcp/agent_id").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty())
}

fn remove_additional_mcp_server_collisions(
    runtime_contract: &Value,
    servers: serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let Some(agent_id) = primary_mcp_agent_id(runtime_contract) else {
        return servers;
    };

    let mut filtered = serde_json::Map::new();
    let mut skipped = Vec::new();

    for (name, value) in servers {
        if name.eq_ignore_ascii_case(agent_id) {
            skipped.push(name);
        } else {
            filtered.insert(name, value);
        }
    }

    if !skipped.is_empty() {
        warn!(
            agent_id,
            skipped_additional_servers = ?skipped,
            "Ignoring additional MCP servers that collide with the primary agent id while building the runtime contract"
        );
    }

    filtered
}

pub fn inject_project_mcp_servers(
    runtime_contract: &mut Value,
    project_root: &str,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) {
    let project_config = match protocol::Config::load_or_default(project_root) {
        Ok(c) => c,
        Err(_) => return,
    };
    if project_config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = ctx.phase_agent_id(phase_id);
    let mut servers = serde_json::Map::new();
    for (name, entry) in &project_config.mcp_servers {
        let assigned = entry.assign_to.is_empty()
            || agent_id.as_deref().is_some_and(|id| entry.assign_to.iter().any(|a| a.eq_ignore_ascii_case(id)));
        if !assigned {
            continue;
        }
        let mut entry_json = serde_json::json!({
            "command": entry.command,
            "args": entry.args,
            "env": entry.env,
        });
        if let Some(transport) = &entry.transport {
            entry_json["transport"] = serde_json::Value::String(transport.clone());
        }
        if let Some(url) = &entry.url {
            entry_json["url"] = serde_json::Value::String(url.clone());
        }
        servers.insert(name.clone(), entry_json);
    }
    let servers = remove_additional_mcp_server_collisions(runtime_contract, servers);
    if servers.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
        mcp.insert("additional_servers".to_string(), Value::Object(servers));
    }
}

pub fn inject_workflow_mcp_servers(runtime_contract: &mut Value, ctx: &RuntimeConfigContext, phase_id: &str) {
    if ctx.workflow_config.config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = ctx.phase_agent_id(phase_id);
    let workflow_profile_servers: Vec<String> = agent_id
        .as_deref()
        .and_then(|id| ctx.workflow_config.config.agent_profiles.get(id))
        .map(|profile| profile.mcp_servers.clone())
        .unwrap_or_default();
    let runtime_profile_servers: Vec<String> = if workflow_profile_servers.is_empty() {
        agent_id
            .as_deref()
            .and_then(|id| ctx.agent_runtime_config.agent_profile(id))
            .map(|profile| profile.mcp_servers.clone())
            .filter(|servers| !servers.is_empty())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let phase_servers = ctx.phase_mcp_servers(phase_id);

    let mut allowed_servers = std::collections::BTreeSet::new();
    for server in workflow_profile_servers.iter().chain(runtime_profile_servers.iter()).chain(phase_servers.iter()) {
        let trimmed = server.trim();
        if !trimmed.is_empty() {
            allowed_servers.insert(trimmed.to_string());
        }
    }

    let existing =
        runtime_contract.pointer("/mcp/additional_servers").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut servers = existing;

    for (name, definition) in &ctx.workflow_config.config.mcp_servers {
        if !allowed_servers.is_empty() && !allowed_servers.contains(name) {
            continue;
        }
        let mut entry_json = serde_json::json!({
            "command": definition.command,
            "args": definition.args,
            "env": definition.env,
        });
        if let Some(transport) = &definition.transport {
            entry_json["transport"] = serde_json::Value::String(transport.clone());
        }
        if let Some(url) = &definition.url {
            entry_json["url"] = serde_json::Value::String(url.clone());
        }
        servers.insert(name.clone(), entry_json);
    }
    let servers = remove_additional_mcp_server_collisions(runtime_contract, servers);
    if servers.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
        mcp.insert("additional_servers".to_string(), Value::Object(servers));
    }
}

pub fn inject_named_mcp_servers(
    runtime_contract: &mut Value,
    project_root: &str,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
    names: &[String],
) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }

    let project_config = protocol::Config::load_or_default(project_root)
        .map_err(|error| anyhow!("failed to load project config: {error}"))?;
    let existing =
        runtime_contract.pointer("/mcp/additional_servers").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut servers = existing;

    for raw_name in names {
        let name = raw_name.trim();
        if name.is_empty() {
            continue;
        }

        if let Some(definition) = ctx.workflow_config.config.mcp_servers.get(name) {
            let mut entry_json = serde_json::json!({
                "command": definition.command,
                "args": definition.args,
                "env": definition.env,
            });
            if let Some(transport) = &definition.transport {
                entry_json["transport"] = serde_json::Value::String(transport.clone());
            }
            if let Some(url) = &definition.url {
                entry_json["url"] = serde_json::Value::String(url.clone());
            }
            servers.insert(name.to_string(), entry_json);
            continue;
        }

        if let Some(definition) = project_config.mcp_servers.get(name) {
            let mut entry_json = serde_json::json!({
                "command": definition.command,
                "args": definition.args,
                "env": definition.env,
            });
            if let Some(transport) = &definition.transport {
                entry_json["transport"] = serde_json::Value::String(transport.clone());
            }
            if let Some(url) = &definition.url {
                entry_json["url"] = serde_json::Value::String(url.clone());
            }
            servers.insert(name.to_string(), entry_json);
            continue;
        }

        return Err(anyhow!(
            "skill requested MCP server '{}' for phase '{}' but no matching server is defined in workflow YAML or project config",
            name,
            phase_id
        ));
    }

    let servers = remove_additional_mcp_server_collisions(runtime_contract, servers);
    if servers.is_empty() {
        return Ok(());
    }
    if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
        mcp.insert("additional_servers".to_string(), Value::Object(servers));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use orchestrator_config::McpServerDefinition;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, workflow_config_hash, LoadedWorkflowConfig,
        PhaseMcpBinding, WorkflowConfigMetadata, WorkflowConfigSource,
    };

    use super::*;

    #[test]
    fn inject_workflow_mcp_servers_includes_phase_bound_pack_servers() {
        let mut workflow_config = builtin_workflow_config();
        workflow_config.mcp_servers.insert(
            "ao.requirements/ao".to_string(),
            McpServerDefinition {
                command: "node".to_string(),
                args: vec!["server.js".to_string()],
                transport: Some("stdio".to_string()),
                url: None,
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
            },
        );
        workflow_config
            .phase_mcp_bindings
            .insert("research".to_string(), PhaseMcpBinding { servers: vec!["ao.requirements/ao".to_string()] });

        let loaded_workflow_config = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: workflow_config.schema.clone(),
                version: workflow_config.version,
                hash: workflow_config_hash(&workflow_config),
                source: WorkflowConfigSource::Builtin,
            },
            config: workflow_config,
            path: PathBuf::from("builtin"),
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: loaded_workflow_config,
        };

        let mut runtime_contract = serde_json::json!({
            "mcp": {}
        });
        inject_workflow_mcp_servers(&mut runtime_contract, &ctx, "research");

        let additional_servers = runtime_contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should be injected");
        assert!(additional_servers.contains_key("ao.requirements/ao"));
    }

    #[test]
    fn inject_workflow_mcp_servers_skips_primary_agent_id_collisions() {
        let loaded_workflow_config = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: builtin_workflow_config().schema.clone(),
                version: builtin_workflow_config().version,
                hash: workflow_config_hash(&builtin_workflow_config()),
                source: WorkflowConfigSource::Builtin,
            },
            config: builtin_workflow_config(),
            path: PathBuf::from("builtin"),
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: loaded_workflow_config,
        };

        let mut runtime_contract = serde_json::json!({
            "mcp": {
                "agent_id": "ao",
                "stdio": {
                    "command": "/path/to/ao/target/debug/ao",
                    "args": ["--project-root", "/path/to/project", "mcp", "serve"]
                }
            }
        });
        inject_workflow_mcp_servers(&mut runtime_contract, &ctx, "requirements");

        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "built-in workflow MCP injection should not duplicate the primary ao server"
        );
    }

    #[test]
    fn inject_named_mcp_servers_skips_primary_agent_id_collisions() {
        let loaded_workflow_config = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: builtin_workflow_config().schema.clone(),
                version: builtin_workflow_config().version,
                hash: workflow_config_hash(&builtin_workflow_config()),
                source: WorkflowConfigSource::Builtin,
            },
            config: builtin_workflow_config(),
            path: PathBuf::from("builtin"),
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: loaded_workflow_config,
        };

        let mut runtime_contract = serde_json::json!({
            "mcp": {
                "agent_id": "ao",
                "stdio": {
                    "command": "/path/to/ao/target/debug/ao",
                    "args": ["--project-root", "/path/to/project", "mcp", "serve"]
                }
            }
        });
        inject_named_mcp_servers(&mut runtime_contract, "/path/to/project", &ctx, "requirements", &["ao".to_string()])
            .expect("named MCP injection should succeed");

        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "named MCP injection should not duplicate the primary ao server"
        );
    }

    #[test]
    fn managed_state_phases_do_not_receive_read_only_cli_flags() {
        let config = builtin_agent_runtime_config();
        let mut runtime_contract = serde_json::json!({
            "cli": {
                "name": "oai-runner",
                "launch": {
                    "args": ["run", "prompt"]
                }
            }
        });

        apply_phase_capability_launch_flags(
            &mut runtime_contract,
            &protocol::PhaseCapabilities { mutates_state: true, ..Default::default() },
            &config,
        );

        let args = runtime_contract.pointer("/cli/launch/args").and_then(Value::as_array).expect("launch args");
        assert!(
            !args.iter().any(|value| value.as_str() == Some("--read-only")),
            "managed state mutation phases should not inject a strict read-only CLI flag"
        );
    }

    #[test]
    fn phase_decision_json_schema_accepts_any_evidence_kind() {
        let workflow_config = builtin_workflow_config();

        let loaded_workflow_config = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: workflow_config.schema.clone(),
                version: workflow_config.version,
                hash: workflow_config_hash(&workflow_config),
                source: WorkflowConfigSource::Builtin,
            },
            config: workflow_config,
            path: PathBuf::from("builtin"),
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: loaded_workflow_config,
        };

        // Test with implementation phase which has required_evidence set
        let schema = phase_decision_json_schema_for(&ctx, "implementation")
            .expect("should generate schema")
            .expect("schema should exist for implementation phase");

        // Get the evidence kind schema from the decision schema
        let evidence_kind_schema =
            schema.pointer("/properties/evidence/items/properties/kind").expect("evidence kind schema should exist");

        // Verify that the kind field accepts any string, not just required kinds
        assert_eq!(
            evidence_kind_schema.get("type"),
            Some(&Value::String("string".to_string())),
            "evidence kind should accept any string type"
        );

        // Verify there's no enum constraint that would restrict to specific kinds
        assert!(
            evidence_kind_schema.get("enum").is_none(),
            "evidence kind should not have enum constraint - agents should be able to use custom evidence kinds like bug_confirmed, fix_identified, etc"
        );
    }

    #[test]
    fn phase_decision_validates_custom_evidence_kinds_like_bug_confirmed() {
        use crate::phase_executor::validate_basic_json_schema;

        let workflow_config = builtin_workflow_config();

        let loaded_workflow_config = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: workflow_config.schema.clone(),
                version: workflow_config.version,
                hash: workflow_config_hash(&workflow_config),
                source: WorkflowConfigSource::Builtin,
            },
            config: workflow_config,
            path: PathBuf::from("builtin"),
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: loaded_workflow_config,
        };

        let schema = phase_decision_json_schema_for(&ctx, "implementation")
            .expect("should generate schema")
            .expect("schema should exist for implementation phase");

        // Test that a phase decision with custom evidence kinds (bug_confirmed, fix_identified)
        // is now accepted by the schema - this was the issue in TASK-222
        let decision_with_custom_evidence = serde_json::json!({
            "kind": "phase_decision",
            "phase_id": "implementation",
            "verdict": "advance",
            "confidence": 0.95,
            "risk": "low",
            "reason": "Issue found and fixed",
            "evidence": [
                {
                    "kind": "bug_confirmed",
                    "description": "Found and documented the bug"
                },
                {
                    "kind": "fix_identified",
                    "description": "Implemented a fix for the issue"
                }
            ]
        });

        // This should validate successfully now
        validate_basic_json_schema(&decision_with_custom_evidence, &schema)
            .expect("phase decision with custom evidence kinds should validate");
    }

    #[test]
    fn phase_decision_evidence_field_optional_when_no_required_evidence() {
        use crate::phase_executor::validate_basic_json_schema;

        let workflow_config = builtin_workflow_config();

        let loaded_workflow_config = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: workflow_config.schema.clone(),
                version: workflow_config.version,
                hash: workflow_config_hash(&workflow_config),
                source: WorkflowConfigSource::Builtin,
            },
            config: workflow_config,
            path: PathBuf::from("builtin"),
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: loaded_workflow_config,
        };

        let schema = phase_decision_json_schema_for(&ctx, "implementation")
            .expect("should generate schema")
            .expect("schema should exist for implementation phase");

        // Verify that evidence is NOT in the required fields when required_evidence is empty
        let required_fields = schema.get("required").and_then(Value::as_array).expect("required should be an array");
        let required_field_strings: Vec<&str> =
            required_fields.iter().filter_map(|v| v.as_str()).collect();

        assert!(!required_field_strings.contains(&"evidence"), "evidence should not be required when required_evidence is empty");
        assert!(required_field_strings.contains(&"verdict"), "verdict should be required");
        assert!(required_field_strings.contains(&"confidence"), "confidence should be required");

        // Test that a phase decision WITHOUT evidence field validates successfully
        let decision_without_evidence = serde_json::json!({
            "kind": "phase_decision",
            "phase_id": "implementation",
            "verdict": "advance",
            "confidence": 0.95,
            "risk": "low",
            "reason": "Implementation complete"
        });

        validate_basic_json_schema(&decision_without_evidence, &schema)
            .expect("phase decision without evidence field should validate when no required evidence types");
    }
}
