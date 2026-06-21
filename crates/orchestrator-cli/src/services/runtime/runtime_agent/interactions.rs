use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use animus_runtime_shared::{
    InteractionAnswer, InteractionKind, InteractionQuestion, InteractionRecord, InteractionStatus,
};
use orchestrator_core::{services::ServiceHub, FileServiceHub, WorkflowStatus};
use orchestrator_daemon_runtime::DaemonEventLog;

use crate::{
    format_age, print_value, render_table, AgentInteractionsAnswerArgs, AgentInteractionsListArgs,
    AgentInteractionsShowArgs,
};

/// One-line human summary of the interaction, carried on the daemon event so
/// notifier plugins can render a push without re-reading the store.
pub(crate) fn interaction_summary(record: &InteractionRecord) -> String {
    match record.kind {
        InteractionKind::Question => {
            format!("agent '{}' asks: {}", record.agent_id, record.question.as_deref().unwrap_or(""))
        }
        InteractionKind::Approval => {
            format!("agent '{}' requests approval: {}", record.agent_id, record.action.as_deref().unwrap_or(""))
        }
    }
}

/// Ready-to-run CLI command that resolves the interaction. Questions need a
/// `--text` placeholder filled in; approvals default to the allow form (the
/// notifier payload is advisory, not an auto-approve).
pub(crate) fn interaction_answer_command(record: &InteractionRecord) -> String {
    match record.kind {
        InteractionKind::Question if !record.questions.is_empty() => {
            format!("animus agent interactions answer {} --select \"1=<label>\" [--text \"<note>\"]", record.id)
        }
        InteractionKind::Question => {
            format!("animus agent interactions answer {} --text \"<answer>\"", record.id)
        }
        InteractionKind::Approval => format!("animus agent interactions answer {} --allow", record.id),
    }
}

// Best-effort observability tee into the daemon event log. The log is a plain
// jsonl file under the global Animus state dir, so emission works without a
// running daemon; failures never block the interaction round-trip itself.
// The daemon's notifier watcher fans `interaction_*` records out to installed
// notifier plugins, so the payload carries everything a push needs: kind,
// agent, summary, and a ready-to-run answer command.
pub(crate) fn emit_interaction_event(event_type: &str, project_root: &str, record: &InteractionRecord) {
    let canonical_root = crate::services::runtime::canonicalize_lossy(project_root);
    let mut seq = 0;
    let event = DaemonEventLog::next_event(&mut seq, event_type, Some(canonical_root), interaction_event_data(record));
    let _ = DaemonEventLog::append(&event);
}

pub(crate) fn interaction_event_data(record: &InteractionRecord) -> Value {
    json!({
        "interaction_id": record.id,
        "kind": record.kind,
        "agent_id": record.agent_id,
        "workflow_id": record.workflow_id,
        "task_id": record.task_id,
        "status": record.status,
        "question": record.question,
        "action": record.action,
        "tool_name": record.tool_name,
        "answer": record.answer,
        "answered_by": record.answered_by,
        "summary": interaction_summary(record),
        "answer_command": interaction_answer_command(record),
    })
}

