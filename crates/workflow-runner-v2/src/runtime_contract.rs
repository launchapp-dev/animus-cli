use serde_json::Value;
use std::path::Path;

use crate::config_context::RuntimeConfigContext;

fn merge_schema_into(base: &mut Value, overlay: &Value) {
    if let Some(extra_properties) = overlay.get("properties").and_then(Value::as_object) {
        let properties = base
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .expect("schema properties should be an object");
        for (key, value) in extra_properties {
            properties.insert(key.clone(), value.clone());
        }
    }

    if let Some(extra_required) = overlay.get("required").and_then(Value::as_array) {
        let required = base
            .get_mut("required")
            .and_then(Value::as_array_mut)
            .expect("schema required should be an array");
        for field in extra_required {
            if !required.contains(field) {
                required.push(field.clone());
            }
        }
    }
}

fn phase_field_schema(
    definition: &orchestrator_core::agent_runtime_config::PhaseFieldDefinition,
) -> Value {
    let mut schema = serde_json::json!({
        "type": definition.field_type
    });

    if !definition.enum_values.is_empty() {
        schema
            .as_object_mut()
            .expect("field schema should be object")
            .insert(
                "enum".to_string(),
                Value::Array(
                    definition
                        .enum_values
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
    }

    if let Some(items) = definition.items.as_ref() {
        schema
            .as_object_mut()
            .expect("field schema should be object")
            .insert("items".to_string(), phase_field_schema(items));
    }

    if !definition.fields.is_empty() {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, nested) in &definition.fields {
            properties.insert(name.clone(), phase_field_schema(nested));
            if nested.required {
                required.push(Value::String(name.clone()));
            }
        }
        let object = schema
            .as_object_mut()
            .expect("field schema should be object");
        object.insert("properties".to_string(), Value::Object(properties));
        if !required.is_empty() {
            object.insert("required".to_string(), Value::Array(required));
        }
        object.insert("additionalProperties".to_string(), Value::Bool(true));
    }

    schema
}

fn apply_contract_fields(
    schema: &mut Value,
    fields: &std::collections::BTreeMap<
        String,
        orchestrator_core::agent_runtime_config::PhaseFieldDefinition,
    >,
    required_fields: &[String],
) {
    let mut property_updates: Vec<(String, Value)> = Vec::new();
    let mut required_updates: Vec<String> = Vec::new();

    for field_name in required_fields {
        required_updates.push(field_name.clone());
        property_updates.push((field_name.clone(), serde_json::json!({})));
    }

    for (field_name, field) in fields {
        property_updates.push((field_name.clone(), phase_field_schema(field)));
        if field.required {
            required_updates.push(field_name.clone());
        }
    }

    {
        let properties = schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .expect("schema properties should be an object");
        for (field_name, field_schema) in property_updates {
            properties.insert(field_name, field_schema);
        }
    }

    {
        let required = schema
            .get_mut("required")
            .and_then(Value::as_array_mut)
            .expect("schema required should be an array");
        for field_name in required_updates {
            let entry = Value::String(field_name);
            if !required.contains(&entry) {
                required.push(entry);
            }
        }
    }
}

pub fn phase_output_json_schema_for(ctx: &RuntimeConfigContext, phase_id: &str) -> Option<Value> {
    let contract = ctx.phase_output_contract(phase_id).cloned();
    let explicit_schema = ctx.phase_output_json_schema(phase_id).cloned();

    match (contract, explicit_schema) {
        (None, None) => None,
        (Some(contract), explicit_schema) => {
            let mut schema = serde_json::json!({
                "type": "object",
                "required": ["kind"],
                "properties": {
                    "kind": { "const": contract.kind }
                },
                "additionalProperties": true
            });
            apply_contract_fields(&mut schema, &contract.fields, &contract.required_fields);
            if let Some(explicit_schema) = explicit_schema.as_ref() {
                merge_schema_into(&mut schema, explicit_schema);
            }
            Some(schema)
        }
        (None, Some(explicit_schema)) => Some(explicit_schema),
    }
}

pub fn phase_decision_json_schema_for(
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) -> Option<Value> {
    let contract = ctx.phase_decision_contract(phase_id)?;
    let allowed_risks = match contract.max_risk {
        orchestrator_core::WorkflowDecisionRisk::Low => vec!["low"],
        orchestrator_core::WorkflowDecisionRisk::Medium => vec!["low", "medium"],
        orchestrator_core::WorkflowDecisionRisk::High => vec!["low", "medium", "high"],
    };
    let evidence_kind_schema = if contract.required_evidence.is_empty() {
        serde_json::json!({ "type": "string" })
    } else {
        serde_json::json!({
            "enum": contract.required_evidence.iter().map(|kind| serde_json::to_value(kind).unwrap_or(serde_json::json!("custom"))).collect::<Vec<_>>()
        })
    };

    let mut schema = serde_json::json!({
        "type": "object",
        "required": ["kind", "phase_id", "verdict", "confidence", "risk", "reason", "evidence"],
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

    apply_contract_fields(&mut schema, &contract.fields, &[]);
    if let Some(extra_schema) = contract.extra_json_schema.as_ref() {
        merge_schema_into(&mut schema, extra_schema);
    }

    Some(schema)
}

pub fn phase_response_json_schema_for(
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) -> Option<Value> {
    let output_schema = phase_output_json_schema_for(ctx, phase_id);
    let decision_schema = phase_decision_json_schema_for(ctx, phase_id);

    match (output_schema, decision_schema) {
        (Some(mut output_schema), Some(decision_schema)) => {
            let required_decision = ctx
                .phase_decision_contract(phase_id)
                .map(|contract| !contract.allow_missing_decision)
                .unwrap_or(false);
            let properties = output_schema
                .get_mut("properties")
                .and_then(Value::as_object_mut)?;
            properties.insert("phase_decision".to_string(), decision_schema);
            if required_decision {
                let required = output_schema
                    .get_mut("required")
                    .and_then(Value::as_array_mut);
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
            Some(output_schema)
        }
        (Some(output_schema), None) => Some(output_schema),
        (None, Some(decision_schema)) => Some(decision_schema),
        (None, None) => None,
    }
}

pub fn inject_read_only_flag(
    runtime_contract: &mut Value,
    config: &orchestrator_core::AgentRuntimeConfig,
) {
    let cli_name = runtime_contract
        .pointer("/cli/name")
        .and_then(Value::as_str)
        .unwrap_or("");

    if let Some(flag) = orchestrator_core::cli_tool_read_only_flag(cli_name, config) {
        if let Some(args) = runtime_contract
            .pointer_mut("/cli/launch/args")
            .and_then(Value::as_array_mut)
        {
            let prompt_idx = args.len().saturating_sub(1);
            args.insert(prompt_idx, Value::String(flag));
        }
    }
}

pub fn inject_response_schema_into_launch_args(
    runtime_contract: &mut Value,
    schema: &Value,
    config: &orchestrator_core::AgentRuntimeConfig,
) {
    let cli_name = runtime_contract
        .pointer("/cli/name")
        .and_then(Value::as_str)
        .unwrap_or("");

    if let Some(flag) = orchestrator_core::cli_tool_response_schema_flag(cli_name, config) {
        if let Some(args) = runtime_contract
            .pointer_mut("/cli/launch/args")
            .and_then(Value::as_array_mut)
        {
            let prompt_idx = args.len().saturating_sub(1);
            let schema_str = serde_json::to_string(schema).unwrap_or_default();
            args.insert(prompt_idx, Value::String(flag));
            args.insert(prompt_idx + 1, Value::String(schema_str));
        }
    }
}

pub fn inject_default_stdio_mcp(runtime_contract: &mut Value, project_root: &str) {
    if runtime_contract
        .pointer("/mcp/stdio/command")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.trim().is_empty())
    {
        return;
    }

    if std::env::var("AO_MCP_TRANSPORT")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| v == "http")
    {
        return;
    }

    let supports_mcp = runtime_contract
        .pointer("/cli/capabilities/supports_mcp")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !supports_mcp {
        return;
    }

    let command = std::env::var("AO_MCP_STDIO_COMMAND")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        });
    let Some(command) = command else {
        return;
    };

    let args = std::env::var("AO_MCP_STDIO_ARGS_JSON")
        .ok()
        .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
        .unwrap_or_else(|| {
            vec![
                "--project-root".to_string(),
                project_root.to_string(),
                "mcp".to_string(),
                "serve".to_string(),
            ]
        });

    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert(
            "stdio".to_string(),
            serde_json::json!({ "command": command, "args": args }),
        );
        let has_agent_id = mcp
            .get("agent_id")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty());
        if !has_agent_id {
            mcp.insert("agent_id".to_string(), serde_json::json!("ao"));
        }
    }
}

