use crate::{
    ensure_safe_run_id, event_matches_run, invalid_input_error, not_found_error, run_dir,
    McpCommand,
};
use anyhow::{Context, Result};
use orchestrator_core::{OrchestratorWorkflow, WorkflowStateManager, WorkflowStatus};
use protocol::{AgentRunEvent, OutputStreamType, RunId};
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
use std::cmp::Ordering;
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::UNIX_EPOCH;
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
const OUTPUT_TAIL_SCHEMA: &str = "ao.output.tail.v1";
const DEFAULT_OUTPUT_TAIL_LIMIT: usize = 50;
const MAX_OUTPUT_TAIL_LIMIT: usize = 500;

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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
struct OutputTailInput {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    event_types: Option<Vec<String>>,
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
                    if let Some(envelope) =
                        result.stdout_json.as_ref().or(result.stderr_json.as_ref())
                    {
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
        name = "ao.output.tail",
        description = "Return the most recent output, error, or thinking events for a run or task.",
        input_schema = ao_schema_for_type::<OutputTailInput>()
    )]
    async fn ao_output_tail(
        &self,
        params: Parameters<OutputTailInput>,
    ) -> Result<CallToolResult, McpError> {
        match build_output_tail_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "ao.output.tail",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "ao.output.tail",
                "error": error.to_string(),
            }))),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputTailEventType {
    Output,
    Error,
    Thinking,
}

impl OutputTailEventType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Error => "error",
            Self::Thinking => "thinking",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OutputTailEventRecord {
    event_type: String,
    run_id: String,
    text: String,
    source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream_type: Option<String>,
}

#[derive(Debug, Clone)]
struct OutputTailResolution {
    run_id: String,
    run_dir: PathBuf,
    resolved_from: &'static str,
}

fn build_output_tail_result(default_project_root: &str, input: OutputTailInput) -> Result<Value> {
    let project_root = resolve_daemon_events_project_root(default_project_root, input.project_root);
    let run_id = normalize_non_empty(input.run_id);
    let task_id = normalize_non_empty(input.task_id);
    let event_types = parse_output_tail_event_types(input.event_types)?;
    let limit = output_tail_limit(input.limit);
    let resolved = resolve_output_tail_resolution(project_root.as_str(), run_id, task_id)?;
    let resolved_run_id = RunId(resolved.run_id.clone());
    let events_path = resolved.run_dir.join("events.jsonl");
    let events = read_output_tail_events(&events_path, &resolved_run_id, &event_types, limit)?;

    Ok(json!({
        "schema": OUTPUT_TAIL_SCHEMA,
        "resolved_run_id": resolved_run_id.0,
        "resolved_from": resolved.resolved_from,
        "events_path": events_path.display().to_string(),
        "limit": limit,
        "event_types": event_types
            .iter()
            .map(|event_type| event_type.as_str())
            .collect::<Vec<_>>(),
        "count": events.len(),
        "events": events,
    }))
}

fn normalize_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn output_tail_limit(limit: Option<usize>) -> usize {
    let normalized = limit.unwrap_or(DEFAULT_OUTPUT_TAIL_LIMIT).max(1);
    normalized.min(MAX_OUTPUT_TAIL_LIMIT)
}

fn parse_output_tail_event_types(raw: Option<Vec<String>>) -> Result<Vec<OutputTailEventType>> {
    let values = match raw {
        Some(values) if values.is_empty() => {
            return Err(invalid_input_error(
                "event_types must include at least one of: output|error|thinking",
            ));
        }
        Some(values) => values,
        None => {
            return Ok(vec![
                OutputTailEventType::Output,
                OutputTailEventType::Thinking,
            ]);
        }
    };

    let mut parsed = Vec::new();
    for value in values {
        let event_type = parse_output_tail_event_type(value.as_str())?;
        if !parsed.contains(&event_type) {
            parsed.push(event_type);
        }
    }
    Ok(parsed)
}

