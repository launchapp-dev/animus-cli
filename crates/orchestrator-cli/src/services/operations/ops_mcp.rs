#[cfg(test)]
use crate::run_dir;
use crate::McpCommand;
use anyhow::Result;
#[cfg(test)]
use orchestrator_core::{OrchestratorWorkflow, WorkflowStateManager, WorkflowStatus};
#[cfg(test)]
use protocol::{AgentRunEvent, RunId};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        Annotated, CallToolResult, ErrorCode, JsonObject, ListResourcesResult, PaginatedRequestParams, RawResource,
        ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    },
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[path = "ops_mcp/agent_command_args.rs"]
mod agent_command_args;
#[path = "ops_mcp/agent_inputs.rs"]
mod agent_inputs;
#[path = "ops_mcp/agent_tools.rs"]
mod agent_tools;
#[path = "ops_mcp/ao_exec.rs"]
mod ao_exec;
#[path = "ops_mcp/common_types.rs"]
mod common_types;
#[path = "ops_mcp/compaction.rs"]
mod compaction;
#[path = "ops_mcp/cost_tools.rs"]
mod cost_tools;
#[path = "ops_mcp/daemon.rs"]
mod daemon;
#[path = "ops_mcp/daemon_inproc.rs"]
mod daemon_inproc;
#[path = "ops_mcp/daemon_inputs.rs"]
mod daemon_inputs;
#[path = "ops_mcp/daemon_tools.rs"]
mod daemon_tools;
#[path = "ops_mcp/exec.rs"]
mod exec;
#[path = "ops_mcp/exec_errors.rs"]
mod exec_errors;
#[path = "ops_mcp/exec_types.rs"]
mod exec_types;
#[path = "ops_mcp/interaction_tools.rs"]
mod interaction_tools;
#[path = "ops_mcp/list_guard.rs"]
mod list_guard;
#[path = "ops_mcp/list_profiles.rs"]
mod list_profiles;
#[path = "ops_mcp/list_types.rs"]
mod list_types;
#[path = "ops_mcp/logs_tools.rs"]
mod logs_tools;
#[path = "ops_mcp/memory_tools.rs"]
mod memory_tools;
#[path = "ops_mcp/output.rs"]
mod output;
#[path = "ops_mcp/output_inputs.rs"]
mod output_inputs;
#[path = "ops_mcp/output_tail_events.rs"]
mod output_tail_events;
#[path = "ops_mcp/output_tail_resolution.rs"]
pub(crate) mod output_tail_resolution;
#[path = "ops_mcp/output_tail_types.rs"]
mod output_tail_types;
#[path = "ops_mcp/output_tools.rs"]
mod output_tools;
#[path = "ops_mcp/plugin_marketplace_tools.rs"]
mod plugin_marketplace_tools;
#[path = "ops_mcp/plugin_tools.rs"]
mod plugin_tools;
#[path = "ops_mcp/queue_command_args.rs"]
mod queue_command_args;
#[path = "ops_mcp/queue_inputs.rs"]
mod queue_inputs;
#[path = "ops_mcp/queue_tools.rs"]
mod queue_tools;
#[path = "ops_mcp/skill_tools.rs"]
mod skill_tools;
#[path = "ops_mcp/subject_command_args.rs"]
mod subject_command_args;
#[path = "ops_mcp/subject_inputs.rs"]
mod subject_inputs;
#[path = "ops_mcp/subject_tools.rs"]
mod subject_tools;
#[path = "ops_mcp/tool_discovery_tools.rs"]
mod tool_discovery_tools;
#[path = "ops_mcp/workflow_command_args.rs"]
mod workflow_command_args;
#[path = "ops_mcp/workflow_definition_tools.rs"]
mod workflow_definition_tools;
#[path = "ops_mcp/workflow_inputs.rs"]
mod workflow_inputs;
#[path = "ops_mcp/workflow_runtime_tools.rs"]
mod workflow_runtime_tools;

