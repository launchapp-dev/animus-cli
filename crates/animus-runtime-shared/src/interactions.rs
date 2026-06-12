use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const INTERACTIONS_DIR: &str = "interactions";

pub const INTERACTION_ANSWER_ALLOW: &str = "allow";
pub const INTERACTION_ANSWER_DENY: &str = "deny";

/// One choice inside a structured (SDK `AskUserQuestion`) question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractionQuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A structured question mirroring the Claude Agent SDK `AskUserQuestion`
/// input shape (`questions[]`). The `multiSelect` alias accepts the SDK's
/// camelCase wire form; records serialize snake_case.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractionQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<InteractionQuestionOption>,
    #[serde(default, alias = "multiSelect")]
    pub multi_select: bool,
}

/// Parse the SDK `AskUserQuestion` tool input (`{ questions: [...] }`) into
/// structured questions. Unknown per-question fields (e.g. `preview`) are
/// ignored here but preserved verbatim on the record's `arguments`.
pub fn parse_sdk_questions(input: &Value) -> Result<Vec<InteractionQuestion>> {
    let questions = input.get("questions").ok_or_else(|| anyhow!("AskUserQuestion input has no `questions` array"))?;
    let parsed: Vec<InteractionQuestion> = serde_json::from_value(questions.clone())
        .map_err(|err| anyhow!("failed to parse AskUserQuestion `questions`: {err}"))?;
    anyhow::ensure!(!parsed.is_empty(), "AskUserQuestion `questions` array is empty");
    for question in &parsed {
        anyhow::ensure!(!question.question.trim().is_empty(), "AskUserQuestion contains an empty question");
    }
    Ok(parsed)
}

/// Full answer payload for [`apply_interaction_answer`]. `answer` is the flat
/// back-compat summary (required, non-empty); the structured fields are only
/// meaningful for the record kinds that carry them and are validated there.
#[derive(Debug, Clone, Default)]
pub struct InteractionAnswer {
    pub answer: String,
    pub message: Option<String>,
    pub answered_by: Option<String>,
    /// Structured per-question answers keyed by exact question text
    /// (SDK `answers` contract: label, array of labels, or free text).
    pub answers: Option<BTreeMap<String, Value>>,
    /// Freeform reply that is not an answer to any specific question
    /// (SDK `response` contract).
    pub response: Option<String>,
    /// Operator-modified tool input echoed as `updatedInput` on an allowed
    /// approval (defaults to the original input when absent).
    pub updated_input: Option<Value>,
    /// Permission suggestions echoed as `updatedPermissions` on an allowed
    /// approval.
    pub updated_permissions: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Question,
    Approval,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    Pending,
    Answered,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRecord {
    pub id: String,
    pub kind: InteractionKind,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// Structured questions (SDK `AskUserQuestion`). Empty for flat
    /// `animus.agent.ask` questions and for approvals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<InteractionQuestion>,
    /// Permission suggestions passed by the prompt-tool caller (SDK
    /// `PermissionUpdate[]`); echoed back as `updatedPermissions` when the
    /// answer asks to remember the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// True only for records created through the suspend wait mode (workflow
    /// pinned on the serving MCP process). The answer paths only trigger a
    /// workflow resume for suspended records, so an untrusted block-mode
    /// payload `workflow_id` can never resume a sibling workflow.
    #[serde(default)]
    pub suspended: bool,
    pub status: InteractionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_message: Option<String>,
    /// Structured per-question answers keyed by question text (string or
    /// array-of-labels values; SDK `answers` contract).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<BTreeMap<String, Value>>,
    /// Freeform reply not tied to a specific question (SDK `response`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// Operator-modified tool input echoed as `updatedInput` on an allowed
    /// approval; absent means pass the original input through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    /// Permission suggestions echoed as `updatedPermissions` on an allowed
    /// approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_permissions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
}

fn scoped_state_base(project_root: &str) -> PathBuf {
    let path = Path::new(project_root);
    protocol::scoped_state_root(path).unwrap_or_else(|| path.join(".animus"))
}

fn interactions_dir(project_root: &str) -> PathBuf {
    scoped_state_base(project_root).join(INTERACTIONS_DIR)
}

fn sanitize_interaction_id(value: &str) -> String {
    value.chars().map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '_' }).collect()
}

