use super::*;
use crate::services::runtime::runtime_agent::interactions::{
    answer_interaction_op_with_resume, emit_interaction_event, pause_workflow_for_suspended_interaction,
    resume_workflow_for_answered_interaction, AnswerOptions,
};
use animus_runtime_shared::{InteractionKind, InteractionQuestion, InteractionRecord, InteractionStatus};
use orchestrator_config::agent_runtime_config::ApprovalPolicyDecision;
use std::collections::BTreeMap;
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
    /// Single-question convenience. Optional when `questions[]` is supplied;
    /// at least one of `question` / `questions` must be present.
    #[serde(default)]
    pub(super) question: Option<String>,
    #[serde(default)]
    pub(super) options: Option<Vec<String>>,
    /// Structured multi-question / multi-select / described-option form
    /// (parity with claude's native AskUserQuestion channel). When present,
    /// the flat `question`/`options` fields are ignored and the answer comes
    /// back as `{ answers: { <question text>: <label | [labels] | free text> },
    /// response?, answer }`.
    #[serde(default)]
    pub(super) questions: Option<Vec<AskQuestionInput>>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) workflow_id: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) wait: Option<String>,
}

/// One structured question accepted by `animus.agent.ask`'s `questions[]`
/// parity path. Mirrors `animus_runtime_shared::InteractionQuestion` but lives
/// in the CLI crate so it can derive `JsonSchema` without pulling schemars
/// into the dependency-light shared crate. Converted via [`From`].
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AskQuestionInput {
    pub(super) question: String,
    #[serde(default)]
    pub(super) header: Option<String>,
    #[serde(default)]
    pub(super) options: Vec<AskQuestionOptionInput>,
    #[serde(default, alias = "multiSelect")]
    pub(super) multi_select: bool,
}

/// One choice inside an [`AskQuestionInput`].
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AskQuestionOptionInput {
    pub(super) label: String,
    #[serde(default)]
    pub(super) description: Option<String>,
}

impl From<AskQuestionInput> for InteractionQuestion {
    fn from(input: AskQuestionInput) -> Self {
        InteractionQuestion {
            question: input.question,
            header: input.header,
            options: input
                .options
                .into_iter()
                .map(|option| animus_runtime_shared::InteractionQuestionOption {
                    label: option.label,
                    description: option.description,
                })
                .collect(),
            multi_select: input.multi_select,
        }
    }
}

// The input accepts BOTH shapes:
// - Voluntary agent escalations: { agent_id, action, tool_name?, arguments?, ... }
// - The claude CLI's `--permission-prompt-tool` contract, which invokes this
//   tool with exactly { tool_name, input, tool_use_id? } for every gated tool
//   call (including the native `AskUserQuestion` tool). For that shape the
//   identity comes from the server pin and `action` is derived.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(super) struct AgentRequestApprovalInput {
    #[serde(default)]
    pub(super) agent_id: Option<String>,
    #[serde(default)]
    pub(super) action: Option<String>,
    #[serde(default)]
    pub(super) tool_name: Option<String>,
    /// The gated tool's input as passed by the SDK permission-prompt-tool
    /// contract; echoed back verbatim as `updatedInput` on allow.
    #[serde(default)]
    pub(super) input: Option<Value>,
    /// The SDK's tool-use request id (accepted for contract completeness).
    #[serde(default)]
    pub(super) tool_use_id: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<Value>,
    /// Optional SDK `PermissionUpdate[]` suggestions; stored on the record and
    /// echoed back as `updatedPermissions` when answered with remember.
    #[serde(default)]
    pub(super) suggestions: Option<Value>,
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
    fn bound_agent_id(&self, payload_agent_id: Option<&str>) -> String {
        self.pinned_agent_id
            .clone()
            .or_else(|| {
                std::env::var(ANIMUS_MCP_AGENT_ID_ENV)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| payload_agent_id.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned))
            // Native prompt-tool calls carry no agent identity at all; fall
            // back to a generic id so the record is still attributable.
            .unwrap_or_else(|| "agent".to_string())
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
//
// `native` marks calls arriving through the claude CLI's
// `--permission-prompt-tool` contract: the CLI only understands
// `behavior: allow|deny` JSON, so the suspend response is emitted as a deny
// whose message carries the end-your-turn instruction (suspend replaces the
// SDK `defer` decision; the session resumes with the answer as feedback, not
// via this tool result).
async fn suspend_pending_response(
    tool_name: &str,
    project_root: &str,
    record: &InteractionRecord,
    native: bool,
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
    if native {
        let message = format!(
            "{SUSPEND_INSTRUCTION} Interaction {} is pending in the Animus inbox; the session resumes with the \
             human's answer delivered as feedback.",
            record.id
        );
        return Ok(CallToolResult::structured(merge_sdk_fields(tool_name, payload, sdk_deny_fields(&message))));
    }
    structured_ok(tool_name, payload)
}

// --- SDK permission-prompt-tool response shapes -------------------------
//
// The claude CLI parses the FIRST text content block of this tool's result
// as JSON and validates it against the SDK permission-result schema:
//   { "behavior": "allow", "updatedInput": { ... }, "updatedPermissions"?: [...] }
//   { "behavior": "deny", "message": "..." , "interrupt"?: bool }
// (verified against claude CLI v2.1.175; unknown keys are stripped, so the
// legacy `{ tool, result: { decision, ... } }` envelope rides alongside).
// `CallToolResult::structured` serializes the payload into exactly one text
// block, which is what the CLI requires.

/// Merge the SDK top-level fields into the legacy `{ tool, result }` envelope.
fn merge_sdk_fields(tool_name: &str, legacy_result: Value, sdk_fields: Value) -> Value {
    let mut payload = json!({ "tool": tool_name, "result": legacy_result });
    if let (Value::Object(map), Value::Object(fields)) = (&mut payload, sdk_fields) {
        for (key, value) in fields {
            map.insert(key, value);
        }
    }
    payload
}

fn sdk_allow_fields(updated_input: Value, updated_permissions: Option<Value>) -> Value {
    let mut fields = json!({ "behavior": "allow", "updatedInput": updated_input });
    if let (Value::Object(map), Some(permissions)) = (&mut fields, updated_permissions) {
        map.insert("updatedPermissions".to_string(), permissions);
    }
    fields
}

fn sdk_deny_fields(message: &str) -> Value {
    json!({ "behavior": "deny", "message": message })
}

/// SDK `updatedInput` for an answered native `AskUserQuestion` interaction:
/// `{ questions: <original array>, answers: { <question text>: <label | [labels] | free text> }, response? }`.
fn ask_user_question_updated_input(record: &InteractionRecord) -> Value {
    let questions = record
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get("questions"))
        .cloned()
        .unwrap_or_else(|| serde_json::to_value(&record.questions).unwrap_or_else(|_| json!([])));
    let mut updated = json!({
        "questions": questions,
        "answers": record.answers.clone().unwrap_or_default(),
    });
    if let (Value::Object(map), Some(response)) = (&mut updated, record.response.as_deref()) {
        map.insert("response".to_string(), json!(response));
    }
    updated
}

/// Answer payload for an `animus.agent.ask` call that supplied structured
/// `questions[]`. Unlike the native AskUserQuestion channel, this does NOT
/// emit the SDK `behavior/updatedInput` envelope — codex/gemini/opencode read
/// a plain `{ answers, response?, answer }` shape. The flat `answer` string
/// is the readable join so agents reading `.answer` still get something
/// sensible.
fn ask_structured_answered_payload(record: &InteractionRecord) -> Value {
    json!({
        "id": record.id,
        "answers": record.answers.clone().unwrap_or_default(),
        "response": record.response,
        "answer": record.answer,
        "answered_by": record.answered_by,
        "answer_message": record.answer_message,
    })
}

