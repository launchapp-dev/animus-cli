use super::*;
use crate::services::runtime::runtime_agent::interactions::{
    answer_interaction_op_with_resume, emit_interaction_event, pause_workflow_for_suspended_interaction,
    resume_workflow_for_answered_interaction,
};
use animus_runtime_shared::{InteractionRecord, InteractionStatus};
use orchestrator_config::agent_runtime_config::ApprovalPolicyDecision;
use std::time::Duration;

const DEFAULT_INTERACTION_TIMEOUT_SECS: u64 = 600;
const MAX_INTERACTION_TIMEOUT_SECS: u64 = 3600;
const INTERACTION_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// When set on the MCP server process, pins the agent identity used by the
/// blocking `animus.agent.ask` / `animus.agent.request_approval` tools. The
/// payload `agent_id` is ignored so a spawned agent cannot claim a sibling
/// profile whose `approval_policy` is more permissive.
pub(crate) const ANIMUS_MCP_AGENT_ID_ENV: &str = "ANIMUS_MCP_AGENT_ID";

/// When set on the MCP server process, pins the workflow context used by the
/// blocking interaction tools (env fallback for `animus mcp serve
/// --workflow-id`). A pinned workflow flips the default wait mode to
/// "suspend" and overrides the payload `workflow_id`.
pub(crate) const ANIMUS_MCP_WORKFLOW_ID_ENV: &str = "ANIMUS_MCP_WORKFLOW_ID";

/// Returned with `status: "pending"` suspend responses so the agent knows how
/// to end its turn; the session resumes with the answer via the workflow
/// resume path.
const SUSPEND_INSTRUCTION: &str = "This interaction is pending a human decision and the workflow has been \
     suspended. Summarize your in-progress state (what you changed, what remains, any assumptions), then end \
     your turn cleanly. The session resumes automatically once the interaction is answered.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractionWaitMode {
    Block,
    Suspend,
}

// Wait-mode resolution: the default is "suspend" when the server is pinned
// to a workflow (daemon phase runs) and "block" otherwise (ad-hoc
// `animus agent run` / `animus chat`). The payload may downgrade
// suspend -> block; block -> suspend is ignored with a warning because a
// non-workflow run has nothing to resume.
fn resolve_wait_mode(workflow_pinned: bool, requested: Option<&str>, tool_name: &str) -> InteractionWaitMode {
    let default_mode = if workflow_pinned { InteractionWaitMode::Suspend } else { InteractionWaitMode::Block };
    match requested.map(str::trim).filter(|value| !value.is_empty()) {
        None => default_mode,
        Some(value) if value.eq_ignore_ascii_case("block") => InteractionWaitMode::Block,
        Some(value) if value.eq_ignore_ascii_case("suspend") => {
            if workflow_pinned {
                InteractionWaitMode::Suspend
            } else {
                tracing::warn!(
                    tool = tool_name,
                    "wait=\"suspend\" ignored: server is not pinned to a workflow (no --workflow-id / {ANIMUS_MCP_WORKFLOW_ID_ENV}); using block"
                );
                InteractionWaitMode::Block
            }
        }
        Some(other) => {
            tracing::warn!(tool = tool_name, wait = other, "unknown wait mode; using the default");
            default_mode
        }
    }
}

// The blocking escalation inputs intentionally carry NO `project_root`
// override: the policy lookup and the pending-interaction store are always
// bound to the server's own project scope so a payload cannot route an
// approval through another project's (more permissive) policy.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AgentAskInput {
    pub(super) agent_id: String,
    pub(super) question: String,
    #[serde(default)]
    pub(super) options: Option<Vec<String>>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) workflow_id: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) wait: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AgentRequestApprovalInput {
    pub(super) agent_id: String,
    pub(super) action: String,
    #[serde(default)]
    pub(super) tool_name: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<Value>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) workflow_id: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) wait: Option<String>,
}