pub fn inject_agent_tool_policy(
    runtime_contract: &mut Value,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) {
    let agent_id = ctx.phase_agent_id(phase_id);

    let wf_profile = agent_id
        .as_deref()
        .and_then(|id| ctx.workflow_config.config.agent_profiles.get(id));

    let rt_profile = agent_id
        .as_deref()
        .and_then(|id| ctx.agent_runtime_config.agent_profile(id));

    let policy = wf_profile
        .map(|p| &p.tool_policy)
        .or_else(|| rt_profile.map(|p| &p.tool_policy));

    let Some(policy) = policy else {
        return;
    };
    if policy.allow.is_empty() && policy.deny.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert(
            "tool_policy".to_string(),
            serde_json::json!({
                "allow": policy.allow,
                "deny": policy.deny,
            }),
        );
    }
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
            || agent_id
                .as_deref()
                .is_some_and(|id| entry.assign_to.iter().any(|a| a.eq_ignore_ascii_case(id)));
        if !assigned {
            continue;
        }
        servers.insert(
            name.clone(),
            serde_json::json!({
                "command": entry.command,
                "args": entry.args,
                "env": entry.env,
            }),
        );
    }
    if servers.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert("additional_servers".to_string(), Value::Object(servers));
    }
}

pub fn inject_workflow_mcp_servers(
    runtime_contract: &mut Value,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) {
    if ctx.workflow_config.config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = ctx.phase_agent_id(phase_id);
    let workflow_profile_servers: Option<&[String]> = agent_id
        .as_deref()
        .and_then(|id| ctx.workflow_config.config.agent_profiles.get(id))
        .map(|profile| profile.mcp_servers.as_slice())
        .filter(|servers| !servers.is_empty());
    let runtime_profile_servers: Option<Vec<String>> = if workflow_profile_servers.is_none() {
        agent_id
            .as_deref()
            .and_then(|id| ctx.agent_runtime_config.agent_profile(id))
            .map(|profile| profile.mcp_servers.clone())
            .filter(|servers| !servers.is_empty())
    } else {
        None
    };
    let allowed_servers: Option<&[String]> =
        workflow_profile_servers.or_else(|| runtime_profile_servers.as_deref());

    let existing = runtime_contract
        .pointer("/mcp/additional_servers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut servers = existing;

    for (name, definition) in &ctx.workflow_config.config.mcp_servers {
        if let Some(allowed) = allowed_servers {
            if !allowed.iter().any(|a| a == name) {
                continue;
            }
        }
        servers.insert(
            name.clone(),
            serde_json::json!({
                "command": definition.command,
                "args": definition.args,
                "env": definition.env,
            }),
        );
    }
    if servers.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert("additional_servers".to_string(), Value::Object(servers));
    }
}