/// Build the full response payload for an answered interaction parked on by
/// the blocking `animus.agent.request_approval` tool.
fn sdk_answered_payload(tool_name: &str, record: &InteractionRecord) -> Value {
    if record.kind == InteractionKind::Question && !record.questions.is_empty() {
        let legacy = json!({
            "id": record.id,
            "answer": record.answer,
            "answers": record.answers,
            "response": record.response,
            "answered_by": record.answered_by,
            "source": "human",
        });
        return merge_sdk_fields(tool_name, legacy, sdk_allow_fields(ask_user_question_updated_input(record), None));
    }
    let legacy = json!({
        "id": record.id,
        "decision": record.answer,
        "message": record.answer_message,
        "answered_by": record.answered_by,
        "source": "human",
    });
    if record.answer.as_deref() == Some(animus_runtime_shared::INTERACTION_ANSWER_ALLOW) {
        let updated_input =
            record.updated_input.clone().or_else(|| record.arguments.clone()).unwrap_or_else(|| json!({}));
        merge_sdk_fields(tool_name, legacy, sdk_allow_fields(updated_input, record.updated_permissions.clone()))
    } else {
        let message = record
            .answer_message
            .clone()
            .unwrap_or_else(|| format!("denied by {}", record.answered_by.as_deref().unwrap_or("human")));
        merge_sdk_fields(tool_name, legacy, sdk_deny_fields(&message))
    }
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
    /// Structured per-question answers for native AskUserQuestion records,
    /// keyed by exact question text (string, or array of labels for
    /// multi-select).
    #[serde(default)]
    pub(super) answers: Option<BTreeMap<String, Value>>,
    /// Freeform reply that is not an answer to any specific question.
    #[serde(default)]
    pub(super) response: Option<String>,
    /// Operator-modified tool input echoed as `updatedInput` on an allowed
    /// approval (defaults to the original input).
    #[serde(default)]
    pub(super) updated_input: Option<Value>,
    /// Explicit SDK `PermissionUpdate[]` echoed as `updatedPermissions` on an
    /// allowed approval; wins over `remember`.
    #[serde(default)]
    pub(super) updated_permissions: Option<Value>,
    /// Echo the record's localSettings-destination permission suggestions
    /// back as `updatedPermissions` (allowed approvals only).
    #[serde(default)]
    pub(super) remember: Option<bool>,
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

// --- LLM auto-approve evaluator ----------------------------------------
//
// When an agent's approval_policy has `default: llm`, a gated tool call is
// judged by a model instead of escalating to a human. The judge runs as a
// one-shot provider session with NO MCP servers (so it has no tools and
// cannot recurse back into request_approval) and NO permission gating; it
// must answer with a single JSON verdict. ANY failure (no model/tool, session
// error, unparseable output) returns None so the caller falls back to manual
// human escalation — an LLM outage never silently auto-allows or hard-denies.

const APPROVAL_JUDGE_TIMEOUT_SECS: u64 = 60;
const APPROVAL_JUDGE_MAX_ARG_CHARS: usize = 4000;

fn build_judge_system_prompt(extra_instructions: Option<&str>) -> String {
    let mut prompt = String::from(
        "You are an approval gate for an autonomous coding agent. You decide whether a single requested \
         action is safe to ALLOW or must be DENIED. Be conservative and fail safe: DENY irreversible, \
         destructive, or exfiltrating actions (deleting data, dropping databases, writing to production, \
         force-pushing or rewriting shared history, disabling security controls, sending data or messages to \
         external parties, leaking secrets/credentials) unless the request is clearly routine and safe. ALLOW \
         ordinary local development actions (reading files, running tests, building, editing code in the \
         working tree). When in doubt, DENY. Respond with ONLY a single JSON object and nothing else: \
         {\"decision\":\"allow\"|\"deny\",\"reason\":\"<one short sentence>\"}.",
    );
    if let Some(extra) = extra_instructions.map(str::trim).filter(|value| !value.is_empty()) {
        prompt.push_str("\n\nAdditional operator policy for this agent (overrides the defaults above on conflict):\n");
        prompt.push_str(extra);
    }
    prompt
}

fn build_judge_user_prompt(action: &str, tool_name: Option<&str>, arguments: Option<&Value>) -> String {
    let mut prompt = format!("Requested action: {action}\n");
    if let Some(tool) = tool_name {
        prompt.push_str(&format!("Tool: {tool}\n"));
    }
    if let Some(args) = arguments {
        let mut rendered = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
        if rendered.len() > APPROVAL_JUDGE_MAX_ARG_CHARS {
            // Truncate on a char boundary (arbitrary JSON may carry non-ASCII;
            // a byte-index truncate would panic mid-codepoint).
            let cut = (0..=APPROVAL_JUDGE_MAX_ARG_CHARS).rev().find(|&i| rendered.is_char_boundary(i)).unwrap_or(0);
            rendered.truncate(cut);
            rendered.push_str("\n…(truncated)");
        }
        prompt.push_str(&format!("Tool input:\n{rendered}\n"));
    }
    prompt.push_str("\nReturn your JSON verdict now.");
    prompt
}

/// Extract `{"decision":"allow|deny","reason":...}` from possibly-fenced or
/// prose-wrapped model output. Returns `(allow, reason)`.
fn parse_judge_verdict(text: &str) -> Option<(bool, String)> {
    // Scan for balanced top-level JSON objects; models sometimes prefix
    // reasoning before the JSON, so try the last one containing a decision.
    let bytes = text.as_bytes();
    let mut candidates: Vec<&str> = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s) = start.take() {
                            candidates.push(&text[s..=i]);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for candidate in candidates.iter().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            if let Some(decision) = value.get("decision").and_then(Value::as_str) {
                let reason = value
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| "no reason given".to_string());
                return match decision.trim().to_ascii_lowercase().as_str() {
                    "allow" | "approve" | "approved" | "yes" => Some((true, reason)),
                    "deny" | "reject" | "rejected" | "no" => Some((false, reason)),
                    _ => None,
                };
            }
        }
    }
    None
}

async fn evaluate_approval_with_llm(
    project_root: &str,
    judge_tool: &str,
    judge_model: &str,
    action: &str,
    tool_name: Option<&str>,
    arguments: Option<&Value>,
    extra_instructions: Option<&str>,
) -> Option<(bool, String)> {
    use crate::services::runtime::runtime_agent::provider_client;
    use animus_session_backend::session::{SessionEvent, SessionRequest};

    let request = SessionRequest {
        tool: judge_tool.to_string(),
        model: judge_model.to_string(),
        prompt: build_judge_user_prompt(action, tool_name, arguments),
        cwd: std::path::PathBuf::from(project_root),
        project_root: Some(std::path::PathBuf::from(project_root)),
        // No MCP endpoint -> the judge has no tools and cannot recurse into
        // request_approval.
        mcp_endpoint: None,
        permission_mode: None,
        timeout_secs: Some(APPROVAL_JUDGE_TIMEOUT_SECS),
        env_vars: Vec::new(),
        extras: json!({ "system_prompt": build_judge_system_prompt(extra_instructions) }),
    };

    let mut run = match provider_client::start_session(std::path::Path::new(project_root), request).await {
        Ok(run) => run,
        Err(err) => {
            tracing::warn!(error = %err, "approval LLM evaluator: session start failed; falling back to manual escalation");
            return None;
        }
    };

    let mut final_text = String::new();
    let mut deltas = String::new();
    while let Some(event) = run.events.recv().await {
        match event {
            SessionEvent::FinalText { text } => final_text = text,
            SessionEvent::TextDelta { text } => deltas.push_str(&text),
            SessionEvent::Error { message, recoverable } => {
                if !recoverable {
                    tracing::warn!(
                        message,
                        "approval LLM evaluator: provider error; falling back to manual escalation"
                    );
                }
            }
            SessionEvent::Finished { .. } => break,
            _ => {}
        }
    }

    let text = if !final_text.trim().is_empty() { final_text } else { deltas };
    let verdict = parse_judge_verdict(&text);
    if verdict.is_none() {
        tracing::warn!("approval LLM evaluator: could not parse a verdict; falling back to manual escalation");
    }
    verdict
}

/// LLM autopilot config for an agent whose `approval_policy.default` is `llm`:
/// `(judge_tool, judge_model, evaluator_instructions)`. Returns `None` unless
/// the agent is in LLM mode AND a tool + model can be resolved (else the caller
/// escalates to a human, fail safe). The judge model is `evaluator_model` or
/// the agent's own model; the tool is the agent's own provider tool.
fn resolve_llm_autopilot(
    profile: Option<&orchestrator_config::agent_runtime_config::AgentProfile>,
) -> Option<(String, String, Option<String>)> {
    let profile = profile?;
    let policy = profile.approval_policy.as_ref()?;
    if policy.default != orchestrator_config::agent_runtime_config::ApprovalPolicyDefault::Llm {
        return None;
    }
    let tool = profile.tool.clone()?;
    let model = policy.evaluator_model.clone().or_else(|| profile.model.clone())?;
    Some((tool, model, policy.evaluator_instructions.clone()))
}