impl AoMcpServer {
    // Identity precedence for the blocking interaction tools: the CLI pin
    // (`animus mcp serve --agent-id <id>`, set by the host that injects the
    // server) wins, then the `ANIMUS_MCP_AGENT_ID` env pin, then — only when
    // neither pin is present — the untrusted payload `agent_id`.
    fn bound_agent_id(&self, payload_agent_id: &str) -> String {
        self.pinned_agent_id
            .clone()
            .or_else(|| {
                std::env::var(ANIMUS_MCP_AGENT_ID_ENV)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| payload_agent_id.trim().to_string())
    }

    // Workflow pin for the blocking interaction tools: the CLI pin
    // (`animus mcp serve --workflow-id <id>`) wins, then the
    // `ANIMUS_MCP_WORKFLOW_ID` env pin. `None` means the server is not bound
    // to a workflow.
    fn workflow_pin(&self) -> Option<String> {
        self.pinned_workflow_id.clone().or_else(|| {
            std::env::var(ANIMUS_MCP_WORKFLOW_ID_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    }

    // Workflow context precedence mirrors `bound_agent_id`: a pin overrides
    // the untrusted payload so a spawned agent cannot suspend a sibling
    // workflow; without a pin the payload value is kept on the record for
    // observability (block mode only).
    fn bound_workflow_id(&self, payload_workflow_id: Option<&str>) -> Option<String> {
        self.workflow_pin().or_else(|| normalize_non_empty(payload_workflow_id.map(ToOwned::to_owned)))
    }
}

// Suspend-mode return path shared by ask + request_approval: flag the record
// as suspend-created (the answer path only resumes flagged records, so an
// untrusted block-mode payload workflow_id can never trigger a resume),
// pause the bound workflow (best-effort), and hand the agent a pending
// payload telling it to end its turn.
async fn suspend_pending_response(
    tool_name: &str,
    project_root: &str,
    record: &InteractionRecord,
) -> Result<CallToolResult, McpError> {
    let record = match animus_runtime_shared::mark_interaction_suspended(project_root, &record.id) {
        Ok(updated) => updated,
        Err(err) => return structured_err(tool_name, err.to_string()),
    };
    let workflow_paused = pause_workflow_for_suspended_interaction(project_root, &record).await;
    // Close the mark->pause race: an answer landing in that window saw the
    // workflow still Running and skipped its resume, so the pause above
    // would strand the workflow. Re-check the record and run the resume
    // here if it was already answered (codex round-2 P2).
    let mut late_resume = None;
    if workflow_paused {
        if let Ok(Some(current)) = animus_runtime_shared::load_interaction(project_root, &record.id) {
            if current.status == InteractionStatus::Answered {
                late_resume = resume_workflow_for_answered_interaction(project_root, &current).await;
            }
        }
    }
    let mut payload = json!({
        "status": "pending",
        "interaction_id": record.id,
        "workflow_id": record.workflow_id,
        "workflow_paused": workflow_paused,
        "instruction": SUSPEND_INSTRUCTION,
    });
    if let (Value::Object(map), Some(resume)) = (&mut payload, late_resume) {
        map.insert("workflow_resume".to_string(), resume);
    }
    structured_ok(tool_name, payload)
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct InteractionsListInput {
    #[serde(default)]
    pub(super) all: Option<bool>,
    #[serde(default)]
    pub(super) agent_id: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct InteractionsAnswerInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) decision: Option<String>,
    #[serde(default)]
    pub(super) message: Option<String>,
    #[serde(default)]
    pub(super) answered_by: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

fn interaction_project_root(default: &str, override_value: Option<String>) -> String {
    override_value
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn effective_timeout_secs(timeout_secs: Option<u64>) -> u64 {
    timeout_secs.unwrap_or(DEFAULT_INTERACTION_TIMEOUT_SECS).clamp(1, MAX_INTERACTION_TIMEOUT_SECS)
}

fn structured_ok(tool_name: &str, data: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::structured(json!({
        "tool": tool_name,
        "result": data,
    })))
}

fn structured_err(tool_name: &str, message: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::structured_error(json!({
        "tool": tool_name,
        "error": message,
    })))
}

enum InteractionWait {
    Answered(Box<InteractionRecord>),
    TimedOut,
    Lost(String),
}

async fn wait_for_answer(project_root: &str, interaction_id: &str, timeout_secs: u64) -> InteractionWait {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match animus_runtime_shared::load_interaction(project_root, interaction_id) {
            Ok(Some(record)) => match record.status {
                InteractionStatus::Answered => return InteractionWait::Answered(Box::new(record)),
                InteractionStatus::Expired => return InteractionWait::TimedOut,
                InteractionStatus::Pending => {}
            },
            Ok(None) => return InteractionWait::Lost(format!("interaction '{}' disappeared", interaction_id)),
            Err(err) => return InteractionWait::Lost(err.to_string()),
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            // expire_interaction only flips pending records; a human answer that
            // raced the deadline comes back as Answered and must win over the
            // timeout so the agent and the store agree.
            return match animus_runtime_shared::expire_interaction(project_root, interaction_id) {
                Ok(Some(record)) if record.status == InteractionStatus::Answered => {
                    InteractionWait::Answered(Box::new(record))
                }
                _ => InteractionWait::TimedOut,
            };
        }
        tokio::time::sleep(INTERACTION_POLL_INTERVAL.min(deadline - now)).await;
    }
}

fn interaction_to_json(record: &InteractionRecord) -> Value {
    serde_json::to_value(record).unwrap_or(Value::Null)
}

#[tool_router(router = interaction_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.agent.ask",
        description = "Ask a human a question and WAIT for the answer. Purpose: Human-in-the-loop round-trip for agents that hit an ambiguity mid-run; the question lands in the `animus agent interactions` inbox. Always operates on the server's own project scope. Wait modes: \"block\" parks the call until answered or timeout (default for ad-hoc runs); \"suspend\" returns { status: \"pending\", interaction_id, instruction } immediately, pauses the bound workflow, and the session resumes with the answer (default when the server pins a workflow via --workflow-id / ANIMUS_MCP_WORKFLOW_ID; suspend->block override allowed, block->suspend is ignored). Params: agent_id (ignored when the server pins ANIMUS_MCP_AGENT_ID), question, optional options (suggested answers), timeout_secs (default 600, max 3600), workflow_id (ignored when the server pins a workflow), task_id, wait. Returns: { id, answer, answered_by, answer_message? } once answered, the pending payload in suspend mode, or a structured timeout error instructing the agent to proceed with its best judgment. Example: {\"agent_id\": \"swe\", \"question\": \"Migrate in place or copy table?\", \"options\": [\"in place\", \"copy\"]}.",
        input_schema = ao_schema_for_type::<AgentAskInput>()
    )]
    async fn ao_agent_ask(&self, params: Parameters<AgentAskInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = self.default_project_root.clone();
        let agent_id = self.bound_agent_id(&input.agent_id);
        let workflow_pinned = self.workflow_pin().is_some();
        let workflow_id = self.bound_workflow_id(input.workflow_id.as_deref());
        let wait_mode = resolve_wait_mode(workflow_pinned, input.wait.as_deref(), "animus.agent.ask");
        let timeout_secs = effective_timeout_secs(input.timeout_secs);
        let options = input.options.unwrap_or_default();
        let created = match animus_runtime_shared::create_question_interaction(
            &project_root,
            &agent_id,
            &input.question,
            &options,
            Some(timeout_secs),
            workflow_id.as_deref(),
            input.task_id.as_deref(),
        ) {
            Ok(record) => record,
            Err(err) => return structured_err("animus.agent.ask", err.to_string()),
        };
        emit_interaction_event("interaction_created", &project_root, &created);

        if wait_mode == InteractionWaitMode::Suspend {
            return suspend_pending_response("animus.agent.ask", &project_root, &created).await;
        }

        match wait_for_answer(&project_root, &created.id, timeout_secs).await {
            InteractionWait::Answered(record) => structured_ok(
                "animus.agent.ask",
                json!({
                    "id": record.id,
                    "answer": record.answer,
                    "answered_by": record.answered_by,
                    "answer_message": record.answer_message,
                }),
            ),
            InteractionWait::TimedOut => {
                if let Ok(Some(expired)) = animus_runtime_shared::load_interaction(&project_root, &created.id) {
                    emit_interaction_event("interaction_expired", &project_root, &expired);
                }
                Ok(CallToolResult::structured_error(json!({
                    "tool": "animus.agent.ask",
                    "error": format!(
                        "no human answered within {timeout_secs}s. Proceed with your best judgment, state the assumption you made, and continue."
                    ),
                    "interaction_id": created.id,
                    "timed_out": true,
                })))
            }
            InteractionWait::Lost(message) => structured_err("animus.agent.ask", message),
        }
    }

