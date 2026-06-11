use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const INTERACTIONS_DIR: &str = "interactions";

pub const INTERACTION_ANSWER_ALLOW: &str = "allow";
pub const INTERACTION_ANSWER_DENY: &str = "deny";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    pub status: InteractionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_message: Option<String>,
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
        timeout_secs,
        status: InteractionStatus::Pending,
        answer: None,
        answer_message: None,
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
        timeout_secs,
        status: InteractionStatus::Pending,
        answer: None,
        answer_message: None,
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
    let id = id.trim();
    let answer = answer.trim();
    anyhow::ensure!(!id.is_empty(), "interaction id must not be empty");
    anyhow::ensure!(!answer.is_empty(), "answer must not be empty");

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
        if record.kind == InteractionKind::Approval
            && !matches!(answer, INTERACTION_ANSWER_ALLOW | INTERACTION_ANSWER_DENY)
        {
            return Err(anyhow!("approval answer must be '{INTERACTION_ANSWER_ALLOW}' or '{INTERACTION_ANSWER_DENY}'"));
        }
        record.status = InteractionStatus::Answered;
        record.answer = Some(answer.to_string());
        record.answer_message = normalize_opt(message);
        record.answered_at = Some(chrono::Utc::now().to_rfc3339());
        record.answered_by = Some(normalize_opt(answered_by).unwrap_or_else(|| "human".to_string()));
        write_interaction_atomic(&path, &record)?;
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
