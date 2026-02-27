use crate::McpCommand;
use anyhow::{Context, Result};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, JsonObject, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::generate::SchemaSettings;
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
    linked_requirement: Vec<String>,
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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
struct DaemonStartInput {
    #[serde(default)]
    max_agents: Option<usize>,
    #[serde(default)]
    interval_secs: Option<u64>,
    #[serde(default)]
    stale_threshold_hours: Option<u64>,
    #[serde(default)]
    max_tasks_per_tick: Option<usize>,
    #[serde(default)]
    phase_timeout_secs: Option<u64>,
    #[serde(default)]
    idle_timeout_secs: Option<u64>,
    #[serde(default)]
    skip_runner: Option<bool>,
    #[serde(default)]
    autonomous: Option<bool>,
    #[serde(default)]
    include_registry: Option<bool>,
    #[serde(default)]
    ai_task_generation: Option<bool>,
    #[serde(default)]
    auto_run_ready: Option<bool>,
    #[serde(default)]
    auto_merge: Option<bool>,
    #[serde(default)]
    auto_pr: Option<bool>,
    #[serde(default)]
    auto_commit_before_merge: Option<bool>,
    #[serde(default)]
    auto_prune_worktrees_after_merge: Option<bool>,
    #[serde(default)]
    startup_cleanup: Option<bool>,
    #[serde(default)]
    resume_interrupted: Option<bool>,
    #[serde(default)]
    reconcile_stale: Option<bool>,
    #[serde(default)]
    runner_scope: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
struct DaemonEventsInput {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    project_root: Option<String>,
}

const DEFAULT_DAEMON_EVENTS_LIMIT: usize = 100;
const MAX_DAEMON_EVENTS_LIMIT: usize = 500;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct AgentRunInput {
    #[serde(default = "default_codex")]
    tool: String,
    #[serde(default = "default_codex")]
    model: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    context_json: Option<String>,
    #[serde(default)]
    runtime_contract_json: Option<String>,
    #[serde(default = "default_true")]
    detach: bool,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    runner_scope: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct AgentControlInput {
    run_id: String,
    action: String,
    #[serde(default)]
    runner_scope: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct AgentStatusInput {
    run_id: String,
    #[serde(default)]
    runner_scope: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
struct RunnerScopeInput {
    #[serde(default)]
    runner_scope: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct RunIdInput {
    run_id: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct OutputMonitorInput {
    run_id: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    phase_id: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct OutputJsonlInput {
    run_id: String,
    #[serde(default)]
    entries: bool,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct ExecutionIdInput {
    execution_id: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskUpdateInput {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    input_json: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskAssignInput {
    id: String,
    assignee: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskCancelInput {
    task_id: String,
    #[serde(default)]
    confirm: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskSetPriorityInput {
    task_id: String,
    priority: String,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct TaskSetDeadlineInput {
    task_id: String,
    #[serde(default)]
    deadline: Option<String>,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct WorkflowDestructiveInput {
    id: String,
    #[serde(default)]
    confirm: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct IdInput {
    id: String,
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
                if result.success {
                    let data = result
                        .stdout_json
                        .map(|envelope| match envelope {
                            Value::Object(mut map) => {
                                map.remove("data").unwrap_or(Value::Object(map))
                            }
                            other => other,
                        })
                        .unwrap_or(Value::Null);

                    let data = summarize_list_if_needed(tool_name, data);

                    Ok(CallToolResult::structured(json!({
                        "tool": tool_name,
                        "result": data,
                    })))
                } else {
                    let mut payload = json!({ "tool": tool_name });
                    if let Some(envelope) = result.stdout_json {
                        if let Some(error) = envelope.get("error") {
                            payload["error"] = error.clone();
                        } else if let Some(data) = envelope.get("data") {
                            payload["error"] = data.clone();
                        }
                    }
                    payload["exit_code"] = json!(result.exit_code);
                    let stderr = result.stderr.trim().to_string();
                    if !stderr.is_empty() {
                        payload["stderr"] = json!(stderr);
                    }
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
    #[tool(
        name = "ao.project.list",
        description = "List known projects.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
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
        description = "List tasks with optional filters.",
        input_schema = ao_schema_for_type::<TaskListInput>()
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

    #[tool(
        name = "ao.task.create",
        description = "Create a task.",
        input_schema = ao_schema_for_type::<TaskCreateInput>()
    )]
    async fn ao_task_create(
        &self,
        params: Parameters<TaskCreateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_create_args(&input);
        self.run_tool("ao.task.create", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.status",
        description = "Update task status.",
        input_schema = ao_schema_for_type::<TaskStatusInput>()
    )]
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

    #[tool(
        name = "ao.task.get",
        description = "Fetch a task by id.",
        input_schema = ao_schema_for_type::<TaskGetInput>()
    )]
    async fn ao_task_get(
        &self,
        params: Parameters<TaskGetInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_get_args(input.id);
        self.run_tool("ao.task.get", args, input.project_root).await
    }

    #[tool(
        name = "ao.task.delete",
        description = "Delete a task.",
        input_schema = ao_schema_for_type::<TaskDeleteInput>()
    )]
    async fn ao_task_delete(
        &self,
        params: Parameters<TaskDeleteInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_delete_args(input.id, input.confirm, input.dry_run);
        self.run_tool("ao.task.delete", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.pause",
        description = "Pause a task.",
        input_schema = ao_schema_for_type::<TaskControlInput>()
    )]
    async fn ao_task_pause(
        &self,
        params: Parameters<TaskControlInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_control_args("pause", input.task_id);
        self.run_tool("ao.task.pause", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.resume",
        description = "Resume a paused task.",
        input_schema = ao_schema_for_type::<TaskControlInput>()
    )]
    async fn ao_task_resume(
        &self,
        params: Parameters<TaskControlInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_task_control_args("resume", input.task_id);
        self.run_tool("ao.task.resume", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.requirements.list",
        description = "List requirements.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
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

    #[tool(
        name = "ao.requirements.get",
        description = "Get a requirement by id.",
        input_schema = ao_schema_for_type::<RequirementGetInput>()
    )]
    async fn ao_requirements_get(
        &self,
        params: Parameters<RequirementGetInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_requirements_get_args(input.id);
        self.run_tool("ao.requirements.get", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.list",
        description = "List workflows.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
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
        description = "Run a workflow for a task and optional pipeline.",
        input_schema = ao_schema_for_type::<WorkflowRunInput>()
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

    #[tool(
        name = "ao.daemon.start",
        description = "Start the AO daemon.",
        input_schema = ao_schema_for_type::<DaemonStartInput>()
    )]
    async fn ao_daemon_start(
        &self,
        params: Parameters<DaemonStartInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_daemon_start_args(&input);
        self.run_tool("ao.daemon.start", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.daemon.stop",
        description = "Stop the AO daemon.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_stop(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.stop",
            vec!["daemon".to_string(), "stop".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.status",
        description = "Get daemon status.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_status(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.status",
            vec!["daemon".to_string(), "status".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.health",
        description = "Check daemon health.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_health(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.health",
            vec!["daemon".to_string(), "health".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.pause",
        description = "Pause the daemon scheduler.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_pause(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.pause",
            vec!["daemon".to_string(), "pause".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.resume",
        description = "Resume the daemon scheduler.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_resume(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.resume",
            vec!["daemon".to_string(), "resume".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.daemon.events",
        description = "List recent daemon events.",
        input_schema = ao_schema_for_type::<DaemonEventsInput>()
    )]
    async fn ao_daemon_events(
        &self,
        params: Parameters<DaemonEventsInput>,
    ) -> Result<CallToolResult, McpError> {
        match build_daemon_events_poll_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "ao.daemon.events",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "ao.daemon.events",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "ao.daemon.agents",
        description = "List active daemon agents.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_agents(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.daemon.agents",
            vec!["daemon".to_string(), "agents".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.agent.run",
        description = "Run an agent. Defaults to detached mode for MCP.",
        input_schema = ao_schema_for_type::<AgentRunInput>()
    )]
    async fn ao_agent_run(
        &self,
        params: Parameters<AgentRunInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_agent_run_args(&input);
        self.run_tool("ao.agent.run", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.agent.control",
        description = "Control a running agent (pause, resume, terminate).",
        input_schema = ao_schema_for_type::<AgentControlInput>()
    )]
    async fn ao_agent_control(
        &self,
        params: Parameters<AgentControlInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "agent".to_string(),
            "control".to_string(),
            "--run-id".to_string(),
            input.run_id,
            "--action".to_string(),
            input.action,
        ];
        push_opt(&mut args, "--runner-scope", input.runner_scope);
        self.run_tool("ao.agent.control", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.agent.status",
        description = "Get status of an agent run.",
        input_schema = ao_schema_for_type::<AgentStatusInput>()
    )]
    async fn ao_agent_status(
        &self,
        params: Parameters<AgentStatusInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "agent".to_string(),
            "status".to_string(),
            "--run-id".to_string(),
            input.run_id,
        ];
        push_opt(&mut args, "--runner-scope", input.runner_scope);
        self.run_tool("ao.agent.status", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.agent.runner-status",
        description = "Get agent runner process status.",
        input_schema = ao_schema_for_type::<RunnerScopeInput>()
    )]
    async fn ao_agent_runner_status(
        &self,
        params: Parameters<RunnerScopeInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec!["agent".to_string(), "runner-status".to_string()];
        push_opt(&mut args, "--runner-scope", input.runner_scope);
        self.run_tool("ao.agent.runner-status", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.output.run",
        description = "Get output for an agent run.",
        input_schema = ao_schema_for_type::<RunIdInput>()
    )]
    async fn ao_output_run(
        &self,
        params: Parameters<RunIdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "output".to_string(),
            "run".to_string(),
            "--run-id".to_string(),
            input.run_id,
        ];
        self.run_tool("ao.output.run", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.output.monitor",
        description = "Monitor output for a run, task, or phase.",
        input_schema = ao_schema_for_type::<OutputMonitorInput>()
    )]
    async fn ao_output_monitor(
        &self,
        params: Parameters<OutputMonitorInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "output".to_string(),
            "monitor".to_string(),
            "--run-id".to_string(),
            input.run_id,
        ];
        push_opt(&mut args, "--task-id", input.task_id);
        push_opt(&mut args, "--phase-id", input.phase_id);
        self.run_tool("ao.output.monitor", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.output.jsonl",
        description = "Get JSONL log for an agent run.",
        input_schema = ao_schema_for_type::<OutputJsonlInput>()
    )]
    async fn ao_output_jsonl(
        &self,
        params: Parameters<OutputJsonlInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "output".to_string(),
            "jsonl".to_string(),
            "--run-id".to_string(),
            input.run_id,
        ];
        if input.entries {
            args.push("--entries".to_string());
        }
        self.run_tool("ao.output.jsonl", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.output.artifacts",
        description = "Get artifacts for an execution.",
        input_schema = ao_schema_for_type::<ExecutionIdInput>()
    )]
    async fn ao_output_artifacts(
        &self,
        params: Parameters<ExecutionIdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "output".to_string(),
            "artifacts".to_string(),
            "--execution-id".to_string(),
            input.execution_id,
        ];
        self.run_tool("ao.output.artifacts", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.runner.health",
        description = "Check runner process health.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_runner_health(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.runner.health",
            vec!["runner".to_string(), "health".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.runner.orphans-detect",
        description = "Detect orphaned runner processes.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_runner_orphans_detect(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.runner.orphans-detect",
            vec![
                "runner".to_string(),
                "orphans".to_string(),
                "detect".to_string(),
            ],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.runner.restart-stats",
        description = "Get runner restart statistics.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_runner_restart_stats(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.runner.restart-stats",
            vec!["runner".to_string(), "restart-stats".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.task.update",
        description = "Update task fields.",
        input_schema = ao_schema_for_type::<TaskUpdateInput>()
    )]
    async fn ao_task_update(
        &self,
        params: Parameters<TaskUpdateInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "task".to_string(),
            "update".to_string(),
            "--id".to_string(),
            input.id,
        ];
        push_opt(&mut args, "--title", input.title);
        push_opt(&mut args, "--description", input.description);
        push_opt(&mut args, "--priority", input.priority);
        push_opt(&mut args, "--status", input.status);
        push_opt(&mut args, "--assignee", input.assignee);
        push_opt(&mut args, "--input-json", input.input_json);
        self.run_tool("ao.task.update", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.assign",
        description = "Assign a task to a user or agent.",
        input_schema = ao_schema_for_type::<TaskAssignInput>()
    )]
    async fn ao_task_assign(
        &self,
        params: Parameters<TaskAssignInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task".to_string(),
            "assign".to_string(),
            "--id".to_string(),
            input.id,
            "--assignee".to_string(),
            input.assignee,
        ];
        self.run_tool("ao.task.assign", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.prioritized",
        description = "List tasks in priority order.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_task_prioritized(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.task.prioritized",
            vec!["task".to_string(), "prioritized".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.task.next",
        description = "Get the next task to work on.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_task_next(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.task.next",
            vec!["task".to_string(), "next".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.task.stats",
        description = "Get task statistics.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_task_stats(
        &self,
        params: Parameters<ProjectRootInput>,
    ) -> Result<CallToolResult, McpError> {
        self.run_tool(
            "ao.task.stats",
            vec!["task".to_string(), "stats".to_string()],
            params.0.project_root,
        )
        .await
    }

    #[tool(
        name = "ao.task.cancel",
        description = "Cancel a task.",
        input_schema = ao_schema_for_type::<TaskCancelInput>()
    )]
    async fn ao_task_cancel(
        &self,
        params: Parameters<TaskCancelInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "task-control".to_string(),
            "cancel".to_string(),
            "--task-id".to_string(),
            input.task_id,
        ];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.task.cancel", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.set-priority",
        description = "Set task priority.",
        input_schema = ao_schema_for_type::<TaskSetPriorityInput>()
    )]
    async fn ao_task_set_priority(
        &self,
        params: Parameters<TaskSetPriorityInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "task-control".to_string(),
            "set-priority".to_string(),
            "--task-id".to_string(),
            input.task_id,
            "--priority".to_string(),
            input.priority,
        ];
        self.run_tool("ao.task.set-priority", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.task.set-deadline",
        description = "Set or clear a task deadline.",
        input_schema = ao_schema_for_type::<TaskSetDeadlineInput>()
    )]
    async fn ao_task_set_deadline(
        &self,
        params: Parameters<TaskSetDeadlineInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "task-control".to_string(),
            "set-deadline".to_string(),
            "--task-id".to_string(),
            input.task_id,
        ];
        push_opt(&mut args, "--deadline", input.deadline);
        self.run_tool("ao.task.set-deadline", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.get",
        description = "Get workflow details by id.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_get(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "get".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.get", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.pause",
        description = "Pause a running workflow.",
        input_schema = ao_schema_for_type::<WorkflowDestructiveInput>()
    )]
    async fn ao_workflow_pause(
        &self,
        params: Parameters<WorkflowDestructiveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "workflow".to_string(),
            "pause".to_string(),
            "--id".to_string(),
            input.id,
        ];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.workflow.pause", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.cancel",
        description = "Cancel a running workflow.",
        input_schema = ao_schema_for_type::<WorkflowDestructiveInput>()
    )]
    async fn ao_workflow_cancel(
        &self,
        params: Parameters<WorkflowDestructiveInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let mut args = vec![
            "workflow".to_string(),
            "cancel".to_string(),
            "--id".to_string(),
            input.id,
        ];
        push_opt(&mut args, "--confirm", input.confirm);
        if input.dry_run {
            args.push("--dry-run".to_string());
        }
        self.run_tool("ao.workflow.cancel", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.resume",
        description = "Resume a paused workflow.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_resume(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "resume".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.resume", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.decisions",
        description = "List workflow decisions.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_decisions(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "decisions".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.decisions", args, input.project_root)
            .await
    }

    #[tool(
        name = "ao.workflow.checkpoints.list",
        description = "List workflow checkpoints.",
        input_schema = ao_schema_for_type::<IdInput>()
    )]
    async fn ao_workflow_checkpoints_list(
        &self,
        params: Parameters<IdInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = vec![
            "workflow".to_string(),
            "checkpoints".to_string(),
            "list".to_string(),
            "--id".to_string(),
            input.id,
        ];
        self.run_tool("ao.workflow.checkpoints.list", args, input.project_root)
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

fn use_draft07_tool_schema() -> bool {
    std::env::var("AO_MCP_SCHEMA_DRAFT")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "07" | "draft07" | "draft-07" | "draft_07"
            )
        })
        .unwrap_or(false)
}

fn ao_schema_for_type<T: JsonSchema + std::any::Any>() -> std::sync::Arc<JsonObject> {
    if !use_draft07_tool_schema() {
        return rmcp::handler::server::common::schema_for_type::<T>();
    }

    let mut settings = SchemaSettings::draft07();
    settings.transforms = vec![Box::new(schemars::transform::AddNullable::default())];
    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let value = serde_json::to_value(schema).expect("failed to serialize draft07 schema");
    let object = match value {
        Value::Object(object) => object,
        other => panic!(
            "Schema serialization produced non-object value: expected JSON object but got {other:?}"
        ),
    };
    std::sync::Arc::new(object)
}

const TASK_SUMMARY_FIELDS: &[&str] = &[
    "id",
    "title",
    "status",
    "priority",
    "type",
    "linked_requirements",
    "dependencies",
    "tags",
    "assignee",
];

const REQUIREMENT_SUMMARY_FIELDS: &[&str] = &[
    "id",
    "title",
    "status",
    "priority",
    "category",
    "type",
    "linked_task_ids",
];

fn summarize_list_if_needed(tool_name: &str, data: Value) -> Value {
    let keep_fields: &[&str] = match tool_name {
        "ao.task.list" | "ao.task.prioritized" => TASK_SUMMARY_FIELDS,
        "ao.requirements.list" => REQUIREMENT_SUMMARY_FIELDS,
        _ => return data,
    };

    match data {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| retain_fields(item, keep_fields))
                .collect(),
        ),
        other => other,
    }
}