/// Extract `{"answer": "..."}` (or a bare `{"answer": ...}` coerced to string)
/// from possibly-fenced or prose-wrapped model output.
fn parse_answer_text(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut candidates: Vec<&str> = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s) = start.take() {
                            candidates.push(&text[s..=i]);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for candidate in candidates.iter().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            if let Some(answer) = value.get("answer") {
                let rendered = match answer {
                    Value::String(s) => s.trim().to_string(),
                    other => other.to_string(),
                };
                if !rendered.is_empty() {
                    return Some(rendered);
                }
            }
        }
    }
    None
}

/// LLM autopilot for `animus.agent.ask`: answer a flat question on the human's
/// behalf. Returns `Some(answer)` or `None` (caller escalates to a human). Runs
/// as a one-shot provider session with no MCP endpoint (no tools).
async fn answer_question_with_llm(
    project_root: &str,
    judge_tool: &str,
    judge_model: &str,
    question: &str,
    options: &[String],
    extra_instructions: Option<&str>,
) -> Option<String> {
    use crate::services::runtime::runtime_agent::provider_client;
    use animus_session_backend::session::{SessionEvent, SessionRequest};

    let mut system = String::from(
        "You are answering, on behalf of a human operator, a clarifying question from an autonomous coding \
         agent. Answer concisely and decisively using the operator's likely intent and the safest sensible \
         default. If a list of options is given, choose EXACTLY ONE option verbatim. Respond with ONLY a single \
         JSON object and nothing else: {\"answer\":\"<your answer>\"}.",
    );
    if let Some(extra) = extra_instructions.map(str::trim).filter(|value| !value.is_empty()) {
        system.push_str("\n\nOperator guidance:\n");
        system.push_str(extra);
    }
    let mut user = format!("Question: {question}\n");
    if !options.is_empty() {
        user.push_str("Options (choose exactly one, verbatim):\n");
        for option in options {
            user.push_str(&format!("- {option}\n"));
        }
    }
    user.push_str("\nReturn your JSON answer now.");

    let request = SessionRequest {
        tool: judge_tool.to_string(),
        model: judge_model.to_string(),
        prompt: user,
        cwd: std::path::PathBuf::from(project_root),
        project_root: Some(std::path::PathBuf::from(project_root)),
        mcp_endpoint: None,
        permission_mode: None,
        timeout_secs: Some(APPROVAL_JUDGE_TIMEOUT_SECS),
        env_vars: Vec::new(),
        extras: json!({ "system_prompt": system }),
    };

    let mut run = match provider_client::start_session(std::path::Path::new(project_root), request).await {
        Ok(run) => run,
        Err(err) => {
            tracing::warn!(error = %err, "ask LLM autopilot: session start failed; escalating to a human");
            return None;
        }
    };
    let mut final_text = String::new();
    let mut deltas = String::new();
    while let Some(event) = run.events.recv().await {
        match event {
            SessionEvent::FinalText { text } => final_text = text,
            SessionEvent::TextDelta { text } => deltas.push_str(&text),
            SessionEvent::Finished { .. } => break,
            _ => {}
        }
    }
    let text = if !final_text.trim().is_empty() { final_text } else { deltas };
    let answer = parse_answer_text(&text);
    if answer.is_none() {
        tracing::warn!("ask LLM autopilot: could not parse an answer; escalating to a human");
    }
    answer
}

