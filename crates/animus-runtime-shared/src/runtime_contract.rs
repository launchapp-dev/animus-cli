use std::sync::{OnceLock, RwLock};

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::warn;

use crate::config_context::RuntimeConfigContext;

type MemoryMcpStdioCommandOverride = Box<dyn Fn() -> Option<String> + Send + Sync>;

static MEMORY_MCP_STDIO_COMMAND_OVERRIDE: OnceLock<RwLock<Option<MemoryMcpStdioCommandOverride>>> = OnceLock::new();

fn override_slot() -> &'static RwLock<Option<MemoryMcpStdioCommandOverride>> {
    MEMORY_MCP_STDIO_COMMAND_OVERRIDE.get_or_init(|| RwLock::new(None))
}

pub fn install_memory_mcp_stdio_command_override(resolver: Option<MemoryMcpStdioCommandOverride>) {
    if let Ok(mut guard) = override_slot().write() {
        *guard = resolver;
    }
}

/// Resolves the daemon-supplied animus CLI path for memory MCP injection.
/// Order: (1) in-process init-extension override (`install_memory_mcp_stdio_command_override`,
/// used by the plugin handshake path), (2) `ANIMUS_HOST_CLI_PATH` env var
/// (used by the daemon's `ProcessManager` direct-spawn path). Returns `None`
/// when neither is set so callers skip memory MCP injection rather than
/// recursively launching the workflow_runner as its own MCP server.
fn memory_mcp_stdio_command_override() -> Option<String> {
    if let Some(command) = override_slot().read().ok().and_then(|guard| guard.as_ref().and_then(|resolver| resolver()))
    {
        return Some(command);
    }
    std::env::var("ANIMUS_HOST_CLI_PATH").ok().filter(|value| !value.trim().is_empty())
}

pub fn validate_basic_json_schema(instance: &Value, schema: &Value) -> Result<()> {
    let validator = jsonschema::validator_for(schema).map_err(|e| anyhow!("invalid JSON Schema: {}", e))?;

    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| {
            let path = e.instance_path().to_string();
            if path.is_empty() {
                format!("{}", e)
            } else {
                format!("at '{}': {}", path, e)
            }
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("schema validation failed: {}", errors.join("; ")))
    }
}

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

    // Codex P2 follow-up: when the host supplies a non-empty `endpoint` (even
    // without an explicit `transport: "http"`), prefer it. The agent runner
    // resolves stdio before endpoint, so injecting a stdio command alongside
    // a host-supplied endpoint silently shadows the endpoint. The stdio
    // command must only be injected when the host has NOT requested an
    // endpoint AND has not explicitly supplied its own stdio command.
    let host_supplied_endpoint = mcp_config.endpoint.as_deref().map(str::trim).is_some_and(|value| !value.is_empty());
    let host_supplied_stdio_command =
        mcp_config.stdio_command.as_deref().map(str::trim).is_some_and(|value| !value.is_empty());
    if host_supplied_endpoint && !host_supplied_stdio_command {
        return;
    }

    let supports_mcp =
        runtime_contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool).unwrap_or(false);
    if !supports_mcp {
        return;
    }

    let command =
        mcp_config.stdio_command.clone().filter(|v| !v.trim().is_empty()).or_else(memory_mcp_stdio_command_override);
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
            mcp.insert("agent_id".to_string(), serde_json::json!("animus"));
        }
    }
}