pub(super) fn handle_agent_interactions_list(
    args: AgentInteractionsListArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let interactions = animus_runtime_shared::list_interactions(project_root, args.all, args.agent.as_deref())?;
    if !json_output {
        if interactions.is_empty() {
            println!("no pending interactions");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = interactions
            .iter()
            .map(|record| {
                let kind = match record.kind {
                    InteractionKind::Question => "question",
                    InteractionKind::Approval => "approval",
                };
                vec![
                    record.id.clone(),
                    record.agent_id.clone(),
                    kind.to_string(),
                    interaction_summary(record),
                    format_age(&record.created_at),
                ]
            })
            .collect();
        render_table(&["ID", "AGENT", "TYPE", "SUMMARY", "AGE"], &rows);
        let has_approvals = interactions.iter().any(|r| matches!(r.kind, InteractionKind::Approval));
        let has_questions = interactions.iter().any(|r| matches!(r.kind, InteractionKind::Question));
        match (has_approvals, has_questions) {
            (true, true) => println!(
                "answer with: animus agent interactions answer <id> --allow|--deny (approvals) or --text <answer> (questions)"
            ),
            (true, false) => println!("answer with: animus agent interactions answer <id> --allow|--deny"),
            _ => println!("answer with: animus agent interactions answer <id> --text <answer>"),
        }
        return Ok(());
    }
    print_value(json!({ "count": interactions.len(), "interactions": interactions }), json_output)
}

/// Human-readable rendering of a structured-question record for the
/// non-JSON `animus agent interactions show` output.
pub(crate) fn render_structured_questions(record: &InteractionRecord) -> String {
    let mut lines = Vec::new();
    for (index, question) in record.questions.iter().enumerate() {
        let header = question.header.as_deref().map(|header| format!(" [{header}]")).unwrap_or_default();
        let multi = if question.multi_select { " (multi-select)" } else { "" };
        lines.push(format!("{}.{} {}{}", index + 1, header, question.question, multi));
        for option in &question.options {
            match option.description.as_deref() {
                Some(description) => lines.push(format!("   - {} — {}", option.label, description)),
                None => lines.push(format!("   - {}", option.label)),
            }
        }
        if let Some(value) = record.answers.as_ref().and_then(|answers| answers.get(&question.question)) {
            lines.push(format!("   answer: {}", render_answer_value(value)));
        }
    }
    if let Some(response) = record.response.as_deref() {
        lines.push(format!("response: {response}"));
    }
    if record.suggestions.is_some() {
        lines.push("permission suggestions attached (answer with --remember to echo localSettings rules)".to_string());
    }
    if record.status == InteractionStatus::Pending {
        lines.push(format!("answer with: {}", interaction_answer_command(record)));
    }
    lines.join("\n")
}

pub(super) fn handle_agent_interactions_show(
    args: AgentInteractionsShowArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let record = animus_runtime_shared::load_interaction(project_root, &args.id)?
        .ok_or_else(|| anyhow!("no interaction with id '{}'", args.id))?;
    if !json_output && !record.questions.is_empty() {
        println!("{}\n", render_structured_questions(&record));
    }
    print_value(record, json_output)
}

/// Answer inputs shared by the CLI inbox and the management-gated
/// `animus.interactions.answer` MCP tool.
#[derive(Debug, Clone, Default)]
pub(crate) struct AnswerOptions {
    pub text: Option<String>,
    pub allow: bool,
    pub deny: bool,
    pub message: Option<String>,
    pub answered_by: Option<String>,
    /// CLI-style structured selections: `"<question|header|1-based index>=<label[,label...]>"`.
    pub selects: Vec<String>,
    /// Direct structured answers (MCP path), keyed by exact question text.
    pub answers: Option<BTreeMap<String, Value>>,
    /// Freeform reply not tied to a specific question.
    pub response: Option<String>,
    /// Echo the record's localSettings-destination permission suggestions
    /// back as `updatedPermissions` (allowed approvals only).
    pub remember: bool,
    /// Operator-modified tool input echoed as `updatedInput` (allowed
    /// approvals only).
    pub updated_input: Option<Value>,
    /// Explicit `updatedPermissions` payload; wins over `remember`.
    pub updated_permissions: Option<Value>,
}

fn render_answer_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Resolve one `--select "<question-or-index>=<label[,label...]>"` spec
/// against the record's structured questions.
fn resolve_select(questions: &[InteractionQuestion], spec: &str) -> Result<(String, Vec<String>)> {
    let (selector, labels_raw) = spec
        .split_once('=')
        .ok_or_else(|| anyhow!("--select expects \"<question-or-index>=<label[,label...]>\" (got '{spec}')"))?;
    let selector = selector.trim();
    let labels: Vec<String> =
        labels_raw.split(',').map(str::trim).filter(|label| !label.is_empty()).map(ToOwned::to_owned).collect();
    anyhow::ensure!(!labels.is_empty(), "--select '{spec}' has no label after '='");
    anyhow::ensure!(!selector.is_empty(), "--select '{spec}' has no question before '='");

    let question = if let Ok(index) = selector.parse::<usize>() {
        questions
            .get(index.checked_sub(1).ok_or_else(|| anyhow!("--select question index is 1-based (got 0)"))?)
            .ok_or_else(|| anyhow!("--select question index {index} is out of range (1..={})", questions.len()))?
    } else {
        questions
            .iter()
            .find(|candidate| candidate.question == selector)
            .or_else(|| questions.iter().find(|candidate| candidate.question.eq_ignore_ascii_case(selector)))
            .or_else(|| {
                questions.iter().find(|candidate| {
                    candidate.header.as_deref().is_some_and(|header| header.eq_ignore_ascii_case(selector))
                })
            })
            .ok_or_else(|| anyhow!("--select '{selector}' matches no question text, header, or index"))?
    };
    Ok((question.question.clone(), labels))
}

/// Build the structured `answers` map from CLI `--select` specs plus any
/// direct `answers` (MCP path), applying the `--text` mapping rules:
/// single-question records map bare text to that question's answer;
/// multi-question records route bare text to the freeform `response`.
fn structured_answer_inputs(
    record: &InteractionRecord,
    opts: &AnswerOptions,
) -> Result<(BTreeMap<String, Value>, Option<String>)> {
    let mut answers = opts.answers.clone().unwrap_or_default();
    for spec in &opts.selects {
        let (question, labels) = resolve_select(&record.questions, spec)?;
        let merged: Vec<String> = match answers.remove(&question) {
            Some(Value::String(existing)) => std::iter::once(existing).chain(labels).collect(),
            Some(Value::Array(existing)) => {
                existing.into_iter().filter_map(|item| item.as_str().map(ToOwned::to_owned)).chain(labels).collect()
            }
            _ => labels,
        };
        let value = if merged.len() == 1 {
            Value::String(merged.into_iter().next().unwrap_or_default())
        } else {
            json!(merged)
        };
        answers.insert(question, value);
    }
    let mut response = opts.response.clone().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    let text = opts.text.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
    if let Some(text) = text {
        if answers.is_empty() && response.is_none() && record.questions.len() == 1 {
            answers.insert(record.questions[0].question.clone(), Value::String(text));
        } else if response.is_none() {
            response = Some(text);
        }
    }
    Ok((answers, response))
}

/// Flat back-compat `answer` summary for a structured answer.
fn structured_answer_summary(answers: &BTreeMap<String, Value>, response: Option<&str>) -> String {
    let mut parts: Vec<String> =
        answers.iter().map(|(question, value)| format!("{question}: {}", render_answer_value(value))).collect();
    if let Some(response) = response {
        parts.push(if parts.is_empty() { response.to_string() } else { format!("note: {response}") });
    }
    parts.join("; ")
}

/// Permission suggestions to persist when the answer asks to remember the
/// decision: the localSettings-destination subset of the record's stored
/// suggestions (mirrors the SDK "Approve and remember" flow).
fn remembered_permissions(suggestions: Option<&Value>) -> Option<Value> {
    let filtered: Vec<Value> = suggestions?
        .as_array()?
        .iter()
        .filter(|entry| entry.get("destination").and_then(Value::as_str) == Some("localSettings"))
        .cloned()
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(Value::Array(filtered))
    }
}

pub(super) async fn handle_agent_interactions_answer(
    args: AgentInteractionsAnswerArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let updated_input = args
        .updated_input
        .as_deref()
        .map(|raw| {
            serde_json::from_str::<Value>(raw).map_err(|err| anyhow!("--updated-input is not valid JSON: {err}"))
        })
        .transpose()?;
    let (record, workflow_resume) = answer_interaction_op_with_resume(
        project_root,
        &args.id,
        AnswerOptions {
            text: args.text,
            allow: args.allow,
            deny: args.deny,
            message: args.message,
            answered_by: args.answered_by,
            selects: args.select,
            remember: args.remember,
            updated_input,
            ..AnswerOptions::default()
        },
    )
    .await?;
    let mut payload = serde_json::to_value(&record)?;
    if let (Value::Object(map), Some(resume)) = (&mut payload, workflow_resume) {
        map.insert("workflow_resume".to_string(), resume);
    }
    print_value(payload, json_output)
}

pub(crate) fn answer_interaction_op(project_root: &str, id: &str, opts: &AnswerOptions) -> Result<InteractionRecord> {
    let record = animus_runtime_shared::load_interaction(project_root, id)?
        .ok_or_else(|| anyhow!("no interaction with id '{}'", id))?;
    let answer = match record.kind {
        InteractionKind::Question if !record.questions.is_empty() => {
            if opts.allow || opts.deny {
                return Err(anyhow!("interaction '{}' is a structured question; answer it with --select/--text", id));
            }
            let (answers, response) = structured_answer_inputs(&record, opts)?;
            if answers.is_empty() && response.is_none() {
                return Err(anyhow!("a structured question answer requires --select and/or --text"));
            }
            let summary = structured_answer_summary(&answers, response.as_deref());
            let answered = animus_runtime_shared::apply_interaction_answer(
                project_root,
                id,
                InteractionAnswer {
                    answer: summary,
                    message: opts.message.clone(),
                    answered_by: opts.answered_by.clone(),
                    answers: Some(answers).filter(|map| !map.is_empty()),
                    response,
                    updated_input: None,
                    updated_permissions: None,
                },
            )?;
            emit_interaction_event("interaction_answered", project_root, &answered);
            return Ok(answered);
        }
        InteractionKind::Question => {
            if opts.allow || opts.deny {
                return Err(anyhow!("interaction '{}' is a question; answer it with --text", id));
            }
            if !opts.selects.is_empty() {
                return Err(anyhow!("interaction '{}' has no structured questions; answer it with --text", id));
            }
            opts.text
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("a question answer requires --text"))?
                .to_string()
        }
        InteractionKind::Approval => match (opts.allow, opts.deny) {
            (true, false) => animus_runtime_shared::INTERACTION_ANSWER_ALLOW.to_string(),
            (false, true) => animus_runtime_shared::INTERACTION_ANSWER_DENY.to_string(),
            _ => return Err(anyhow!("an approval answer requires exactly one of --allow or --deny")),
        },
    };
    let is_allow = record.kind == InteractionKind::Approval && opts.allow;
    let updated_permissions = if is_allow {
        opts.updated_permissions.clone().or_else(|| {
            if opts.remember {
                remembered_permissions(record.suggestions.as_ref())
            } else {
                None
            }
        })
    } else {
        None
    };
    let answered = animus_runtime_shared::apply_interaction_answer(
        project_root,
        id,
        InteractionAnswer {
            answer,
            message: opts.message.clone(),
            answered_by: opts.answered_by.clone(),
            answers: None,
            response: None,
            updated_input: if is_allow { opts.updated_input.clone() } else { None },
            updated_permissions,
        },
    )?;
    emit_interaction_event("interaction_answered", project_root, &answered);
    Ok(answered)
}

