use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const AGENT_STATE_DIR: &str = "agents";
const MEMORY_DIR: &str = "memory";
const MESSAGE_DIR: &str = "messages";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMemoryEntry {
    pub id: String,
    pub created_at: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentMemoryDocument {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub entries: Vec<AgentMemoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub channel: String,
    pub from_agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_agent: Option<String>,
    pub text: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AgentMessageDocument {
    #[serde(default)]
    messages: Vec<AgentMessage>,
}

fn scoped_state_base(project_root: &str) -> PathBuf {
    let path = Path::new(project_root);
    protocol::scoped_state_root(path).unwrap_or_else(|| path.join(".animus"))
}

fn sanitize_state_key(value: &str) -> String {
    value.chars().map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '_' }).collect()
}

fn agent_state_dir(project_root: &str) -> PathBuf {
    scoped_state_base(project_root).join("state").join(AGENT_STATE_DIR)
}

fn agent_memory_path(project_root: &str, agent_id: &str) -> PathBuf {
    agent_state_dir(project_root).join(MEMORY_DIR).join(format!("{}.json", sanitize_state_key(agent_id)))
}

fn agent_message_path(project_root: &str, channel: &str) -> PathBuf {
    agent_state_dir(project_root).join(MESSAGE_DIR).join(format!("{}.json", sanitize_state_key(channel)))
}

fn read_json_or_default<T>(path: &Path) -> Result<T>
where
    T: Default + for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

// Agent memory writes use the same durable pattern as session checkpoints:
// fsync the staged tempfile, atomic rename, then fsync the parent dir so
// the rename survives power loss. See `orchestrator_store::fsync_rename`
// for the macOS F_FULLFSYNC caveat.
fn write_json_atomic<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("agent-state"),
        Uuid::new_v4()
    ));
    {
        use std::io::Write;
        let mut file =
            std::fs::File::create(&tmp_path).with_context(|| format!("failed to create {}", tmp_path.display()))?;
        file.write_all(payload.as_bytes()).with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.sync_all().with_context(|| format!("failed to fsync {}", tmp_path.display()))?;
    }
    orchestrator_store::fsync_rename(&tmp_path, path)
        .with_context(|| format!("failed to durably rename {} -> {}", tmp_path.display(), path.display()))?;
    Ok(())
}

pub fn load_agent_memory(project_root: &str, agent_id: &str) -> Result<AgentMemoryDocument> {
    let mut document: AgentMemoryDocument = read_json_or_default(&agent_memory_path(project_root, agent_id))?;
    if document.agent_id.is_empty() {
        document.agent_id = agent_id.to_string();
    }
    Ok(document)
}

pub fn append_agent_memory(
    project_root: &str,
    agent_id: &str,
    text: &str,
    source: Option<&str>,
) -> Result<AgentMemoryDocument> {
    let trimmed = text.trim();
    anyhow::ensure!(!trimmed.is_empty(), "memory text must not be empty");

    let mut document = load_agent_memory(project_root, agent_id)?;
    let now = chrono::Utc::now().to_rfc3339();
    document.entries.push(AgentMemoryEntry {
        id: Uuid::new_v4().to_string(),
        created_at: now.clone(),
        text: trimmed.to_string(),
        source: source.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned),
    });
    document.updated_at = Some(now);
    write_json_atomic(&agent_memory_path(project_root, agent_id), &document)?;
    Ok(document)
}

pub fn clear_agent_memory(project_root: &str, agent_id: &str) -> Result<AgentMemoryDocument> {
    let document = AgentMemoryDocument {
        agent_id: agent_id.to_string(),
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        entries: Vec::new(),
    };
    write_json_atomic(&agent_memory_path(project_root, agent_id), &document)?;
    Ok(document)
}

pub fn delete_agent_memory_entry(
    project_root: &str,
    agent_id: &str,
    entry_id: &str,
) -> Result<(AgentMemoryDocument, bool)> {
    let trimmed = entry_id.trim();
    anyhow::ensure!(!trimmed.is_empty(), "memory entry id must not be empty");

    let mut document = load_agent_memory(project_root, agent_id)?;
    let before = document.entries.len();
    document.entries.retain(|entry| entry.id != trimmed);
    let removed = document.entries.len() < before;
    if removed {
        document.updated_at = Some(chrono::Utc::now().to_rfc3339());
        write_json_atomic(&agent_memory_path(project_root, agent_id), &document)?;
    }
    Ok((document, removed))
}