#[tool_router(router = interaction_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.agent.ask",
        description = "Ask a human one or more questions and WAIT for the answer. Purpose: Human-in-the-loop round-trip for agents that hit an ambiguity mid-run; the question lands in the `animus agent interactions` inbox. Two forms: (1) flat single question — pass `question` plus optional `options` (suggested answer strings); returns { id, answer, answered_by, answer_message? }. (2) structured `questions[]` — multi-question / multi-select / described-option form giving codex/gemini/opencode parity with claude's native AskUserQuestion channel; each entry is { question, header?, options: [{ label, description? }], multi_select? }, and the answer comes back as { id, answers: { <question text>: <label | [labels] | free text> }, response?, answer } where `answer` is a readable join for back-compat. When `questions[]` is present the flat `question`/`options` are ignored. Always operates on the server's own project scope. Wait modes: \"block\" parks the call until answered or timeout (default for ad-hoc runs); \"suspend\" returns { status: \"pending\", interaction_id, instruction } immediately, pauses the bound workflow, and the session resumes with the answer (default when the server pins a workflow via --workflow-id / ANIMUS_MCP_WORKFLOW_ID; suspend->block override allowed, block->suspend is ignored). Params: agent_id (ignored when the server pins ANIMUS_MCP_AGENT_ID), question, options, questions, timeout_secs (default 600, max 3600), workflow_id (ignored when the server pins a workflow), task_id, wait. On timeout returns a structured error instructing the agent to proceed with its best judgment. Examples: {\"agent_id\": \"swe\", \"question\": \"Migrate in place or copy table?\", \"options\": [\"in place\", \"copy\"]} or {\"agent_id\": \"swe\", \"questions\": [{\"question\": \"Which sections?\", \"header\": \"Sections\", \"options\": [{\"label\": \"Intro\"}, {\"label\": \"Conclusion\"}], \"multi_select\": true}]}.",
        input_schema = ao_schema_for_type::<AgentAskInput>()
    )]
    async fn ao_agent_ask(&self, params: Parameters<AgentAskInput>) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "animus.agent.ask";
        let input = params.0;
        let project_root = self.default_project_root.clone();
        let agent_id = self.bound_agent_id(Some(&input.agent_id));
        let workflow_pinned = self.workflow_pin().is_some();
        let workflow_id = self.bound_workflow_id(input.workflow_id.as_deref());
        let wait_mode = resolve_wait_mode(workflow_pinned, input.wait.as_deref(), TOOL);
        let timeout_secs = effective_timeout_secs(input.timeout_secs);

        // LLM autopilot: when the agent's approval_policy.default is `llm`, the
        // judge answers questions on the human's behalf (the question still lands
        // in the inbox, answered_by "llm", for auditability). Resolved once; the
        // flat path uses it. Any failure falls through to human escalation.
        let runtime_config = orchestrator_core::agent_runtime_config::load_agent_runtime_config_or_default(
            std::path::Path::new(&project_root),
        );
        let autopilot = resolve_llm_autopilot(runtime_config.agent_profile(&agent_id));

        // Structured path: when `questions[]` is supplied, create the
        // interaction via the shared structured constructor (tool_name = None,
        // so it is NOT marked as the native AskUserQuestion SDK channel) and
        // answer with the plain `{ answers, response, answer }` shape. The
        // flat `question`/`options` fields are ignored for this call.
        if let Some(questions_input) = input.questions {
            if questions_input.is_empty() {
                return structured_err(TOOL, "`questions` must not be an empty array".to_string());
            }
            if questions_input.iter().any(|q| q.question.trim().is_empty()) {
                return structured_err(TOOL, "each `questions[].question` must be non-empty".to_string());
            }
            let questions: Vec<InteractionQuestion> =
                questions_input.into_iter().map(InteractionQuestion::from).collect();
            // Synthesize the `arguments` raw input from the questions so the
            // record carries the question shapes verbatim (mirrors the native
            // path's preserved tool input).
            let raw_input = json!({ "questions": questions });
            let created = match animus_runtime_shared::create_structured_question_interaction(
                &project_root,
                &agent_id,
                questions,
                None,
                raw_input,
                None,
                Some(timeout_secs),
                workflow_id.as_deref(),
                input.task_id.as_deref(),
            ) {
                Ok(record) => record,
                Err(err) => return structured_err(TOOL, err.to_string()),
            };
            emit_interaction_event("interaction_created", &project_root, &created);

            if wait_mode == InteractionWaitMode::Suspend {
                return suspend_pending_response(TOOL, &project_root, &created, false).await;
            }
            return match wait_for_answer(&project_root, &created.id, timeout_secs).await {
                InteractionWait::Answered(record) => structured_ok(TOOL, ask_structured_answered_payload(&record)),
                InteractionWait::TimedOut => {
                    if let Ok(Some(expired)) = animus_runtime_shared::load_interaction(&project_root, &created.id) {
                        emit_interaction_event("interaction_expired", &project_root, &expired);
                    }
                    Ok(CallToolResult::structured_error(json!({
                        "tool": TOOL,
                        "error": format!(
                            "no human answered within {timeout_secs}s. Proceed with your best judgment, state the assumption you made, and continue."
                        ),
                        "interaction_id": created.id,
                        "timed_out": true,
                    })))
                }
                InteractionWait::Lost(message) => structured_err(TOOL, message),
            };
        }

        // Flat single-question path (back-compat, unchanged behaviour).
        let question = match normalize_non_empty(input.question) {
            Some(question) => question,
            None => {
                return structured_err(
                    TOOL,
                    "either `question` or a non-empty `questions` array is required".to_string(),
                );
            }
        };
        let options = input.options.unwrap_or_default();
        let created = match animus_runtime_shared::create_question_interaction(
            &project_root,
            &agent_id,
            &question,
            &options,
            Some(timeout_secs),
            workflow_id.as_deref(),
            input.task_id.as_deref(),
        ) {
            Ok(record) => record,
            Err(err) => return structured_err(TOOL, err.to_string()),
        };
        emit_interaction_event("interaction_created", &project_root, &created);

        // LLM autopilot: answer the question with a model instead of escalating.
        // The answer is recorded on the inbox record (answered_by "llm") so it
        // stays auditable. On any failure we fall through to normal escalation.
        if let Some((judge_tool, judge_model, instructions)) = &autopilot {
            if let Some(answer) = answer_question_with_llm(
                &project_root,
                judge_tool,
                judge_model,
                &question,
                &options,
                instructions.as_deref(),
            )
            .await
            {
                match animus_runtime_shared::answer_interaction(&project_root, &created.id, &answer, None, Some("llm"))
                {
                    Ok(record) => {
                        emit_interaction_event("interaction_answered", &project_root, &record);
                        return structured_ok(
                            TOOL,
                            json!({
                                "id": record.id,
                                "answer": record.answer,
                                "answered_by": record.answered_by,
                                "answer_message": record.answer_message,
                                "source": "llm",
                            }),
                        );
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "ask LLM autopilot: failed to record answer; escalating to a human");
                    }
                }
            }
        }

        if wait_mode == InteractionWaitMode::Suspend {
            return suspend_pending_response(TOOL, &project_root, &created, false).await;
        }

        match wait_for_answer(&project_root, &created.id, timeout_secs).await {
            InteractionWait::Answered(record) => structured_ok(
                TOOL,
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
                    "tool": TOOL,
                    "error": format!(
                        "no human answered within {timeout_secs}s. Proceed with your best judgment, state the assumption you made, and continue."
                    ),
                    "interaction_id": created.id,
                    "timed_out": true,
                })))
            }
            InteractionWait::Lost(message) => structured_err(TOOL, message),
        }
    }

    #[tool(
        name = "animus.agent.request_approval",
        description = "Request human approval for a sensitive action and WAIT for the decision (block-mode timeout denies — fail closed). Purpose: Gate dangerous operations behind a human decision; the agent profile's approval_policy can auto-allow or auto-deny without escalating (auto_deny patterns win, matched against tool_name when present, else action, with `*` glob semantics). Also serves as the claude CLI's --permission-prompt-tool: the CLI invokes it with { tool_name, input, tool_use_id } for every gated tool call, and the result's text content is the SDK permission payload — { behavior: \"allow\", updatedInput: <original or modified input>, updatedPermissions? } or { behavior: \"deny\", message } — with the legacy { tool, result: { decision, source, ... } } envelope alongside. When tool_name is \"AskUserQuestion\" the input's questions[] become a structured Question interaction in the inbox, and the allow response carries updatedInput { questions, answers: { <question text>: <label | [labels] | free text> }, response? }. Always operates on the server's own project scope; the policy profile comes from agent_id, which is ignored when the server pins ANIMUS_MCP_AGENT_ID. Wait modes: \"block\" parks the call until decided or timeout (default for ad-hoc runs); \"suspend\" pauses the bound workflow and returns immediately — { status: \"pending\", interaction_id, instruction } for voluntary calls, behavior \"deny\" with the end-your-turn instruction for native prompt-tool calls (the session resumes with the answer as feedback; default when the server pins a workflow via --workflow-id / ANIMUS_MCP_WORKFLOW_ID; suspend->block override allowed, block->suspend is ignored). Params: agent_id, action (derived from tool_name when omitted), tool_name, input (SDK contract) or arguments, tool_use_id, suggestions (SDK PermissionUpdate[]), timeout_secs (default 600, max 3600), workflow_id (ignored when the server pins a workflow), task_id, wait. Example: {\"agent_id\": \"swe\", \"action\": \"git push --force to main\", \"tool_name\": \"git.push\"}.",
        input_schema = ao_schema_for_type::<AgentRequestApprovalInput>()
    )]
    async fn ao_agent_request_approval(
        &self,
        params: Parameters<AgentRequestApprovalInput>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "animus.agent.request_approval";
        let input = params.0;
        let agent_id = self.bound_agent_id(input.agent_id.as_deref());
        let project_root = self.default_project_root.clone();
        let tool_name = normalize_non_empty(input.tool_name);
        // Native = invoked by the claude CLI as its --permission-prompt-tool
        // ({ tool_name, input, tool_use_id }); the response must follow the
        // SDK behavior:allow/deny contract even for suspend.
        let native = input.input.is_some();
        // The gated tool's original input: the SDK `input` key wins, the
        // voluntary `arguments` key is the fallback. Echoed as `updatedInput`
        // on allow (pass-through), so it is stored on the record.
        let original_input = input.input.or(input.arguments);

        let workflow_pinned = self.workflow_pin().is_some();
        let workflow_id = self.bound_workflow_id(input.workflow_id.as_deref());
        let wait_mode = resolve_wait_mode(workflow_pinned, input.wait.as_deref(), TOOL);
        let timeout_secs = effective_timeout_secs(input.timeout_secs);

        // Native AskUserQuestion calls are clarifying questions, not
        // approvals: parse the structured questions, surface them in the
        // inbox as a Question record, and answer with the SDK
        // `{ questions, answers, response? }` updatedInput shape. The
        // approval policy never applies here (there is nothing to allow or
        // deny — only answers to collect).
        if tool_name.as_deref() == Some("AskUserQuestion") {
            let raw_input = original_input.unwrap_or_else(|| json!({}));
            let questions = match animus_runtime_shared::parse_sdk_questions(&raw_input) {
                Ok(questions) => questions,
                Err(err) => return structured_err(TOOL, err.to_string()),
            };
            let created = match animus_runtime_shared::create_native_question_interaction(
                &project_root,
                &agent_id,
                questions,
                raw_input,
                input.suggestions,
                Some(timeout_secs),
                workflow_id.as_deref(),
                input.task_id.as_deref(),
            ) {
                Ok(record) => record,
                Err(err) => return structured_err(TOOL, err.to_string()),
            };
            emit_interaction_event("interaction_created", &project_root, &created);

            if wait_mode == InteractionWaitMode::Suspend {
                return suspend_pending_response(TOOL, &project_root, &created, native).await;
            }
            return match wait_for_answer(&project_root, &created.id, timeout_secs).await {
                InteractionWait::Answered(record) => {
                    Ok(CallToolResult::structured(sdk_answered_payload(TOOL, &record)))
                }
                InteractionWait::TimedOut => {
                    if let Ok(Some(expired)) = animus_runtime_shared::load_interaction(&project_root, &created.id) {
                        emit_interaction_event("interaction_expired", &project_root, &expired);
                    }
                    let message = format!(
                        "no human answered within {timeout_secs}s. Proceed with your best judgment, state the assumption you made, and continue."
                    );
                    Ok(CallToolResult::structured(merge_sdk_fields(
                        TOOL,
                        json!({ "id": created.id, "source": "timeout", "message": message, "timed_out": true }),
                        sdk_deny_fields(&message),
                    )))
                }
                InteractionWait::Lost(message) => structured_err(TOOL, message),
            };
        }

        let action = match normalize_non_empty(input.action) {
            Some(action) => action,
            None => match tool_name.as_deref() {
                Some(tool) => format!("use tool {tool}"),
                None => return structured_err(TOOL, "action (or tool_name) must not be empty".to_string()),
            },
        };

        let runtime_config = orchestrator_core::agent_runtime_config::load_agent_runtime_config_or_default(
            std::path::Path::new(&project_root),
        );
        let profile = runtime_config.agent_profile(&agent_id);
        if let Some(policy) = profile.and_then(|profile| profile.approval_policy.as_ref()) {
            let subject = tool_name.as_deref().unwrap_or(&action);
            match policy.evaluate(subject) {
                ApprovalPolicyDecision::Allow => {
                    return Ok(CallToolResult::structured(merge_sdk_fields(
                        TOOL,
                        json!({ "decision": "allow", "source": "policy" }),
                        sdk_allow_fields(original_input.unwrap_or_else(|| json!({})), None),
                    )));
                }
                ApprovalPolicyDecision::Deny => {
                    let message = "denied by the agent profile's approval_policy";
                    return Ok(CallToolResult::structured(merge_sdk_fields(
                        TOOL,
                        json!({ "decision": "deny", "source": "policy", "message": message }),
                        sdk_deny_fields(message),
                    )));
                }
                // LLM auto-approve mode: judge the call with a model. A missing
                // model/tool or any evaluator failure falls through to the
                // manual human escalation below (fail safe).
                ApprovalPolicyDecision::Evaluate => {
                    let judge_tool = profile.and_then(|profile| profile.tool.clone());
                    let judge_model =
                        policy.evaluator_model.clone().or_else(|| profile.and_then(|profile| profile.model.clone()));
                    let judge_instructions = policy.evaluator_instructions.clone();
                    match (judge_tool, judge_model) {
                        (Some(judge_tool), Some(judge_model)) => {
                            let verdict = evaluate_approval_with_llm(
                                &project_root,
                                &judge_tool,
                                &judge_model,
                                &action,
                                tool_name.as_deref(),
                                original_input.as_ref(),
                                judge_instructions.as_deref(),
                            )
                            .await;
                            match verdict {
                                Some((true, reason)) => {
                                    return Ok(CallToolResult::structured(merge_sdk_fields(
                                        TOOL,
                                        json!({ "decision": "allow", "source": "llm", "message": reason }),
                                        sdk_allow_fields(original_input.unwrap_or_else(|| json!({})), None),
                                    )));
                                }
                                Some((false, reason)) => {
                                    return Ok(CallToolResult::structured(merge_sdk_fields(
                                        TOOL,
                                        json!({ "decision": "deny", "source": "llm", "message": reason }),
                                        sdk_deny_fields(&reason),
                                    )));
                                }
                                None => {
                                    tracing::warn!(
                                        agent = agent_id,
                                        "approval llm mode: evaluator returned no verdict; escalating to a human"
                                    );
                                }
                            }
                        }
                        _ => {
                            tracing::warn!(
                                agent = agent_id,
                                "approval llm mode: no evaluator_model and no agent model/tool; escalating to a human"
                            );
                        }
                    }
                }
                ApprovalPolicyDecision::Ask => {}
            }
        }

        let created = match animus_runtime_shared::create_approval_interaction(
            &project_root,
            &agent_id,
            &action,
            tool_name.as_deref(),
            original_input,
            input.suggestions,
            Some(timeout_secs),
            workflow_id.as_deref(),
            input.task_id.as_deref(),
        ) {
            Ok(record) => record,
            Err(err) => return structured_err(TOOL, err.to_string()),
        };
        emit_interaction_event("interaction_created", &project_root, &created);

        if wait_mode == InteractionWaitMode::Suspend {
            return suspend_pending_response(TOOL, &project_root, &created, native).await;
        }

        match wait_for_answer(&project_root, &created.id, timeout_secs).await {
            InteractionWait::Answered(record) => Ok(CallToolResult::structured(sdk_answered_payload(TOOL, &record))),
            InteractionWait::TimedOut => {
                if let Ok(Some(expired)) = animus_runtime_shared::load_interaction(&project_root, &created.id) {
                    emit_interaction_event("interaction_expired", &project_root, &expired);
                }
                let message = format!(
                    "no human decided within {timeout_secs}s; denied (fail closed). Do not perform the action."
                );
                Ok(CallToolResult::structured(merge_sdk_fields(
                    TOOL,
                    json!({ "id": created.id, "decision": "deny", "source": "timeout", "message": message }),
                    sdk_deny_fields(&message),
                )))
            }
            InteractionWait::Lost(message) => structured_err(TOOL, message),
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
        description = "Answer a pending interaction (non-blocking; unblocks the agent parked on it). Purpose: Resolve a question with `text`, a structured (AskUserQuestion) question with `answers` keyed by exact question text (string, or array of labels for multi-select) and/or a freeform `response`, or an approval with `decision` (\"allow\" or \"deny\") plus optional `message`. Allowed approvals may carry `updated_input` (operator-modified tool input echoed as updatedInput), explicit `updated_permissions`, or `remember: true` (echoes the record's localSettings-destination suggestions as updatedPermissions). Exactly one answered_by wins a race; later answers fail with a not-pending error. When the record carries a workflow_id and that workflow is suspended, the answer triggers the detached-runner resume with the decision as feedback; a resume failure never fails the answer and surfaces a `workflow_resume.guidance` command instead. Params: id, text?, decision?, message?, answers?, response?, updated_input?, updated_permissions?, remember?, answered_by? (default \"human\"), project_root. Returns: the updated interaction record (plus `workflow_resume` when a resume was attempted). Example question: {\"id\": \"<uuid>\", \"text\": \"use the copy-table migration\"}. Example structured: {\"id\": \"<uuid>\", \"answers\": {\"Which sections?\": [\"Intro\", \"Conclusion\"]}}. Example approval: {\"id\": \"<uuid>\", \"decision\": \"deny\", \"message\": \"too risky\"}.",
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
            AnswerOptions {
                text: input.text,
                allow,
                deny,
                message: input.message,
                answered_by: input.answered_by,
                selects: Vec::new(),
                answers: input.answers,
                response: input.response,
                remember: input.remember.unwrap_or(false),
                updated_input: input.updated_input,
                updated_permissions: input.updated_permissions,
            },
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
                    question: Some("Migrate in place or copy table?".to_string()),
                    options: Some(vec!["in place".to_string(), "copy".to_string()]),
                    questions: None,
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
                    question: Some("Anyone home?".to_string()),
                    options: None,
                    questions: None,
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
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let server = new_ao_mcp_server(&project_root);

            let allowed = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    agent_id: Some("swe".to_string()),
                    action: Some("run the test suite".to_string()),
                    tool_name: Some("cargo test".to_string()),
                    arguments: None,
                    input: None,
                    tool_use_id: None,
                    suggestions: None,
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
                    agent_id: Some("swe".to_string()),
                    action: Some("force push".to_string()),
                    tool_name: Some("git.push --force".to_string()),
                    arguments: None,
                    input: None,
                    tool_use_id: None,
                    suggestions: None,
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
                    agent_id: Some("swe".to_string()),
                    action: Some("anything else".to_string()),
                    tool_name: None,
                    arguments: None,
                    input: None,
                    tool_use_id: None,
                    suggestions: None,
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
                let _config_source_seam =
                    orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                        project.path(),
                    );
                let server = new_ao_mcp_server(&project_root);

                let result = server
                    .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                        agent_id: Some("permissive".to_string()),
                        action: Some("delete everything".to_string()),
                        tool_name: None,
                        arguments: None,
                        input: None,
                        tool_use_id: None,
                        suggestions: None,
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
                let _config_source_seam =
                    orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                        project.path(),
                    );
                let server = new_ao_mcp_server_with_options(&project_root, false, Some("restricted".to_string()), None);

                let result = server
                    .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                        agent_id: Some("permissive".to_string()),
                        action: Some("delete everything".to_string()),
                        tool_name: None,
                        arguments: None,
                        input: None,
                        tool_use_id: None,
                        suggestions: None,
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
                    agent_id: Some("swe".to_string()),
                    action: Some("drop the production database".to_string()),
                    tool_name: Some("Bash".to_string()),
                    arguments: Some(serde_json::json!({ "command": "dropdb prod" })),
                    input: None,
                    tool_use_id: None,
                    suggestions: None,
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
                                answers: None,
                                response: None,
                                updated_input: None,
                                updated_permissions: None,
                                remember: None,
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
                    agent_id: Some("swe".to_string()),
                    action: Some("rotate the API keys".to_string()),
                    tool_name: None,
                    arguments: None,
                    input: None,
                    tool_use_id: None,
                    suggestions: None,
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
                    answers: None,
                    response: None,
                    updated_input: None,
                    updated_permissions: None,
                    remember: None,
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
                    answers: None,
                    response: None,
                    updated_input: None,
                    updated_permissions: None,
                    remember: None,
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
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let workflow = bootstrap_running_workflow(&project_root).await;
            assert_eq!(workflow.status, orchestrator_core::WorkflowStatus::Running);

            let server = new_ao_mcp_server_with_options(&project_root, false, None, Some(workflow.id.clone()));
            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: Some("Which approach?".to_string()),
                    options: None,
                    questions: None,
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
                        agent_id: Some("swe".to_string()),
                        action: Some("rotate the API keys".to_string()),
                        tool_name: None,
                        arguments: None,
                        input: None,
                        tool_use_id: None,
                        suggestions: None,
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
                    question: Some("Anyone home?".to_string()),
                    options: None,
                    questions: None,
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
                    question: Some("Anyone home?".to_string()),
                    options: None,
                    questions: None,
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

    /// Parse the SDK permission payload exactly as the claude CLI does:
    /// take the FIRST text content block of the tool result and JSON-parse
    /// its text (verified against claude CLI v2.1.175).
    fn sdk_text_payload(result: &rmcp::model::CallToolResult) -> Value {
        assert_eq!(result.content.len(), 1, "SDK contract requires a single text content block");
        let text = result.content[0].raw.as_text().expect("first content block must be text").text.clone();
        serde_json::from_str(&text).expect("content text must be valid JSON")
    }

    // Golden conformance: a native prompt-tool approval ({tool_name, input,
    // tool_use_id}) answered allow must emit exactly
    // {"behavior":"allow","updatedInput":<original input>} at the top level
    // of the single text block, with the legacy envelope alongside.
    #[test]
    fn native_approval_allow_emits_sdk_contract_with_input_passthrough() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);
            let original_input = serde_json::json!({ "command": "rm -rf build", "timeout": 5000 });

            let answer_root = project_root.clone();
            let answerer = tokio::spawn(async move {
                loop {
                    let pending =
                        animus_runtime_shared::list_interactions(&answer_root, false, None).expect("list pending");
                    if let Some(record) = pending.first() {
                        animus_runtime_shared::answer_interaction(
                            &answer_root,
                            &record.id,
                            "allow",
                            None,
                            Some("sami"),
                        )
                        .expect("answer allow");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("Bash".to_string()),
                    input: Some(original_input.clone()),
                    tool_use_id: Some("toolu_123".to_string()),
                    timeout_secs: Some(10),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("approval should not error");
            answerer.await.expect("answerer task");
            assert_ne!(result.is_error, Some(true));

            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("allow"));
            assert_eq!(
                payload.pointer("/updatedInput"),
                Some(&original_input),
                "allow must pass the original input through"
            );
            assert!(payload.pointer("/updatedPermissions").is_none());
            // Legacy envelope rides alongside (unknown keys are stripped by
            // the CLI's schema).
            assert_eq!(payload.pointer("/result/decision").and_then(Value::as_str), Some("allow"));
            assert_eq!(payload.pointer("/result/source").and_then(Value::as_str), Some("human"));
            assert_eq!(payload.pointer("/tool").and_then(Value::as_str), Some("animus.agent.request_approval"));
        });
    }

    // Golden conformance: deny answers and block-mode timeouts must emit
    // {"behavior":"deny","message":<string>}.
    #[test]
    fn native_approval_deny_and_timeout_emit_sdk_deny() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            // Timeout path (fail closed).
            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("Bash".to_string()),
                    input: Some(serde_json::json!({ "command": "dropdb prod" })),
                    timeout_secs: Some(1),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("approval should not error");
            assert_ne!(result.is_error, Some(true), "timeout deny is a structured success payload");
            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("deny"));
            assert!(payload.pointer("/message").and_then(Value::as_str).is_some_and(|m| m.contains("fail closed")));
            assert_eq!(payload.pointer("/result/source").and_then(Value::as_str), Some("timeout"));

            // Human deny path.
            let answer_root = project_root.clone();
            let answerer = tokio::spawn(async move {
                loop {
                    let pending =
                        animus_runtime_shared::list_interactions(&answer_root, false, None).expect("list pending");
                    if let Some(record) = pending.first() {
                        animus_runtime_shared::answer_interaction(
                            &answer_root,
                            &record.id,
                            "deny",
                            Some("too risky"),
                            Some("sami"),
                        )
                        .expect("answer deny");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });
            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("Bash".to_string()),
                    input: Some(serde_json::json!({ "command": "dropdb prod" })),
                    timeout_secs: Some(10),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("approval should not error");
            answerer.await.expect("answerer task");
            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("deny"));
            assert_eq!(payload.pointer("/message").and_then(Value::as_str), Some("too risky"));
            assert!(payload.pointer("/updatedInput").is_none(), "deny carries no updatedInput");
        });
    }

    // Golden conformance: a native AskUserQuestion call surfaces a structured
    // Question record (with notifier events) and the answer emits
    // {"behavior":"allow","updatedInput":{questions:<original>,answers:{...},response?}}.
    #[test]
    fn native_ask_user_question_round_trips_sdk_answer_shape() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);
            let raw_input = serde_json::json!({
                "questions": [
                    {
                        "question": "How should I format the output?",
                        "header": "Format",
                        "options": [
                            { "label": "Summary", "description": "Brief overview" },
                            { "label": "Detailed", "description": "Full explanation" }
                        ],
                        "multiSelect": false
                    },
                    {
                        "question": "Which sections should I include?",
                        "header": "Sections",
                        "options": [{ "label": "Introduction" }, { "label": "Conclusion" }],
                        "multiSelect": true
                    }
                ]
            });

            let answer_root = project_root.clone();
            let answer_server = server.clone();
            let answerer = tokio::spawn(async move {
                loop {
                    let pending =
                        animus_runtime_shared::list_interactions(&answer_root, false, None).expect("list pending");
                    if let Some(record) = pending.first() {
                        assert_eq!(record.kind, animus_runtime_shared::InteractionKind::Question);
                        assert_eq!(record.questions.len(), 2, "structured questions parsed onto the record");
                        let mut answers = std::collections::BTreeMap::new();
                        answers.insert(
                            "How should I format the output?".to_string(),
                            Value::String("Summary".to_string()),
                        );
                        answers.insert(
                            "Which sections should I include?".to_string(),
                            serde_json::json!(["Introduction", "Conclusion"]),
                        );
                        let result = answer_server
                            .ao_interactions_answer(Parameters(InteractionsAnswerInput {
                                id: record.id.clone(),
                                text: None,
                                decision: None,
                                message: None,
                                answered_by: Some("sami".to_string()),
                                answers: Some(answers),
                                response: Some("Keep it short".to_string()),
                                updated_input: None,
                                updated_permissions: None,
                                remember: None,
                                project_root: Some(answer_root.clone()),
                            }))
                            .await
                            .expect("answer should not error");
                        assert_ne!(result.is_error, Some(true), "structured answer must succeed: {result:?}");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("AskUserQuestion".to_string()),
                    input: Some(raw_input.clone()),
                    tool_use_id: Some("toolu_456".to_string()),
                    timeout_secs: Some(10),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("question should not error");
            answerer.await.expect("answerer task");
            assert_ne!(result.is_error, Some(true));

            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("allow"));
            assert_eq!(
                payload.pointer("/updatedInput/questions"),
                raw_input.pointer("/questions"),
                "updatedInput.questions must be the original array"
            );
            assert_eq!(
                payload.pointer("/updatedInput/answers/How should I format the output?").and_then(Value::as_str),
                Some("Summary")
            );
            assert_eq!(
                payload.pointer("/updatedInput/answers/Which sections should I include?"),
                Some(&serde_json::json!(["Introduction", "Conclusion"]))
            );
            assert_eq!(payload.pointer("/updatedInput/response").and_then(Value::as_str), Some("Keep it short"));

            // The native question fired the notifier event flow like any
            // other interaction.
            let events = orchestrator_daemon_runtime::DaemonEventLog::read_records(None, None).expect("read events");
            assert!(
                events.iter().any(|event| event.event_type == "interaction_created"),
                "interaction_created event must fire for native questions"
            );
            assert!(
                events.iter().any(|event| event.event_type == "interaction_answered"),
                "interaction_answered event must fire for native questions"
            );
        });
    }

    // Policy auto-allow on a native call must still satisfy the SDK contract
    // (updatedInput passthrough), not just the legacy decision payload.
    #[test]
    fn native_policy_allow_passes_input_through() {
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
      auto_allow: ["Bash"]
      default: deny
phases:
  impl:
    mode: agent
    agent: swe
"#,
            )
            .expect("write workflows.yaml");
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let server = new_ao_mcp_server_with_options(&project_root, false, Some("swe".to_string()), None);
            let original_input = serde_json::json!({ "command": "cargo test" });

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("Bash".to_string()),
                    input: Some(original_input.clone()),
                    timeout_secs: Some(1),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("approval should not error");
            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("allow"));
            assert_eq!(payload.pointer("/updatedInput"), Some(&original_input));
            assert_eq!(payload.pointer("/result/source").and_then(Value::as_str), Some("policy"));

            // Policy default-deny on another tool emits the SDK deny shape.
            let denied = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("WebFetch".to_string()),
                    input: Some(serde_json::json!({ "url": "https://example.com" })),
                    timeout_secs: Some(1),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("approval should not error");
            let payload = sdk_text_payload(&denied);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("deny"));
            assert!(payload.pointer("/message").and_then(Value::as_str).is_some());
        });
    }

    // Suggestions passthrough: stored on the record, echoed back as
    // updatedPermissions when the allow answer asks to remember, and an
    // operator-modified updated_input replaces the passthrough input.
    #[test]
    fn native_approval_remember_echoes_suggestions_and_updated_input() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);
            let suggestions = serde_json::json!([
                { "type": "addRules", "behavior": "allow", "destination": "localSettings" },
                { "type": "addRules", "behavior": "allow", "destination": "session" }
            ]);
            let local_settings_only = serde_json::json!([suggestions[0].clone()]);

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
                                message: None,
                                answered_by: Some("sami".to_string()),
                                answers: None,
                                response: None,
                                updated_input: Some(serde_json::json!({ "command": "rm -rf build/sandbox" })),
                                updated_permissions: None,
                                remember: Some(true),
                                project_root: Some(answer_root.clone()),
                            }))
                            .await
                            .expect("answer should not error");
                        assert_ne!(result.is_error, Some(true));
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("Bash".to_string()),
                    input: Some(serde_json::json!({ "command": "rm -rf build" })),
                    suggestions: Some(suggestions.clone()),
                    timeout_secs: Some(10),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("approval should not error");
            answerer.await.expect("answerer task");

            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("allow"));
            assert_eq!(
                payload.pointer("/updatedInput"),
                Some(&serde_json::json!({ "command": "rm -rf build/sandbox" }))
            );
            assert_eq!(
                payload.pointer("/updatedPermissions"),
                Some(&local_settings_only),
                "remember echoes only the localSettings-destination suggestions"
            );
        });
    }

    // Native suspend mode cannot return a pending payload (the CLI only
    // understands behavior allow|deny), so it denies with the end-your-turn
    // instruction; the record is suspended for the feedback-based resume.
    #[test]
    fn native_suspend_returns_sdk_deny_with_instruction() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server_with_options(&project_root, false, None, Some("wf-native".to_string()));

            let result = server
                .ao_agent_request_approval(Parameters(AgentRequestApprovalInput {
                    tool_name: Some("AskUserQuestion".to_string()),
                    input: Some(serde_json::json!({
                        "questions": [{ "question": "Proceed?", "options": [{ "label": "Yes" }, { "label": "No" }] }]
                    })),
                    timeout_secs: Some(600),
                    ..AgentRequestApprovalInput::default()
                }))
                .await
                .expect("question should not error");
            assert_ne!(result.is_error, Some(true));
            let payload = sdk_text_payload(&result);
            assert_eq!(payload.pointer("/behavior").and_then(Value::as_str), Some("deny"));
            assert!(payload
                .pointer("/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("end") && message.contains("turn")));
            assert_eq!(payload.pointer("/result/status").and_then(Value::as_str), Some("pending"));

            let interaction_id = payload
                .pointer("/result/interaction_id")
                .and_then(Value::as_str)
                .expect("interaction_id in legacy payload")
                .to_string();
            let record = animus_runtime_shared::load_interaction(&project_root, &interaction_id)
                .expect("load")
                .expect("record exists");
            assert!(record.suspended, "native suspend records resume via feedback");
            assert_eq!(record.status, InteractionStatus::Pending);
            assert_eq!(record.kind, animus_runtime_shared::InteractionKind::Question);
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
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
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

            let result = suspend_pending_response("animus.agent.ask", &project_root, &created, false)
                .await
                .expect("suspend response");
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

    fn two_structured_questions() -> Vec<AskQuestionInput> {
        serde_json::from_value(serde_json::json!([
            {
                "question": "How should I format the output?",
                "header": "Format",
                "options": [{ "label": "Summary" }, { "label": "Detailed" }],
                "multi_select": false
            },
            {
                "question": "Which sections should I include?",
                "header": "Sections",
                "options": [{ "label": "Introduction" }, { "label": "Conclusion" }],
                "multi_select": true
            }
        ]))
        .expect("parse structured questions")
    }

    fn two_structured_questions_record() -> Vec<InteractionQuestion> {
        two_structured_questions().into_iter().map(InteractionQuestion::from).collect()
    }

    // Structured ask (block mode): create with questions[] -> answer with
    // --select -> the tool returns the answers map, response, and a readable
    // legacy `answer` join. Codex/gemini/opencode parity with the native
    // claude AskUserQuestion channel.
    #[test]
    fn structured_ask_block_round_trips_answers_map_and_legacy_answer() {
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
                        // The ask-originated structured record must NOT be
                        // marked as the native AskUserQuestion SDK channel.
                        assert_eq!(record.kind, InteractionKind::Question);
                        assert_eq!(record.questions.len(), 2, "structured questions parsed onto the record");
                        assert!(record.tool_name.is_none(), "ask-originated questions are not the native SDK channel");
                        let _ = &answer_server;
                        // Emulate the CLI `--select` answer path (resolves
                        // labels by question header / text into the answers map).
                        crate::services::runtime::runtime_agent::interactions::answer_interaction_op_with_resume(
                            &answer_root,
                            &record.id,
                            crate::services::runtime::runtime_agent::interactions::AnswerOptions {
                                selects: vec![
                                    "Format=Summary".to_string(),
                                    "Sections=Introduction,Conclusion".to_string(),
                                ],
                                response: Some("keep it short".to_string()),
                                answered_by: Some("sami".to_string()),
                                ..Default::default()
                            },
                        )
                        .await
                        .expect("structured answer via --select");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: None,
                    options: None,
                    questions: Some(two_structured_questions()),
                    timeout_secs: Some(10),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("structured ask should not error");
            answerer.await.expect("answerer task");

            assert_ne!(result.is_error, Some(true), "structured ask must succeed once answered");
            let payload = data(&result);
            assert_eq!(
                payload.pointer("/answers/How should I format the output?").and_then(Value::as_str),
                Some("Summary")
            );
            assert_eq!(
                payload.pointer("/answers/Which sections should I include?"),
                Some(&serde_json::json!(["Introduction", "Conclusion"]))
            );
            assert_eq!(payload.pointer("/response").and_then(Value::as_str), Some("keep it short"));
            // Legacy readable join still present for back-compat.
            assert!(payload
                .pointer("/answer")
                .and_then(Value::as_str)
                .is_some_and(|answer| answer.contains("Summary") && answer.contains("Introduction")));
            // The structured ask is NOT the SDK channel: no behavior/updatedInput.
            assert!(structured(&result).pointer("/behavior").is_none());
        });
    }

    // multiSelect-only answer through the direct `answers` map (MCP path).
    #[test]
    fn structured_ask_block_multi_select_via_answers_map() {
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
                        let mut answers = BTreeMap::new();
                        answers.insert(
                            "Which sections should I include?".to_string(),
                            serde_json::json!(["Introduction", "Conclusion"]),
                        );
                        let result = answer_server
                            .ao_interactions_answer(Parameters(InteractionsAnswerInput {
                                id: record.id.clone(),
                                text: None,
                                decision: None,
                                message: None,
                                answered_by: Some("sami".to_string()),
                                answers: Some(answers),
                                response: None,
                                updated_input: None,
                                updated_permissions: None,
                                remember: None,
                                project_root: Some(answer_root.clone()),
                            }))
                            .await
                            .expect("answer should not error");
                        assert_ne!(result.is_error, Some(true), "multi-select answer must succeed: {result:?}");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let only_multi = vec![two_structured_questions()[1].clone()];
            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: None,
                    options: None,
                    questions: Some(only_multi),
                    timeout_secs: Some(10),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("structured ask should not error");
            answerer.await.expect("answerer task");

            let payload = data(&result);
            assert_eq!(
                payload.pointer("/answers/Which sections should I include?"),
                Some(&serde_json::json!(["Introduction", "Conclusion"]))
            );
        });
    }

    // Response-only answer to a structured ask (freeform reply, no per-question
    // answers).
    #[test]
    fn structured_ask_block_response_only_answer() {
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
                                decision: None,
                                message: None,
                                answered_by: Some("sami".to_string()),
                                answers: None,
                                response: Some("just ship it".to_string()),
                                updated_input: None,
                                updated_permissions: None,
                                remember: None,
                                project_root: Some(answer_root.clone()),
                            }))
                            .await
                            .expect("answer should not error");
                        assert_ne!(result.is_error, Some(true), "response-only answer must succeed: {result:?}");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            });

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: None,
                    options: None,
                    questions: Some(two_structured_questions()),
                    timeout_secs: Some(10),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("structured ask should not error");
            answerer.await.expect("answerer task");

            let payload = data(&result);
            assert_eq!(payload.pointer("/response").and_then(Value::as_str), Some("just ship it"));
            assert!(payload.pointer("/answers").and_then(Value::as_object).is_some_and(|map| map.is_empty()));
            assert_eq!(payload.pointer("/answer").and_then(Value::as_str), Some("just ship it"));
        });
    }

    // Flat ask regression: the single-question convenience form is unchanged.
    #[test]
    fn flat_ask_still_returns_legacy_shape() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let answer_root = project_root.clone();
            let answerer = tokio::spawn(async move {
                loop {
                    let pending =
                        animus_runtime_shared::list_interactions(&answer_root, false, None).expect("list pending");
                    if let Some(record) = pending.first() {
                        assert!(record.questions.is_empty(), "flat ask carries no structured questions");
                        animus_runtime_shared::answer_interaction(
                            &answer_root,
                            &record.id,
                            "copy table",
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
                    question: Some("Migrate in place or copy table?".to_string()),
                    options: Some(vec!["in place".to_string(), "copy".to_string()]),
                    questions: None,
                    timeout_secs: Some(10),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("flat ask should not error");
            answerer.await.expect("answerer task");

            let payload = data(&result);
            assert_eq!(payload.pointer("/answer").and_then(Value::as_str), Some("copy table"));
            assert_eq!(payload.pointer("/answered_by").and_then(Value::as_str), Some("sami"));
            assert!(payload.pointer("/answers").is_none(), "flat ask returns no answers map");
        });
    }

    // Empty `questions: []` is rejected with a structured error (not silently
    // treated as a flat ask with an empty question).
    #[test]
    fn structured_ask_rejects_empty_questions_array() {
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();
            let server = new_ao_mcp_server(&project_root);

            let result = server
                .ao_agent_ask(Parameters(AgentAskInput {
                    agent_id: "swe".to_string(),
                    question: None,
                    options: None,
                    questions: Some(Vec::new()),
                    timeout_secs: Some(1),
                    workflow_id: None,
                    task_id: None,
                    wait: None,
                }))
                .await
                .expect("ask should produce a structured result");
            assert_eq!(result.is_error, Some(true));
            assert!(structured(&result)
                .pointer("/error")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("empty")));
        });
    }

    // Suspend feedback for an ask-originated structured question: the resume
    // feedback renders the per-question answers (the sdk-conform builder keys
    // off `questions` being non-empty, not the native tool_name marker).
    #[test]
    fn ask_structured_suspend_feedback_renders_per_question_answers() {
        use crate::services::runtime::runtime_agent::interactions::resume_feedback_for_interaction;
        with_isolated_scope(async {
            let project = tempdir().expect("tempdir");
            let project_root = project.path().to_string_lossy().to_string();

            let raw_input = serde_json::json!({ "questions": two_structured_questions_record() });
            let created = animus_runtime_shared::create_structured_question_interaction(
                &project_root,
                "swe",
                two_structured_questions_record(),
                None,
                raw_input,
                None,
                Some(600),
                Some("wf-1"),
                None,
            )
            .expect("create ask-originated structured question");
            assert!(created.tool_name.is_none(), "ask-originated structured questions are not the native SDK channel");

            let mut answers = BTreeMap::new();
            answers.insert("How should I format the output?".to_string(), Value::String("Summary".to_string()));
            answers.insert(
                "Which sections should I include?".to_string(),
                serde_json::json!(["Introduction", "Conclusion"]),
            );
            let answered = animus_runtime_shared::apply_interaction_answer(
                &project_root,
                &created.id,
                animus_runtime_shared::InteractionAnswer {
                    answer: "Format: Summary; Sections: Introduction, Conclusion".to_string(),
                    answers: Some(answers),
                    response: Some("keep it short".to_string()),
                    answered_by: Some("sami".to_string()),
                    ..Default::default()
                },
            )
            .expect("structured answer");

            let feedback = resume_feedback_for_interaction(&answered);
            assert!(feedback.contains("The user answered your questions:"));
            assert!(feedback.contains("How should I format the output?"));
            assert!(feedback.contains("Summary"));
            assert!(feedback.contains("Introduction, Conclusion"));
            assert!(feedback.contains("keep it short"));
        });
    }
}