// Shared answer path for the CLI inbox and the `animus.interactions.answer`
// MCP tool: answer the record, then — when it carries a workflow id and that
// workflow is suspended — trigger the detached-runner resume with the
// decision as feedback. Resume failures never fail the answer; they come
// back in the second tuple slot with the exact `animus workflow resume`
// command as guidance.
pub(crate) async fn answer_interaction_op_with_resume(
    project_root: &str,
    id: &str,
    opts: AnswerOptions,
) -> Result<(InteractionRecord, Option<Value>)> {
    let record = answer_interaction_op(project_root, id, &opts)?;
    let workflow_resume = resume_workflow_for_answered_interaction(project_root, &record).await;
    Ok((record, workflow_resume))
}

/// Resume prompt carried as workflow feedback so the runner can hand the
/// decision to the resumed provider session.
pub(crate) fn resume_feedback_for_interaction(record: &InteractionRecord) -> String {
    match record.kind {
        // Structured (native AskUserQuestion) records resume via session
        // feedback, not via the original prompt-tool result — the CLI
        // process that asked is gone. Carry the per-question answers
        // explicitly so the resumed session can act on them.
        InteractionKind::Question if !record.questions.is_empty() => {
            let mut lines = Vec::new();
            let answers = record.answers.clone().unwrap_or_default();
            if answers.is_empty() {
                if let Some(response) = record.response.as_deref() {
                    return format!("The user responded to your questions: {response}. Continue.");
                }
            }
            lines.push("The user answered your questions:".to_string());
            for question in &record.questions {
                if let Some(value) = answers.get(&question.question) {
                    lines.push(format!("- \"{}\": {}", question.question, render_answer_value(value)));
                }
            }
            if let Some(response) = record.response.as_deref() {
                lines.push(format!("Additional note from the user: {response}"));
            }
            lines.push("Continue.".to_string());
            lines.join("\n")
        }
        InteractionKind::Question => format!(
            "Answer to your question \"{}\": {}. Continue.",
            record.question.as_deref().unwrap_or(""),
            record.answer.as_deref().unwrap_or("")
        ),
        InteractionKind::Approval => {
            let decision = if record.answer.as_deref() == Some(animus_runtime_shared::INTERACTION_ANSWER_ALLOW) {
                "granted"
            } else {
                "denied"
            };
            let action = record.action.as_deref().unwrap_or("");
            match record.answer_message.as_deref() {
                Some(message) => format!("Approval {decision} for {action}: {message}. Continue."),
                None => format!("Approval {decision} for {action}. Continue."),
            }
        }
    }
}

/// Best-effort resume of a suspended workflow after its interaction was
/// answered. Returns `None` when the record has no workflow id; otherwise a
/// JSON report of the attempt. Never returns `Err` — the answer itself is
/// already durable and must not be rolled back by a resume failure.
pub(crate) async fn resume_workflow_for_answered_interaction(
    project_root: &str,
    record: &InteractionRecord,
) -> Option<Value> {
    let workflow_id = record.workflow_id.as_deref()?;
    if record.status != InteractionStatus::Answered {
        return None;
    }
    // Only suspend-created records resume a workflow. A block-mode payload
    // may carry an arbitrary workflow_id (kept for observability only) and
    // must never spawn a resume against a paused sibling workflow.
    if !record.suspended {
        return None;
    }
    let guidance = format!("animus workflow resume {workflow_id}");
    let report_error =
        |error: String| json!({ "workflow_id": workflow_id, "resumed": false, "error": error, "guidance": guidance });
    let hub: Arc<dyn ServiceHub> = match FileServiceHub::new(project_root) {
        Ok(hub) => Arc::new(hub),
        Err(error) => return Some(report_error(format!("failed to open project services: {error:#}"))),
    };
    let workflow = match hub.workflows().get(workflow_id).await {
        Ok(workflow) => workflow,
        Err(error) => return Some(report_error(format!("failed to load workflow: {error:#}"))),
    };
    if workflow.status != WorkflowStatus::Paused {
        return Some(json!({
            "workflow_id": workflow_id,
            "resumed": false,
            "skipped": true,
            "reason": format!("workflow status is {:?}, not paused", workflow.status),
        }));
    }
    let feedback = resume_feedback_for_interaction(record);
    // TODO(codex-p2): when the answer lands while the original
    // workflow-runner process is still draining (agent summarizing before
    // ending its turn), the live-runner guard inside
    // resume_workflow_with_runner rejects this one-shot attempt and the
    // workflow stays paused until the operator runs the guidance command.
    // A daemon-tick retry that resumes paused workflows whose suspended
    // interaction is answered would close that window.
    match crate::services::operations::resume_workflow_with_runner(hub, project_root, workflow_id, Some(feedback)).await
    {
        Ok(_) => Some(json!({ "workflow_id": workflow_id, "resumed": true })),
        Err(error) => Some(report_error(format!("{error:#}"))),
    }
}