    #[tool(
        name = "animus.agent.request_approval",
        description = "Request human approval for a sensitive action and WAIT for the decision (block-mode timeout denies — fail closed). Purpose: Gate dangerous operations behind a human decision; the agent profile's approval_policy can auto-allow or auto-deny without escalating (auto_deny patterns win, matched against tool_name when present, else action, with `*` glob semantics). Always operates on the server's own project scope; the policy profile comes from agent_id, which is ignored when the server pins ANIMUS_MCP_AGENT_ID. Wait modes: \"block\" parks the call until decided or timeout (default for ad-hoc runs); \"suspend\" returns { status: \"pending\", interaction_id, instruction } immediately after the policy check, pauses the bound workflow, and the session resumes with the decision (default when the server pins a workflow via --workflow-id / ANIMUS_MCP_WORKFLOW_ID; suspend->block override allowed, block->suspend is ignored). Params: agent_id, action (human-readable description), optional tool_name, arguments, timeout_secs (default 600, max 3600), workflow_id (ignored when the server pins a workflow), task_id, wait. Returns: { decision: \"allow\"|\"deny\", message?, answered_by?, source: \"policy\"|\"human\"|\"timeout\" }, or the pending payload in suspend mode. Example: {\"agent_id\": \"swe\", \"action\": \"git push --force to main\", \"tool_name\": \"git.push\"}.",
        input_schema = ao_schema_for_type::<AgentRequestApprovalInput>()
    )]
    async fn ao_agent_request_approval(
        &self,
        params: Parameters<AgentRequestApprovalInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let agent_id = self.bound_agent_id(&input.agent_id);
        if agent_id.is_empty() {
            return structured_err("animus.agent.request_approval", "agent_id must not be empty".to_string());
        }
        let action = input.action.trim().to_string();
        if action.is_empty() {
            return structured_err("animus.agent.request_approval", "action must not be empty".to_string());
        }
        let project_root = self.default_project_root.clone();
        let tool_name = normalize_non_empty(input.tool_name);

        let runtime_config = orchestrator_core::agent_runtime_config::load_agent_runtime_config_or_default(
            std::path::Path::new(&project_root),
        );
        if let Some(policy) =
            runtime_config.agent_profile(&agent_id).and_then(|profile| profile.approval_policy.as_ref())
        {
            let subject = tool_name.as_deref().unwrap_or(&action);
            match policy.evaluate(subject) {
                ApprovalPolicyDecision::Allow => {
                    return structured_ok(
                        "animus.agent.request_approval",
                        json!({ "decision": "allow", "source": "policy" }),
                    );
                }
                ApprovalPolicyDecision::Deny => {
                    return structured_ok(
                        "animus.agent.request_approval",
                        json!({
                            "decision": "deny",
                            "source": "policy",
                            "message": "denied by the agent profile's approval_policy",
                        }),
                    );
                }
                ApprovalPolicyDecision::Ask => {}
            }
        }

        let workflow_pinned = self.workflow_pin().is_some();
        let workflow_id = self.bound_workflow_id(input.workflow_id.as_deref());
        let wait_mode = resolve_wait_mode(workflow_pinned, input.wait.as_deref(), "animus.agent.request_approval");
        let timeout_secs = effective_timeout_secs(input.timeout_secs);
        let created = match animus_runtime_shared::create_approval_interaction(
            &project_root,
            &agent_id,
            &action,
            tool_name.as_deref(),
            input.arguments,
            Some(timeout_secs),
            workflow_id.as_deref(),
            input.task_id.as_deref(),
        ) {
            Ok(record) => record,
            Err(err) => return structured_err("animus.agent.request_approval", err.to_string()),
        };
        emit_interaction_event("interaction_created", &project_root, &created);

        if wait_mode == InteractionWaitMode::Suspend {
            return suspend_pending_response("animus.agent.request_approval", &project_root, &created).await;
        }

        match wait_for_answer(&project_root, &created.id, timeout_secs).await {
            InteractionWait::Answered(record) => structured_ok(
                "animus.agent.request_approval",
                json!({
                    "id": record.id,
                    "decision": record.answer,
                    "message": record.answer_message,
                    "answered_by": record.answered_by,
                    "source": "human",
                }),
            ),
            InteractionWait::TimedOut => {
                if let Ok(Some(expired)) = animus_runtime_shared::load_interaction(&project_root, &created.id) {
                    emit_interaction_event("interaction_expired", &project_root, &expired);
                }
                structured_ok(
                    "animus.agent.request_approval",
                    json!({
                        "id": created.id,
                        "decision": "deny",
                        "source": "timeout",
                        "message": format!("no human decided within {timeout_secs}s; denied (fail closed). Do not perform the action."),
                    }),
                )
            }
            InteractionWait::Lost(message) => structured_err("animus.agent.request_approval", message),
        }
    }
}

