use serde_json::Value;
use std::path::Path;

use super::phase_executor::load_agent_runtime_config;

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

pub(crate) fn phase_agent_id_for(project_root: &str, phase_id: &str) -> Option<String> {
    let workflow_override =
        orchestrator_core::load_workflow_config_or_default(Path::new(project_root))
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.agent_id.clone());
    if workflow_override.is_some() {
        return workflow_override;
    }
    load_agent_runtime_config(project_root)
        .phase_agent_id(phase_id)
        .map(ToOwned::to_owned)
}

pub(super) fn phase_system_prompt_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_system_prompt(phase_id)
        .map(ToOwned::to_owned)
}

pub(super) fn phase_tool_override_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_tool_override(phase_id)
        .map(ToOwned::to_owned)
}

pub(super) fn phase_model_override_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_model_override(phase_id)
        .map(ToOwned::to_owned)
}

pub(super) fn phase_fallback_models_for(project_root: &str, phase_id: &str) -> Vec<String> {
    load_agent_runtime_config(project_root).phase_fallback_models(phase_id)
}

pub(super) fn load_phase_capabilities(
    project_root: &str,
    phase_id: &str,
) -> protocol::PhaseCapabilities {
    load_agent_runtime_config(project_root).phase_capabilities(phase_id)
}

pub(super) fn phase_output_contract_for(
    project_root: &str,
    phase_id: &str,
) -> Option<orchestrator_core::PhaseOutputContract> {
    load_agent_runtime_config(project_root)
        .phase_output_contract(phase_id)
        .cloned()
}