fn parse_output_tail_event_type(value: &str) -> Result<OutputTailEventType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "output" => Ok(OutputTailEventType::Output),
        "error" => Ok(OutputTailEventType::Error),
        "thinking" => Ok(OutputTailEventType::Thinking),
        _ => Err(invalid_input_error(format!(
            "invalid event type '{value}'; expected one of: output|error|thinking"
        ))),
    }
}

fn resolve_output_tail_resolution(
    project_root: &str,
    run_id: Option<String>,
    task_id: Option<String>,
) -> Result<OutputTailResolution> {
    match (run_id, task_id) {
        (Some(run_id), None) => resolve_output_tail_run_id(project_root, run_id),
        (None, Some(task_id)) => resolve_output_tail_task_id(project_root, task_id),
        (Some(_), Some(_)) => Err(invalid_input_error(
            "provide exactly one of run_id or task_id",
        )),
        (None, None) => Err(invalid_input_error(
            "provide exactly one of run_id or task_id",
        )),
    }
}

fn resolve_output_tail_run_id(project_root: &str, run_id: String) -> Result<OutputTailResolution> {
    ensure_safe_run_id(run_id.as_str())?;
    let run_dir =
        crate::services::operations::resolve_run_dir_for_lookup(project_root, run_id.as_str())?
            .ok_or_else(|| not_found_error(format!("run directory not found for {run_id}")))?;
    Ok(OutputTailResolution {
        run_id,
        run_dir,
        resolved_from: "run_id",
    })
}

fn resolve_output_tail_task_id(
    project_root: &str,
    task_id: String,
) -> Result<OutputTailResolution> {
    let workflows = workflow_candidates_for_task(project_root, task_id.as_str())?;
    if workflows.is_empty() {
        return Err(not_found_error(format!(
            "workflow not found for task {task_id}"
        )));
    }

    for workflow in workflows {
        if let Some((run_id, run_dir)) =
            resolve_latest_workflow_run_dir(project_root, workflow.id.as_str())?
        {
            return Ok(OutputTailResolution {
                run_id,
                run_dir,
                resolved_from: "task_id",
            });
        }
    }

    Err(not_found_error(format!(
        "run directory not found for task {task_id}"
    )))
}

fn workflow_candidates_for_task(
    project_root: &str,
    task_id: &str,
) -> Result<Vec<OrchestratorWorkflow>> {
    let manager = WorkflowStateManager::new(project_root);
    let mut workflows: Vec<OrchestratorWorkflow> = manager
        .list()
        .with_context(|| format!("failed to load workflows for task {task_id}"))?
        .into_iter()
        .filter(|workflow| workflow.task_id.eq_ignore_ascii_case(task_id))
        .collect();
    workflows.sort_by(compare_workflow_candidates);
    Ok(workflows)
}

fn compare_workflow_candidates(
    left: &OrchestratorWorkflow,
    right: &OrchestratorWorkflow,
) -> Ordering {
    workflow_status_priority(left.status)
        .cmp(&workflow_status_priority(right.status))
        .then_with(|| workflow_timestamp(right).cmp(&workflow_timestamp(left)))
        .then_with(|| left.id.cmp(&right.id))
}

fn workflow_status_priority(status: WorkflowStatus) -> usize {
    match status {
        WorkflowStatus::Running => 0,
        WorkflowStatus::Paused => 1,
        WorkflowStatus::Pending => 2,
        WorkflowStatus::Failed => 3,
        WorkflowStatus::Completed => 4,
        WorkflowStatus::Cancelled => 5,
    }
}

fn workflow_timestamp(workflow: &OrchestratorWorkflow) -> i64 {
    workflow
        .completed_at
        .unwrap_or(workflow.started_at)
        .timestamp_millis()
}

