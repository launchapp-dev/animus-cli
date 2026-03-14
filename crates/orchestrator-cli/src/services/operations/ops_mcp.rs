#[cfg(test)]
use crate::run_dir;
use crate::McpCommand;
use anyhow::Result;
#[cfg(test)]
use orchestrator_core::WorkflowSubject;
#[cfg(test)]
use orchestrator_core::{OrchestratorWorkflow, WorkflowStateManager, WorkflowStatus};
#[cfg(test)]
use protocol::{AgentRunEvent, RunId};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        Annotated, CallToolResult, ErrorCode, JsonObject, ListResourcesResult,
        PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult,
        ResourceContents, ServerCapabilities, ServerInfo,
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
use std::path::{Path, PathBuf};
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
#[path = "ops_mcp/daemon.rs"]
mod daemon;
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
#[path = "ops_mcp/list_guard.rs"]
mod list_guard;
#[path = "ops_mcp/list_profiles.rs"]
mod list_profiles;
#[path = "ops_mcp/list_types.rs"]
mod list_types;
#[path = "ops_mcp/output.rs"]
mod output;
#[path = "ops_mcp/output_inputs.rs"]
mod output_inputs;
#[path = "ops_mcp/output_tail_events.rs"]
mod output_tail_events;
#[path = "ops_mcp/output_tail_resolution.rs"]
mod output_tail_resolution;
#[path = "ops_mcp/output_tail_types.rs"]
mod output_tail_types;
#[path = "ops_mcp/output_tools.rs"]
mod output_tools;
#[path = "ops_mcp/queue_command_args.rs"]
mod queue_command_args;
#[path = "ops_mcp/queue_inputs.rs"]
mod queue_inputs;
#[path = "ops_mcp/queue_tools.rs"]
mod queue_tools;
#[path = "ops_mcp/requirements_command_args.rs"]
mod requirements_command_args;
#[path = "ops_mcp/requirements_inputs.rs"]
mod requirements_inputs;
#[path = "ops_mcp/requirements_tools.rs"]
mod requirements_tools;
#[path = "ops_mcp/runner_tools.rs"]
mod runner_tools;
#[path = "ops_mcp/task_command_args.rs"]
mod task_command_args;
#[path = "ops_mcp/task_inputs.rs"]
mod task_inputs;
#[path = "ops_mcp/task_mutation_tools.rs"]
mod task_mutation_tools;
#[path = "ops_mcp/task_query_tools.rs"]
mod task_query_tools;
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
    build_daemon_config_set_args, build_daemon_events_poll_result, build_daemon_logs_result,
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
use queue_command_args::{build_queue_enqueue_args, build_queue_reorder_args};
use queue_inputs::*;
use requirements_command_args::{
    build_requirements_create_args, build_requirements_delete_args, build_requirements_get_args,
    build_requirements_refine_args, build_requirements_update_args,
};
use requirements_inputs::*;
use task_command_args::{
    build_bulk_status_item_args, build_bulk_update_item_args, build_task_control_args,
    build_task_create_args, build_task_delete_args, build_task_get_args,
    validate_bulk_status_input, validate_bulk_update_input,
};
use task_inputs::*;
use workflow_command_args::{
    build_bulk_workflow_run_item_args, validate_workflow_run_multiple_input,
};
use workflow_inputs::*;

const DEFAULT_DAEMON_EVENTS_LIMIT: usize = 100;
const MAX_DAEMON_EVENTS_LIMIT: usize = 500;
const OUTPUT_TAIL_SCHEMA: &str = "ao.output.tail.v1";
const DEFAULT_OUTPUT_TAIL_LIMIT: usize = 50;
const MAX_OUTPUT_TAIL_LIMIT: usize = 500;
const MCP_LIST_RESULT_SCHEMA: &str = "ao.mcp.list.result.v1";
const DEFAULT_MCP_LIST_LIMIT: usize = 25;
const MAX_MCP_LIST_LIMIT: usize = 200;
const DEFAULT_MCP_LIST_MAX_TOKENS: usize = 3000;
const MIN_MCP_LIST_MAX_TOKENS: usize = 256;
const MAX_MCP_LIST_MAX_TOKENS: usize = 12_000;
const BATCH_RESULT_SCHEMA: &str = "ao.mcp.batch.result.v1";
const MAX_BATCH_SIZE: usize = 100;

