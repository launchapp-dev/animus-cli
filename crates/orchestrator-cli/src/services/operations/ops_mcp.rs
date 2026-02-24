use crate::McpCommand;
use anyhow::{Context, Result};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
struct ProjectRootInput {
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
struct TaskListInput {
    #[serde(default)]
    project_root: Option<String>,
    #[serde(default)]
    task_type: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    assignee_type: Option<String>,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    linked_requirement: Option<String>,
    #[serde(default)]
    search: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskCreateInput {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    task_type: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskStatusInput {
    id: String,
    status: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskGetInput {
    id: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskDeleteInput {
    id: String,
    #[serde(default)]
    confirm: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskControlInput {
    task_id: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct RequirementGetInput {
    id: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct WorkflowRunInput {
    task_id: String,
    #[serde(default)]
    pipeline_id: Option<String>,
    #[serde(default)]
    input_json: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CliExecutionResult {
    command: String,
    args: Vec<String>,
    requested_args: Vec<String>,
    project_root: String,
    exit_code: i32,
    success: bool,
    stdout: String,
    stderr: String,
    stdout_json: Option<Value>,
    stderr_json: Option<Value>,
}

#[derive(Debug, Clone)]
struct AoMcpServer {
    default_project_root: String,
    tool_router: ToolRouter<Self>,
}

impl AoMcpServer {
    fn new(default_project_root: &str) -> Self {
        Self {
            default_project_root: default_project_root.to_string(),
            tool_router: Self::tool_router(),
        }
    }

    async fn execute_ao(
        &self,
        requested_args: Vec<String>,
        project_root_override: Option<String>,
    ) -> Result<CliExecutionResult> {
        let project_root =
            project_root_override.unwrap_or_else(|| self.default_project_root.clone());
        let mut args = vec![
            "--json".to_string(),
            "--project-root".to_string(),
            project_root.clone(),
        ];
        args.extend(requested_args.iter().cloned());

        let binary = std::env::current_exe().context("failed to resolve ao binary path")?;
        let output = TokioCommand::new(binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("failed to execute ao command")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout_json = parse_json(&stdout);
        let stderr_json = parse_json(&stderr);

        Ok(CliExecutionResult {
            command: "ao".to_string(),
            args,
            requested_args,
            project_root,
            exit_code: output.status.code().unwrap_or(-1),
            success: output.status.success(),
            stdout,
            stderr,
            stdout_json,
            stderr_json,
        })
    }

    async fn run_tool(
        &self,
        tool_name: &str,
        requested_args: Vec<String>,
        project_root_override: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        match self.execute_ao(requested_args, project_root_override).await {
            Ok(result) => {
                let payload = json!({
                    "tool": tool_name,
                    "result": result,
                });
                if result.success {
                    Ok(CallToolResult::structured(payload))
                } else {
                    Ok(CallToolResult::structured_error(payload))
                }
            }
            Err(err) => Ok(CallToolResult::structured_error(json!({
                "tool": tool_name,
                "error": err.to_string(),
            }))),
        }
    }
}

#[tool_router(router = tool_router)]
impl AoMcpServer {
    #[tool(name = "ao.project.list", description = "List known projects.")]
    async fn ao_project_list(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.project.list",
            vec!["project".to_string(), "list".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.task.list",
        description = "List tasks with optional filters."
    )]
    async fn ao_task_list(
        &self,
        params: Parameters<TaskListInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["task".to_string(), "list".to_string()];
        push_opt(&mut args, "--task-type", input.task_type);
        push_opt(&mut args, "--status", input.status);
        push_opt(&mut args, "--priority", input.priority);
        push_opt(&mut args, "--assignee-type", input.assignee_type);
        for tag in input.tag {
            args.push("--tag".to_string());
            args.push(tag);
        }
        push_opt(&mut args, "--linked-requirement", input.linked_requirement);
        push_opt(&mut args, "--search", input.search);
        self.run_tool("ao.task.list", args, input.project_root)
            .await
    }

    #[tool(name = "ao.task.create", description = "Create a task.")]
    async fn ao_task_create(
        &self,
        params: Parameters<TaskCreateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "task".to_string(),
            "create".to_string(),
            "--title".to_string(),
            input.title,
            "--description".to_string(),
            input.description.unwrap_or_default(),
        ];
        push_opt(&mut args, "--task-type", input.task_type);
        push_opt(&mut args, "--priority", input.priority);
        self.run_tool("ao.task.create", args, input.project_root)
            .await
    }

    #[tool(name = "ao.task.status", description = "Update task status.")]
    async fn ao_task_status(
        &self,
        params: Parameters<TaskStatusInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task".to_string(),
            "status".to_string(),
            "--id".to_string(),
            input.id,
            "--status".to_string(),
            input.status,
        ];
        self.run_tool("ao.task.status", args, input.project_root)
            .await
    }

    #[tool(name = "ao.task.get", description = "Fetch a task by id.")]
    async fn ao_task_get(
        &self,
        params: Parameters<TaskGetInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_get_args(input.id);
        self.run_tool("ao.task.get", args, input.project_root).await
    }

    #[tool(name = "ao.task.delete", description = "Delete a task.")]
    async fn ao_task_delete(
        &self,
        params: Parameters<TaskDeleteInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_delete_args(input.id, input.confirm, input.dry_run);
        self.run_tool("ao.task.delete", args, input.project_root)
            .await
    }

    #[tool(name = "ao.task.pause", description = "Pause a task.")]
    async fn ao_task_pause(
        &self,
        params: Parameters<TaskControlInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_control_args("pause", input.task_id);
        self.run_tool("ao.task.pause", args, input.project_root)
            .await
    }

    #[tool(name = "ao.task.resume", description = "Resume a paused task.")]
    async fn ao_task_resume(
        &self,
        params: Parameters<TaskControlInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_control_args("resume", input.task_id);
        self.run_tool("ao.task.resume", args, input.project_root)
            .await
    }

    #[tool(name = "ao.requirements.list", description = "List requirements.")]
    async fn ao_requirements_list(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.requirements.list",
            vec!["requirements".to_string(), "list".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(name = "ao.requirements.get", description = "Get a requirement by id.")]
    async fn ao_requirements_get(
        &self,
        params: Parameters<RequirementGetInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_get_args(input.id);
        self.run_tool("ao.requirements.get", args, input.project_root)
            .await
    }

    #[tool(name = "ao.workflow.list", description = "List workflows.")]
    async fn ao_workflow_list(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.workflow.list",
            vec!["workflow".to_string(), "list".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.workflow.run",
        description = "Run a workflow for a task and optional pipeline."
    )]
    async fn ao_workflow_run(
        &self,
        params: Parameters<WorkflowRunInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "workflow".to_string(),
            "run".to_string(),
            "--task-id".to_string(),
            input.task_id,
        ];
        push_opt(&mut args, "--pipeline-id", input.pipeline_id);
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.workflow.run", args, input.project_root)
            .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AoMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Use these typed AO tools to run orchestrator CLI operations over MCP.".to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

pub(crate) async fn handle_mcp(command: McpCommand, project_root: &str) -> Result<()> {
    match command {
        McpCommand::Serve => {
            let service = AoMcpServer::new(project_root).serve(stdio()).await?;
            service.waiting().await?;
            Ok(())
        }
    }
}

fn push_opt(args: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(flag.to_string());
        args.push(value);
    }
}

fn build_task_get_args(id: String) -> Vec<String> {
    vec![
        "task".to_string(),
        "get".to_string(),
        "--id".to_string(),
        id,
    ]
}

fn build_task_delete_args(id: String, confirm: Option<String>, dry_run: bool) -> Vec<String> {
    let mut args = vec![
        "task".to_string(),
        "delete".to_string(),
        "--id".to_string(),
        id,
    ];
    if let Some(confirm) = confirm {
        args.push("--confirm".to_string());
        args.push(confirm);
    }
    if dry_run {
        args.push("--dry-run".to_string());
    }
    args
}

fn build_task_control_args(action: &str, task_id: String) -> Vec<String> {
    vec![
        "task-control".to_string(),
        action.to_string(),
        "--task-id".to_string(),
        task_id,
    ]
}

fn build_requirements_get_args(id: String) -> Vec<String> {
    vec![
        "requirements".to_string(),
        "get".to_string(),
        "--id".to_string(),
        id,
    ]
}

fn parse_json(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_task_get_args_includes_id() {
        let args = build_task_get_args("task-123".to_string());
        assert_eq!(
            args,
            vec![
                "task".to_string(),
                "get".to_string(),
                "--id".to_string(),
                "task-123".to_string()
            ]
        );
    }

    #[test]
    fn build_task_delete_args_includes_id() {
        let args = build_task_delete_args("task-123".to_string(), None, false);
        assert_eq!(
            args,
            vec![
                "task".to_string(),
                "delete".to_string(),
                "--id".to_string(),
                "task-123".to_string()
            ]
        );
    }

    #[test]
    fn build_task_delete_args_supports_confirmation_and_dry_run() {
        let args =
            build_task_delete_args("task-123".to_string(), Some("task-123".to_string()), true);
        assert_eq!(
            args,
            vec![
                "task".to_string(),
                "delete".to_string(),
                "--id".to_string(),
                "task-123".to_string(),
                "--confirm".to_string(),
                "task-123".to_string(),
                "--dry-run".to_string(),
            ]
        );
    }

    #[test]
    fn build_task_control_args_emits_pause() {
        let args = build_task_control_args("pause", "TASK-123".to_string());
        assert_eq!(
            args,
            vec![
                "task-control".to_string(),
                "pause".to_string(),
                "--task-id".to_string(),
                "TASK-123".to_string(),
            ]
        );
    }

    #[test]
    fn build_task_control_args_emits_resume() {
        let args = build_task_control_args("resume", "TASK-456".to_string());
        assert_eq!(
            args,
            vec![
                "task-control".to_string(),
                "resume".to_string(),
                "--task-id".to_string(),
                "TASK-456".to_string(),
            ]
        );
    }

    #[test]
    fn builds_requirements_get_args() {
        let args = build_requirements_get_args("REQ-123".to_string());
        assert_eq!(
            args,
            vec![
                "requirements".to_string(),
                "get".to_string(),
                "--id".to_string(),
                "REQ-123".to_string(),
            ]
        );
    }
}