fn resolve_latest_workflow_run_dir(
    project_root: &str,
    workflow_id: &str,
) -> Result<Option<(String, PathBuf)>> {
    let run_ids = run_ids_for_workflow(project_root, workflow_id)?;
    let mut candidates = Vec::new();
    for run_id in run_ids {
        let Some(run_dir) =
            crate::services::operations::resolve_run_dir_for_lookup(project_root, run_id.as_str())?
        else {
            continue;
        };
        let events_path = run_dir.join("events.jsonl");
        let has_events = events_path.exists();
        let modified_millis = if has_events {
            path_modified_millis(events_path.as_path())
        } else {
            path_modified_millis(run_dir.as_path())
        };
        candidates.push((run_id, run_dir, has_events, modified_millis));
    }

    candidates.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| left.0.cmp(&right.0))
    });

    Ok(candidates
        .into_iter()
        .next()
        .map(|(run_id, run_dir, _, _)| (run_id, run_dir)))
}

fn run_ids_for_workflow(project_root: &str, workflow_id: &str) -> Result<BTreeSet<String>> {
    let mut run_ids = BTreeSet::new();
    let prefix = format!("wf-{workflow_id}-");
    for runs_root in runs_root_candidates(project_root) {
        if !runs_root.exists() {
            continue;
        }
        for entry in fs::read_dir(&runs_root)
            .with_context(|| format!("failed to read run directory {}", runs_root.display()))?
        {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if !name.starts_with(prefix.as_str()) {
                continue;
            }
            if ensure_safe_run_id(name.as_str()).is_err() {
                continue;
            }
            run_ids.insert(name);
        }
    }
    Ok(run_ids)
}

fn runs_root_candidates(project_root: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(scoped_parent) = run_dir(
        project_root,
        &RunId("output-tail-root-probe".to_string()),
        None,
    )
    .parent()
    {
        candidates.push(scoped_parent.to_path_buf());
    }
    candidates.push(Path::new(project_root).join(".ao").join("runs"));
    candidates.push(
        Path::new(project_root)
            .join(".ao")
            .join("state")
            .join("runs"),
    );

    let mut deduped = Vec::new();
    for candidate in candidates {
        if deduped.iter().all(|existing| existing != &candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn path_modified_millis(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn read_output_tail_events(
    events_path: &Path,
    run_id: &RunId,
    event_types: &[OutputTailEventType],
    limit: usize,
) -> Result<Vec<OutputTailEventRecord>> {
    let file = match fs::File::open(events_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read events log {}", events_path.display()));
        }
    };

    let mut reader = BufReader::new(file);
    let mut line_buffer = Vec::new();
    let mut tail = VecDeque::new();
    loop {
        line_buffer.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line_buffer)
            .with_context(|| format!("failed to read events log {}", events_path.display()))?;
        if bytes_read == 0 {
            break;
        }

        let line = String::from_utf8_lossy(&line_buffer);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, run_id) {
            continue;
        }
        let Some(record) = normalize_tail_event(event, event_types) else {
            continue;
        };
        if tail.len() == limit {
            let _ = tail.pop_front();
        }
        tail.push_back(record);
    }
    Ok(tail.into_iter().collect())
}

fn output_stream_type_label(stream_type: OutputStreamType) -> &'static str {
    match stream_type {
        OutputStreamType::Stdout => "stdout",
        OutputStreamType::Stderr => "stderr",
        OutputStreamType::System => "system",
    }
}