// Human-side management tools, only registered when the server runs with
// `animus mcp serve --management`. Keeping them off the agent-injected server
// means an agent cannot answer its own question or approve its own request.
#[tool_router(router = interaction_management_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.interactions.list",
        description = "List pending agent questions and approval requests (non-blocking inbox read). Purpose: Power inbox UIs over the same store the blocking animus.agent.ask / animus.agent.request_approval tools park on. Params: all (include answered/expired, default false), agent_id filter, limit (keeps the most recent N), project_root. Returns: { count, interactions: [{ id, kind, agent_id, status, question?, action?, options?, tool_name?, created_at, answer?, answered_by?, ... }] }. Example: {\"all\": false}.",
        input_schema = ao_schema_for_type::<InteractionsListInput>()
    )]
    async fn ao_interactions_list(
        &self,
        params: Parameters<InteractionsListInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = interaction_project_root(&self.default_project_root, input.project_root);
        match animus_runtime_shared::list_interactions(
            &project_root,
            input.all.unwrap_or(false),
            input.agent_id.as_deref(),
        ) {
            Ok(mut records) => {
                if let Some(limit) = input.limit {
                    if records.len() > limit {
                        records = records.split_off(records.len() - limit);
                    }
                }
                structured_ok(
                    "animus.interactions.list",
                    json!({
                        "count": records.len(),
                        "interactions": records.iter().map(interaction_to_json).collect::<Vec<_>>(),
                    }),
                )
            }
            Err(err) => structured_err("animus.interactions.list", err.to_string()),
        }
    }

    #[tool(
        name = "animus.interactions.answer",
        description = "Answer a pending interaction (non-blocking; unblocks the agent parked on it). Purpose: Resolve a question with `text`, or an approval with `decision` (\"allow\" or \"deny\") plus optional `message`. Exactly one answered_by wins a race; later answers fail with a not-pending error. When the record carries a workflow_id and that workflow is suspended, the answer triggers the detached-runner resume with the decision as feedback; a resume failure never fails the answer and surfaces a `workflow_resume.guidance` command instead. Params: id, text?, decision?, message?, answered_by? (default \"human\"), project_root. Returns: the updated interaction record (plus `workflow_resume` when a resume was attempted). Example question: {\"id\": \"<uuid>\", \"text\": \"use the copy-table migration\"}. Example approval: {\"id\": \"<uuid>\", \"decision\": \"deny\", \"message\": \"too risky\"}.",
        input_schema = ao_schema_for_type::<InteractionsAnswerInput>()
    )]
    async fn ao_interactions_answer(
        &self,
        params: Parameters<InteractionsAnswerInput>,
    ) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let project_root = interaction_project_root(&self.default_project_root, input.project_root);
        let decision = normalize_non_empty(input.decision);
        let (allow, deny) = match decision.as_deref() {
            None => (false, false),
            Some("allow") => (true, false),
            Some("deny") => (false, true),
            Some(other) => {
                return structured_err(
                    "animus.interactions.answer",
                    format!("decision must be 'allow' or 'deny' (got '{other}')"),
                );
            }
        };
        match answer_interaction_op_with_resume(
            &project_root,
            &input.id,
            input.text.as_deref(),
            allow,
            deny,
            input.message.as_deref(),
            input.answered_by.as_deref(),
        )
        .await
        {
            Ok((record, workflow_resume)) => {
                let mut payload = interaction_to_json(&record);
                if let (Value::Object(map), Some(resume)) = (&mut payload, workflow_resume) {
                    map.insert("workflow_resume".to_string(), resume);
                }
                structured_ok("animus.interactions.answer", payload)
            }
            Err(err) => structured_err("animus.interactions.answer", err.to_string()),
        }
    }
}