#[derive(Debug, Clone)]
struct AoMcpServer {
    default_project_root: String,
    tool_router: ToolRouter<Self>,
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

fn default_codex() -> String {
    "codex".to_string()
}

fn normalize_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn new_ao_mcp_server(default_project_root: &str) -> AoMcpServer {
    let tool_router = AoMcpServer::task_query_tools()
        + AoMcpServer::task_mutation_tools()
        + AoMcpServer::requirements_tool_router()
        + AoMcpServer::daemon_tool_router()
        + AoMcpServer::queue_tool_router()
        + AoMcpServer::agent_tool_router()
        + AoMcpServer::output_tool_router()
        + AoMcpServer::runner_tool_router()
        + AoMcpServer::workflow_runtime_tools()
        + AoMcpServer::workflow_definition_tools();

    AoMcpServer {
        default_project_root: default_project_root.to_string(),
        tool_router,
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AoMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(
            "Use these typed AO tools to run orchestrator CLI operations over MCP.",
        )
    }

    async fn list_resources(
        &self,
        _params: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::model::ErrorData> {
        let mut resource_tasks = RawResource::new("ao://project/tasks", "Tasks Index");
        resource_tasks.description =
            Some("AO project task index with id, title, status, priority".to_string());
        resource_tasks.mime_type = Some("application/json".to_string());

        let mut resource_requirements =
            RawResource::new("ao://project/requirements", "Requirements Index");
        resource_requirements.description =
            Some("AO project requirements index with id, title, status, priority".to_string());
        resource_requirements.mime_type = Some("application/json".to_string());

        let mut resource_daemon = RawResource::new("ao://project/daemon-events", "Daemon Events");
        resource_daemon.description = Some(
            "Recent daemon events for project observability. Supports ?limit=N query param"
                .to_string(),
        );
        resource_daemon.mime_type = Some("application/json".to_string());

        let resources = vec![
            Annotated::new(resource_tasks, None),
            Annotated::new(resource_requirements, None),
            Annotated::new(resource_daemon, None),
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
            "ao://project/tasks" => {
                let path = PathBuf::from(&self.default_project_root).join(".ao/tasks/index.json");
                let (content, _modified) = read_file_with_mtime(&path).map_err(|e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("failed to read tasks: {}", e),
                        None,
                    )
                })?;
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(content, uri.clone())
                        .with_mime_type("application/json"),
                ]))
            }
            "ao://project/requirements" => {
                let path =
                    PathBuf::from(&self.default_project_root).join(".ao/requirements/index.json");
                let (content, _modified) = read_file_with_mtime(&path).map_err(|e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("failed to read requirements: {}", e),
                        None,
                    )
                })?;
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(content, uri.clone())
                        .with_mime_type("application/json"),
                ]))
            }
            "ao://project/daemon-events" => {
                let limit = query
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(100);
                let content =
                    read_daemon_events(&self.default_project_root, limit).map_err(|e| {
                        McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            format!("failed to read daemon events: {}", e),
                            None,
                        )
                    })?;
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(content, uri.clone())
                        .with_mime_type("application/json"),
                ]))
            }
            _ => Err(McpError::new(
                ErrorCode::RESOURCE_NOT_FOUND,
                format!("unknown resource: {}", uri),
                None,
            )),
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
    let response =
        crate::services::runtime::poll_daemon_events(Some(limit), Some(canonical_root.as_str()))
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

pub(crate) async fn handle_mcp(command: McpCommand, project_root: &str) -> Result<()> {
    match command {
        McpCommand::Serve => {
            let service = new_ao_mcp_server(project_root).serve(stdio()).await?;
            service.waiting().await?;
            Ok(())
        }
    }
}

fn ao_schema_for_type<T: JsonSchema + std::any::Any>() -> std::sync::Arc<JsonObject> {
    rmcp::handler::server::common::schema_for_type::<T>()
}


#[cfg(test)]
#[path = "ops_mcp/tests.rs"]
mod tests;