pub fn inject_agent_tool_policy(runtime_contract: &mut Value, ctx: &RuntimeConfigContext, phase_id: &str) {
    let agent_id = ctx.phase_agent_id(phase_id);

    let wf_profile = agent_id.as_deref().and_then(|id| ctx.workflow_config.config.agent_profiles.get(id));

    let rt_profile = agent_id.as_deref().and_then(|id| ctx.agent_runtime_config.agent_profile(id));

    let policy = wf_profile.and_then(|p| p.tool_policy.as_ref()).or_else(|| rt_profile.map(|p| &p.tool_policy));

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
    let existing =
        runtime_contract.pointer("/mcp/additional_servers").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut servers = existing;
    for (name, entry) in &project_config.mcp_servers {
        let assigned = entry.assign_to.is_empty()
            || agent_id.as_deref().is_some_and(|id| entry.assign_to.iter().any(|a| a.eq_ignore_ascii_case(id)));
        if !assigned {
            continue;
        }
        let entry_json = build_project_mcp_server_entry(name, entry, project_root);
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

/// Back-compat entry point for out-of-tree `workflow_runner` plugins
/// that pin an older `animus-runtime-shared`. The `animus-mcp-proxy`
/// rewrite needs a project root so the proxy can resolve the server's
/// credentials, but plugins that haven't migrated yet still call this
/// three-argument form (the proxy entry then carries an empty
/// `--project-root`). Migrating plugins should call
/// `inject_workflow_mcp_servers_with_project_root` so HTTP MCP servers
/// with an `oauth:` block get a working proxy entry.
pub fn inject_workflow_mcp_servers(runtime_contract: &mut Value, ctx: &RuntimeConfigContext, phase_id: &str) {
    inject_workflow_mcp_servers_with_project_root(runtime_contract, ctx, phase_id, "");
}

pub fn inject_workflow_mcp_servers_with_project_root(
    runtime_contract: &mut Value,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
    project_root: &str,
) {
    if ctx.workflow_config.config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = ctx.phase_agent_id(phase_id);
    let workflow_profile_servers: Option<Vec<String>> = agent_id
        .as_deref()
        .and_then(|id| ctx.workflow_config.config.agent_profiles.get(id))
        .and_then(|profile| profile.mcp_servers.clone());
    let runtime_profile_servers: Vec<String> = if workflow_profile_servers.is_none() {
        agent_id
            .as_deref()
            .and_then(|id| ctx.agent_runtime_config.agent_profile(id))
            .map(|profile| profile.mcp_servers.clone())
            .filter(|servers| !servers.is_empty())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // An explicitly declared workflow profile scope restricts injection even
    // when it is empty (`mcp_servers: []` means "no servers"); only a fully
    // undeclared scope falls through to "all configured servers".
    let scope_is_restrictive = workflow_profile_servers.is_some();
    let workflow_profile_servers = workflow_profile_servers.unwrap_or_default();
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
        if (scope_is_restrictive || !allowed_servers.is_empty()) && !allowed_servers.contains(name) {
            continue;
        }
        let entry_json = build_additional_mcp_server_entry(name, definition, project_root);
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

/// Base name of the proxy binary (no platform suffix).
const MCP_PROXY_BIN: &str = "animus-mcp-proxy";

/// Resolve the `animus-mcp-proxy` binary path. The proxy ships in the same
/// package as the `animus` CLI, so it sits next to whichever `animus` binary
/// is in use. Resolution order:
///
/// 1. Sibling of the daemon-supplied host CLI path
///    (`ANIMUS_HOST_CLI_PATH` / the in-process override) — this is the
///    installed `animus`, even when contract assembly runs inside the
///    workflow-runner subprocess whose `current_exe()` is the runner.
/// 2. Sibling of the current executable.
/// 3. Bare name (PATH lookup).
///
/// On Windows the sibling carries the `.exe` suffix that Cargo emits.
fn mcp_proxy_command() -> String {
    let file_name = format!("{MCP_PROXY_BIN}{}", std::env::consts::EXE_SUFFIX);

    // (1) Sibling of the host CLI path.
    if let Some(host_cli) = memory_mcp_stdio_command_override() {
        if let Some(dir) = std::path::Path::new(&host_cli).parent() {
            let candidate = dir.join(&file_name);
            if candidate.exists() {
                return candidate.display().to_string();
            }
        }
    }

    // (2) Sibling of the current executable.
    if let Some(candidate) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&file_name)))
        .filter(|candidate| candidate.exists())
    {
        return candidate.display().to_string();
    }

    // (3) PATH lookup.
    MCP_PROXY_BIN.to_string()
}

/// Rewrite an OAuth-protected MCP server entry to launch the local
/// `animus-mcp-proxy` over stdio instead of pointing the agent at the
/// upstream HTTP URL. The proxy resolves the live credential itself at
/// connect time — the keychain token for `authorization_code`, the broker
/// (`manual_bearer` / `client_credentials` / `refresh_token`) otherwise —
/// injects the bearer, and refreshes on expiry/401, so the agent connects to
/// a local auth-free endpoint and the resolved secret never appears in the
/// contract, in `.mcp.json`, or on any argv. Returns the rewritten entry.
///
/// The selected definition's `url` is passed as `--url` so the proxy binds to
/// exactly the upstream the contract selected, rather than re-resolving the
/// server name (which could pick a same-named entry from a different config
/// source).
fn build_oauth_proxy_entry(
    name: &str,
    url: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
    project_root: &str,
) -> Value {
    let mut args =
        vec!["--server".to_string(), name.to_string(), "--project-root".to_string(), project_root.to_string()];
    if let Some(url) = url.filter(|u| !u.trim().is_empty()) {
        args.push("--url".to_string());
        args.push(url.to_string());
    }
    serde_json::json!({
        "command": mcp_proxy_command(),
        "args": args,
        "env": env,
        "transport": "stdio",
    })
}

/// Shape a single MCP server entry for `/mcp/additional_servers`. Any entry
/// with an `oauth:` block — regardless of flow — is repointed at the local
/// `animus-mcp-proxy` stdio bridge, which resolves the live credential at
/// connect time. No resolved bearer token ever rides the contract, so the
/// `.mcp.json` / wire channels can carry the same entry verbatim.
fn build_additional_mcp_server_entry(
    name: &str,
    definition: &orchestrator_config::McpServerDefinition,
    project_root: &str,
) -> Value {
    if definition.oauth.is_some() {
        return build_oauth_proxy_entry(name, definition.url.as_deref(), &definition.env, project_root);
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
    entry_json
}

fn build_project_mcp_server_entry(
    name: &str,
    definition: &protocol::ProjectMcpServerEntry,
    project_root: &str,
) -> Value {
    if let Some(oauth_value) = definition.oauth.as_ref() {
        match serde_json::from_value::<orchestrator_config::OauthConfig>(oauth_value.clone()) {
            Ok(_) => {
                return build_oauth_proxy_entry(name, definition.url.as_deref(), &definition.env, project_root);
            }
            Err(err) => {
                warn!(
                    server = name,
                    error = %err,
                    "malformed `oauth` block in project mcp_servers entry; emitting MCP entry without auth"
                );
            }
        }
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
    entry_json
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
            let entry_json = build_additional_mcp_server_entry(name, definition, project_root);
            servers.insert(name.to_string(), entry_json);
            continue;
        }

        if let Some(definition) = project_config.mcp_servers.get(name) {
            let entry_json = build_project_mcp_server_entry(name, definition, project_root);
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

/// Inject the project-scoped memory MCP server into the agent's runtime contract when the
/// active agent profile has `capabilities.memory: true`. When the capability is `false` or
/// absent the runtime contract is left untouched, so the spawned agent does not see the
/// `animus.memory.*` tools.
///
/// This is the daemon-side wiring that makes the `capabilities.memory` flag observable. The
/// memory MCP server itself is implemented as a stdio surface invoked via `ao mcp memory`.
pub fn inject_memory_mcp_for_capable_agent(
    runtime_contract: &mut Value,
    project_root: &str,
    ctx: &RuntimeConfigContext,
    phase_id: &str,
) {
    let Some(agent_id) = ctx.phase_agent_id(phase_id) else {
        return;
    };
    let profile = match ctx.agent_runtime_config.agent_profile(&agent_id) {
        Some(profile) => profile,
        None => return,
    };
    if !orchestrator_core::agent_runtime_config::agent_memory_capability_enabled(profile) {
        return;
    }

    let supports_mcp =
        runtime_contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool).unwrap_or(false);
    if !supports_mcp {
        return;
    }

    let Some(command) = current_ao_command() else {
        return;
    };
    let args = vec!["--project-root".to_string(), project_root.to_string(), "mcp".to_string(), "memory".to_string()];

    let server_name = "animus.memory";
    if let Some(agent_mcp_id) = primary_mcp_agent_id(runtime_contract) {
        if server_name.eq_ignore_ascii_case(agent_mcp_id) {
            warn!(
                agent_id = agent_mcp_id,
                "Skipping memory MCP injection because it collides with the primary agent id"
            );
            return;
        }
    }

    let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) else {
        return;
    };
    let entry = serde_json::json!({
        "command": command,
        "args": args,
        "env": serde_json::Map::<String, Value>::new(),
        "transport": "stdio",
    });
    let mut existing = mcp.get("additional_servers").and_then(Value::as_object).cloned().unwrap_or_default();
    existing.insert(server_name.to_string(), entry);
    mcp.insert("additional_servers".to_string(), Value::Object(existing));
}

fn current_ao_command() -> Option<String> {
    memory_mcp_stdio_command_override()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use orchestrator_config::McpServerDefinition;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, workflow_config_hash, LoadedWorkflowConfig,
        PhaseMcpBinding, WorkflowConfigMetadata, WorkflowConfigSource,
    };

    use super::*;

    fn memory_mcp_override_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn inject_workflow_mcp_servers_includes_phase_bound_pack_servers() {
        let mut workflow_config = builtin_workflow_config();
        workflow_config.mcp_servers.insert(
            "animus.requirements/ao".to_string(),
            McpServerDefinition {
                command: "node".to_string(),
                args: vec!["server.js".to_string()],
                transport: Some("stdio".to_string()),
                url: None,
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
                oauth: None,
            },
        );
        workflow_config
            .phase_mcp_bindings
            .insert("research".to_string(), PhaseMcpBinding { servers: vec!["animus.requirements/ao".to_string()] });

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
        inject_workflow_mcp_servers_with_project_root(
            &mut runtime_contract,
            &ctx,
            "research",
            "/tmp/animus-runtime-shared-test",
        );

        let additional_servers = runtime_contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should be injected");
        assert!(additional_servers.contains_key("animus.requirements/ao"));
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
                "agent_id": "animus",
                "stdio": {
                    "command": "/path/to/animus/target/debug/animus",
                    "args": ["--project-root", "/path/to/project", "mcp", "serve"]
                }
            }
        });
        inject_workflow_mcp_servers_with_project_root(
            &mut runtime_contract,
            &ctx,
            "requirements",
            "/tmp/animus-runtime-shared-test",
        );

        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "built-in workflow MCP injection should not duplicate the primary animus server"
        );
    }

    #[test]
    fn inject_named_mcp_servers_skips_primary_agent_id_collisions() {
        let temp = tempfile::tempdir().expect("tempdir for project root");
        let project_root = temp.path().to_string_lossy().to_string();
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
                "agent_id": "animus",
                "stdio": {
                    "command": "/path/to/animus/target/debug/animus",
                    "args": ["--project-root", &project_root, "mcp", "serve"]
                }
            }
        });
        inject_named_mcp_servers(&mut runtime_contract, &project_root, &ctx, "requirements", &["animus".to_string()])
            .expect("named MCP injection should succeed");

        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "named MCP injection should not duplicate the primary animus server"
        );
    }

    #[test]
    fn inject_project_mcp_servers_merges_with_existing_additional_servers() {
        let temp = tempfile::tempdir().expect("tempdir for project root");
        let project_root = temp.path().to_string_lossy().to_string();
        let animus_dir = temp.path().join(".animus");
        std::fs::create_dir_all(&animus_dir).expect("create .animus dir");
        std::fs::write(
            animus_dir.join("config.json"),
            serde_json::json!({
                "mcp_servers": {
                    "project-db": { "command": "node", "args": ["db.js"] }
                }
            })
            .to_string(),
        )
        .expect("write project config");

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
                "additional_servers": {
                    "workflow-server": { "command": "wf", "args": [] }
                }
            }
        });
        inject_project_mcp_servers(&mut runtime_contract, &project_root, &ctx, "research");

        let additional_servers = runtime_contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .expect("additional_servers should exist");
        assert!(
            additional_servers.contains_key("workflow-server"),
            "project injection must merge with (not overwrite) previously injected servers"
        );
        assert!(additional_servers.contains_key("project-db"), "project server should be injected");
    }

    fn workflow_config_with_phase_agent(phase_id: &str, agent_id: &str) -> LoadedWorkflowConfig {
        use orchestrator_config::agent_runtime_config::{Idempotency, PhaseExecutionDefinition, PhaseExecutionMode};
        let mut workflow_config = builtin_workflow_config();
        let phase_definition = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some(agent_id.to_string()),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Idempotency::Unknown,
            worktree: None,
            evals: None,
        };
        workflow_config.phase_definitions.insert(phase_id.to_string(), phase_definition);
        LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: workflow_config.schema.clone(),
                version: workflow_config.version,
                hash: workflow_config_hash(&workflow_config),
                source: WorkflowConfigSource::Builtin,
            },
            config: workflow_config,
            path: PathBuf::from("builtin"),
        }
    }

    fn agent_runtime_config_with_memory(agent_id: &str, memory_enabled: bool) -> orchestrator_core::AgentRuntimeConfig {
        let mut config = builtin_agent_runtime_config();
        let mut profile = config.agents.get(agent_id).cloned().unwrap_or_default();
        profile.capabilities.insert("memory".to_string(), memory_enabled);
        config.agents.insert(agent_id.to_string(), profile);
        config
    }

    #[test]
    fn inject_memory_mcp_added_when_capability_enabled() {
        // v0.5.1 #5c: sibling-binary discovery removed. Memory MCP injection
        // requires the daemon to supply `init_extensions.memory_mcp_stdio_command`
        // via `install_memory_mcp_stdio_command_override`.
        let _guard = memory_mcp_override_lock().lock().unwrap_or_else(|p| p.into_inner());
        let workflow_config = workflow_config_with_phase_agent("research", "default");
        let agent_runtime_config = agent_runtime_config_with_memory("default", true);
        let ctx = RuntimeConfigContext { agent_runtime_config, workflow_config };

        install_memory_mcp_stdio_command_override(Some(Box::new(|| Some("/opt/host/bin/animus".to_string()))));

        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "agent_id": "animus" }
        });
        inject_memory_mcp_for_capable_agent(&mut runtime_contract, "/tmp/project", &ctx, "research");

        let entry = runtime_contract
            .pointer("/mcp/additional_servers/animus.memory")
            .expect("animus.memory server entry should be injected for capability=true");
        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert_eq!(entry.pointer("/command").and_then(Value::as_str), Some("/opt/host/bin/animus"));
        let args = entry.pointer("/args").and_then(Value::as_array).expect("args");
        assert!(args.iter().any(|value| value.as_str() == Some("mcp")));
        assert!(args.iter().any(|value| value.as_str() == Some("memory")));

        install_memory_mcp_stdio_command_override(None);
    }

    #[test]
    fn inject_memory_mcp_omitted_when_init_extension_absent() {
        // v0.5.1 #5c: absent init-extension override → no recursive
        // self-launch fallback. Caller must skip memory MCP injection.
        let _guard = memory_mcp_override_lock().lock().unwrap_or_else(|p| p.into_inner());
        install_memory_mcp_stdio_command_override(None);
        let prior_env = std::env::var("ANIMUS_HOST_CLI_PATH").ok();
        std::env::remove_var("ANIMUS_HOST_CLI_PATH");

        let workflow_config = workflow_config_with_phase_agent("research", "default");
        let agent_runtime_config = agent_runtime_config_with_memory("default", true);
        let ctx = RuntimeConfigContext { agent_runtime_config, workflow_config };

        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "agent_id": "animus" }
        });
        inject_memory_mcp_for_capable_agent(&mut runtime_contract, "/tmp/project", &ctx, "research");

        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "without init-extension override, memory MCP injection must be skipped (no sibling-binary fallback)"
        );

        match prior_env {
            Some(value) => std::env::set_var("ANIMUS_HOST_CLI_PATH", value),
            None => std::env::remove_var("ANIMUS_HOST_CLI_PATH"),
        }
    }

    #[test]
    fn inject_memory_mcp_omitted_when_capability_disabled() {
        let workflow_config = workflow_config_with_phase_agent("research", "default");
        let agent_runtime_config = agent_runtime_config_with_memory("default", false);
        let ctx = RuntimeConfigContext { agent_runtime_config, workflow_config };

        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "agent_id": "animus" }
        });
        inject_memory_mcp_for_capable_agent(&mut runtime_contract, "/tmp/project", &ctx, "research");
        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "memory MCP should not be injected for capability=false"
        );
    }

    #[test]
    fn inject_memory_mcp_omitted_when_capability_absent() {
        let workflow_config = workflow_config_with_phase_agent("research", "default");
        let mut agent_runtime_config = builtin_agent_runtime_config();
        let mut profile = agent_runtime_config.agents.get("default").cloned().unwrap_or_default();
        profile.capabilities.clear();
        agent_runtime_config.agents.insert("default".to_string(), profile);
        let ctx = RuntimeConfigContext { agent_runtime_config, workflow_config };

        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "agent_id": "animus" }
        });
        inject_memory_mcp_for_capable_agent(&mut runtime_contract, "/tmp/project", &ctx, "research");
        assert!(
            runtime_contract.pointer("/mcp/additional_servers").is_none(),
            "memory MCP should not be injected for capability=absent"
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
        use crate::runtime_contract::validate_basic_json_schema;

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
        use crate::runtime_contract::validate_basic_json_schema;

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
        let required_field_strings: Vec<&str> = required_fields.iter().filter_map(|v| v.as_str()).collect();

        assert!(
            !required_field_strings.contains(&"evidence"),
            "evidence should not be required when required_evidence is empty"
        );
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

    /// Codex P2 #4: when the daemon supplies `init_extensions.memory_mcp_stdio_command`,
    /// the plugin uses that explicit binary path instead of probing for a
    /// sibling `animus`. v0.5.1 #5c removed the sibling-discovery fallback
    /// entirely; the override is now the only source of truth.
    #[test]
    fn inject_memory_mcp_uses_init_extension_stdio_command_override() {
        let _guard = memory_mcp_override_lock().lock().unwrap_or_else(|p| p.into_inner());
        let stub_command = "/opt/host/bin/host-supplied-memory-mcp";
        let stub_owned = stub_command.to_string();
        install_memory_mcp_stdio_command_override(Some(Box::new(move || Some(stub_owned.clone()))));

        let workflow_config = workflow_config_with_phase_agent("research", "default");
        let agent_runtime_config = agent_runtime_config_with_memory("default", true);
        let ctx = RuntimeConfigContext { agent_runtime_config, workflow_config };

        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "agent_id": "animus" }
        });
        inject_memory_mcp_for_capable_agent(&mut runtime_contract, "/tmp/project", &ctx, "research");

        let entry = runtime_contract
            .pointer("/mcp/additional_servers/animus.memory")
            .expect("animus.memory server entry should be injected when init-extension override is set");
        assert_eq!(
            entry.pointer("/command").and_then(Value::as_str),
            Some(stub_command),
            "init-extension stdio command override must be used"
        );

        install_memory_mcp_stdio_command_override(None);
    }

    /// Codex P2 #1 follow-up: host-supplied `endpoint` and `agent_id` must
    /// reach the runtime contract via `build_runtime_contract_with_resume_and_mcp_config`,
    /// not just the stdio injection path. Pre-fix the wire-through only
    /// covered stdio_command; HTTP endpoints stayed at the default.
    #[test]
    fn build_runtime_contract_with_resume_and_mcp_config_honors_endpoint_and_agent_id() {
        let mcp_config = protocol::McpRuntimeConfig {
            endpoint: Some("https://host.example.com/mcp".to_string()),
            agent_id: Some("custom-agent".to_string()),
            ..Default::default()
        };
        let runtime_contract = crate::ipc::build_runtime_contract_with_resume_and_mcp_config(
            "codex",
            "claude-sonnet-4-6",
            "the prompt",
            None,
            &mcp_config,
        )
        .expect("runtime contract should build");

        assert_eq!(
            runtime_contract.pointer("/mcp/endpoint").and_then(Value::as_str),
            Some("https://host.example.com/mcp"),
            "host-supplied mcp_config.endpoint must reach /mcp/endpoint"
        );
        assert_eq!(
            runtime_contract.pointer("/mcp/agent_id").and_then(Value::as_str),
            Some("custom-agent"),
            "host-supplied mcp_config.agent_id must reach /mcp/agent_id"
        );
    }

    /// Codex P2 round 7: when the host supplies only `mcp_config.stdio_command`
    /// (no endpoint), `build_runtime_contract` would leave `mcp.enforce_only`
    /// at `false` because that helper keys enforcement on the endpoint. The
    /// agent runner then skips native MCP setup and the stdio config is
    /// ignored. Asserts the new ipc wrapper flips `enforce_only` to true and
    /// seeds the allowed-tool prefixes when a stdio command is supplied.
    #[test]
    fn host_supplied_stdio_command_enables_mcp_enforcement() {
        let mcp_config = protocol::McpRuntimeConfig {
            stdio_command: Some("/opt/host/bin/host-mcp".to_string()),
            ..Default::default()
        };
        let runtime_contract = crate::ipc::build_runtime_contract_with_resume_and_mcp_config(
            "codex",
            "claude-sonnet-4-6",
            "the prompt",
            None,
            &mcp_config,
        )
        .expect("runtime contract should build");

        assert_eq!(
            runtime_contract.pointer("/mcp/enforce_only").and_then(Value::as_bool),
            Some(true),
            "host-supplied stdio_command must enable mcp.enforce_only so the agent runner performs native MCP setup"
        );
        let prefixes =
            runtime_contract.pointer("/mcp/allowed_tool_prefixes").and_then(Value::as_array).expect("prefixes");
        assert!(!prefixes.is_empty(), "allowed_tool_prefixes must be seeded when enforce_only is true");
    }

    /// Codex P2 round 4: when the host sends `mcp_config.endpoint` without
    /// `transport: "http"`, stdio injection must NOT silently shadow the
    /// host-supplied endpoint. Pre-fix, the runtime contract ended up with
    /// both `/mcp/endpoint` and `/mcp/stdio` set; the agent runner resolves
    /// stdio first, so the endpoint was effectively ignored in co-located
    /// deployments with a sibling `animus` binary.
    #[test]
    fn host_supplied_endpoint_suppresses_default_stdio_injection() {
        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "endpoint": "https://host.example.com/mcp" }
        });
        let mcp_config = protocol::McpRuntimeConfig {
            endpoint: Some("https://host.example.com/mcp".to_string()),
            ..Default::default()
        };
        inject_default_stdio_mcp_with_config(&mut runtime_contract, "/tmp/project", &mcp_config);
        assert!(
            runtime_contract.pointer("/mcp/stdio").is_none(),
            "stdio injection must be skipped when the host supplied an endpoint, so the endpoint is not shadowed"
        );
        assert_eq!(
            runtime_contract.pointer("/mcp/endpoint").and_then(Value::as_str),
            Some("https://host.example.com/mcp"),
            "host-supplied endpoint must remain on the contract"
        );
    }

    /// Codex P2 #1 (mcp_config wire-through): when the host supplies a
    /// non-default `McpRuntimeConfig` with an explicit `stdio_command`, the
    /// stdio injection must honor it instead of falling back to a sibling
    /// `animus` binary search. Asserts the runtime config visible at the
    /// phase execution layer reflects the host-supplied override.
    #[test]
    fn inject_default_stdio_mcp_with_config_honors_host_supplied_stdio_command() {
        let mut runtime_contract = serde_json::json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {}
        });
        let mcp_config = protocol::McpRuntimeConfig {
            stdio_command: Some("/opt/host/bin/host-mcp".to_string()),
            stdio_args_json: Some(serde_json::to_string(&vec!["--from-host", "--json"]).unwrap()),
            ..Default::default()
        };
        inject_default_stdio_mcp_with_config(&mut runtime_contract, "/tmp/project", &mcp_config);

        assert_eq!(
            runtime_contract.pointer("/mcp/stdio/command").and_then(Value::as_str),
            Some("/opt/host/bin/host-mcp"),
            "host-supplied stdio_command must be threaded into /mcp/stdio/command"
        );
        let args = runtime_contract.pointer("/mcp/stdio/args").and_then(Value::as_array).expect("stdio args");
        let arg_strings: Vec<&str> = args.iter().filter_map(|value| value.as_str()).collect();
        assert_eq!(
            arg_strings,
            vec!["--from-host", "--json"],
            "host-supplied stdio_args_json must override the project-root fallback"
        );
    }

    #[test]
    fn inject_workflow_mcp_servers_injects_http_transport_servers() {
        let mut workflow_config = builtin_workflow_config();
        workflow_config.mcp_servers.insert(
            "robinhood-trading".to_string(),
            McpServerDefinition {
                command: String::new(),
                args: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://agent.robinhood.com/mcp/trading".to_string()),
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
                oauth: None,
            },
        );
        workflow_config
            .phase_mcp_bindings
            .insert("research".to_string(), PhaseMcpBinding { servers: vec!["robinhood-trading".to_string()] });

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

        let mut runtime_contract = serde_json::json!({ "mcp": {} });
        inject_workflow_mcp_servers_with_project_root(
            &mut runtime_contract,
            &ctx,
            "research",
            "/tmp/animus-runtime-shared-test",
        );

        let entry = runtime_contract
            .pointer("/mcp/additional_servers/robinhood-trading")
            .expect("robinhood server should be injected");
        assert_eq!(entry.get("url").and_then(Value::as_str), Some("https://agent.robinhood.com/mcp/trading"));
        assert_eq!(entry.get("transport").and_then(Value::as_str), Some("http"));
    }

    #[test]
    fn inject_workflow_mcp_servers_rewrites_manual_bearer_to_stdio_proxy() {
        use orchestrator_config::workflow_config::{OauthConfig, OauthFlow};

        // A manual_bearer server is repointed at the local
        // `animus-mcp-proxy` (which reads the bearer env var itself at
        // connect time) instead of receiving a resolved Authorization
        // header — the token must never appear anywhere in the contract.
        let bearer_env_name = "ANIMUS_TEST_BEARER_HEADER_INJECT";
        std::env::set_var(bearer_env_name, "tok-xyz");

        let mut workflow_config = builtin_workflow_config();
        workflow_config.mcp_servers.insert(
            "robinhood-trading".to_string(),
            McpServerDefinition {
                command: String::new(),
                args: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://agent.robinhood.com/mcp/trading".to_string()),
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
                oauth: Some(OauthConfig {
                    flow: OauthFlow::ManualBearer,
                    token_url: None,
                    client_id_env: None,
                    client_secret_env: None,
                    refresh_token_env: None,
                    bearer_env: Some(bearer_env_name.to_string()),
                    scopes: vec![],
                    audience: None,
                    cache: false,
                    client_id: None,
                }),
            },
        );
        workflow_config
            .phase_mcp_bindings
            .insert("research".to_string(), PhaseMcpBinding { servers: vec!["robinhood-trading".to_string()] });

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

        let mut runtime_contract = serde_json::json!({ "mcp": {} });
        inject_workflow_mcp_servers_with_project_root(
            &mut runtime_contract,
            &ctx,
            "research",
            "/tmp/animus-oauth-runtime-test",
        );

        let entry = runtime_contract
            .pointer("/mcp/additional_servers/robinhood-trading")
            .expect("robinhood server should be injected");
        std::env::remove_var(bearer_env_name);
        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert!(entry.get("url").is_none(), "proxy entry must not carry the upstream url");
        assert!(entry.get("headers").is_none(), "manual_bearer must not inject a resolved header");
        let command = entry.pointer("/command").and_then(Value::as_str).expect("proxy command");
        assert!(command.contains("animus-mcp-proxy"), "command should be the proxy binary, got {command}");
        let args: Vec<&str> = entry
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("args set")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            args,
            vec![
                "--server",
                "robinhood-trading",
                "--project-root",
                "/tmp/animus-oauth-runtime-test",
                "--url",
                "https://agent.robinhood.com/mcp/trading"
            ]
        );
        let serialized = serde_json::to_string(&runtime_contract).unwrap();
        assert!(
            !serialized.contains("tok-xyz"),
            "the resolved bearer token must never ride the contract: {serialized}"
        );
    }

    #[test]
    fn client_credentials_flow_rewrites_entry_to_stdio_proxy() {
        // client_credentials servers are proxied exactly like
        // authorization_code: no token-endpoint call happens at contract
        // assembly and no client secret can appear in the entry.
        use orchestrator_config::workflow_config::{OauthConfig, OauthFlow};
        let definition = McpServerDefinition {
            command: String::new(),
            args: Vec::new(),
            transport: Some("http".to_string()),
            url: Some("https://api.example.com/mcp".to_string()),
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: Some(OauthConfig {
                flow: OauthFlow::ClientCredentials,
                token_url: Some("https://auth.example.com/token".to_string()),
                client_id_env: Some("CC_CLIENT_ID".to_string()),
                client_secret_env: Some("CC_CLIENT_SECRET".to_string()),
                refresh_token_env: None,
                bearer_env: None,
                scopes: vec!["read".to_string()],
                audience: None,
                cache: true,
                client_id: None,
            }),
        };

        let entry = build_additional_mcp_server_entry("cc-api", &definition, "/tmp/proj-cc");

        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert!(entry.get("url").is_none(), "proxy entry must not carry the upstream url");
        assert!(entry.get("headers").is_none(), "client_credentials must not inject a resolved header");
        let command = entry.pointer("/command").and_then(Value::as_str).expect("proxy command");
        assert!(command.contains("animus-mcp-proxy"), "command should be the proxy binary, got {command}");
        let args: Vec<&str> = entry
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("args set")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            args,
            vec!["--server", "cc-api", "--project-root", "/tmp/proj-cc", "--url", "https://api.example.com/mcp"]
        );
    }

    #[test]
    fn project_manual_bearer_flow_rewrites_entry_to_stdio_proxy() {
        let definition = protocol::ProjectMcpServerEntry {
            command: String::new(),
            args: vec![],
            env: BTreeMap::new(),
            assign_to: vec![],
            transport: Some("http".to_string()),
            url: Some("https://trading.example.com/mcp".to_string()),
            oauth: Some(serde_json::json!({ "flow": "manual_bearer", "bearer_env": "TRADING_TOKEN" })),
        };

        let entry = build_project_mcp_server_entry("trading", &definition, "/tmp/proj-mb");

        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert!(entry.get("url").is_none());
        assert!(entry.get("headers").is_none(), "manual_bearer must not inject a resolved header");
        let command = entry.pointer("/command").and_then(Value::as_str).expect("proxy command");
        assert!(command.contains("animus-mcp-proxy"), "command should be the proxy binary, got {command}");
    }

    #[test]
    fn authorization_code_flow_rewrites_entry_to_stdio_proxy() {
        // The interactive authorization_code flow must NOT inject a bearer
        // header. Instead the agent's entry is repointed at the local
        // `animus-mcp-proxy` over stdio, which pulls the live token from the
        // keychain. This is a pure-function assertion over the rewritten
        // entry shape (no network, no keychain).
        use orchestrator_config::workflow_config::{OauthConfig, OauthFlow};
        let mut env = BTreeMap::new();
        env.insert("EXTRA".to_string(), "1".to_string());
        let definition = McpServerDefinition {
            command: "ignored".to_string(),
            args: vec!["ignored".to_string()],
            transport: Some("http".to_string()),
            url: Some("https://api.githubcopilot.com/mcp/".to_string()),
            config: BTreeMap::new(),
            tools: Vec::new(),
            env,
            oauth: Some(OauthConfig {
                flow: OauthFlow::AuthorizationCode,
                token_url: None,
                client_id_env: None,
                client_secret_env: None,
                refresh_token_env: None,
                bearer_env: None,
                scopes: vec!["repo".to_string()],
                audience: None,
                cache: true,
                client_id: None,
            }),
        };

        let entry = build_additional_mcp_server_entry("github", &definition, "/tmp/proj");

        // Repointed at the proxy over stdio; upstream URL is dropped so the
        // agent never talks to the OAuth endpoint directly.
        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert!(entry.get("url").is_none(), "proxy entry must not carry the upstream url");
        let command = entry.pointer("/command").and_then(Value::as_str).expect("command set");
        assert!(command.contains("animus-mcp-proxy"), "command should be the proxy binary, got {command}");
        let args: Vec<&str> = entry
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("args set")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            args,
            vec!["--server", "github", "--project-root", "/tmp/proj", "--url", "https://api.githubcopilot.com/mcp/"]
        );
        // Env is preserved.
        assert_eq!(entry.pointer("/env/EXTRA").and_then(Value::as_str), Some("1"));
        // No Authorization header is injected for this flow.
        assert!(entry.pointer("/headers").is_none(), "authorization_code must not inject a header");
    }

    #[test]
    fn project_authorization_code_flow_rewrites_entry_to_stdio_proxy() {
        let mut env = BTreeMap::new();
        env.insert("K".to_string(), "v".to_string());
        let definition = protocol::ProjectMcpServerEntry {
            command: "ignored".to_string(),
            args: vec![],
            env,
            assign_to: vec![],
            transport: Some("http".to_string()),
            url: Some("https://mcp.linear.app/mcp".to_string()),
            oauth: Some(serde_json::json!({ "flow": "authorization_code", "scopes": ["read"] })),
        };

        let entry = build_project_mcp_server_entry("linear", &definition, "/tmp/proj2");

        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert!(entry.get("url").is_none());
        let command = entry.pointer("/command").and_then(Value::as_str).expect("command set");
        assert!(command.contains("animus-mcp-proxy"), "command should be the proxy binary, got {command}");
        let args: Vec<&str> = entry
            .pointer("/args")
            .and_then(Value::as_array)
            .expect("args set")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            args,
            vec!["--server", "linear", "--project-root", "/tmp/proj2", "--url", "https://mcp.linear.app/mcp"]
        );
    }
}
