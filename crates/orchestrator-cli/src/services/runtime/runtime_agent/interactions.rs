use anyhow::{anyhow, Result};
use serde_json::json;

use animus_runtime_shared::{InteractionKind, InteractionRecord};
use orchestrator_daemon_runtime::DaemonEventLog;

use crate::{print_value, AgentInteractionsAnswerArgs, AgentInteractionsListArgs, AgentInteractionsShowArgs};

// Best-effort observability tee into the daemon event log. The log is a plain
// jsonl file under the global Animus state dir, so emission works without a
// running daemon; failures never block the interaction round-trip itself.
pub(crate) fn emit_interaction_event(event_type: &str, project_root: &str, record: &InteractionRecord) {
    let canonical_root = crate::services::runtime::canonicalize_lossy(project_root);
    let mut seq = 0;
    let event = DaemonEventLog::next_event(
        &mut seq,
        event_type,
        Some(canonical_root),
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
        }),
    );
    let _ = DaemonEventLog::append(&event);
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

pub(super) fn handle_agent_interactions_answer(
    args: AgentInteractionsAnswerArgs,
    project_root: &str,
    json_output: bool,
) -> Result<()> {
    let record = answer_interaction_op(
        project_root,
        &args.id,
        args.text.as_deref(),
        args.allow,
        args.deny,
        args.message.as_deref(),
        args.answered_by.as_deref(),
    )?;
    print_value(record, json_output)
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
}