fn interaction_path(project_root: &str, id: &str) -> PathBuf {
    interactions_dir(project_root).join(format!("{}.json", sanitize_interaction_id(id)))
}

fn normalize_opt(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
}

// Same lock-file + exclusive-flock pattern as `agent_state::with_state_file_lock`:
// `.lock` sidecar next to the interaction file, created on demand, never deleted.
fn with_interaction_file_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    use fs2::FileExt;

    let lock_path = path
        .with_file_name(format!("{}.lock", path.file_name().and_then(|name| name.to_str()).unwrap_or("interaction")));
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open interaction lock at {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed to acquire interaction lock at {}", lock_path.display()))?;
    let result = f();
    let _ = lock_file.unlock();
    result
}

fn write_interaction_atomic(path: &Path, record: &InteractionRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let payload = serde_json::to_string_pretty(record)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("interaction"),
        Uuid::new_v4()
    ));
    {
        use std::io::Write;
        let mut file =
            std::fs::File::create(&tmp_path).with_context(|| format!("failed to create {}", tmp_path.display()))?;
        file.write_all(payload.as_bytes()).with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.sync_all().with_context(|| format!("failed to fsync {}", tmp_path.display()))?;
    }
    orchestrator_core::store::fsync_rename(&tmp_path, path)
        .with_context(|| format!("failed to durably rename {} -> {}", tmp_path.display(), path.display()))?;
    Ok(())
}