fn retain_fields(value: Value, fields: &[&str]) -> Value {
    match value {
        Value::Object(map) => {
            let filtered: serde_json::Map<String, Value> = map
                .into_iter()
                .filter(|(key, _)| fields.contains(&key.as_str()))
                .collect();
            Value::Object(filtered)
        }
        other => other,
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

fn default_codex() -> String {
    "codex".to_string()
}

fn build_daemon_start_args(input: &DaemonStartInput) -> Vec<String> {
    let mut args = vec!["daemon".to_string(), "start".to_string()];
    push_opt_usize(&mut args, "--max-agents", input.max_agents);
    push_opt_num(&mut args, "--interval-secs", input.interval_secs);
    push_opt_num(
        &mut args,
        "--stale-threshold-hours",
        input.stale_threshold_hours,
    );
    push_opt_usize(&mut args, "--max-tasks-per-tick", input.max_tasks_per_tick);
    push_opt_num(&mut args, "--phase-timeout-secs", input.phase_timeout_secs);
    push_opt_num(&mut args, "--idle-timeout-secs", input.idle_timeout_secs);
    push_bool_flag(&mut args, "--skip-runner", input.skip_runner);
    push_bool_flag(&mut args, "--autonomous", input.autonomous);
    push_bool_set(&mut args, "--include-registry", input.include_registry);
    push_bool_set(&mut args, "--ai-task-generation", input.ai_task_generation);
    push_bool_set(&mut args, "--auto-run-ready", input.auto_run_ready);
    push_bool_set(&mut args, "--auto-merge", input.auto_merge);
    push_bool_set(&mut args, "--auto-pr", input.auto_pr);
    push_bool_set(
        &mut args,
        "--auto-commit-before-merge",
        input.auto_commit_before_merge,
    );
    push_bool_set(
        &mut args,
        "--auto-prune-worktrees-after-merge",
        input.auto_prune_worktrees_after_merge,
    );
    push_bool_set(&mut args, "--startup-cleanup", input.startup_cleanup);
    push_bool_set(&mut args, "--resume-interrupted", input.resume_interrupted);
    push_bool_set(&mut args, "--reconcile-stale", input.reconcile_stale);
    push_opt(&mut args, "--runner-scope", input.runner_scope.clone());
    args
}

fn build_agent_run_args(input: &AgentRunInput) -> Vec<String> {
    let mut args = vec![
        "agent".to_string(),
        "run".to_string(),
        "--tool".to_string(),
        input.tool.clone(),
        "--model".to_string(),
        input.model.clone(),
        "--stream".to_string(),
        "false".to_string(),
    ];
    if input.detach {
        args.push("--detach".to_string());
    }
    push_opt(&mut args, "--prompt", input.prompt.clone());
    push_opt(&mut args, "--cwd", input.cwd.clone());
    push_opt_num(&mut args, "--timeout-secs", input.timeout_secs);
    push_opt(&mut args, "--context-json", input.context_json.clone());
    push_opt(
        &mut args,
        "--runtime-contract-json",
        input.runtime_contract_json.clone(),
    );
    push_opt(&mut args, "--run-id", input.run_id.clone());
    push_opt(&mut args, "--runner-scope", input.runner_scope.clone());
    args
}

fn build_task_create_args(input: &TaskCreateInput) -> Vec<String> {
    let mut args = vec![
        "task".to_string(),
        "create".to_string(),
        "--title".to_string(),
        input.title.clone(),
        "--description".to_string(),
        input.description.clone().unwrap_or_default(),
    ];
    push_opt(&mut args, "--task-type", input.task_type.clone());
    push_opt(&mut args, "--priority", input.priority.clone());
    for requirement_id in &input.linked_requirement {
        args.push("--linked-requirement".to_string());
        args.push(requirement_id.clone());
    }
    args
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

fn daemon_events_poll_limit(limit: Option<usize>) -> usize {
    let normalized = limit.unwrap_or(DEFAULT_DAEMON_EVENTS_LIMIT).max(1);
    normalized.min(MAX_DAEMON_EVENTS_LIMIT)
}

fn resolve_daemon_events_project_root(
    default_project_root: &str,
    project_root_override: Option<String>,
) -> String {
    let candidate = project_root_override
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_project_root.to_string());
    crate::services::runtime::canonicalize_lossy(candidate.as_str())
}

fn build_daemon_events_poll_result(
    default_project_root: &str,
    input: DaemonEventsInput,
) -> Result<Value> {
    let project_root = resolve_daemon_events_project_root(default_project_root, input.project_root);
    let limit = daemon_events_poll_limit(input.limit);
    let response =
        crate::services::runtime::poll_daemon_events(Some(limit), Some(project_root.as_str()))?;
    Ok(json!({
        "schema": response.schema,
        "events_path": response.events_path,
        "project_root": project_root,
        "limit": limit,
        "count": response.count,
        "events": response.events,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::daemon_events_log_path;
    use crate::services::runtime::DaemonEventRecord;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn sample_event(seq: u64, event_type: &str, project_root: &str) -> DaemonEventRecord {
        DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: format!("evt-{seq}"),
            seq,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            event_type: event_type.to_string(),
            project_root: Some(project_root.to_string()),
            data: json!({ "seq": seq }),
        }
    }

    fn write_events(lines: &[String]) {
        let path = daemon_events_log_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("daemon event parent directory should exist");
        }
        let content = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(path, content).expect("daemon event log should be written");
    }

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
    fn build_task_create_args_includes_linked_requirements() {
        let args = build_task_create_args(&TaskCreateInput {
            title: "Traceability task".to_string(),
            description: Some("desc".to_string()),
            task_type: Some("feature".to_string()),
            priority: Some("high".to_string()),
            linked_requirement: vec!["REQ-123".to_string(), "REQ-456".to_string()],
            project_root: None,
        });
        assert_eq!(
            args,
            vec![
                "task".to_string(),
                "create".to_string(),
                "--title".to_string(),
                "Traceability task".to_string(),
                "--description".to_string(),
                "desc".to_string(),
                "--task-type".to_string(),
                "feature".to_string(),
                "--priority".to_string(),
                "high".to_string(),
                "--linked-requirement".to_string(),
                "REQ-123".to_string(),
                "--linked-requirement".to_string(),
                "REQ-456".to_string(),
            ]
        );
    }

    #[test]
    fn build_task_create_args_uses_empty_description_when_omitted() {
        let args = build_task_create_args(&TaskCreateInput {
            title: "Task".to_string(),
            description: None,
            task_type: None,
            priority: None,
            linked_requirement: Vec::new(),
            project_root: None,
        });
        assert_eq!(
            args,
            vec![
                "task".to_string(),
                "create".to_string(),
                "--title".to_string(),
                "Task".to_string(),
                "--description".to_string(),
                String::new(),
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

    #[test]
    fn build_daemon_start_args_defaults_minimal() {
        let input = DaemonStartInput::default();
        let args = build_daemon_start_args(&input);
        assert_eq!(args, vec!["daemon".to_string(), "start".to_string()]);
    }

    #[test]
    fn build_daemon_start_args_with_flags() {
        let input = DaemonStartInput {
            max_agents: Some(4),
            skip_runner: Some(true),
            include_registry: Some(false),
            auto_run_ready: Some(true),
            runner_scope: Some("project".to_string()),
            ..Default::default()
        };
        let args = build_daemon_start_args(&input);
        assert_eq!(
            args,
            vec![
                "daemon".to_string(),
                "start".to_string(),
                "--max-agents".to_string(),
                "4".to_string(),
                "--skip-runner".to_string(),
                "--include-registry".to_string(),
                "false".to_string(),
                "--auto-run-ready".to_string(),
                "true".to_string(),
                "--runner-scope".to_string(),
                "project".to_string(),
            ]
        );
    }

    #[test]
    fn build_daemon_start_args_includes_stale_threshold_hours() {
        let input = DaemonStartInput {
            stale_threshold_hours: Some(48),
            ..Default::default()
        };
        let args = build_daemon_start_args(&input);
        assert_eq!(
            args,
            vec![
                "daemon".to_string(),
                "start".to_string(),
                "--stale-threshold-hours".to_string(),
                "48".to_string(),
            ]
        );
    }

    #[test]
    fn build_agent_run_args_defaults_detach_and_stream() {
        let input = AgentRunInput {
            tool: "codex".to_string(),
            model: "codex".to_string(),
            prompt: None,
            cwd: None,
            timeout_secs: None,
            context_json: None,
            runtime_contract_json: None,
            detach: true,
            run_id: None,
            runner_scope: None,
            project_root: None,
        };
        let args = build_agent_run_args(&input);
        assert_eq!(
            args,
            vec![
                "agent".to_string(),
                "run".to_string(),
                "--tool".to_string(),
                "codex".to_string(),
                "--model".to_string(),
                "codex".to_string(),
                "--stream".to_string(),
                "false".to_string(),
                "--detach".to_string(),
            ]
        );
    }

    #[test]
    fn build_agent_run_args_with_all_options() {
        let input = AgentRunInput {
            tool: "claude".to_string(),
            model: "opus".to_string(),
            prompt: Some("hello".to_string()),
            cwd: Some("/tmp".to_string()),
            timeout_secs: Some(300),
            context_json: Some("{}".to_string()),
            runtime_contract_json: Some("{\"k\":1}".to_string()),
            detach: false,
            run_id: Some("run-1".to_string()),
            runner_scope: Some("global".to_string()),
            project_root: None,
        };
        let args = build_agent_run_args(&input);
        assert_eq!(
            args,
            vec![
                "agent".to_string(),
                "run".to_string(),
                "--tool".to_string(),
                "claude".to_string(),
                "--model".to_string(),
                "opus".to_string(),
                "--stream".to_string(),
                "false".to_string(),
                "--prompt".to_string(),
                "hello".to_string(),
                "--cwd".to_string(),
                "/tmp".to_string(),
                "--timeout-secs".to_string(),
                "300".to_string(),
                "--context-json".to_string(),
                "{}".to_string(),
                "--runtime-contract-json".to_string(),
                "{\"k\":1}".to_string(),
                "--run-id".to_string(),
                "run-1".to_string(),
                "--runner-scope".to_string(),
                "global".to_string(),
            ]
        );
    }

    #[test]
    fn daemon_events_poll_limit_defaults_and_clamps() {
        assert_eq!(daemon_events_poll_limit(None), DEFAULT_DAEMON_EVENTS_LIMIT);
        assert_eq!(daemon_events_poll_limit(Some(0)), 1);
        assert_eq!(
            daemon_events_poll_limit(Some(MAX_DAEMON_EVENTS_LIMIT + 25)),
            MAX_DAEMON_EVENTS_LIMIT
        );
    }

    #[test]
    fn resolve_daemon_events_project_root_uses_default_when_override_blank() {
        let default_root = TempDir::new().expect("default project root");
        let expected = crate::services::runtime::canonicalize_lossy(
            default_root.path().to_string_lossy().as_ref(),
        );
        assert_eq!(
            resolve_daemon_events_project_root(expected.as_str(), Some("   ".to_string())),
            expected
        );
    }

    #[test]
    fn build_daemon_events_poll_result_returns_non_null_structured_events() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project = TempDir::new().expect("project temp dir");
        let project_root = project.path().to_string_lossy().to_string();
        write_events(&[
            serde_json::to_string(&sample_event(1, "queue", project_root.as_str()))
                .expect("event json"),
            "{not-json".to_string(),
            serde_json::to_string(&sample_event(2, "workflow", project_root.as_str()))
                .expect("event json"),
        ]);

        let result = build_daemon_events_poll_result(
            project_root.as_str(),
            DaemonEventsInput {
                limit: Some(10),
                project_root: Some(project_root.clone()),
            },
        )
        .expect("poll result should be built");

        assert_eq!(
            result.get("schema").and_then(Value::as_str),
            Some("ao.daemon.events.poll.v1")
        );
        assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].get("seq").and_then(Value::as_u64), Some(1));
        assert_eq!(events[1].get("seq").and_then(Value::as_u64), Some(2));
    }

    #[test]
    fn build_daemon_events_poll_result_filters_by_project_root() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project_a = TempDir::new().expect("project A");
        let project_b = TempDir::new().expect("project B");
        let root_a = project_a.path().to_string_lossy().to_string();
        let root_b = project_b.path().to_string_lossy().to_string();
        write_events(&[
            serde_json::to_string(&sample_event(1, "queue", root_a.as_str())).expect("event json"),
            serde_json::to_string(&sample_event(2, "queue", root_b.as_str())).expect("event json"),
            serde_json::to_string(&sample_event(3, "log", root_a.as_str())).expect("event json"),
        ]);

        let result = build_daemon_events_poll_result(
            root_a.as_str(),
            DaemonEventsInput {
                limit: Some(50),
                project_root: Some(root_a.clone()),
            },
        )
        .expect("poll result should be built");
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| {
            event.get("project_root").and_then(Value::as_str) == Some(root_a.as_str())
        }));
        assert_eq!(events[0].get("seq").and_then(Value::as_u64), Some(1));
        assert_eq!(events[1].get("seq").and_then(Value::as_u64), Some(3));
    }

    #[test]
    fn build_daemon_events_poll_result_blank_project_root_falls_back_to_default() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project_a = TempDir::new().expect("project A");
        let project_b = TempDir::new().expect("project B");
        let root_a = crate::services::runtime::canonicalize_lossy(
            project_a.path().to_string_lossy().as_ref(),
        );
        let root_b = crate::services::runtime::canonicalize_lossy(
            project_b.path().to_string_lossy().as_ref(),
        );
        write_events(&[
            serde_json::to_string(&sample_event(1, "queue", root_a.as_str())).expect("event json"),
            serde_json::to_string(&sample_event(2, "queue", root_b.as_str())).expect("event json"),
            serde_json::to_string(&sample_event(3, "log", root_a.as_str())).expect("event json"),
        ]);

        let result = build_daemon_events_poll_result(
            root_a.as_str(),
            DaemonEventsInput {
                limit: Some(50),
                project_root: Some("   ".to_string()),
            },
        )
        .expect("poll result should be built");
        assert_eq!(
            result.get("project_root").and_then(Value::as_str),
            Some(root_a.as_str())
        );
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| {
            event.get("project_root").and_then(Value::as_str) == Some(root_a.as_str())
        }));
    }
}