use agent_command_args::build_agent_run_args;
use agent_inputs::*;
use common_types::*;
#[cfg(test)]
use compaction::compact_json_str;
use compaction::compact_json_text;
use daemon::{
    build_daemon_config_set_args, build_daemon_events_poll_result, build_daemon_logs_result, build_daemon_observe_args,
    build_daemon_start_args,
};
#[cfg(test)]
use daemon::{daemon_events_poll_limit, resolve_daemon_events_project_root};
use daemon_inputs::*;
#[cfg(test)]
use exec_errors::build_cli_error_payload;
#[cfg(test)]
use exec_errors::extract_cli_success_data;
use exec_types::*;
use list_guard::build_guarded_list_result;
#[cfg(test)]
use list_guard::{list_limit, list_max_tokens};
use list_types::*;
use output::build_output_tail_result;
use output_inputs::*;
use queue_command_args::{build_queue_enqueue_args, build_queue_reorder_args, build_queue_subject_args};
use queue_inputs::*;
use subject_command_args::{
    build_subject_batch_create_item_args, build_subject_batch_update_item_args, build_subject_create_args,
    build_subject_get_args, build_subject_list_args, build_subject_next_args, build_subject_status_args,
    build_subject_update_args, validate_subject_batch_create_input, validate_subject_batch_update_input,
};
use subject_inputs::*;
use workflow_command_args::{
    build_bulk_workflow_run_item_args, build_workflow_list_args, validate_workflow_run_multiple_input,
};
use workflow_inputs::*;

const DEFAULT_DAEMON_EVENTS_LIMIT: usize = 100;
const MAX_DAEMON_EVENTS_LIMIT: usize = 500;
const OUTPUT_TAIL_SCHEMA: &str = "animus.output.tail.v1";
const DEFAULT_OUTPUT_TAIL_LIMIT: usize = 50;
const MAX_OUTPUT_TAIL_LIMIT: usize = 500;
const MCP_LIST_RESULT_SCHEMA: &str = "animus.mcp.list.result.v1";
const DEFAULT_MCP_LIST_LIMIT: usize = 25;
const MAX_MCP_LIST_LIMIT: usize = 200;
const DEFAULT_MCP_LIST_MAX_TOKENS: usize = 3000;
const MIN_MCP_LIST_MAX_TOKENS: usize = 256;
const MAX_MCP_LIST_MAX_TOKENS: usize = 12_000;
const BATCH_RESULT_SCHEMA: &str = "animus.mcp.batch.result.v1";
const MAX_BATCH_SIZE: usize = 100;

/// Per-project plugin registry cache. Each project root gets its own
/// `PluginRegistry` so a call against `/repo/a` never reuses plugins discovered
/// under `/repo/b`. The outer mutex guards the map; each entry has its own
/// mutex because `PluginRegistry::get_plugin` borrows `&mut self`.
///
/// Sentinel key for "no project_root provided": the server's
/// `default_project_root` (canonicalized) is used. We never collapse "missing
/// override" into an empty path — that would silently merge with a project
/// root that canonicalizes to `""`. The sentinel is documented at the call
/// site in `resolve_registry_key`.
type PluginRegistryCache =
    std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<std::path::PathBuf, PluginRegistryEntry>>>;

type PluginRegistryEntry = std::sync::Arc<tokio::sync::Mutex<orchestrator_plugin_host::PluginRegistry>>;

#[derive(Clone)]
struct AoMcpServer {
    default_project_root: String,
    tool_router: ToolRouter<Self>,
    plugin_registry: PluginRegistryCache,
    // CLI-pinned agent identity (`animus mcp serve --agent-id <id>`) for the
    // blocking interaction tools; overrides both the env pin and the payload.
    pinned_agent_id: Option<String>,
    // CLI-pinned workflow context (`animus mcp serve --workflow-id <id>`).
    // When present, the blocking interaction tools default to
    // wait="suspend" and pending records carry this workflow id; overrides
    // both the env pin and the payload.
    pinned_workflow_id: Option<String>,
}

impl std::fmt::Debug for AoMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AoMcpServer").field("default_project_root", &self.default_project_root).finish_non_exhaustive()
    }
}

impl AoMcpServer {
    fn scoped_state_root(&self) -> std::path::PathBuf {
        protocol::scoped_state_root(std::path::Path::new(&self.default_project_root))
            .unwrap_or_else(|| std::path::PathBuf::from(&self.default_project_root).join(".animus"))
    }
}

fn push_opt(args: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(flag.to_string());
        args.push(value);
    }
}

fn push_bool_flag(args: &mut Vec<String>, flag: &str, value: Option<bool>) {
    if value == Some(true) {
        args.push(flag.to_string());
    }
}

fn push_bool_set(args: &mut Vec<String>, flag: &str, value: Option<bool>) {
    if let Some(v) = value {
        args.push(flag.to_string());
        args.push(v.to_string());
    }
}

fn push_opt_num(args: &mut Vec<String>, flag: &str, value: Option<u64>) {
    if let Some(v) = value {
        args.push(flag.to_string());
        args.push(v.to_string());
    }
}