/// Pause the workflow a suspend-mode interaction is bound to so the orphan
/// reconciler leaves it alone while the decision is pending, and stamp the
/// interaction id into the phase session checkpoint context. Best-effort:
/// a failure is logged and reported but never fails the interaction.
pub(crate) async fn pause_workflow_for_suspended_interaction(project_root: &str, record: &InteractionRecord) -> bool {
    let Some(workflow_id) = record.workflow_id.as_deref() else {
        return false;
    };
    let hub: Arc<dyn ServiceHub> = match FileServiceHub::new(project_root) {
        Ok(hub) => Arc::new(hub),
        Err(error) => {
            tracing::warn!(workflow_id = %workflow_id, interaction_id = %record.id, error = %format!("{error:#}"), "failed to open project services to pause suspended workflow");
            return false;
        }
    };
    let workflow = match hub.workflows().get(workflow_id).await {
        Ok(workflow) => workflow,
        Err(error) => {
            tracing::warn!(workflow_id = %workflow_id, interaction_id = %record.id, error = %format!("{error:#}"), "failed to load workflow for suspended interaction");
            return false;
        }
    };
    match workflow.status {
        WorkflowStatus::Running | WorkflowStatus::Pending => {
            if let Err(error) = hub.workflows().pause(workflow_id).await {
                tracing::warn!(workflow_id = %workflow_id, interaction_id = %record.id, error = %format!("{error:#}"), "failed to pause workflow for suspended interaction");
                return false;
            }
        }
        WorkflowStatus::Paused => {}
        status => {
            tracing::warn!(workflow_id = %workflow_id, interaction_id = %record.id, status = ?status, "workflow not pausable for suspended interaction");
            return false;
        }
    }
    // Stamp the pending interaction into the phase session checkpoint so
    // `animus workflow list` / `animus status` readers see why the run is
    // paused. The checkpoint may not exist (manual phases, pre-checkpoint
    // failures) — that only loses the breadcrumb, not the pause.
    let phase_id = workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()));
    if let (Some(phase_id), Some(scoped_root)) = (phase_id, protocol::scoped_state_root(Path::new(project_root))) {
        let reason = format!(
            "interaction_pending: {} — answer with `{}` to resume",
            record.id,
            interaction_answer_command(record)
        );
        let _ =
            animus_runtime_shared::phase_session::update_session_blocked(&scoped_root, workflow_id, &phase_id, &reason);
    }
    true
}

// --- External provider-CLI approval hook (`animus agent approve-hook`) ------
//
// Lets non-claude provider CLIs (gemini BeforeTool command hook, an opencode
// plugin, our oai harness) obtain an approval decision from the SAME logic that
// backs the MCP `animus.agent.request_approval` tool: `decide_approval` resolves
// the agent profile's `approval_policy` (auto_allow / auto_deny / default), the
// LLM judge, and the human-escalation/inbox path. claude already routes through
// the MCP tool natively via `--permission-prompt-tool`; this verb is the parity
// on-ramp for everyone else.
//
// NOTE: these outputs are deliberately NOT the `animus.cli.v1` envelope. Each
// provider's command hook parses a fixed raw JSON contract on stdout (gemini in
// particular treats ANY stray stdout as a parse failure and then defaults to
// ALLOW), so the verb must emit exactly the provider's shape on stdout and send
// all diagnostics to stderr. Every failure path FAILS SAFE = deny.

/// The decision the verb resolved, normalized away from the
/// `interaction_tools::ApprovalDecision` type so this module owns its render.
struct HookDecision {
    allow: bool,
    reason: Option<String>,
    updated_input: Option<Value>,
    /// Where the decision came from (policy / llm / human / timeout / error),
    /// surfaced on the generic contract for auditability.
    source: &'static str,
}

/// Parse the stdin payload for the requested format into `(tool_name, input)`.
fn parse_hook_stdin(format: &crate::ApproveHookFormat, stdin: &str) -> Result<(String, Option<Value>)> {
    let value: Value = serde_json::from_str(stdin.trim()).map_err(|err| anyhow!("invalid stdin JSON: {err}"))?;
    let tool_name = value
        .get("tool_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("stdin is missing a non-empty `tool_name`"))?
        .to_string();
    let input = match format {
        // Gemini BeforeTool and claude PreToolUse payloads both nest the tool
        // input under `tool_input`.
        crate::ApproveHookFormat::Gemini | crate::ApproveHookFormat::Claude => value.get("tool_input").cloned(),
        crate::ApproveHookFormat::Generic => value.get("input").cloned(),
    };
    // Treat an explicit JSON `null` the same as absent.
    let input = input.filter(|value| !value.is_null());
    Ok((tool_name, input))
}

/// Render the resolved decision into the requested provider's JSON contract.
/// The caller serializes the returned value to stdout (and nothing else).
fn render_hook_decision(format: &crate::ApproveHookFormat, decision: &HookDecision) -> Value {
    match format {
        // Gemini: allow = `{}` (empty object), deny = `{ decision, reason }`.
        crate::ApproveHookFormat::Gemini => {
            if decision.allow {
                json!({})
            } else {
                json!({ "decision": "deny", "reason": decision.reason.clone().unwrap_or_default() })
            }
        }
        // Claude PreToolUse: both allow and deny emit an explicit
        // `hookSpecificOutput.permissionDecision` so an Animus-approved tool
        // call auto-approves instead of falling through to claude's normal
        // permission flow (which could still prompt or block). Mirrors the
        // in-tree `animus-hook` PreToolUse contract.
        crate::ApproveHookFormat::Claude => {
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": if decision.allow { "allow" } else { "deny" },
                    "permissionDecisionReason": decision.reason.clone().unwrap_or_default(),
                }
            })
        }
        // Generic: always `{ decision, source, reason, updated_input? }`.
        crate::ApproveHookFormat::Generic => {
            let mut payload = json!({
                "decision": if decision.allow { "allow" } else { "deny" },
                "source": decision.source,
                "reason": decision.reason.clone().unwrap_or_default(),
            });
            if let (Value::Object(map), true, Some(updated)) = (&mut payload, decision.allow, &decision.updated_input) {
                map.insert("updated_input".to_string(), updated.clone());
            }
            payload
        }
    }
}

/// Extract the shell command a shell-style provider hook is asking to run, so
/// the approval policy can match deny-list patterns against the COMMAND (not
/// just the stable tool name). Provider hooks for shell tools (claude `Bash`,
/// gemini `run_shell_command`, opencode `bash`) all carry the command under one
/// of these input keys. Returns the trimmed command, or `None` when the input
/// is not a shell-runner shape (a structured tool with a `{file_path, ...}`
/// input has no single "command" to match and is governed by the tool name).
fn hook_command_subject(input: Option<&Value>) -> Option<String> {
    let input = input?.as_object()?;
    for key in ["command", "cmd", "script", "shell_command"] {
        if let Some(command) = input.get(key).and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()) {
            return Some(command.to_string());
        }
    }
    None
}