pub fn send_agent_message(
    project_root: &str,
    channel: &str,
    from_agent: &str,
    to_agent: Option<&str>,
    text: &str,
    workflow_id: Option<&str>,
    phase_id: Option<&str>,
) -> Result<AgentMessage> {
    let channel = channel.trim();
    let from_agent = from_agent.trim();
    let text = text.trim();
    anyhow::ensure!(!channel.is_empty(), "message channel must not be empty");
    anyhow::ensure!(!from_agent.is_empty(), "message sender must not be empty");
    anyhow::ensure!(!text.is_empty(), "message text must not be empty");

    let path = agent_message_path(project_root, channel);
    let mut document: AgentMessageDocument = read_json_or_default(&path)?;
    let message = AgentMessage {
        id: Uuid::new_v4().to_string(),
        channel: channel.to_string(),
        from_agent: from_agent.to_string(),
        to_agent: to_agent.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned),
        text: text.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        workflow_id: workflow_id.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned),
        phase_id: phase_id.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned),
    };
    document.messages.push(message.clone());
    write_json_atomic(&path, &document)?;
    Ok(message)
}

pub fn list_agent_messages(
    project_root: &str,
    channel: Option<&str>,
    agent_id: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<AgentMessage>> {
    let mut messages = Vec::new();
    let message_root = agent_state_dir(project_root).join(MESSAGE_DIR);
    if let Some(channel) = channel.map(str::trim).filter(|value| !value.is_empty()) {
        let document: AgentMessageDocument = read_json_or_default(&agent_message_path(project_root, channel))?;
        messages.extend(document.messages);
    } else if message_root.is_dir() {
        for entry in
            std::fs::read_dir(&message_root).with_context(|| format!("failed to read {}", message_root.display()))?
        {
            let entry = entry?;
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let document: AgentMessageDocument = read_json_or_default(&entry.path())?;
            messages.extend(document.messages);
        }
    }

    if let Some(agent_id) = agent_id.map(str::trim).filter(|value| !value.is_empty()) {
        messages.retain(|message| {
            message.from_agent.eq_ignore_ascii_case(agent_id)
                || message.to_agent.as_deref().is_some_and(|target| target.eq_ignore_ascii_case(agent_id))
        });
    }
    messages.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    if let Some(limit) = limit {
        if messages.len() > limit {
            messages = messages.split_off(messages.len() - limit);
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_append_and_clear_roundtrips() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        let memory = append_agent_memory(&project_root, "architect", "Prefer explicit contracts.", Some("test"))
            .expect("append memory");
        assert_eq!(memory.agent_id, "architect");
        assert_eq!(memory.entries.len(), 1);
        assert_eq!(memory.entries[0].text, "Prefer explicit contracts.");

        let loaded = load_agent_memory(&project_root, "architect").expect("load memory");
        assert_eq!(loaded.entries.len(), 1);

        let cleared = clear_agent_memory(&project_root, "architect").expect("clear memory");
        assert!(cleared.entries.is_empty());
    }

    #[test]
    fn memory_delete_entry_by_id_removes_only_matching_entry() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        let after_first = append_agent_memory(&project_root, "architect", "First note", None).expect("append first");
        let after_second = append_agent_memory(&project_root, "architect", "Second note", None).expect("append second");
        assert_eq!(after_second.entries.len(), 2);

        let target_id = after_first.entries[0].id.clone();
        let (after_delete, removed) =
            delete_agent_memory_entry(&project_root, "architect", &target_id).expect("delete entry");
        assert!(removed);
        assert_eq!(after_delete.entries.len(), 1);
        assert_eq!(after_delete.entries[0].text, "Second note");

        let (after_noop, removed_again) =
            delete_agent_memory_entry(&project_root, "architect", &target_id).expect("delete missing entry");
        assert!(!removed_again);
        assert_eq!(after_noop.entries.len(), 1);
    }

    // TODO: intermittently flakes under `cargo test --workspace` — even with the
    // scoped_state_serializer mutex held, list_agent_messages occasionally returns
    // 0 of the 2 messages just sent. Always passes in isolation
    // (`cargo test -p workflow-runner-v2 --lib agent_state`). Suspect a parallel
    // test mutating HOME or scope-dir state that this serializer doesn't cover.
    #[ignore = "intermittent parallel-test race on scoped_state_serializer; passes in isolation"]
    #[test]
    fn messages_can_be_filtered_by_agent_and_channel() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = tempfile::tempdir().expect("temp dir");
        let project_root = tmp.path().to_string_lossy();

        send_agent_message(
            &project_root,
            "engineering",
            "architect",
            Some("implementer"),
            "Check the API.",
            None,
            None,
        )
        .expect("send targeted message");
        send_agent_message(&project_root, "engineering", "reviewer", None, "Looks good.", None, None)
            .expect("send channel message");

        let all = list_agent_messages(&project_root, Some("engineering"), None, None).expect("list all");
        assert_eq!(all.len(), 2);

        let architect =
            list_agent_messages(&project_root, Some("engineering"), Some("architect"), None).expect("list architect");
        assert_eq!(architect.len(), 1);
        assert_eq!(architect[0].from_agent, "architect");
    }
}