fn push_opt_usize(args: &mut Vec<String>, flag: &str, value: Option<usize>) {
    if let Some(v) = value {
        args.push(flag.to_string());
        args.push(v.to_string());
    }
}

fn default_true() -> bool {
    true
}

fn default_claude() -> String {
    "claude".to_string()
}

fn normalize_non_empty(value: Option<String>) -> Option<String> {
    value.map(|raw| raw.trim().to_string()).filter(|raw| !raw.is_empty())
}

#[derive(Debug, Clone)]
pub(super) struct MemoryMcpServer {
    pub(super) default_project_root: String,
    tool_router: ToolRouter<Self>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemoryMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Project-scoped agent memory tools. Use animus.memory.append to record durable notes, \
                 animus.memory.get / animus.memory.list to read them back, and animus.memory.clear to prune.",
        )
    }
}

pub(super) fn new_memory_mcp_server(default_project_root: &str) -> MemoryMcpServer {
    let tool_router = MemoryMcpServer::memory_tool_router();
    MemoryMcpServer { default_project_root: default_project_root.to_string(), tool_router }
}

#[cfg(test)]
fn new_ao_mcp_server(default_project_root: &str) -> AoMcpServer {
    new_ao_mcp_server_with_options(default_project_root, false, None, None)
}

// `management` gates the human-side `animus.interactions.*` tools. The default
// (agent-injected) server only carries the blocking `animus.agent.ask` /
// `animus.agent.request_approval` escalation tools, so an agent can never list
// or answer its own pending approvals; inbox UIs opt in via
// `animus mcp serve --management`. `pinned_agent_id` (from `--agent-id`) binds
// the blocking tools' identity so the payload cannot select another profile.
// `pinned_workflow_id` (from `--workflow-id`) binds the workflow context so
// escalations default to wait="suspend" and pause/resume that workflow.
fn new_ao_mcp_server_with_options(
    default_project_root: &str,
    management: bool,
    pinned_agent_id: Option<String>,
    pinned_workflow_id: Option<String>,
) -> AoMcpServer {
    let mut tool_router = AoMcpServer::daemon_tool_router()
        + AoMcpServer::cost_tool_router()
        + AoMcpServer::queue_tool_router()
        + AoMcpServer::agent_tool_router()
        + AoMcpServer::output_tool_router()
        + AoMcpServer::workflow_runtime_tools()
        + AoMcpServer::workflow_definition_tools()
        + AoMcpServer::plugin_tool_router()
        + AoMcpServer::plugin_marketplace_tool_router()
        + AoMcpServer::skill_tool_router()
        + AoMcpServer::subject_tool_router()
        + AoMcpServer::logs_tool_router()
        + AoMcpServer::memory_tool_router_for_ao()
        + AoMcpServer::interaction_tool_router()
        + AoMcpServer::tool_discovery_tool_router();
    if management {
        tool_router += AoMcpServer::interaction_management_tool_router();
    }

    AoMcpServer {
        default_project_root: default_project_root.to_string(),
        tool_router,
        plugin_registry: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        pinned_agent_id: pinned_agent_id.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()),
        pinned_workflow_id: pinned_workflow_id.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()),
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AoMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_instructions("Use these typed Animus tools to run orchestrator CLI operations over MCP.")
    }

    async fn list_resources(
        &self,
        _params: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::model::ErrorData> {
        let mut resource_tasks = RawResource::new("animus://project/tasks", "Tasks Index");
        resource_tasks.description = Some("Animus project task index with id, title, status, priority".to_string());
        resource_tasks.mime_type = Some("application/json".to_string());

        let mut resource_requirements = RawResource::new("animus://project/requirements", "Requirements Index");
        resource_requirements.description =
            Some("Animus project requirements index with id, title, status, priority".to_string());
        resource_requirements.mime_type = Some("application/json".to_string());

        let mut resource_daemon = RawResource::new("animus://project/daemon-events", "Daemon Events");
        resource_daemon.description =
            Some("Recent daemon events for project observability. Supports ?limit=N query param".to_string());
        resource_daemon.mime_type = Some("application/json".to_string());

        // Back-compat: advertise the legacy `ao://` URIs alongside the new
        // `animus://` ones so clients that enumerate resources and look for
        // the v0.3 names continue to find them. Both URIs are also accepted
        // by `read_resource` below.
        let mut legacy_resource_tasks = RawResource::new("ao://project/tasks", "Tasks Index (legacy URI)");
        legacy_resource_tasks.description =
            Some("Deprecated alias of animus://project/tasks. Retained for back-compat with v0.3 clients.".to_string());
        legacy_resource_tasks.mime_type = Some("application/json".to_string());

        let mut legacy_resource_requirements =
            RawResource::new("ao://project/requirements", "Requirements Index (legacy URI)");
        legacy_resource_requirements.description = Some(
            "Deprecated alias of animus://project/requirements. Retained for back-compat with v0.3 clients."
                .to_string(),
        );
        legacy_resource_requirements.mime_type = Some("application/json".to_string());

        let mut legacy_resource_daemon = RawResource::new("ao://project/daemon-events", "Daemon Events (legacy URI)");
        legacy_resource_daemon.description = Some(
            "Deprecated alias of animus://project/daemon-events. Retained for back-compat with v0.3 clients."
                .to_string(),
        );
        legacy_resource_daemon.mime_type = Some("application/json".to_string());

        let resources = vec![
            Annotated::new(resource_tasks, None),
            Annotated::new(resource_requirements, None),
            Annotated::new(resource_daemon, None),
            Annotated::new(legacy_resource_tasks, None),
            Annotated::new(legacy_resource_requirements, None),
            Annotated::new(legacy_resource_daemon, None),
        ];
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::model::ErrorData> {
        let uri = params.uri.to_string();
        let (resource_uri, query) = parse_resource_uri(&uri);

        match resource_uri.as_str() {
            // `ao://*` accepted alongside `animus://*` for back-compat with clients
            // that cached the legacy resource URIs.
            "animus://project/tasks" | "ao://project/tasks" => {
                let path = self.scoped_state_root().join("tasks").join("index.json");
                let (content, _modified) = read_file_with_mtime(&path).map_err(|e| {
                    McpError::new(ErrorCode::INTERNAL_ERROR, format!("failed to read tasks: {}", e), None)
                })?;
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(content, uri.clone()).with_mime_type("application/json")
                ]))
            }
            "animus://project/requirements" | "ao://project/requirements" => {
                let path = self.scoped_state_root().join("requirements").join("index.json");
                let (content, _modified) = read_file_with_mtime(&path).map_err(|e| {
                    McpError::new(ErrorCode::INTERNAL_ERROR, format!("failed to read requirements: {}", e), None)
                })?;
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(content, uri.clone()).with_mime_type("application/json")
                ]))
            }
            "animus://project/daemon-events" | "ao://project/daemon-events" => {
                let limit = query.get("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(100);
                let content = read_daemon_events(&self.default_project_root, limit).map_err(|e| {
                    McpError::new(ErrorCode::INTERNAL_ERROR, format!("failed to read daemon events: {}", e), None)
                })?;
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(content, uri.clone()).with_mime_type("application/json")
                ]))
            }
            _ => Err(McpError::new(ErrorCode::RESOURCE_NOT_FOUND, format!("unknown resource: {}", uri), None)),
        }
    }
}