fn normalize_tail_event(
    event: AgentRunEvent,
    event_types: &[OutputTailEventType],
) -> Option<OutputTailEventRecord> {
    match event {
        AgentRunEvent::OutputChunk {
            run_id,
            stream_type,
            text,
        } => {
            if !event_types.contains(&OutputTailEventType::Output) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Output.as_str().to_string(),
                run_id: run_id.0,
                text,
                source_kind: "output_chunk".to_string(),
                stream_type: Some(output_stream_type_label(stream_type).to_string()),
            })
        }
        AgentRunEvent::Error { run_id, error } => {
            if !event_types.contains(&OutputTailEventType::Error) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Error.as_str().to_string(),
                run_id: run_id.0,
                text: error,
                source_kind: "error".to_string(),
                stream_type: None,
            })
        }
        AgentRunEvent::Thinking { run_id, content } => {
            if !event_types.contains(&OutputTailEventType::Thinking) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Thinking.as_str().to_string(),
                run_id: run_id.0,
                text: content,
                source_kind: "thinking".to_string(),
                stream_type: None,
            })
        }
        AgentRunEvent::Started { .. }
        | AgentRunEvent::Metadata { .. }
        | AgentRunEvent::Finished { .. }
        | AgentRunEvent::ToolCall { .. }
        | AgentRunEvent::ToolResult { .. }
        | AgentRunEvent::Artifact { .. } => None,
    }
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
    use chrono::{Duration, Utc};
    use std::collections::HashMap;
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

    fn write_run_events(project_root: &str, run_id: &str, lines: &[String]) {
        let run_path = run_dir(project_root, &RunId(run_id.to_string()), None);
        std::fs::create_dir_all(&run_path).expect("run directory should be created");
        let payload = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(run_path.join("events.jsonl"), payload)
            .expect("run events should be written");
    }

    fn output_event(run_id: &str, text: &str) -> String {
        output_event_with_stream(run_id, text, protocol::OutputStreamType::Stdout)
    }

    fn output_event_with_stream(
        run_id: &str,
        text: &str,
        stream_type: protocol::OutputStreamType,
    ) -> String {
        serde_json::to_string(&AgentRunEvent::OutputChunk {
            run_id: RunId(run_id.to_string()),
            stream_type,
            text: text.to_string(),
        })
        .expect("output event should serialize")
    }

    fn thinking_event(run_id: &str, content: &str) -> String {
        serde_json::to_string(&AgentRunEvent::Thinking {
            run_id: RunId(run_id.to_string()),
            content: content.to_string(),
        })
        .expect("thinking event should serialize")
    }

    fn error_event(run_id: &str, error: &str) -> String {
        serde_json::to_string(&AgentRunEvent::Error {
            run_id: RunId(run_id.to_string()),
            error: error.to_string(),
        })
        .expect("error event should serialize")
    }

    fn save_workflow(
        project_root: &str,
        workflow_id: &str,
        task_id: &str,
        status: WorkflowStatus,
        started_at: chrono::DateTime<Utc>,
        completed_at: Option<chrono::DateTime<Utc>>,
    ) {
        let manager = WorkflowStateManager::new(project_root);
        manager
            .save(&OrchestratorWorkflow {
                id: workflow_id.to_string(),
                task_id: task_id.to_string(),
                pipeline_id: None,
                status,
                current_phase_index: 0,
                phases: Vec::new(),
                machine_state: orchestrator_core::WorkflowMachineState::Idle,
                current_phase: None,
                started_at,
                completed_at,
                failure_reason: None,
                checkpoint_metadata: orchestrator_core::WorkflowCheckpointMetadata::default(),
                rework_counts: HashMap::new(),
                total_reworks: 0,
                decision_history: Vec::new(),
            })
            .expect("workflow should be written");
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

    #[test]
    fn build_output_tail_result_requires_exactly_one_identifier() {
        let err_none = build_output_tail_result(
            "/tmp/project",
            OutputTailInput {
                run_id: None,
                task_id: None,
                limit: None,
                event_types: None,
                project_root: None,
            },
        )
        .expect_err("missing identifiers should fail");
        assert!(err_none.to_string().contains("exactly one"));

        let err_both = build_output_tail_result(
            "/tmp/project",
            OutputTailInput {
                run_id: Some("run-1".to_string()),
                task_id: Some("TASK-1".to_string()),
                limit: None,
                event_types: None,
                project_root: None,
            },
        )
        .expect_err("multiple identifiers should fail");
        assert!(err_both.to_string().contains("exactly one"));
    }

    #[test]
    fn build_output_tail_result_rejects_invalid_event_type() {
        let err = build_output_tail_result(
            "/tmp/project",
            OutputTailInput {
                run_id: Some("run-1".to_string()),
                task_id: None,
                limit: None,
                event_types: Some(vec!["unknown".to_string()]),
                project_root: None,
            },
        )
        .expect_err("unknown filter should fail");
        assert!(err.to_string().contains("invalid event type"));
    }

    #[test]
    fn build_output_tail_result_rejects_unsafe_run_id() {
        let err = build_output_tail_result(
            "/tmp/project",
            OutputTailInput {
                run_id: Some("../escape".to_string()),
                task_id: None,
                limit: None,
                event_types: None,
                project_root: None,
            },
        )
        .expect_err("unsafe run id should fail");
        assert!(err.to_string().contains("invalid run_id"));
    }

    #[test]
    fn build_output_tail_result_filters_out_events_for_other_runs() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-filter-run-match-phase-0-d4";
        let other_run = "wf-filter-run-other-phase-0-e5";
        write_run_events(
            root.as_str(),
            run_id,
            &[
                output_event(run_id, "keep-output"),
                output_event(other_run, "drop-output"),
                thinking_event(other_run, "drop-thinking"),
                thinking_event(run_id, "keep-thinking"),
                error_event(run_id, "keep-error"),
            ],
        );

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: Some(10),
                event_types: Some(vec![
                    "output".to_string(),
                    "thinking".to_string(),
                    "error".to_string(),
                ]),
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(result.get("count").and_then(Value::as_u64), Some(3));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].get("text").and_then(Value::as_str),
            Some("keep-output")
        );
        assert_eq!(
            events[1].get("text").and_then(Value::as_str),
            Some("keep-thinking")
        );
        assert_eq!(
            events[2].get("text").and_then(Value::as_str),
            Some("keep-error")
        );
    }

    #[test]
    fn build_output_tail_result_returns_empty_when_events_log_missing() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-missing-events-phase-0-f6";
        let run_path = run_dir(root.as_str(), &RunId(run_id.to_string()), None);
        std::fs::create_dir_all(&run_path).expect("run directory should exist");

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: Some(10),
                event_types: None,
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(result.get("count").and_then(Value::as_u64), Some(0));
        assert_eq!(
            result.get("events").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn build_output_tail_result_skips_invalid_utf8_log_lines() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-invalid-utf8-phase-0-g7";
        let run_path = run_dir(root.as_str(), &RunId(run_id.to_string()), None);
        std::fs::create_dir_all(&run_path).expect("run directory should be created");
        let mut payload = Vec::new();
        payload.extend_from_slice(output_event(run_id, "visible-output").as_bytes());
        payload.push(b'\n');
        payload.extend_from_slice(&[0xff, 0xfe, b'\n']);
        payload.extend_from_slice(thinking_event(run_id, "visible-thinking").as_bytes());
        payload.push(b'\n');
        std::fs::write(run_path.join("events.jsonl"), payload).expect("events should be written");

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: Some(10),
                event_types: None,
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].get("text").and_then(Value::as_str),
            Some("visible-output")
        );
        assert_eq!(
            events[1].get("text").and_then(Value::as_str),
            Some("visible-thinking")
        );
    }

    #[test]
    fn build_output_tail_result_defaults_to_output_and_thinking() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-default-filter-phase-0-a1";
        write_run_events(
            root.as_str(),
            run_id,
            &[
                output_event(run_id, "first output"),
                "{malformed".to_string(),
                error_event(run_id, "ignored error"),
                thinking_event(run_id, "visible thought"),
            ],
        );

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: None,
                event_types: None,
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(
            result.get("schema").and_then(Value::as_str),
            Some(OUTPUT_TAIL_SCHEMA)
        );
        assert_eq!(
            result.get("resolved_from").and_then(Value::as_str),
            Some("run_id")
        );
        assert_eq!(result.get("limit").and_then(Value::as_u64), Some(50));
        assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].get("event_type").and_then(Value::as_str),
            Some("output")
        );
        assert_eq!(
            events[0].get("text").and_then(Value::as_str),
            Some("first output")
        );
        assert_eq!(
            events[1].get("event_type").and_then(Value::as_str),
            Some("thinking")
        );
        assert_eq!(
            events[1].get("text").and_then(Value::as_str),
            Some("visible thought")
        );
    }

    #[test]
    fn build_output_tail_result_normalizes_output_stream_types() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-stream-types-phase-0-s9";
        write_run_events(
            root.as_str(),
            run_id,
            &[
                output_event_with_stream(run_id, "stdout line", protocol::OutputStreamType::Stdout),
                output_event_with_stream(run_id, "stderr line", protocol::OutputStreamType::Stderr),
                output_event_with_stream(run_id, "system line", protocol::OutputStreamType::System),
            ],
        );

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: Some(10),
                event_types: Some(vec!["output".to_string()]),
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(result.get("count").and_then(Value::as_u64), Some(3));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].get("stream_type").and_then(Value::as_str),
            Some("stdout")
        );
        assert_eq!(
            events[1].get("stream_type").and_then(Value::as_str),
            Some("stderr")
        );
        assert_eq!(
            events[2].get("stream_type").and_then(Value::as_str),
            Some("system")
        );
    }

    #[test]
    fn build_output_tail_result_applies_filter_and_limit_in_order() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-limit-filter-phase-0-b2";
        write_run_events(
            root.as_str(),
            run_id,
            &[
                output_event(run_id, "out-1"),
                thinking_event(run_id, "think-1"),
                output_event(run_id, "out-2"),
                error_event(run_id, "err-1"),
            ],
        );

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: Some(2),
                event_types: Some(vec![
                    "output".to_string(),
                    "thinking".to_string(),
                    "error".to_string(),
                ]),
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events[0].get("text").and_then(Value::as_str), Some("out-2"));
        assert_eq!(events[1].get("text").and_then(Value::as_str), Some("err-1"));
        assert_eq!(
            events[1].get("event_type").and_then(Value::as_str),
            Some("error")
        );
    }

    #[test]
    fn build_output_tail_result_clamps_limit_to_minimum() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "wf-limit-min-phase-0-c3";
        write_run_events(
            root.as_str(),
            run_id,
            &[error_event(run_id, "first"), error_event(run_id, "second")],
        );

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: Some(run_id.to_string()),
                task_id: None,
                limit: Some(0),
                event_types: Some(vec!["error".to_string()]),
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(result.get("limit").and_then(Value::as_u64), Some(1));
        assert_eq!(result.get("count").and_then(Value::as_u64), Some(1));
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("text").and_then(Value::as_str),
            Some("second")
        );
    }

    #[test]
    fn build_output_tail_result_resolves_task_to_running_workflow_run() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let temp = TempDir::new().expect("tempdir should be created");
        let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should exist");
        let root = project_root.to_string_lossy().to_string();
        let now = Utc::now();

        save_workflow(
            root.as_str(),
            "wf-completed",
            "TASK-043",
            WorkflowStatus::Completed,
            now - Duration::minutes(20),
            Some(now - Duration::minutes(10)),
        );
        save_workflow(
            root.as_str(),
            "wf-running",
            "TASK-043",
            WorkflowStatus::Running,
            now - Duration::minutes(1),
            None,
        );

        let completed_run = "wf-wf-completed-implementation-0-old";
        let running_run = "wf-wf-running-implementation-0-new";
        write_run_events(
            root.as_str(),
            completed_run,
            &[output_event(completed_run, "completed-output")],
        );
        write_run_events(
            root.as_str(),
            running_run,
            &[output_event(running_run, "running-output")],
        );

        let result = build_output_tail_result(
            root.as_str(),
            OutputTailInput {
                run_id: None,
                task_id: Some("TASK-043".to_string()),
                limit: Some(10),
                event_types: Some(vec!["output".to_string()]),
                project_root: None,
            },
        )
        .expect("tail result should build");

        assert_eq!(
            result.get("resolved_from").and_then(Value::as_str),
            Some("task_id")
        );
        assert_eq!(
            result.get("resolved_run_id").and_then(Value::as_str),
            Some(running_run)
        );
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .expect("events should be an array");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("text").and_then(Value::as_str),
            Some("running-output")
        );
    }
}