fn read_interaction(path: &Path) -> Result<InteractionRecord> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
pub fn create_question_interaction(
    project_root: &str,
    agent_id: &str,
    question: &str,
    options: &[String],
    timeout_secs: Option<u64>,
    workflow_id: Option<&str>,
    task_id: Option<&str>,
) -> Result<InteractionRecord> {
    let agent_id = agent_id.trim();
    let question = question.trim();
    anyhow::ensure!(!agent_id.is_empty(), "agent_id must not be empty");
    anyhow::ensure!(!question.is_empty(), "question must not be empty");

    let record = InteractionRecord {
        id: Uuid::new_v4().to_string(),
        kind: InteractionKind::Question,
        agent_id: agent_id.to_string(),
        workflow_id: normalize_opt(workflow_id),
        task_id: normalize_opt(task_id),
        created_at: chrono::Utc::now().to_rfc3339(),
        question: Some(question.to_string()),
        action: None,
        options: options.iter().map(|opt| opt.trim().to_string()).filter(|opt| !opt.is_empty()).collect(),
        tool_name: None,
        arguments: None,
        questions: Vec::new(),
        suggestions: None,
        timeout_secs,
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
    write_interaction_atomic(&interaction_path(project_root, &record.id), &record)?;
    Ok(record)
}

/// Create a structured-question interaction from a native SDK
/// `AskUserQuestion` prompt-tool call. `raw_input` is the verbatim tool
/// input (preserved on `arguments` so the answer path can echo the original
/// `questions` array — including fields we do not model, like `preview` —
/// back in `updatedInput`).
#[allow(clippy::too_many_arguments)]
pub fn create_native_question_interaction(
    project_root: &str,
    agent_id: &str,
    questions: Vec<InteractionQuestion>,
    raw_input: Value,
    suggestions: Option<Value>,
    timeout_secs: Option<u64>,
    workflow_id: Option<&str>,
    task_id: Option<&str>,
) -> Result<InteractionRecord> {
    let agent_id = agent_id.trim();
    anyhow::ensure!(!agent_id.is_empty(), "agent_id must not be empty");
    anyhow::ensure!(!questions.is_empty(), "questions must not be empty");

    let flat_question = if questions.len() == 1 {
        questions[0].question.clone()
    } else {
        questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join(" | ")
    };
    let record = InteractionRecord {
        id: Uuid::new_v4().to_string(),
        kind: InteractionKind::Question,
        agent_id: agent_id.to_string(),
        workflow_id: normalize_opt(workflow_id),
        task_id: normalize_opt(task_id),
        created_at: chrono::Utc::now().to_rfc3339(),
        question: Some(flat_question),
        action: None,
        options: Vec::new(),
        tool_name: Some("AskUserQuestion".to_string()),
        arguments: Some(raw_input),
        questions,
        suggestions,
        timeout_secs,
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
    write_interaction_atomic(&interaction_path(project_root, &record.id), &record)?;
    Ok(record)
}

#[allow(clippy::too_many_arguments)]
pub fn create_approval_interaction(
    project_root: &str,
    agent_id: &str,
    action: &str,
    tool_name: Option<&str>,
    arguments: Option<Value>,
    suggestions: Option<Value>,
    timeout_secs: Option<u64>,
    workflow_id: Option<&str>,
    task_id: Option<&str>,
) -> Result<InteractionRecord> {
    let agent_id = agent_id.trim();
    let action = action.trim();
    anyhow::ensure!(!agent_id.is_empty(), "agent_id must not be empty");
    anyhow::ensure!(!action.is_empty(), "action must not be empty");

    let record = InteractionRecord {
        id: Uuid::new_v4().to_string(),
        kind: InteractionKind::Approval,
        agent_id: agent_id.to_string(),
        workflow_id: normalize_opt(workflow_id),
        task_id: normalize_opt(task_id),
        created_at: chrono::Utc::now().to_rfc3339(),
        question: None,
        action: Some(action.to_string()),
        options: Vec::new(),
        tool_name: normalize_opt(tool_name),
        arguments,
        questions: Vec::new(),
        suggestions,
        timeout_secs,
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
    write_interaction_atomic(&interaction_path(project_root, &record.id), &record)?;
    Ok(record)
}

pub fn load_interaction(project_root: &str, id: &str) -> Result<Option<InteractionRecord>> {
    let id = id.trim();
    anyhow::ensure!(!id.is_empty(), "interaction id must not be empty");
    let path = interaction_path(project_root, id);
    if !path.exists() {
        return Ok(None);
    }
    read_interaction(&path).map(Some)
}

pub fn list_interactions(
    project_root: &str,
    include_resolved: bool,
    agent_id: Option<&str>,
) -> Result<Vec<InteractionRecord>> {
    let dir = interactions_dir(project_root);
    let mut records = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(record) = read_interaction(&path) else {
                continue;
            };
            records.push(record);
        }
    }
    if !include_resolved {
        records.retain(|record| record.status == InteractionStatus::Pending);
    }
    if let Some(agent_id) = normalize_opt(agent_id) {
        records.retain(|record| record.agent_id.eq_ignore_ascii_case(&agent_id));
    }
    records.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(records)
}

pub fn answer_interaction(
    project_root: &str,
    id: &str,
    answer: &str,
    message: Option<&str>,
    answered_by: Option<&str>,
) -> Result<InteractionRecord> {
    apply_interaction_answer(
        project_root,
        id,
        InteractionAnswer {
            answer: answer.to_string(),
            message: message.map(ToOwned::to_owned),
            answered_by: answered_by.map(ToOwned::to_owned),
            ..InteractionAnswer::default()
        },
    )
}

fn validate_structured_answers(record: &InteractionRecord, answer: &InteractionAnswer) -> Result<()> {
    let has_answers = answer.answers.as_ref().is_some_and(|map| !map.is_empty());
    anyhow::ensure!(
        has_answers || answer.response.as_deref().is_some_and(|value| !value.trim().is_empty()),
        "a structured question answer requires per-question `answers` and/or a freeform `response`"
    );
    if let Some(answers) = answer.answers.as_ref() {
        for (question, value) in answers {
            anyhow::ensure!(
                record.questions.iter().any(|candidate| candidate.question == *question),
                "answer key '{question}' does not match any question on this interaction"
            );
            let valid = match value {
                Value::String(text) => !text.trim().is_empty(),
                Value::Array(items) => {
                    !items.is_empty()
                        && items.iter().all(|item| item.as_str().is_some_and(|text| !text.trim().is_empty()))
                }
                _ => false,
            };
            anyhow::ensure!(
                valid,
                "answer for '{question}' must be a non-empty string or a non-empty array of strings"
            );
        }
    }
    Ok(())
}

/// Answer a pending interaction with the full structured payload. Exactly one
/// answer wins (flock'd read-modify-write); validation depends on the record:
/// approvals require `allow`/`deny`, structured questions require `answers`
/// keyed by question text and/or a freeform `response`.
pub fn apply_interaction_answer(project_root: &str, id: &str, answer: InteractionAnswer) -> Result<InteractionRecord> {
    let id = id.trim();
    let flat_answer = answer.answer.trim().to_string();
    anyhow::ensure!(!id.is_empty(), "interaction id must not be empty");
    anyhow::ensure!(!flat_answer.is_empty(), "answer must not be empty");

    let path = interaction_path(project_root, id);
    with_interaction_file_lock(&path, || {
        if !path.exists() {
            return Err(anyhow!("no interaction with id '{}'", id));
        }
        let mut record = read_interaction(&path)?;
        if record.status != InteractionStatus::Pending {
            return Err(anyhow!(
                "interaction '{}' is not pending (status: {})",
                id,
                serde_json::to_value(record.status)?.as_str().unwrap_or("unknown")
            ));
        }
        match record.kind {
            InteractionKind::Approval => {
                if !matches!(flat_answer.as_str(), INTERACTION_ANSWER_ALLOW | INTERACTION_ANSWER_DENY) {
                    return Err(anyhow!(
                        "approval answer must be '{INTERACTION_ANSWER_ALLOW}' or '{INTERACTION_ANSWER_DENY}'"
                    ));
                }
            }
            InteractionKind::Question => {
                if answer.updated_input.is_some() || answer.updated_permissions.is_some() {
                    return Err(anyhow!("updated_input / updated_permissions only apply to approval interactions"));
                }
                if !record.questions.is_empty() {
                    validate_structured_answers(&record, &answer)?;
                }
            }
        }
        record.status = InteractionStatus::Answered;
        record.answer = Some(flat_answer);
        record.answer_message = normalize_opt(answer.message.as_deref());
        record.answers = answer.answers.filter(|map| !map.is_empty());
        record.response = answer.response.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        record.updated_input = answer.updated_input;
        record.updated_permissions = answer.updated_permissions;
        record.answered_at = Some(chrono::Utc::now().to_rfc3339());
        record.answered_by = Some(normalize_opt(answer.answered_by.as_deref()).unwrap_or_else(|| "human".to_string()));
        write_interaction_atomic(&path, &record)?;
        Ok(record)
    })
}

/// Flag a freshly-created pending record as suspend-mode. Only suspended
/// records trigger the workflow resume path when answered; the flag is set
/// exclusively by the kernel's suspend wait mode (never from payloads).
pub fn mark_interaction_suspended(project_root: &str, id: &str) -> Result<InteractionRecord> {
    let id = id.trim();
    anyhow::ensure!(!id.is_empty(), "interaction id must not be empty");
    let path = interaction_path(project_root, id);
    with_interaction_file_lock(&path, || {
        if !path.exists() {
            return Err(anyhow!("no interaction with id '{}'", id));
        }
        let mut record = read_interaction(&path)?;
        if !record.suspended {
            record.suspended = true;
            write_interaction_atomic(&path, &record)?;
        }
        Ok(record)
    })
}

pub fn expire_interaction(project_root: &str, id: &str) -> Result<Option<InteractionRecord>> {
    let id = id.trim();
    anyhow::ensure!(!id.is_empty(), "interaction id must not be empty");
    let path = interaction_path(project_root, id);
    with_interaction_file_lock(&path, || {
        if !path.exists() {
            return Ok(None);
        }
        let mut record = read_interaction(&path)?;
        if record.status != InteractionStatus::Pending {
            return Ok(Some(record));
        }
        record.status = InteractionStatus::Expired;
        write_interaction_atomic(&path, &record)?;
        Ok(Some(record))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_create_answer_roundtrips() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        let created = create_question_interaction(
            &project_root,
            "swe",
            "Ship now or wait?",
            &["ship".to_string(), "wait".to_string()],
            Some(600),
            Some("wf-1"),
            Some("TASK-1"),
        )
        .expect("create question");
        assert_eq!(created.kind, InteractionKind::Question);
        assert_eq!(created.status, InteractionStatus::Pending);
        assert_eq!(created.options, vec!["ship".to_string(), "wait".to_string()]);

        let pending = list_interactions(&project_root, false, None).expect("list pending");
        assert_eq!(pending.len(), 1);

        let answered =
            answer_interaction(&project_root, &created.id, "ship", None, Some("sami")).expect("answer question");
        assert_eq!(answered.status, InteractionStatus::Answered);
        assert_eq!(answered.answer.as_deref(), Some("ship"));
        assert_eq!(answered.answered_by.as_deref(), Some("sami"));
        assert!(answered.answered_at.is_some());

        let pending_after = list_interactions(&project_root, false, None).expect("list pending after");
        assert!(pending_after.is_empty());
        let all = list_interactions(&project_root, true, None).expect("list all");
        assert_eq!(all.len(), 1);

        let loaded = load_interaction(&project_root, &created.id).expect("load").expect("record exists");
        assert_eq!(loaded.status, InteractionStatus::Answered);
    }

    #[test]
    fn approval_answer_rejects_non_decision_values() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        let created = create_approval_interaction(
            &project_root,
            "swe",
            "delete production database",
            Some("Bash"),
            Some(serde_json::json!({ "command": "dropdb prod" })),
            None,
            None,
            None,
            None,
        )
        .expect("create approval");

        let err = answer_interaction(&project_root, &created.id, "yes", None, None).expect_err("invalid decision");
        assert!(err.to_string().contains("'allow' or 'deny'"));

        let denied = answer_interaction(&project_root, &created.id, "deny", Some("too risky"), None).expect("deny");
        assert_eq!(denied.answer.as_deref(), Some("deny"));
        assert_eq!(denied.answer_message.as_deref(), Some("too risky"));
        assert_eq!(denied.answered_by.as_deref(), Some("human"));
    }

    #[test]
    fn answering_twice_fails_with_not_pending() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        let created = create_question_interaction(&project_root, "swe", "Proceed?", &[], None, None, None)
            .expect("create question");
        answer_interaction(&project_root, &created.id, "yes", None, None).expect("first answer");
        let err = answer_interaction(&project_root, &created.id, "no", None, None).expect_err("second answer");
        assert!(err.to_string().contains("not pending"));
    }

    #[test]
    fn concurrent_answers_let_exactly_one_win() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy().to_string();

        let created =
            create_question_interaction(&project_root, "swe", "Race?", &[], None, None, None).expect("create question");
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let root = project_root.clone();
                let id = created.id.clone();
                std::thread::spawn(move || {
                    answer_interaction(&root, &id, &format!("answer-{index}"), None, Some(&format!("racer-{index}")))
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|handle| handle.join().expect("answer thread")).collect();
        let winners = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(winners, 1, "exactly one concurrent answer must win");
        for result in results.iter().filter(|result| result.is_err()) {
            assert!(result.as_ref().unwrap_err().to_string().contains("not pending"));
        }

        let loaded = load_interaction(&project_root, &created.id).expect("load").expect("record exists");
        assert_eq!(loaded.status, InteractionStatus::Answered);
        let winning_by = loaded.answered_by.expect("answered_by");
        let winning_answer = loaded.answer.expect("answer");
        assert_eq!(winning_by.replace("racer-", ""), winning_answer.replace("answer-", ""));
    }

    #[test]
    fn expire_only_flips_pending_records() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        let created = create_question_interaction(&project_root, "swe", "Still there?", &[], Some(1), None, None)
            .expect("create question");
        let expired = expire_interaction(&project_root, &created.id).expect("expire").expect("record exists");
        assert_eq!(expired.status, InteractionStatus::Expired);

        let err = answer_interaction(&project_root, &created.id, "late", None, None).expect_err("answer expired");
        assert!(err.to_string().contains("not pending"));

        let other = create_question_interaction(&project_root, "swe", "Answer me", &[], None, None, None)
            .expect("create second");
        answer_interaction(&project_root, &other.id, "ok", None, None).expect("answer second");
        let still_answered = expire_interaction(&project_root, &other.id).expect("expire answered").expect("record");
        assert_eq!(still_answered.status, InteractionStatus::Answered);

        assert!(expire_interaction(&project_root, "missing-id").expect("expire missing").is_none());
    }

    // Back-compat: records written before the structured-question fields
    // existed (no questions/answers/response/suggestions/updated_*) must
    // still load.
    #[test]
    fn pre_structured_record_json_still_loads() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy().to_string();

        let legacy = serde_json::json!({
            "id": "legacy-1",
            "kind": "approval",
            "agent_id": "swe",
            "created_at": "2026-06-01T00:00:00Z",
            "action": "git push --force",
            "tool_name": "git.push",
            "status": "answered",
            "answer": "deny",
            "answer_message": "too risky",
            "answered_at": "2026-06-01T00:01:00Z",
            "answered_by": "sami"
        });
        let dir = super::interactions_dir(&project_root);
        std::fs::create_dir_all(&dir).expect("interactions dir");
        std::fs::write(dir.join("legacy-1.json"), serde_json::to_string_pretty(&legacy).expect("json"))
            .expect("write legacy record");

        let loaded = load_interaction(&project_root, "legacy-1").expect("load").expect("record exists");
        assert_eq!(loaded.kind, InteractionKind::Approval);
        assert_eq!(loaded.status, InteractionStatus::Answered);
        assert!(loaded.questions.is_empty());
        assert!(loaded.answers.is_none());
        assert!(loaded.response.is_none());
        assert!(loaded.suggestions.is_none());
        assert!(loaded.updated_input.is_none());
        assert!(loaded.updated_permissions.is_none());
        assert!(!loaded.suspended);
    }

    #[test]
    fn parse_sdk_questions_accepts_camel_case_multi_select() {
        let input = serde_json::json!({
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
        let questions = parse_sdk_questions(&input).expect("parse questions");
        assert_eq!(questions.len(), 2);
        assert!(!questions[0].multi_select);
        assert!(questions[1].multi_select);
        assert_eq!(questions[0].options[0].label, "Summary");
        assert_eq!(questions[1].options[0].description, None);

        assert!(parse_sdk_questions(&serde_json::json!({})).is_err());
        assert!(parse_sdk_questions(&serde_json::json!({ "questions": [] })).is_err());
    }

    #[test]
    fn structured_question_answer_round_trips() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy().to_string();

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
        let questions = parse_sdk_questions(&raw_input).expect("parse");
        let created = create_native_question_interaction(
            &project_root,
            "swe",
            questions,
            raw_input.clone(),
            None,
            Some(600),
            None,
            None,
        )
        .expect("create native question");
        assert_eq!(created.kind, InteractionKind::Question);
        assert_eq!(created.tool_name.as_deref(), Some("AskUserQuestion"));
        assert_eq!(created.arguments.as_ref(), Some(&raw_input));
        assert_eq!(created.questions.len(), 2);

        // Unknown answer keys and non-string values are rejected.
        let mut bad = BTreeMap::new();
        bad.insert("Nope?".to_string(), Value::String("x".to_string()));
        let err = apply_interaction_answer(
            &project_root,
            &created.id,
            InteractionAnswer { answer: "x".to_string(), answers: Some(bad), ..InteractionAnswer::default() },
        )
        .expect_err("unknown question key");
        assert!(err.to_string().contains("does not match any question"));

        let mut answers = BTreeMap::new();
        answers.insert("How should I format the output?".to_string(), Value::String("Summary".to_string()));
        answers
            .insert("Which sections should I include?".to_string(), serde_json::json!(["Introduction", "Conclusion"]));
        let answered = apply_interaction_answer(
            &project_root,
            &created.id,
            InteractionAnswer {
                answer: "Format: Summary; Sections: Introduction, Conclusion".to_string(),
                answers: Some(answers.clone()),
                response: Some("Keep it short overall".to_string()),
                answered_by: Some("sami".to_string()),
                ..InteractionAnswer::default()
            },
        )
        .expect("structured answer");
        assert_eq!(answered.status, InteractionStatus::Answered);
        assert_eq!(answered.answers.as_ref(), Some(&answers));
        assert_eq!(answered.response.as_deref(), Some("Keep it short overall"));

        let loaded = load_interaction(&project_root, &created.id).expect("load").expect("record exists");
        assert_eq!(loaded.answers.as_ref(), Some(&answers));
        assert_eq!(loaded.response.as_deref(), Some("Keep it short overall"));
    }

    #[test]
    fn structured_question_requires_answers_or_response() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy().to_string();

        let raw_input = serde_json::json!({
            "questions": [{ "question": "Proceed?", "options": [{ "label": "Yes" }, { "label": "No" }] }]
        });
        let questions = parse_sdk_questions(&raw_input).expect("parse");
        let created =
            create_native_question_interaction(&project_root, "swe", questions, raw_input, None, None, None, None)
                .expect("create native question");

        let err = apply_interaction_answer(
            &project_root,
            &created.id,
            InteractionAnswer { answer: "summary".to_string(), ..InteractionAnswer::default() },
        )
        .expect_err("no structured answer");
        assert!(err.to_string().contains("answers"));

        // Response-only answers are valid (freeform reply path).
        let answered = apply_interaction_answer(
            &project_root,
            &created.id,
            InteractionAnswer {
                answer: "just ship it".to_string(),
                response: Some("just ship it".to_string()),
                ..InteractionAnswer::default()
            },
        )
        .expect("response-only answer");
        assert!(answered.answers.is_none());
        assert_eq!(answered.response.as_deref(), Some("just ship it"));
    }

    #[test]
    fn approval_answer_carries_updated_input_and_permissions() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy().to_string();

        let suggestions = serde_json::json!([
            { "type": "addRules", "rules": [{ "toolName": "Bash" }], "behavior": "allow", "destination": "localSettings" },
            { "type": "addRules", "rules": [{ "toolName": "Bash" }], "behavior": "allow", "destination": "session" }
        ]);
        let created = create_approval_interaction(
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
        assert_eq!(created.suggestions.as_ref(), Some(&suggestions));

        let answered = apply_interaction_answer(
            &project_root,
            &created.id,
            InteractionAnswer {
                answer: INTERACTION_ANSWER_ALLOW.to_string(),
                updated_input: Some(serde_json::json!({ "command": "migrate --dry-run" })),
                updated_permissions: Some(serde_json::json!([suggestions[0].clone()])),
                ..InteractionAnswer::default()
            },
        )
        .expect("allow with overrides");
        assert_eq!(answered.updated_input, Some(serde_json::json!({ "command": "migrate --dry-run" })));
        assert_eq!(answered.updated_permissions, Some(serde_json::json!([suggestions[0].clone()])));
    }

    #[test]
    fn list_filters_by_agent() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        create_question_interaction(&project_root, "swe", "A?", &[], None, None, None).expect("create swe");
        create_question_interaction(&project_root, "po", "B?", &[], None, None, None).expect("create po");

        let swe_only = list_interactions(&project_root, true, Some("swe")).expect("list swe");
        assert_eq!(swe_only.len(), 1);
        assert_eq!(swe_only[0].agent_id, "swe");
    }
}