#[cfg(test)]
mod interaction_tool_tests {
    use super::super::{new_ao_mcp_server, new_ao_mcp_server_with_options};
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::Value;
    use tempfile::tempdir;

    fn structured(result: &rmcp::model::CallToolResult) -> Value {
        result.structured_content.clone().expect("expected structured_content on tool result")
    }

    fn data(result: &rmcp::model::CallToolResult) -> Value {
        let payload = structured(result);
        payload.get("result").cloned().expect("structured result should include `result`")
    }

    fn init_git_repo(path: &std::path::Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git").args(args).current_dir(path).status().expect("git runs");
            assert!(status.success(), "git {:?} should succeed", args);
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "ao-test@example.com"]);
        run(&["config", "user.name", "Animus Test"]);
        std::fs::write(path.join("README.md"), "# test\n").expect("readme written");
        run(&["add", "README.md"]);
        run(&["commit", "-m", "initial"]);
    }

    async fn bootstrap_running_workflow(project_root: &str) -> orchestrator_core::OrchestratorWorkflow {
        use orchestrator_core::{services::ServiceHub, FileServiceHub, Priority, TaskCreateInput, TaskType};
        let hub: std::sync::Arc<dyn ServiceHub> =
            std::sync::Arc::new(FileServiceHub::new(project_root).expect("file service hub"));
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "suspend interaction".to_string(),
                description: "suspend-mode pause test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task created");
        hub.workflows()
            .run(orchestrator_core::WorkflowRunInput::for_task(task.id, None))
            .await
            .expect("workflow started")
    }

    fn with_isolated_scope<F: std::future::Future<Output = ()>>(body: F) {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime")
            .block_on(body);
    }

    #[tokio::test]
    async fn ao_mcp_serve_router_exposes_interaction_tools() {
        let server = new_ao_mcp_server("/tmp/project");
        let names: Vec<String> = server.tool_router.list_all().into_iter().map(|tool| tool.name.to_string()).collect();
        for expected in ["animus.agent.ask", "animus.agent.request_approval"] {
            assert!(names.contains(&expected.to_string()), "router missing {expected}; saw: {names:?}");
        }
        for management_only in ["animus.interactions.list", "animus.interactions.answer"] {
            assert!(
                !names.contains(&management_only.to_string()),
                "agent-facing router must not expose {management_only}"
            );
        }

        let management = new_ao_mcp_server_with_options("/tmp/project", true, None, None);
        let names: Vec<String> =
            management.tool_router.list_all().into_iter().map(|tool| tool.name.to_string()).collect();
        for expected in [
            "animus.agent.ask",
            "animus.agent.request_approval",
            "animus.interactions.list",
            "animus.interactions.answer",
        ] {
            assert!(names.contains(&expected.to_string()), "management router missing {expected}; saw: {names:?}");
        }
    }

    #[test]
    fn ask_blocks_until_a_concurrent_answer_arrives() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let answer_root = project_root.clone();
            let answerer = tokio::spawn(async move {
                loop {
                    let pending =
                        animus_runtime_shared::list_interactions(&answer_root, false, None).expect("list interactions");
                    if let Some(record) = pending.first() {
                        animus_runtime_shared::answer_interaction(
                            &answer_root,
                            &record.id,
                            "use the copy table",
                            None,
                            Some("sami"),
                        )
                        .expect("answer interaction");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: "Migrate in place or copy table?".to_string(),
                    options: Some(vec!["in place".to_string(), "copy".to_string()]),
                    timeout_secs: Some(10),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("ask should not error");
            answerer.await.expect("answerer task");

            assert_ne!(result.is_error, Some(true), "ask must succeed once answered");
            let payload = data(&result);
            assert_eq!(payload.pointer("/answer").and_then(Value::as_str), Some("use the copy table"));
            assert_eq!(payload.pointer("/answered_by").and_then(Value::as_str), Some("sami"));
        });
    }

    #[test]
    fn ask_times_out_with_structured_error_and_expires_the_record() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: "Anyone home?".to_string(),
                    options: None,
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("ask should produce a structured result");
            assert_eq!(result.is_error, Some(true), "timeout must surface as a tool error");
            let payload = structured(&result);
            assert_eq!(payload.pointer("/timed_out").and_then(Value::as_bool), Some(true));
            assert!(payload
                .pointer("/error")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("best judgment")));

            let id = payload.pointer("/interaction_id").and_then(Value::as_str).expect("interaction_id").to_string();
            let record =
                animus_runtime_shared::load_interaction(&project_root, &id).expect("load").expect("record exists");
            assert_eq!(record.status, animus_runtime_shared::InteractionStatus::Expired);
        });
    }

    #[test]
    fn request_approval_policy_short_circuits_without_escalating() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            std::fs::create_dir_all(project.path().join(".animus")).expect("create .animus");
            std::fs::write(
                project.path().join(".animus").join("workflows.yaml"),
                r#"
agents:
  swe:
    system_prompt: Build the change.
    approval_policy:
      auto_allow: ["cargo *"]
      auto_deny: ["git.push*"]
      default: deny
phases:
  impl:
    mode: agent
    agent: swe
"#,
            )
            .expect("write workflows.yaml");
            let server = new_ao_mcp_server(&project_root);

            let allowed = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    agent_id: "swe".to_string(),
                    action: "run the test suite".to_string(),
                    tool_name: Some("cargo test".to_string()),
                    arguments: None,
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("approval should not error");
            let payload = data(&allowed);
            assert_eq!(payload.pointer("/decision").and_then(Value::as_str), Some("allow"));
            assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("policy"));

            let denied = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    agent_id: "swe".to_string(),
                    action: "force push".to_string(),
                    tool_name: Some("git.push --force".to_string()),
                    arguments: None,
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("approval should not error");
            let payload = data(&denied);
            assert_eq!(payload.pointer("/decision").and_then(Value::as_str), Some("deny"));
            assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("policy"));

            let default_denied = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    agent_id: "swe".to_string(),
                    action: "anything else".to_string(),
                    tool_name: None,
                    arguments: None,
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("approval should not error");
            let payload = data(&default_denied);
            assert_eq!(payload.pointer("/decision").and_then(Value::as_str), Some("deny"));
            assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("policy"));

            let pending = animus_runtime_shared::list_interactions(&project_root, true, None).expect("list");
            assert!(pending.is_empty(), "policy short-circuits must not write pending interactions");
        });
    }

    #[test]
    fn request_approval_env_pinned_identity_overrides_payload() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _agent = protocol::test_utils::EnvVarGuard::set(ANIMUS_MCP_AGENT_ID_ENV, Some("restricted"));
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread runtime").block_on(
            async {
                let project = tempdir().expect("tempdir");
                let project_root = project.path().to_string_lossy().to_string();
                std::fs::create_dir_all(project.path().join(".animus")).expect("create .animus");
                std::fs::write(
                    project.path().join(".animus").join("workflows.yaml"),
                    r#"
agents:
  restricted:
    system_prompt: Restricted agent.
    approval_policy:
      default: deny
  permissive:
    system_prompt: Permissive agent.
    approval_policy:
      default: allow
phases:
  impl:
    mode: agent
    agent: restricted
"#,
                )
                .expect("write workflows.yaml");
                let server = new_ao_mcp_server(&project_root);

                let result = server
                    .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                        agent_id: "permissive".to_string(),
                        action: "delete everything".to_string(),
                        tool_name: None,
                        arguments: None,
                        timeout_secs: Some(1),
                        workflow_id: None,
                        task_id: None,
                        wait: None,
                    }))
                    .await
                    .expect("approval should not error");
                let payload = data(&result);
                assert_eq!(
                    payload.pointer("/decision").and_then(Value::as_str),
                    Some("deny"),
                    "env-pinned identity must beat the payload's claimed profile"
                );
                assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("policy"));
            },
        );
    }

    #[test]
    fn request_approval_cli_pinned_identity_overrides_payload_and_env() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _agent = protocol::test_utils::EnvVarGuard::set(ANIMUS_MCP_AGENT_ID_ENV, Some("permissive"));
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread runtime").block_on(
            async {
                let project = tempdir().expect("tempdir");
                let project_root = project.path().to_string_lossy().to_string();
                std::fs::create_dir_all(project.path().join(".animus")).expect("create .animus");
                std::fs::write(
                    project.path().join(".animus").join("workflows.yaml"),
                    r#"
agents:
  restricted:
    system_prompt: Restricted agent.
    approval_policy:
      default: deny
  permissive:
    system_prompt: Permissive agent.
    approval_policy:
      default: allow
phases:
  impl:
    mode: agent
    agent: restricted
"#,
                )
                .expect("write workflows.yaml");
                let server = new_ao_mcp_server_with_options(&project_root, false, Some("restricted".to_string()), None);

                let result = server
                    .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                        agent_id: "permissive".to_string(),
                        action: "delete everything".to_string(),
                        tool_name: None,
                        arguments: None,
                        timeout_secs: Some(1),
                        workflow_id: None,
                        task_id: None,
                        wait: None,
                    }))
                    .await
                    .expect("approval should not error");
                let payload = data(&result);
                assert_eq!(
                    payload.pointer("/decision").and_then(Value::as_str),
                    Some("deny"),
                    "the --agent-id pin must beat both the env pin and the payload"
                );
                assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("policy"));
            },
        );
    }

    #[test]
    fn request_approval_escalates_and_times_out_denied() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    agent_id: "swe".to_string(),
                    action: "drop the production database".to_string(),
                    tool_name: Some("Bash".to_string()),
                    arguments: Some(serde_json::json!({ "command": "dropdb prod" })),
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("approval should not error");
            assert_ne!(result.is_error, Some(true), "timeout deny is a structured success payload");
            let payload = data(&result);
            assert_eq!(payload.pointer("/decision").and_then(Value::as_str), Some("deny"));
            assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("timeout"));
        });
    }

    #[test]
    fn request_approval_returns_concurrent_human_decision() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let answer_root = project_root.clone();
            let answer_server = server.clone();
            let answerer = tokio::spawn(async move {
                loop {
                    let pending =
                        animus_runtime_shared::list_interactions(&answer_root, false, None).expect("list pending");
                    if let Some(record) = pending.first() {
                        let result = answer_server
                            .ao_interactions_answer(Parameters(InteractionsAnswerInput {
                                id: record.id.clone(),
                                text: None,
                                decision: Some("allow".to_string()),
                                message: Some("go ahead".to_string()),
                                answered_by: Some("sami".to_string()),
                                project_root: Some(answer_root.clone()),
                            }))
                            .await
                            .expect("answer should not error");
                        assert_ne!(result.is_error, Some(true), "answer tool must succeed");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    agent_id: "swe".to_string(),
                    action: "rotate the API keys".to_string(),
                    tool_name: None,
                    arguments: None,
                    timeout_secs: Some(10),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("approval should not error");
            answerer.await.expect("answerer task");

            let payload = data(&result);
            assert_eq!(payload.pointer("/decision").and_then(Value::as_str), Some("allow"));
            assert_eq!(payload.pointer("/message").and_then(Value::as_str), Some("go ahead"));
            assert_eq!(payload.pointer("/answered_by").and_then(Value::as_str), Some("sami"));
            assert_eq!(payload.pointer("/source").and_then(Value::as_str), Some("human"));
        });
    }

    #[test]
    fn interactions_list_and_answer_tools_roundtrip() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let created = animus_runtime_shared::create_question_interaction(
                &project_root,
                "swe",
                "Which migration?",
                &[],
                None,
                None,
                None,
            )
            .expect("create question");

            let listed = server
                .ao_interactions_list(Parameters(InteractionsListInput {
                    all: None,
                    agent_id: None,
                    limit: None,
                    project_root: None,
                }))
                .await
                .expect("list should not error");
            let payload = data(&listed);
            assert_eq!(payload.pointer("/count").and_then(Value::as_u64), Some(1));
            assert_eq!(payload.pointer("/interactions/0/id").and_then(Value::as_str), Some(created.id.as_str()));

            let bad_decision = server
                .ao_interactions_answer(Parameters(InteractionsAnswerInput {
                    id: created.id.clone(),
                    text: None,
                    decision: Some("maybe".to_string()),
                    message: None,
                    answered_by: None,
                    project_root: None,
                }))
                .await
                .expect("answer should produce a structured result");
            assert_eq!(bad_decision.is_error, Some(true));

            let answered = server
                .ao_interactions_answer(Parameters(InteractionsAnswerInput {
                    id: created.id.clone(),
                    text: Some("copy table".to_string()),
                    decision: None,
                    message: None,
                    answered_by: None,
                    project_root: None,
                }))
                .await
                .expect("answer should not error");
            let payload = data(&answered);
            assert_eq!(payload.pointer("/status").and_then(Value::as_str), Some("answered"));
            assert_eq!(payload.pointer("/answer").and_then(Value::as_str), Some("copy table"));
            assert_eq!(payload.pointer("/answered_by").and_then(Value::as_str), Some("human"));

            let pending_after = server
                .ao_interactions_list(Parameters(InteractionsListInput {
                    all: None,
                    agent_id: None,
                    limit: None,
                    project_root: None,
                }))
                .await
                .expect("list should not error");
            assert_eq!(data(&pending_after).pointer("/count").and_then(Value::as_u64), Some(0));

            let all_after = server
                .ao_interactions_list(Parameters(InteractionsListInput {
                    all: Some(true),
                    agent_id: None,
                    limit: None,
                    project_root: None,
                }))
                .await
                .expect("list should not error");
            assert_eq!(data(&all_after).pointer("/count").and_then(Value::as_u64), Some(1));
        });
    }

    #[test]
    fn wait_mode_defaults_and_overrides() {
        use InteractionWaitMode::{Block, Suspend};
        assert_eq!(resolve_wait_mode(false, None, "t"), Block, "unpinned default is block");
        assert_eq!(resolve_wait_mode(true, None, "t"), Suspend, "workflow pin flips the default to suspend");
        assert_eq!(resolve_wait_mode(true, Some("block"), "t"), Block, "suspend -> block override is honoured");
        assert_eq!(resolve_wait_mode(true, Some("SUSPEND"), "t"), Suspend);
        assert_eq!(resolve_wait_mode(false, Some("suspend"), "t"), Block, "block -> suspend must be ignored");
        assert_eq!(resolve_wait_mode(false, Some("bogus"), "t"), Block, "unknown mode falls back to the default");
        assert_eq!(resolve_wait_mode(true, Some("bogus"), "t"), Suspend);
    }

    #[test]
    fn ask_suspend_mode_returns_pending_and_pauses_the_pinned_workflow() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            init_git_repo(project.path());
            let project_root = project.path().to_string_lossy().to_string();
            let workflow = bootstrap_running_workflow(&project_root).await;
            assert_eq!(workflow.status, orchestrator_core::WorkflowStatus::Running);

            let server = new_ao_mcp_server_with_options(&project_root, false, None, Some(workflow.id.clone()));
            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: "Which approach?".to_string(),
                    options: None,
                    timeout_secs: Some(600),
                    // Payload workflow_id must be overridden by the pin.
                    workflow_id: Some("wf-other".to_string()),
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("suspend ask should not error");
            assert_ne!(result.is_error, Some(true), "suspend mode returns a structured success");
            let payload = data(&result);
            assert_eq!(payload.pointer("/status").and_then(Value::as_str), Some("pending"));
            assert_eq!(payload.pointer("/workflow_id").and_then(Value::as_str), Some(workflow.id.as_str()));
            assert_eq!(payload.pointer("/workflow_paused").and_then(Value::as_bool), Some(true));
            assert!(payload
                .pointer("/instruction")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("end") && text.contains("turn")));

            let interaction_id =
                payload.pointer("/interaction_id").and_then(Value::as_str).expect("interaction_id").to_string();
            let record = animus_runtime_shared::load_interaction(&project_root, &interaction_id)
                .expect("load")
                .expect("record exists");
            assert_eq!(record.status, InteractionStatus::Pending);
            assert_eq!(record.workflow_id.as_deref(), Some(workflow.id.as_str()), "record carries the pinned id");

            let hub: std::sync::Arc<dyn orchestrator_core::services::ServiceHub> =
                std::sync::Arc::new(orchestrator_core::FileServiceHub::new(&project_root).expect("file service hub"));
            let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow reloads");
            assert_eq!(reloaded.status, orchestrator_core::WorkflowStatus::Paused, "suspend must pause the workflow");
        });
    }

    #[test]
    fn request_approval_suspend_env_pin_returns_pending() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let _workflow = protocol::test_utils::EnvVarGuard::set(ANIMUS_MCP_WORKFLOW_ID_ENV, Some("wf-env-pin"));
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread runtime").block_on(
            async {
                let project = tempdir().expect("tempdir");
                let project_root = project.path().to_string_lossy().to_string();
                let server = new_ao_mcp_server(&project_root);

                let result = server
                    .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                        agent_id: "swe".to_string(),
                        action: "rotate the API keys".to_string(),
                        tool_name: None,
                        arguments: None,
                        timeout_secs: Some(600),
                        workflow_id: None,
                        task_id: None,
                        wait: None,
                    }))
                    .await
                    .expect("approval should not error");
                assert_ne!(result.is_error, Some(true));
                let payload = data(&result);
                assert_eq!(payload.pointer("/status").and_then(Value::as_str), Some("pending"));
                // The workflow does not exist, so the pause is best-effort
                // and reported as not applied.
                assert_eq!(payload.pointer("/workflow_paused").and_then(Value::as_bool), Some(false));

                let interaction_id =
                    payload.pointer("/interaction_id").and_then(Value::as_str).expect("interaction_id").to_string();
                let record = animus_runtime_shared::load_interaction(&project_root, &interaction_id)
                    .expect("load")
                    .expect("record exists");
                assert_eq!(record.workflow_id.as_deref(), Some("wf-env-pin"));
            },
        );
    }

    #[test]
    fn pinned_server_honours_suspend_to_block_override() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server_with_options(&project_root, false, None, Some("wf-pinned".to_string()));

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: "Anyone home?".to_string(),
                    options: None,
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: Some("block".to_string()),
                }))
                .await
                .expect("ask should produce a structured result");
            assert_eq!(result.is_error, Some(true), "wait=block on a pinned server must park and time out");
            let payload = structured(&result);
            assert_eq!(payload.pointer("/timed_out").and_then(Value::as_bool), Some(true));
        });
    }

    #[test]
    fn unpinned_server_ignores_suspend_request_and_blocks() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: "Anyone home?".to_string(),
                    options: None,
                    timeout_secs: Some(1),
                    // Payload workflow_id alone does not enable suspend.
                    workflow_id: Some("wf-unpinned".to_string()),
                    task_id: None,
                    wait: Some("suspend".to_string()),
                }))
                .await
                .expect("ask should produce a structured result");
            assert_eq!(result.is_error, Some(true), "block -> suspend must be ignored; the call parks and times out");
            let payload = structured(&result);
            assert_eq!(payload.pointer("/timed_out").and_then(Value::as_bool), Some(true));
        });
    }

    // Mark->pause race (codex round-2 P2): an answer that lands before the
    // suspend path finishes pausing skipped its own resume, so the suspend
    // path must detect the answered record after pausing and run the resume
    // itself (here it fails on the missing workflow_runner plugin and
    // surfaces the manual-resume guidance instead of stranding silently).
    #[test]
    fn suspend_path_recovers_answer_that_raced_the_pause() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            init_git_repo(project.path());
            let project_root = project.path().to_string_lossy().to_string();
            let workflow = bootstrap_running_workflow(&project_root).await;

            let created = animus_runtime_shared::create_question_interaction(
                &project_root,
                "swe",
                "Quick one?",
                &[],
                None,
                Some(&workflow.id),
                None,
            )
            .expect("create question");
            // Simulate the racing answer arriving before the suspend path
            // marks + pauses.
            animus_runtime_shared::answer_interaction(&project_root, &created.id, "yes", None, Some("sami"))
                .expect("racing answer");

            let result =
                suspend_pending_response("animus.agent.ask", &project_root, &created).await.expect("suspend response");
            let payload = data(&result);
            assert_eq!(payload.pointer("/workflow_paused").and_then(Value::as_bool), Some(true));
            let resume = payload.pointer("/workflow_resume").expect("late resume attempt reported");
            assert_eq!(resume.pointer("/resumed").and_then(Value::as_bool), Some(false));
            assert_eq!(
                resume.pointer("/guidance").and_then(Value::as_str),
                Some(format!("animus workflow resume {}", workflow.id).as_str())
            );
        });
    }
}