#[cfg(test)]
mod approval_judge_tests {
    use super::{build_judge_system_prompt, build_judge_user_prompt, parse_judge_verdict};
    use orchestrator_config::agent_runtime_config::{ApprovalPolicy, ApprovalPolicyDecision, ApprovalPolicyDefault};
    use serde_json::json;

    #[test]
    fn parses_plain_json_verdict() {
        let (allow, reason) =
            parse_judge_verdict(r#"{"decision":"allow","reason":"routine test run"}"#).expect("verdict");
        assert!(allow);
        assert_eq!(reason, "routine test run");
    }

    #[test]
    fn parses_deny_with_prose_and_fences() {
        let text =
            "Let me think about this.\n```json\n{\"decision\": \"deny\", \"reason\": \"drops the prod database\"}\n```";
        let (allow, reason) = parse_judge_verdict(text).expect("verdict");
        assert!(!allow);
        assert_eq!(reason, "drops the prod database");
    }

    #[test]
    fn takes_the_last_decision_object_when_reasoning_precedes_it() {
        // A leading object without a decision must be skipped in favour of the
        // trailing verdict object.
        let text = "{\"thought\":\"weighing risk\"} ... final: {\"decision\":\"deny\",\"reason\":\"force push\"}";
        let (allow, reason) = parse_judge_verdict(text).expect("verdict");
        assert!(!allow);
        assert_eq!(reason, "force push");
    }

    #[test]
    fn accepts_decision_synonyms_and_defaults_reason() {
        let (allow, reason) = parse_judge_verdict(r#"{"decision":"approve"}"#).expect("verdict");
        assert!(allow);
        assert_eq!(reason, "no reason given");
    }

    #[test]
    fn rejects_unparseable_or_missing_decision() {
        assert!(parse_judge_verdict("I cannot decide.").is_none());
        assert!(parse_judge_verdict(r#"{"reason":"no decision key"}"#).is_none());
        assert!(parse_judge_verdict(r#"{"decision":"maybe"}"#).is_none());
        assert!(parse_judge_verdict("").is_none());
    }

    #[test]
    fn llm_default_evaluates_after_allow_deny_lists() {
        let policy = ApprovalPolicy {
            auto_allow: vec!["cargo *".to_string()],
            auto_deny: vec!["git.push*".to_string()],
            default: ApprovalPolicyDefault::Llm,
            evaluator_model: Some("anthropic/claude-haiku-4-5".to_string()),
            evaluator_instructions: None,
        };
        // List matches still short-circuit without the LLM.
        assert_eq!(policy.evaluate("cargo test"), ApprovalPolicyDecision::Allow);
        assert_eq!(policy.evaluate("git.push --force"), ApprovalPolicyDecision::Deny);
        // Everything else defers to the evaluator.
        assert_eq!(policy.evaluate("Bash"), ApprovalPolicyDecision::Evaluate);
    }

    #[test]
    fn parses_answer_text_from_json() {
        assert_eq!(super::parse_answer_text(r#"{"answer":"copy table"}"#).as_deref(), Some("copy table"));
        assert_eq!(
            super::parse_answer_text("Sure.\n```json\n{\"answer\": \"in place\"}\n```").as_deref(),
            Some("in place")
        );
        assert!(super::parse_answer_text("I'm not sure").is_none());
        assert!(super::parse_answer_text(r#"{"answer":""}"#).is_none());
    }

    #[test]
    fn judge_user_prompt_truncates_long_non_ascii_without_panicking() {
        // A long multibyte string would panic a byte-index truncate mid-codepoint.
        let big = "日本語".repeat(5000);
        let user = build_judge_user_prompt("act", Some("Bash"), Some(&json!({ "command": big })));
        assert!(user.contains("(truncated)"));
        assert!(user.len() < 6000);
    }

    #[test]
    fn judge_prompts_carry_action_tool_and_operator_policy() {
        let system = build_judge_system_prompt(Some("never touch billing"));
        assert!(system.contains("approval gate"));
        assert!(system.contains("never touch billing"));
        let user = build_judge_user_prompt("drop prod", Some("Bash"), Some(&json!({ "command": "dropdb prod" })));
        assert!(user.contains("drop prod"));
        assert!(user.contains("Bash"));
        assert!(user.contains("dropdb prod"));
    }
}