pub(super) fn phase_output_json_schema_for(project_root: &str, phase_id: &str) -> Option<Value> {
    let config = load_agent_runtime_config(project_root);
    let contract = config.phase_output_contract(phase_id).cloned();
    let explicit_schema = config.phase_output_json_schema(phase_id).cloned();

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

pub(super) fn phase_decision_contract_for(
    project_root: &str,
    phase_id: &str,
) -> Option<orchestrator_core::PhaseDecisionContract> {
    load_agent_runtime_config(project_root)
        .phase_decision_contract(phase_id)
        .cloned()
}

pub(super) fn phase_decision_json_schema_for(project_root: &str, phase_id: &str) -> Option<Value> {
    let contract = phase_decision_contract_for(project_root, phase_id)?;
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

pub(super) fn phase_response_json_schema_for(project_root: &str, phase_id: &str) -> Option<Value> {
    let output_schema = phase_output_json_schema_for(project_root, phase_id);
    let decision_schema = phase_decision_json_schema_for(project_root, phase_id);

    match (output_schema, decision_schema) {
        (Some(mut output_schema), Some(decision_schema)) => {
            let required_decision = phase_decision_contract_for(project_root, phase_id)
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

pub(super) fn inject_read_only_flag(
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

pub(super) fn inject_response_schema_into_launch_args(
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

pub(super) fn inject_default_stdio_mcp(runtime_contract: &mut Value, project_root: &str) {
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

pub(super) fn inject_agent_tool_policy(
    runtime_contract: &mut Value,
    project_root: &str,
    phase_id: &str,
) {
    let agent_id = phase_agent_id_for(project_root, phase_id);

    let wf_config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
    let wf_profile = agent_id
        .as_deref()
        .and_then(|id| wf_config.config.agent_profiles.get(id));

    let rt_config = load_agent_runtime_config(project_root);
    let rt_profile = agent_id
        .as_deref()
        .and_then(|id| rt_config.agent_profile(id));

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

pub(super) fn inject_project_mcp_servers(
    runtime_contract: &mut Value,
    project_root: &str,
    phase_id: &str,
) {
    let project_config = match protocol::Config::load_or_default(project_root) {
        Ok(c) => c,
        Err(_) => return,
    };
    if project_config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = phase_agent_id_for(project_root, phase_id);
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

pub(super) fn inject_workflow_mcp_servers(
    runtime_contract: &mut Value,
    project_root: &str,
    phase_id: &str,
) {
    let config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
    if config.config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = phase_agent_id_for(project_root, phase_id);
    let workflow_profile_servers: Option<&[String]> = agent_id
        .as_deref()
        .and_then(|id| config.config.agent_profiles.get(id))
        .map(|profile| profile.mcp_servers.as_slice())
        .filter(|servers| !servers.is_empty());
    let runtime_profile_servers: Option<Vec<String>> = if workflow_profile_servers.is_none() {
        let agent_runtime_config = load_agent_runtime_config(project_root);
        agent_id
            .as_deref()
            .and_then(|id| agent_runtime_config.agent_profile(id))
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

    for (name, definition) in &config.config.mcp_servers {
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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn inject_workflow_mcp_servers_adds_servers_to_runtime_contract() {
        let tmp = std::env::temp_dir().join(format!("ao-test-wf-mcp-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let mut config = orchestrator_core::builtin_workflow_config();
        config.mcp_servers.insert(
            "custom-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "custom-mcp".to_string()],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: [("API_KEY".to_string(), "test".to_string())]
                    .into_iter()
                    .collect(),
            },
        );
        let agent_id = load_agent_runtime_config(project_root)
            .phase_agent_id("implementation")
            .unwrap_or("swe")
            .to_owned();
        let mut profile: orchestrator_core::AgentProfile =
            serde_json::from_value(serde_json::json!({})).unwrap();
        profile.mcp_servers = vec!["custom-server".to_string()];
        config.agent_profiles.insert(agent_id, profile);
        orchestrator_core::write_workflow_config(Path::new(project_root), &config).unwrap();

        let mut contract = serde_json::json!({
            "mcp": {
                "stdio": { "command": "ao" }
            }
        });
        inject_workflow_mcp_servers(&mut contract, project_root, "implementation");

        let servers = contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should exist");
        assert!(
            servers.contains_key("custom-server"),
            "workflow mcp_servers should be injected"
        );
        let custom = &servers["custom-server"];
        assert_eq!(custom["command"], "npx");
        assert_eq!(custom["args"], serde_json::json!(vec!["-y", "custom-mcp"]));
        assert_eq!(custom["env"]["API_KEY"], "test");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn inject_workflow_mcp_servers_merges_with_existing_project_servers() {
        let tmp = std::env::temp_dir().join(format!("ao-test-wf-mcp-merge-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let mut config = orchestrator_core::builtin_workflow_config();
        config.mcp_servers.insert(
            "workflow-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "wf-tool".to_string(),
                args: vec![],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: Default::default(),
            },
        );
        let agent_id = load_agent_runtime_config(project_root)
            .phase_agent_id("implementation")
            .unwrap_or("swe")
            .to_owned();
        let mut profile: orchestrator_core::AgentProfile =
            serde_json::from_value(serde_json::json!({})).unwrap();
        profile.mcp_servers = vec!["workflow-server".to_string()];
        config.agent_profiles.insert(agent_id, profile);
        orchestrator_core::write_workflow_config(Path::new(project_root), &config).unwrap();

        let mut contract = serde_json::json!({
            "mcp": {
                "stdio": { "command": "ao" },
                "additional_servers": {
                    "project-server": { "command": "proj-tool", "args": [], "env": {} }
                }
            }
        });
        inject_workflow_mcp_servers(&mut contract, project_root, "implementation");

        let servers = contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should exist");
        assert!(
            servers.contains_key("project-server"),
            "existing project servers should be preserved"
        );
        assert!(
            servers.contains_key("workflow-server"),
            "workflow servers should be merged in"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn inject_workflow_mcp_servers_filters_by_agent_profile() {
        let tmp = std::env::temp_dir().join(format!("ao-test-wf-mcp-filter-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let mut config = orchestrator_core::builtin_workflow_config();
        config.mcp_servers.insert(
            "allowed-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "allowed".to_string(),
                args: vec![],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: Default::default(),
            },
        );
        config.mcp_servers.insert(
            "blocked-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "blocked".to_string(),
                args: vec![],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: Default::default(),
            },
        );
        let agent_id = load_agent_runtime_config(project_root)
            .phase_agent_id("implementation")
            .unwrap_or("swe")
            .to_owned();
        let mut profile: orchestrator_core::AgentProfile =
            serde_json::from_value(serde_json::json!({})).unwrap();
        profile.mcp_servers = vec!["allowed-server".to_string()];
        config.agent_profiles.insert(agent_id, profile);
        orchestrator_core::write_workflow_config(Path::new(project_root), &config).unwrap();

        let mut contract = serde_json::json!({
            "mcp": {
                "stdio": { "command": "ao" }
            }
        });
        inject_workflow_mcp_servers(&mut contract, project_root, "implementation");

        let servers = contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should exist");
        assert!(
            servers.contains_key("allowed-server"),
            "server listed in agent profile should be injected"
        );
        assert!(
            !servers.contains_key("blocked-server"),
            "server not listed in agent profile should be filtered out"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn inject_workflow_mcp_servers_falls_back_to_runtime_config_profile() {
        let tmp =
            std::env::temp_dir().join(format!("ao-test-wf-mcp-rt-fallback-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let runtime_config = load_agent_runtime_config(project_root);
        let agent_id = runtime_config
            .phase_agent_id("implementation")
            .unwrap_or("swe");
        let runtime_allowed: Vec<String> = runtime_config
            .agent_profile(agent_id)
            .map(|p| p.mcp_servers.clone())
            .unwrap_or_default();

        let mut config = orchestrator_core::builtin_workflow_config();
        config.mcp_servers.insert(
            "unlisted-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "unlisted".to_string(),
                args: vec![],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: Default::default(),
            },
        );
        orchestrator_core::write_workflow_config(Path::new(project_root), &config).unwrap();

        let mut contract = serde_json::json!({
            "mcp": { "stdio": { "command": "ao" } }
        });
        inject_workflow_mcp_servers(&mut contract, project_root, "implementation");

        if !runtime_allowed.is_empty() {
            let injected_unlisted = contract
                .pointer("/mcp/additional_servers")
                .and_then(Value::as_object)
                .is_some_and(|s| s.contains_key("unlisted-server"));
            assert!(
                !injected_unlisted,
                "server not in runtime profile mcp_servers ({:?}) should be filtered out",
                runtime_allowed
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn inject_workflow_mcp_servers_respects_phase_definition_agent_id_override() {
        let tmp =
            std::env::temp_dir().join(format!("ao-test-wf-mcp-phase-override-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let mut config = orchestrator_core::builtin_workflow_config();
        config.mcp_servers.insert(
            "research-only-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "research-tool".to_string(),
                args: vec![],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: Default::default(),
            },
        );
        config.mcp_servers.insert(
            "swe-only-server".to_string(),
            orchestrator_core::workflow_config::McpServerDefinition {
                command: "swe-tool".to_string(),
                args: vec![],
                transport: None,
                config: Default::default(),
                tools: vec![],
                env: Default::default(),
            },
        );
        let mut researcher_profile: orchestrator_core::AgentProfile =
            serde_json::from_value(serde_json::json!({})).unwrap();
        researcher_profile.mcp_servers = vec!["research-only-server".to_string()];
        config
            .agent_profiles
            .insert("custom-researcher".to_string(), researcher_profile);
        let mut swe_profile: orchestrator_core::AgentProfile =
            serde_json::from_value(serde_json::json!({})).unwrap();
        swe_profile.mcp_servers = vec!["swe-only-server".to_string()];
        config
            .agent_profiles
            .insert("custom-swe".to_string(), swe_profile);

        config.phase_definitions.insert(
            "implementation".to_string(),
            orchestrator_core::PhaseExecutionDefinition {
                mode: orchestrator_core::PhaseExecutionMode::Agent,
                agent_id: Some("custom-researcher".to_string()),
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                command: None,
                manual: None,
            },
        );
        orchestrator_core::write_workflow_config(Path::new(project_root), &config).unwrap();

        let resolved = phase_agent_id_for(project_root, "implementation");
        assert_eq!(
            resolved.as_deref(),
            Some("custom-researcher"),
            "phase_agent_id_for should use workflow phase_definition agent_id override"
        );

        let mut contract = serde_json::json!({
            "mcp": { "stdio": { "command": "ao" } }
        });
        inject_workflow_mcp_servers(&mut contract, project_root, "implementation");

        let servers = contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should exist");
        assert!(
            servers.contains_key("research-only-server"),
            "server allowed by overridden agent should be injected"
        );
        assert!(
            !servers.contains_key("swe-only-server"),
            "server for non-overridden agent should be filtered out"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn inject_agent_tool_policy_respects_workflow_agent_override() {
        let tmp =
            std::env::temp_dir().join(format!("ao-test-tool-policy-override-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let mut config = orchestrator_core::builtin_workflow_config();
        let profile: orchestrator_core::AgentProfile = serde_json::from_value(serde_json::json!({
            "tool_policy": {
                "allow": ["Read", "Grep"],
                "deny": ["Write"]
            }
        }))
        .unwrap();
        config
            .agent_profiles
            .insert("restricted-agent".to_string(), profile);
        config.phase_definitions.insert(
            "research".to_string(),
            orchestrator_core::PhaseExecutionDefinition {
                mode: orchestrator_core::PhaseExecutionMode::Agent,
                agent_id: Some("restricted-agent".to_string()),
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                command: None,
                manual: None,
            },
        );
        orchestrator_core::write_workflow_config(Path::new(project_root), &config).unwrap();

        let mut contract = serde_json::json!({
            "mcp": { "stdio": { "command": "ao" } }
        });
        inject_agent_tool_policy(&mut contract, project_root, "research");

        let policy = contract
            .pointer("/mcp/tool_policy")
            .expect("tool_policy should be injected");
        assert_eq!(
            policy["allow"],
            serde_json::json!(["Read", "Grep"]),
            "allow list should come from workflow agent profile"
        );
        assert_eq!(
            policy["deny"],
            serde_json::json!(["Write"]),
            "deny list should come from workflow agent profile"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn phase_response_schema_for_decision_only_phase_returns_decision_schema() {
        let temp = tempfile::tempdir().expect("tempdir");
        let schema =
            phase_response_json_schema_for(temp.path().to_str().expect("project root"), "triage")
                .expect("triage should expose response schema");

        assert_eq!(schema["properties"]["kind"]["const"], "phase_decision");
        assert_eq!(schema["properties"]["phase_id"]["const"], "triage");
    }

    #[test]
    fn phase_response_schema_merges_phase_decision_into_output_schema() {
        let temp = tempfile::tempdir().expect("tempdir");
        let schema = phase_response_json_schema_for(
            temp.path().to_str().expect("project root"),
            "implementation",
        )
        .expect("implementation should expose merged response schema");
        let required_decision = phase_decision_contract_for(
            temp.path().to_str().expect("project root"),
            "implementation",
        )
        .map(|contract| !contract.allow_missing_decision)
        .unwrap_or(false);

        assert_eq!(
            schema["properties"]["kind"]["const"],
            "implementation_result"
        );
        assert_eq!(
            schema["properties"]["phase_decision"]["properties"]["kind"]["const"],
            "phase_decision"
        );
        let required = schema["required"]
            .as_array()
            .map(|items| items.contains(&Value::String("phase_decision".to_string())))
            .unwrap_or(false);
        assert_eq!(
            required, required_decision,
            "phase_decision requiredness should follow the phase decision contract"
        );
    }

    #[test]
    fn phase_response_schema_includes_yaml_declared_decision_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow = orchestrator_core::builtin_workflow_config();
        let default_agent = orchestrator_core::builtin_agent_runtime_config()
            .agent_profile("default")
            .expect("default agent profile")
            .clone();
        workflow
            .agent_profiles
            .insert("default".to_string(), default_agent);
        workflow.phase_definitions.insert(
            "triage".to_string(),
            orchestrator_core::PhaseExecutionDefinition {
                mode: orchestrator_core::PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(orchestrator_core::PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: 0.6,
                    max_risk: orchestrator_core::WorkflowDecisionRisk::Medium,
                    allow_missing_decision: false,
                    extra_json_schema: None,
                    fields: std::collections::BTreeMap::from([(
                        "skip_reason".to_string(),
                        orchestrator_core::agent_runtime_config::PhaseFieldDefinition {
                            field_type: "string".to_string(),
                            required: true,
                            description: Some("Reason for skipping.".to_string()),
                            enum_values: vec!["already_done".to_string(), "duplicate".to_string()],
                            items: None,
                            fields: std::collections::BTreeMap::new(),
                        },
                    )]),
                }),
                retry: None,
                command: None,
                manual: None,
            },
        );
        orchestrator_core::write_workflow_config(temp.path(), &workflow).expect("write config");

        let schema =
            phase_response_json_schema_for(temp.path().to_str().expect("project root"), "triage")
                .expect("triage should expose response schema");

        assert_eq!(
            schema["properties"]["skip_reason"]["type"],
            serde_json::json!("string")
        );
        assert_eq!(
            schema["properties"]["skip_reason"]["enum"],
            serde_json::json!(["already_done", "duplicate"])
        );
        assert!(schema["required"]
            .as_array()
            .expect("required array")
            .contains(&Value::String("skip_reason".to_string())));
    }
}
