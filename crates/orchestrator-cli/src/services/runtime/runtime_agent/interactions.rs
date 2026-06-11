use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use animus_runtime_shared::{InteractionKind, InteractionRecord, InteractionStatus};
use orchestrator_core::{services::ServiceHub, FileServiceHub, WorkflowStatus};
use orchestrator_daemon_runtime::DaemonEventLog;

use crate::{print_value, AgentInteractionsAnswerArgs, AgentInteractionsListArgs, AgentInteractionsShowArgs};

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
    print_value(json!({ "count": interactions.len(), "interactions": interactions }), json_output)
}

pub(super) fn handle_agent_interactions_show(
    args: AgentInteractionsShowArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let record = animus_runtime_shared::load_interaction(project_root, &args.id)?
        .ok_or_else(|| anyhow!("no interaction with id '{}'", args.id))?;
    print_value(record, json_output)
}

pub(super) async fn handle_agent_interactions_answer(
    args: AgentInteractionsAnswerArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let (record, workflow_resume) = answer_interaction_op_with_resume(
        project_root,
        &args.id,
        args.text.as_deref(),
        args.allow,
        args.deny,
        args.message.as_deref(),
        args.answered_by.as_deref(),
    )
    .await?;
    let mut payload = serde_json::to_value(&record)?;
    if let (Value::Object(map), Some(resume)) = (&mut payload, workflow_resume) {
        map.insert("workflow_resume".to_string(), resume);
    }
    print_value(payload, json_output)
}

pub(crate) fn answer_interaction_op(
    project_root: &str,
    id: &str,
    text: Option<&str>,
    allow: bool,
    deny: bool,
    message: Option<&str>,
    answered_by: Option<&str>,
) -> Result<InteractionRecord> {
    let record = animus_runtime_shared::load_interaction(project_root, id)?
        .ok_or_else(|| anyhow!("no interaction with id '{}'", id))?;
    let answer = match record.kind {
        InteractionKind::Question => {
            if allow || deny {
                return Err(anyhow!("interaction '{}' is a question; answer it with --text", id));
            }
            text.map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("a question answer requires --text"))?
                .to_string()
        }
        InteractionKind::Approval => match (allow, deny) {
            (true, false) => animus_runtime_shared::INTERACTION_ANSWER_ALLOW.to_string(),
            (false, true) => animus_runtime_shared::INTERACTION_ANSWER_DENY.to_string(),
            _ => return Err(anyhow!("an approval answer requires exactly one of --allow or --deny")),
        },
    };
    let answered = animus_runtime_shared::answer_interaction(project_root, id, &answer, message, answered_by)?;
    emit_interaction_event("interaction_answered", project_root, &answered);
    Ok(answered)
}

// Shared answer path for the CLI inbox and the `animus.interactions.answer`
// MCP tool: answer the record, then — when it carries a workflow id and that
// workflow is suspended — trigger the detached-runner resume with the
// decision as feedback. Resume failures never fail the answer; they come
// back in the second tuple slot with the exact `animus workflow resume`
// command as guidance.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn answer_interaction_op_with_resume(
    project_root: &str,
    id: &str,
    text: Option<&str>,
    allow: bool,
    deny: bool,
    message: Option<&str>,
    answered_by: Option<&str>,
) -> Result<(InteractionRecord, Option<Value>)> {
    let record = answer_interaction_op(project_root, id, text, allow, deny, message, answered_by)?;
    let workflow_resume = resume_workflow_for_answered_interaction(project_root, &record).await;
    Ok((record, workflow_resume))
}

/// Resume prompt carried as workflow feedback so the runner can hand the
/// decision to the resumed provider session.
pub(crate) fn resume_feedback_for_interaction(record: &InteractionRecord) -> String {
    match record.kind {
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

#[cfg(test)]
mod tests {
    use super::*;
    use animus_runtime_shared::InteractionStatus;

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
        let err = answer_interaction_op(&project_root, &question.id, None, true, false, None, None)
            .expect_err("question with --allow");
        assert!(err.to_string().contains("--text"));
        let answered = answer_interaction_op(&project_root, &question.id, Some("main"), false, false, None, None)
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
        )
        .expect("create approval");
        let err = answer_interaction_op(&project_root, &approval.id, None, false, false, None, None)
            .expect_err("approval without decision");
        assert!(err.to_string().contains("--allow or --deny"));
        let denied =
            answer_interaction_op(&project_root, &approval.id, None, false, true, Some("not now"), Some("sami"))
                .expect("deny approval");
        assert_eq!(denied.answer.as_deref(), Some("deny"));
        assert_eq!(denied.answer_message.as_deref(), Some("not now"));
        assert_eq!(denied.answered_by.as_deref(), Some("sami"));
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
            timeout_secs: None,
            suspended: false,
            status: InteractionStatus::Answered,
            answer: Some("copy table".to_string()),
            answer_message: None,
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
            timeout_secs: None,
            suspended: false,
            status: InteractionStatus::Pending,
            answer: None,
            answer_message: None,
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
                let workflow = bootstrap_workflow(&hub).await;
                hub.workflows().pause(&workflow.id).await.expect("workflow pauses");

                let approval = animus_runtime_shared::create_approval_interaction(
                    &project_root,
                    "swe",
                    "force push",
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
                    None,
                    true,
                    false,
                    Some("go ahead"),
                    Some("sami"),
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
                    Some("yes"),
                    false,
                    false,
                    None,
                    None,
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
                    Some("yes"),
                    false,
                    false,
                    None,
                    None,
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