fn parse_resource_uri(uri: &str) -> (String, std::collections::HashMap<String, String>) {
    let mut query = std::collections::HashMap::new();
    if let Some((path, query_str)) = uri.split_once('?') {
        for pair in query_str.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                query.insert(key.to_string(), value.to_string());
            }
        }
        (path.to_string(), query)
    } else {
        (uri.to_string(), query)
    }
}

fn read_daemon_events(project_root: &str, limit: usize) -> Result<String, std::io::Error> {
    let canonical_root = crate::services::runtime::canonicalize_lossy(project_root);
    let response = crate::services::runtime::poll_daemon_events(Some(limit), Some(canonical_root.as_str()))
        .map_err(std::io::Error::other)?;
    let result = serde_json::json!({
        "events": response.events,
        "count": response.count,
        "limit": limit,
        "project_root": canonical_root,
        "events_path": response.events_path,
    });
    Ok(result.to_string())
}

fn read_file_with_mtime(path: &Path) -> Result<(String, Option<u64>), std::io::Error> {
    let content = fs::read_to_string(path)?;
    let modified = fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    Ok((compact_json_text(content), modified))
}

pub(crate) async fn handle_mcp(command: McpCommand, project_root: &str, cli_json: bool) -> Result<()> {
    match command {
        McpCommand::Serve(args) => {
            let service =
                new_ao_mcp_server_with_options(project_root, args.management, args.agent_id, args.workflow_id)
                    .serve(stdio())
                    .await?;
            service.waiting().await?;
            Ok(())
        }
        McpCommand::Memory => {
            let service = new_memory_mcp_server(project_root).serve(stdio()).await?;
            service.waiting().await?;
            Ok(())
        }
        McpCommand::Auth(args) => handle_mcp_auth(args, project_root, cli_json).await,
        McpCommand::AuthStatus(args) => handle_mcp_auth_status(args, project_root, cli_json).await,
        McpCommand::AuthLogout(args) => handle_mcp_auth_logout(args, project_root, cli_json).await,
    }
}