/// Resolve a single gated tool call into a `HookDecision` by routing through the
/// shared `decide_approval` core. `wait` defaults to "block" (an Ask policy
/// escalates to the inbox and blocks until a human decides, fail-closed on
/// timeout) — same posture as the native prompt-tool default for ad-hoc runs.
async fn resolve_hook_decision(
    project_root: &str,
    args: &crate::AgentApproveHookArgs,
    tool_name: String,
    input: Option<Value>,
) -> HookDecision {
    use crate::services::operations::interaction_tools::{
        approval_decision_from_outcome, decide_approval, effective_timeout_secs, evaluate_policy_subject,
    };
    use orchestrator_config::agent_runtime_config::ApprovalPolicyDecision;

    let timeout_secs = effective_timeout_secs(args.timeout_secs);

    // SAFETY pre-check: shell-style hooks report a stable tool name (`Bash`,
    // `run_shell_command`) with the real command buried in the input.
    // `decide_approval` only matches the tool name, so evaluate the policy
    // against the command too and let the most-restrictive verdict win (deny
    // precedence). Without this, `auto_deny: ["git push*"]` + `default: allow`
    // would let `Bash{command:"git push --force"}` straight through. We only
    // honor the COMMAND's deny here; an allow/ask on the command still defers to
    // the full tool-name flow (policy + LLM + human escalation) below.
    if let Some(command) = hook_command_subject(input.as_ref()) {
        if let Some(ApprovalPolicyDecision::Deny) = evaluate_policy_subject(project_root, &args.agent_id, &command) {
            return HookDecision {
                allow: false,
                reason: Some(format!("denied by the agent profile's approval_policy (command: {command})")),
                updated_input: None,
                source: "policy",
            };
        }
    }

    let action = format!("use tool {tool_name}");
    let outcome = decide_approval(
        project_root,
        &args.agent_id,
        &action,
        Some(&tool_name),
        input,
        None,
        timeout_secs,
        args.workflow_id.as_deref(),
        args.task_id.as_deref(),
    )
    .await;
    let decision = approval_decision_from_outcome(outcome, timeout_secs);
    HookDecision {
        allow: decision.is_allow(),
        reason: decision.message().map(ToOwned::to_owned),
        updated_input: decision.updated_input().cloned(),
        source: decision.source().as_str(),
    }
}

/// Read stdin to a string; an empty/closed stdin is an error (fail-safe deny).
fn read_hook_stdin() -> Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).map_err(|err| anyhow!("failed to read stdin: {err}"))?;
    if buf.trim().is_empty() {
        return Err(anyhow!("stdin was empty"));
    }
    Ok(buf)
}