async fn handle_mcp_auth(args: crate::McpAuthArgs, project_root: &str, cli_json: bool) -> Result<()> {
    let root = Path::new(project_root);
    let scopes = args.scopes.clone();
    let opts = animus_mcp_oauth::RunAuthOptions {
        url_override: args.url.as_deref(),
        scopes_override: scopes.as_deref(),
        assume_yes: args.yes,
        json: cli_json,
        dry_run: args.dry_run,
        confirm: animus_mcp_oauth::Confirm::Interactive,
    };
    let result = animus_mcp_oauth::run_auth(root, &args.server, opts).await?;

    match result {
        animus_mcp_oauth::AuthResult::DryRun(dry) => {
            if cli_json {
                return crate::shared::print_value(dry, true);
            }
            let scopes = if dry.requested_scopes.is_empty() {
                "(none — server default / minimal)".to_string()
            } else if dry.scopes_auto_detected {
                format!("{} (auto-detected from server metadata)", dry.requested_scopes.join(", "))
            } else {
                dry.requested_scopes.join(", ")
            };
            println!("Dry run for `{}` ({}):", dry.server, dry.base_url);
            println!("  requested scopes: {scopes}");
            println!(
                "  client registration: {}",
                if dry.would_register_client { "would run DCR" } else { "would use pinned client_id" }
            );
            println!("  no browser opened, no tokens obtained.");
            Ok(())
        }
        animus_mcp_oauth::AuthResult::Completed(outcome) => {
            if cli_json {
                return crate::shared::print_value(outcome, true);
            }
            let expiry = outcome.expires_at.map(|e| e.to_rfc3339()).unwrap_or_else(|| "unknown".to_string());
            println!(
                "Authenticated `{}` (principal `{}`, client_id `{}`). Token expires: {}. Refresh token: {}.",
                outcome.server,
                outcome.principal,
                outcome.client_id,
                expiry,
                if outcome.has_refresh_token { "yes" } else { "no" }
            );
            if !outcome.granted_scopes.is_empty() {
                println!("Granted scopes: {}", outcome.granted_scopes.join(", "));
            }
            Ok(())
        }
    }
}

async fn handle_mcp_auth_status(args: crate::McpAuthStatusArgs, project_root: &str, cli_json: bool) -> Result<()> {
    let root = Path::new(project_root);
    let status = animus_mcp_oauth::auth_status(root, args.server.as_deref(), args.url.as_deref()).await?;

    if !cli_json {
        if status.servers.is_empty() {
            println!("No OAuth-protected (authorization_code) MCP servers found in config.");
            return Ok(());
        }
        for s in &status.servers {
            let state = if !s.authenticated {
                "not authenticated".to_string()
            } else if s.expired && s.has_refresh_token {
                "authenticated (access token expired — proxy will refresh)".to_string()
            } else if s.expired {
                format!("authenticated (token expired, no refresh token — re-run `animus mcp auth {}`)", s.server)
            } else {
                let expiry = s.expires_at.map(|e| e.to_rfc3339()).unwrap_or_else(|| "no expiry".to_string());
                format!("authenticated (expires {expiry})")
            };
            println!("{} [principal {}]: {}", s.server, s.principal, state);
        }
        return Ok(());
    }
    crate::shared::print_value(status, true)
}

async fn handle_mcp_auth_logout(args: crate::McpAuthLogoutArgs, project_root: &str, cli_json: bool) -> Result<()> {
    let root = Path::new(project_root);
    let had = animus_mcp_oauth::auth_logout(root, &args.server, args.url.as_deref()).await?;
    let message = if had {
        format!("Logged out `{}`; stored tokens deleted.", args.server)
    } else {
        format!("No stored tokens for `{}`.", args.server)
    };
    crate::shared::print_ok(&message, cli_json);
    Ok(())
}

fn ao_schema_for_type<T: JsonSchema + std::any::Any>() -> std::sync::Arc<JsonObject> {
    rmcp::handler::server::common::schema_for_type::<T>()
}

#[cfg(test)]
#[path = "ops_mcp/tests.rs"]
mod tests;