/// `animus agent approve-hook` entry point. Always exits 0 and always prints a
/// well-formed decision in the requested format; any internal error is rendered
/// as a fail-safe DENY (with the reason on stderr).
pub(crate) async fn handle_agent_approve_hook(args: crate::AgentApproveHookArgs, project_root: &str) -> Result<()> {
    let decision = match read_hook_stdin().and_then(|stdin| parse_hook_stdin(&args.format, &stdin)) {
        Ok((tool_name, input)) => resolve_hook_decision(project_root, &args, tool_name, input).await,
        Err(err) => {
            // FAIL SAFE: malformed/absent input denies. Diagnostics to stderr
            // only — stdout must carry exactly the decision JSON.
            eprintln!("animus agent approve-hook: {err}; denying (fail safe)");
            HookDecision {
                allow: false,
                reason: Some(format!("approval hook error: {err}")),
                updated_input: None,
                source: "error",
            }
        }
    };
    let payload = render_hook_decision(&args.format, &decision);
    // Compact single-line JSON on stdout; nothing else.
    println!("{}", serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_runtime_shared::InteractionStatus;

    use crate::ApproveHookFormat;

    #[test]
    fn hook_parse_gemini_reads_tool_input() {
        let (tool, input) =
            parse_hook_stdin(&ApproveHookFormat::Gemini, r#"{"tool_name":"Bash","tool_input":{"cmd":"ls"}}"#)
                .expect("parse");
        assert_eq!(tool, "Bash");
        assert_eq!(input, Some(json!({"cmd":"ls"})));
    }

    #[test]
    fn hook_parse_generic_reads_input_and_tolerates_absent() {
        let (tool, input) = parse_hook_stdin(&ApproveHookFormat::Generic, r#"{"tool_name":"echo"}"#).expect("parse");
        assert_eq!(tool, "echo");
        assert_eq!(input, None);
    }

    #[test]
    fn hook_parse_rejects_missing_tool_name() {
        assert!(parse_hook_stdin(&ApproveHookFormat::Generic, r#"{"input":{}}"#).is_err());
        assert!(parse_hook_stdin(&ApproveHookFormat::Gemini, "not json").is_err());
    }

    #[test]
    fn hook_render_gemini_allow_is_empty_object() {
        let allow = HookDecision { allow: true, reason: Some("ok".into()), updated_input: None, source: "policy" };
        // Gemini ALLOW must be exactly `{}` — any extra field risks the CLI
        // mis-parsing and defaulting to allow anyway, but we keep it strict.
        assert_eq!(render_hook_decision(&ApproveHookFormat::Gemini, &allow), json!({}));
    }

    #[test]
    fn hook_render_gemini_deny_carries_reason() {
        let deny =
            HookDecision { allow: false, reason: Some("too risky".into()), updated_input: None, source: "policy" };
        assert_eq!(
            render_hook_decision(&ApproveHookFormat::Gemini, &deny),
            json!({ "decision": "deny", "reason": "too risky" })
        );
    }

    #[test]
    fn hook_parse_claude_reads_tool_input() {
        let (tool, input) =
            parse_hook_stdin(&ApproveHookFormat::Claude, r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#)
                .expect("parse");
        assert_eq!(tool, "Bash");
        assert_eq!(input, Some(json!({"command":"echo hi"})));
    }

    #[test]
    fn hook_render_claude_deny_shape() {
        let deny = HookDecision { allow: false, reason: Some("nope".into()), updated_input: None, source: "policy" };
        assert_eq!(
            render_hook_decision(&ApproveHookFormat::Claude, &deny),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "nope"
                }
            })
        );
    }

    #[test]
    fn hook_render_claude_allow_emits_explicit_allow() {
        let allow = HookDecision { allow: true, reason: Some("ok".into()), updated_input: None, source: "policy" };
        assert_eq!(
            render_hook_decision(&ApproveHookFormat::Claude, &allow),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "ok"
                }
            })
        );
    }

    // An error-source fail-safe deny (allow:false) must render to claude's
    // deny shape just like a policy deny.
    #[test]
    fn hook_render_claude_failsafe_error_deny_shape() {
        let deny = HookDecision {
            allow: false,
            reason: Some("approval hook error: stdin was empty".into()),
            updated_input: None,
            source: "error",
        };
        assert_eq!(
            render_hook_decision(&ApproveHookFormat::Claude, &deny),
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "approval hook error: stdin was empty"
                }
            })
        );
    }

    #[test]
    fn hook_render_generic_both_ways() {
        let allow = HookDecision {
            allow: true,
            reason: Some("policy allow".into()),
            updated_input: Some(json!({"cmd":"ls"})),
            source: "policy",
        };
        assert_eq!(
            render_hook_decision(&ApproveHookFormat::Generic, &allow),
            json!({ "decision": "allow", "source": "policy", "reason": "policy allow", "updated_input": {"cmd":"ls"} })
        );

        let deny = HookDecision { allow: false, reason: Some("nope".into()), updated_input: None, source: "timeout" };
        assert_eq!(
            render_hook_decision(&ApproveHookFormat::Generic, &deny),
            json!({ "decision": "deny", "source": "timeout", "reason": "nope" })
        );
    }

    #[test]
    fn hook_command_subject_extracts_shell_command() {
        assert_eq!(
            hook_command_subject(Some(&json!({"command":"git push --force"}))),
            Some("git push --force".to_string())
        );
        assert_eq!(hook_command_subject(Some(&json!({"cmd":" ls "}))), Some("ls".to_string()));
        // Structured (non-shell) tool input -> no single command to match.
        assert_eq!(hook_command_subject(Some(&json!({"file_path":"/x","content":"y"}))), None);
        // Empty / absent.
        assert_eq!(hook_command_subject(Some(&json!({"command":"  "}))), None);
        assert_eq!(hook_command_subject(None), None);
    }

    #[test]
    fn hook_render_generic_omits_updated_input_on_deny() {
        // A deny never carries updated_input even if one was somehow present.
        let deny = HookDecision {
            allow: false,
            reason: Some("nope".into()),
            updated_input: Some(json!({"x":1})),
            source: "policy",
        };
        let rendered = render_hook_decision(&ApproveHookFormat::Generic, &deny);
        assert!(rendered.get("updated_input").is_none());
    }

    // Regression for the codex [P1]: a shell-style hook reports a stable tool
    // name (`Bash`) with the dangerous command in the input. The COMMAND must
    // be matched against the deny list even when the tool name is allowed and
    // the policy default is `allow`.
    #[test]
    fn resolve_hook_denies_shell_command_matching_deny_list_under_default_allow() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread runtime").block_on(
            async {
                let project = tempfile::tempdir().expect("project tempdir");
                init_git_repo(project.path());
                let project_root = project.path().to_string_lossy().to_string();
                std::fs::create_dir_all(project.path().join(".animus")).expect("create .animus");
                std::fs::write(
                    project.path().join(".animus").join("workflows.yaml"),
                    r#"
tools_allowlist:
  - ls
agents:
  swe:
    system_prompt: Build the change.
    approval_policy:
      auto_allow: []
      auto_deny: ["git push*", "rm *"]
      default: allow
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

                let args = crate::AgentApproveHookArgs {
                    agent_id: "swe".to_string(),
                    format: crate::ApproveHookFormat::Generic,
                    workflow_id: None,
                    task_id: None,
                    timeout_secs: Some(1),
                };

                // Bash tool name is NOT in the deny list, default is allow, but
                // the command IS denied -> the command pre-check must win.
                let denied = resolve_hook_decision(
                    &project_root,
                    &args,
                    "Bash".to_string(),
                    Some(json!({"command":"git push --force origin main"})),
                )
                .await;
                assert!(!denied.allow, "command-level deny must override an allowed tool name");
                assert_eq!(denied.source, "policy");

                // A benign command falls through to the (default: allow) flow.
                let allowed =
                    resolve_hook_decision(&project_root, &args, "Bash".to_string(), Some(json!({"command":"ls -la"})))
                        .await;
                assert!(allowed.allow, "a non-denied command defers to the default-allow policy");
                assert_eq!(allowed.source, "policy");

                // No pending interaction was written for either short-circuit.
                let pending = animus_runtime_shared::list_interactions(&project_root, true, None).expect("list");
                assert!(pending.is_empty(), "policy short-circuits must not escalate");
            },
        );
    }

    #[test]
    fn answer_op_round_trips_question_and_approval() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");
        let project_root = project.path().to_string_lossy().to_string();

        let question = animus_runtime_shared::create_question_interaction(
            &project_root,
            "swe",
            "Which branch?",
            &[],
            None,
            None,
            None,
        )
        .expect("create question");
        let err = answer_interaction_op(
            &project_root,
            &question.id,
            &AnswerOptions { allow: true, ..AnswerOptions::default() },
        )
        .expect_err("question with --allow");
        assert!(err.to_string().contains("--text"));
        let answered = answer_interaction_op(
            &project_root,
            &question.id,
            &AnswerOptions { text: Some("main".to_string()), ..AnswerOptions::default() },
        )
        .expect("answer question");
        assert_eq!(answered.status, InteractionStatus::Answered);
        assert_eq!(answered.answer.as_deref(), Some("main"));

        let approval = animus_runtime_shared::create_approval_interaction(
            &project_root,
            "swe",
            "force push",
            Some("git"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create approval");
        let err = answer_interaction_op(&project_root, &approval.id, &AnswerOptions::default())
            .expect_err("approval without decision");
        assert!(err.to_string().contains("--allow or --deny"));
        let denied = answer_interaction_op(
            &project_root,
            &approval.id,
            &AnswerOptions {
                deny: true,
                message: Some("not now".to_string()),
                answered_by: Some("sami".to_string()),
                ..AnswerOptions::default()
            },
        )
        .expect("deny approval");
        assert_eq!(denied.answer.as_deref(), Some("deny"));
        assert_eq!(denied.answer_message.as_deref(), Some("not now"));
        assert_eq!(denied.answered_by.as_deref(), Some("sami"));
    }

    #[test]
    fn structured_question_select_and_text_mapping() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");
        let project_root = project.path().to_string_lossy().to_string();

        let raw_input = serde_json::json!({
            "questions": [
                {
                    "question": "How should I format the output?",
                    "header": "Format",
                    "options": [{ "label": "Summary" }, { "label": "Detailed" }],
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
        let questions = animus_runtime_shared::parse_sdk_questions(&raw_input).expect("parse questions");

        // Selects by header, by index, multi-label, plus a freeform note.
        let record = animus_runtime_shared::create_native_question_interaction(
            &project_root,
            "swe",
            questions.clone(),
            raw_input.clone(),
            None,
            None,
            None,
            None,
        )
        .expect("create structured question");
        let answered = answer_interaction_op(
            &project_root,
            &record.id,
            &AnswerOptions {
                selects: vec!["Format=Summary".to_string(), "2=Introduction,Conclusion".to_string()],
                text: Some("Keep it short".to_string()),
                ..AnswerOptions::default()
            },
        )
        .expect("structured answer");
        let answers = answered.answers.clone().expect("answers stored");
        assert_eq!(answers.get("How should I format the output?"), Some(&Value::String("Summary".to_string())));
        assert_eq!(
            answers.get("Which sections should I include?"),
            Some(&serde_json::json!(["Introduction", "Conclusion"]))
        );
        assert_eq!(answered.response.as_deref(), Some("Keep it short"));
        assert!(answered.answer.as_deref().is_some_and(|summary| summary.contains("Summary")));

        // Single-question record: bare --text maps to that question's answer.
        let single_input = serde_json::json!({
            "questions": [{ "question": "Proceed?", "options": [{ "label": "Yes" }, { "label": "No" }] }]
        });
        let single_questions = animus_runtime_shared::parse_sdk_questions(&single_input).expect("parse single");
        let single = animus_runtime_shared::create_native_question_interaction(
            &project_root,
            "swe",
            single_questions,
            single_input,
            None,
            None,
            None,
            None,
        )
        .expect("create single question");
        let answered = answer_interaction_op(
            &project_root,
            &single.id,
            &AnswerOptions { text: Some("use jquery".to_string()), ..AnswerOptions::default() },
        )
        .expect("bare text on single question");
        assert_eq!(answered.answers.clone().expect("answers")["Proceed?"], Value::String("use jquery".to_string()));
        assert!(answered.response.is_none());

        // Multi-question record: bare --text becomes the freeform response.
        let record = animus_runtime_shared::create_native_question_interaction(
            &project_root,
            "swe",
            questions,
            raw_input,
            None,
            None,
            None,
            None,
        )
        .expect("create second structured question");
        let answered = answer_interaction_op(
            &project_root,
            &record.id,
            &AnswerOptions { text: Some("just do whatever is fastest".to_string()), ..AnswerOptions::default() },
        )
        .expect("bare text on multi-question");
        assert!(answered.answers.is_none());
        assert_eq!(answered.response.as_deref(), Some("just do whatever is fastest"));
    }

    #[test]
    fn structured_question_select_errors() {
        let raw_input = serde_json::json!({
            "questions": [
                { "question": "Proceed?", "header": "Go", "options": [{ "label": "Yes" }, { "label": "No" }] }
            ]
        });
        let questions = animus_runtime_shared::parse_sdk_questions(&raw_input).expect("parse questions");
        assert!(resolve_select(&questions, "Proceed?=Yes").is_ok());
        assert!(resolve_select(&questions, "go=Yes").is_ok(), "header match is case-insensitive");
        assert!(resolve_select(&questions, "1=Yes").is_ok());
        assert!(resolve_select(&questions, "2=Yes").is_err(), "index out of range");
        assert!(resolve_select(&questions, "0=Yes").is_err(), "index is 1-based");
        assert!(resolve_select(&questions, "Nope?=Yes").is_err(), "unknown question");
        assert!(resolve_select(&questions, "Proceed?=").is_err(), "missing label");
        assert!(resolve_select(&questions, "Proceed?").is_err(), "missing '='");
    }

    #[test]
    fn approval_answer_remember_echoes_local_settings_suggestions() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");
        let project_root = project.path().to_string_lossy().to_string();

        let suggestions = serde_json::json!([
            { "type": "addRules", "behavior": "allow", "destination": "localSettings" },
            { "type": "addRules", "behavior": "allow", "destination": "session" }
        ]);
        let approval = animus_runtime_shared::create_approval_interaction(
            &project_root,
            "swe",
            "run migration",
            Some("Bash"),
            Some(serde_json::json!({ "command": "migrate" })),
            Some(suggestions.clone()),
            None,
            None,
            None,
        )
        .expect("create approval");

        let answered = answer_interaction_op(
            &project_root,
            &approval.id,
            &AnswerOptions {
                allow: true,
                remember: true,
                updated_input: Some(serde_json::json!({ "command": "migrate --safe" })),
                ..AnswerOptions::default()
            },
        )
        .expect("allow with remember");
        assert_eq!(answered.updated_input, Some(serde_json::json!({ "command": "migrate --safe" })));
        assert_eq!(answered.updated_permissions, Some(serde_json::json!([suggestions[0].clone()])));
    }

    #[test]
    fn resume_feedback_templates() {
        let mut question = animus_runtime_shared::InteractionRecord {
            id: "q-1".to_string(),
            kind: InteractionKind::Question,
            agent_id: "swe".to_string(),
            workflow_id: Some("wf-1".to_string()),
            task_id: None,
            created_at: "2026-06-11T00:00:00Z".to_string(),
            question: Some("Migrate in place or copy table?".to_string()),
            action: None,
            options: Vec::new(),
            tool_name: None,
            arguments: None,
            questions: Vec::new(),
            suggestions: None,
            timeout_secs: None,
            suspended: false,
            status: InteractionStatus::Answered,
            answer: Some("copy table".to_string()),
            answer_message: None,
            answers: None,
            response: None,
            updated_input: None,
            updated_permissions: None,
            answered_at: None,
            answered_by: Some("sami".to_string()),
        };
        assert_eq!(
            resume_feedback_for_interaction(&question),
            "Answer to your question \"Migrate in place or copy table?\": copy table. Continue."
        );

        question.kind = InteractionKind::Approval;
        question.question = None;
        question.action = Some("git push --force".to_string());
        question.answer = Some("allow".to_string());
        assert_eq!(resume_feedback_for_interaction(&question), "Approval granted for git push --force. Continue.");

        question.answer = Some("deny".to_string());
        question.answer_message = Some("too risky".to_string());
        assert_eq!(
            resume_feedback_for_interaction(&question),
            "Approval denied for git push --force: too risky. Continue."
        );
    }

    #[test]
    fn resume_feedback_carries_structured_answers() {
        let raw_input = serde_json::json!({
            "questions": [
                { "question": "Format?", "header": "Format", "options": [{ "label": "Summary" }], "multiSelect": false },
                { "question": "Sections?", "header": "Sections", "options": [{ "label": "Intro" }], "multiSelect": true }
            ]
        });
        let questions = animus_runtime_shared::parse_sdk_questions(&raw_input).expect("parse questions");
        let mut answers = BTreeMap::new();
        answers.insert("Format?".to_string(), Value::String("Summary".to_string()));
        answers.insert("Sections?".to_string(), serde_json::json!(["Intro", "Conclusion"]));
        let mut record = animus_runtime_shared::InteractionRecord {
            id: "q-3".to_string(),
            kind: InteractionKind::Question,
            agent_id: "swe".to_string(),
            workflow_id: Some("wf-1".to_string()),
            task_id: None,
            created_at: "2026-06-11T00:00:00Z".to_string(),
            question: Some("Format? | Sections?".to_string()),
            action: None,
            options: Vec::new(),
            tool_name: Some("AskUserQuestion".to_string()),
            arguments: Some(raw_input),
            questions,
            suggestions: None,
            timeout_secs: None,
            suspended: true,
            status: InteractionStatus::Answered,
            answer: Some("Format?: Summary; Sections?: Intro, Conclusion".to_string()),
            answer_message: None,
            answers: Some(answers),
            response: Some("Keep it short".to_string()),
            updated_input: None,
            updated_permissions: None,
            answered_at: None,
            answered_by: Some("sami".to_string()),
        };
        let feedback = resume_feedback_for_interaction(&record);
        assert!(feedback.contains("The user answered your questions:"), "{feedback}");
        assert!(feedback.contains("- \"Format?\": Summary"), "{feedback}");
        assert!(feedback.contains("- \"Sections?\": Intro, Conclusion"), "{feedback}");
        assert!(feedback.contains("Additional note from the user: Keep it short"), "{feedback}");
        assert!(feedback.ends_with("Continue."), "{feedback}");

        // Response-only answers render the SDK "The user responded" form.
        record.answers = None;
        record.response = Some("just ship it".to_string());
        let feedback = resume_feedback_for_interaction(&record);
        assert_eq!(feedback, "The user responded to your questions: just ship it. Continue.");
    }

    #[test]
    fn interaction_event_payload_carries_summary_and_answer_command() {
        let record = animus_runtime_shared::InteractionRecord {
            id: "q-2".to_string(),
            kind: InteractionKind::Question,
            agent_id: "swe".to_string(),
            workflow_id: None,
            task_id: None,
            created_at: "2026-06-11T00:00:00Z".to_string(),
            question: Some("Which branch?".to_string()),
            action: None,
            options: Vec::new(),
            tool_name: None,
            arguments: None,
            questions: Vec::new(),
            suggestions: None,
            timeout_secs: None,
            suspended: false,
            status: InteractionStatus::Pending,
            answer: None,
            answer_message: None,
            answers: None,
            response: None,
            updated_input: None,
            updated_permissions: None,
            answered_at: None,
            answered_by: None,
        };
        let data = interaction_event_data(&record);
        assert_eq!(data.pointer("/summary").and_then(Value::as_str), Some("agent 'swe' asks: Which branch?"));
        assert_eq!(
            data.pointer("/answer_command").and_then(Value::as_str),
            Some("animus agent interactions answer q-2 --text \"<answer>\"")
        );

        let approval = animus_runtime_shared::InteractionRecord {
            id: "a-1".to_string(),
            kind: InteractionKind::Approval,
            action: Some("rotate keys".to_string()),
            question: None,
            ..record
        };
        let data = interaction_event_data(&approval);
        assert_eq!(
            data.pointer("/summary").and_then(Value::as_str),
            Some("agent 'swe' requests approval: rotate keys")
        );
        assert_eq!(
            data.pointer("/answer_command").and_then(Value::as_str),
            Some("animus agent interactions answer a-1 --allow")
        );
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

    async fn bootstrap_workflow(hub: &Arc<dyn ServiceHub>) -> orchestrator_core::OrchestratorWorkflow {
        let task = hub
            .tasks()
            .create(orchestrator_core::TaskCreateInput {
                title: "resume on answer".to_string(),
                description: "resume-with-answer test".to_string(),
                task_type: Some(orchestrator_core::TaskType::Feature),
                priority: Some(orchestrator_core::Priority::Medium),
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

    // Resume-spawn failures must never fail the answer: with no
    // workflow_runner plugin installed, the answer still lands and the
    // report carries the exact `animus workflow resume <id>` guidance.
    #[test]
    fn answer_with_resume_survives_resume_failure_and_returns_guidance() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread runtime").block_on(
            async {
                let project = tempfile::tempdir().expect("project tempdir");
                init_git_repo(project.path());
                let project_root = project.path().to_string_lossy().to_string();
                let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
                let _config_source_seam =
                    orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                        project.path(),
                    );
                let workflow = bootstrap_workflow(&hub).await;
                hub.workflows().pause(&workflow.id).await.expect("workflow pauses");

                let approval = animus_runtime_shared::create_approval_interaction(
                    &project_root,
                    "swe",
                    "force push",
                    None,
                    None,
                    None,
                    None,
                    Some(&workflow.id),
                    None,
                )
                .expect("create approval");
                animus_runtime_shared::mark_interaction_suspended(&project_root, &approval.id).expect("mark suspended");

                let (record, resume) = answer_interaction_op_with_resume(
                    &project_root,
                    &approval.id,
                    AnswerOptions {
                        allow: true,
                        message: Some("go ahead".to_string()),
                        answered_by: Some("sami".to_string()),
                        ..AnswerOptions::default()
                    },
                )
                .await
                .expect("answer must succeed even when the resume spawn fails");
                assert_eq!(record.status, InteractionStatus::Answered);

                let resume = resume.expect("resume attempt reported for a workflow-bound record");
                assert_eq!(resume.pointer("/resumed").and_then(Value::as_bool), Some(false));
                assert_eq!(
                    resume.pointer("/guidance").and_then(Value::as_str),
                    Some(format!("animus workflow resume {}", workflow.id).as_str())
                );
                assert!(
                    resume.pointer("/error").and_then(Value::as_str).is_some_and(|e| e.contains("workflow_runner")),
                    "the missing-plugin error should surface: {resume}"
                );

                let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow reloads");
                assert_eq!(reloaded.status, WorkflowStatus::Paused, "failed resume leaves the workflow paused");
            },
        );
    }

    // A workflow-bound answer against a Running (not suspended) workflow
    // must skip the resume rather than fight the live runner.
    #[test]
    fn answer_with_resume_skips_non_paused_workflows() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread runtime").block_on(
            async {
                let project = tempfile::tempdir().expect("project tempdir");
                init_git_repo(project.path());
                let project_root = project.path().to_string_lossy().to_string();
                let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
                let _config_source_seam =
                    orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                        project.path(),
                    );
                let workflow = bootstrap_workflow(&hub).await;
                assert_eq!(workflow.status, WorkflowStatus::Running);

                // A block-mode record carrying a workflow_id (untrusted
                // payload) must not even attempt a resume.
                let block_mode = animus_runtime_shared::create_question_interaction(
                    &project_root,
                    "swe",
                    "Proceed?",
                    &[],
                    None,
                    Some(&workflow.id),
                    None,
                )
                .expect("create block-mode question");
                let (record, resume) = answer_interaction_op_with_resume(
                    &project_root,
                    &block_mode.id,
                    AnswerOptions { text: Some("yes".to_string()), ..AnswerOptions::default() },
                )
                .await
                .expect("answer succeeds");
                assert_eq!(record.status, InteractionStatus::Answered);
                assert!(resume.is_none(), "non-suspended records never trigger a resume attempt");

                // A suspend-created record against a Running workflow skips
                // the resume rather than fight the live runner.
                let suspended = animus_runtime_shared::create_question_interaction(
                    &project_root,
                    "swe",
                    "Still proceed?",
                    &[],
                    None,
                    Some(&workflow.id),
                    None,
                )
                .expect("create suspended question");
                animus_runtime_shared::mark_interaction_suspended(&project_root, &suspended.id)
                    .expect("mark suspended");
                let (record, resume) = answer_interaction_op_with_resume(
                    &project_root,
                    &suspended.id,
                    AnswerOptions { text: Some("yes".to_string()), ..AnswerOptions::default() },
                )
                .await
                .expect("answer succeeds");
                assert_eq!(record.status, InteractionStatus::Answered);
                let resume = resume.expect("resume attempt reported");
                assert_eq!(resume.pointer("/skipped").and_then(Value::as_bool), Some(true));
                assert_eq!(resume.pointer("/resumed").and_then(Value::as_bool), Some(false));

                let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow reloads");
                assert_eq!(reloaded.status, WorkflowStatus::Running);
            },
        );
    }
}
