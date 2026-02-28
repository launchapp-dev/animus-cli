use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    parsing::parse_json_body,
    requests::{QueueHoldRequest, QueueReleaseRequest, QueueReorderRequest},
    WebApiError, WebApiService,
};

const EM_WORK_QUEUE_STATE_FILE: &str = "em-work-queue.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EmWorkQueueEntryStatus {
    Pending,
    Assigned,
    Held,
    #[serde(other)]
    Unknown,
}

impl Default for EmWorkQueueEntryStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct EmWorkQueueEntry {
    task_id: String,
    #[serde(default)]
    status: EmWorkQueueEntryStatus,
    #[serde(default)]
    workflow_id: Option<String>,
    #[serde(default)]
    assigned_at: Option<String>,
    #[serde(default)]
    held_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EmWorkQueueState {
    #[serde(default)]
    entries: Vec<EmWorkQueueEntry>,
}

fn daemon_repo_runtime_root(project_root: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve home directory")?;
    let scope = protocol::repository_scope_for_path(std::path::Path::new(project_root));
    Ok(home.join(".ao").join(scope))
}

fn queue_state_path(project_root: &str) -> Result<PathBuf> {
    Ok(daemon_repo_runtime_root(project_root)?
        .join("scheduler")
        .join(EM_WORK_QUEUE_STATE_FILE))
}

fn load_queue_state(project_root: &str) -> Result<Option<EmWorkQueueState>> {
    let path = queue_state_path(project_root)?;
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read EM work queue state file at {}",
            path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(Some(EmWorkQueueState::default()));
    }

    serde_json::from_str::<EmWorkQueueState>(&content)
        .map(Some)
        .or_else(|_| {
            serde_json::from_str::<Vec<EmWorkQueueEntry>>(&content)
                .map(|entries| Some(EmWorkQueueState { entries }))
        })
        .with_context(|| {
            format!(
                "failed to parse EM work queue state file at {}",
                path.display()
            )
        })
}

fn save_queue_state(project_root: &str, state: &EmWorkQueueState) -> Result<()> {
    let path = queue_state_path(project_root)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if state.entries.is_empty() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }

    let payload = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, payload)?;
    Ok(())
}

fn hold_queue_entry(project_root: &str, task_id: &str) -> Result<bool> {
    let Some(mut state) = load_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut updated = false;
    for entry in &mut state.entries {
        if entry.task_id != task_id {
            continue;
        }
        if entry.status != EmWorkQueueEntryStatus::Pending {
            continue;
        }
        entry.status = EmWorkQueueEntryStatus::Held;
        entry.held_at = Some(Utc::now().to_rfc3339());
        updated = true;
        break;
    }

    if updated {
        save_queue_state(project_root, &state)?;
    }

    Ok(updated)
}

fn release_queue_entry(project_root: &str, task_id: &str) -> Result<bool> {
    let Some(mut state) = load_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut updated = false;
    for entry in &mut state.entries {
        if entry.task_id != task_id {
            continue;
        }
        if entry.status != EmWorkQueueEntryStatus::Held {
            continue;
        }
        entry.status = EmWorkQueueEntryStatus::Pending;
        entry.held_at = None;
        updated = true;
        break;
    }

    if updated {
        save_queue_state(project_root, &state)?;
    }

    Ok(updated)
}

fn reorder_queue(project_root: &str, task_ids: Vec<String>) -> Result<bool> {
    let Some(mut state) = load_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut new_entries = Vec::new();
    let task_id_set: std::collections::HashSet<&str> =
        task_ids.iter().map(|s| s.as_str()).collect();

    for task_id in &task_ids {
        if let Some(entry) = state.entries.iter().find(|e| e.task_id == *task_id) {
            new_entries.push(entry.clone());
        }
    }

    for entry in &state.entries {
        if !task_id_set.contains(entry.task_id.as_str()) {
            new_entries.push(entry.clone());
        }
    }

    if new_entries != state.entries {
        state.entries = new_entries;
        save_queue_state(project_root, &state)?;
        return Ok(true);
    }

    Ok(false)
}

impl WebApiService {
    pub async fn queue_list(&self) -> Result<serde_json::Value, WebApiError> {
        let project_root = &self.context.project_root;
        let queue_state = load_queue_state(project_root).map_err(|e| {
            WebApiError::new(
                "internal_error",
                format!("failed to load queue: {}", e),
                1,
            )
        })?;

        let Some(queue) = queue_state else {
            return Ok(serde_json::json!({
                "entries": [],
                "stats": {
                    "total": 0,
                    "pending": 0,
                    "assigned": 0,
                    "held": 0
                }
            }));
        };

        let tasks = self.context.hub.tasks().list().await.unwrap_or_default();
        let task_lookup: std::collections::HashMap<&str, _> =
            tasks.iter().map(|t| (t.id.as_str(), t)).collect();

        let entries: Vec<serde_json::Value> = queue
            .entries
            .iter()
            .map(|entry| {
                let task = task_lookup.get(entry.task_id.as_str());
                serde_json::json!({
                    "task_id": entry.task_id,
                    "status": entry.status,
                    "workflow_id": entry.workflow_id,
                    "assigned_at": entry.assigned_at,
                    "held_at": entry.held_at,
                    "task": task.map(|t| serde_json::json!({
                        "id": t.id,
                        "title": t.title,
                        "description": t.description,
                        "status": t.status,
                        "priority": t.priority,
                    }))
                })
            })
            .collect();

        let pending = queue
            .entries
            .iter()
            .filter(|e| matches!(e.status, EmWorkQueueEntryStatus::Pending))
            .count();
        let assigned = queue
            .entries
            .iter()
            .filter(|e| matches!(e.status, EmWorkQueueEntryStatus::Assigned))
            .count();
        let held = queue
            .entries
            .iter()
            .filter(|e| matches!(e.status, EmWorkQueueEntryStatus::Held))
            .count();

        Ok(serde_json::json!({
            "entries": entries,
            "stats": {
                "total": queue.entries.len(),
                "pending": pending,
                "assigned": assigned,
                "held": held
            }
        }))
    }

    pub async fn queue_stats(&self) -> Result<serde_json::Value, WebApiError> {
        let project_root = &self.context.project_root;
        let queue_state = load_queue_state(project_root).map_err(|e| {
            WebApiError::new(
                "internal_error",
                format!("failed to load queue: {}", e),
                1,
            )
        })?;

        let Some(queue) = queue_state else {
            return Ok(serde_json::json!({
                "depth": 0,
                "pending": 0,
                "assigned": 0,
                "held": 0,
                "throughput_last_hour": 0,
                "avg_wait_time_secs": 0
            }));
        };

        let pending = queue
            .entries
            .iter()
            .filter(|e| matches!(e.status, EmWorkQueueEntryStatus::Pending))
            .count();
        let assigned = queue
            .entries
            .iter()
            .filter(|e| matches!(e.status, EmWorkQueueEntryStatus::Assigned))
            .count();
        let held = queue
            .entries
            .iter()
            .filter(|e| matches!(e.status, EmWorkQueueEntryStatus::Held))
            .count();

        let now = Utc::now();
        let mut throughput = 0usize;
        let mut total_wait_secs: i64 = 0;
        let mut wait_count = 0usize;

        for entry in &queue.entries {
            if let Some(assigned_at) = &entry.assigned_at {
                if let Ok(assigned_time) = DateTime::parse_from_rfc3339(assigned_at) {
                    let elapsed = now.signed_duration_since(assigned_time.with_timezone(&Utc));
                    if elapsed.num_hours() < 1 {
                        throughput += 1;
                    }
                }
            }
            if entry.status == EmWorkQueueEntryStatus::Pending {
                total_wait_secs += now.timestamp();
                wait_count += 1;
            }
        }

        let avg_wait_time_secs = if wait_count > 0 {
            let now_ts = now.timestamp();
            (now_ts * wait_count as i64 - total_wait_secs) / wait_count as i64
        } else {
            0
        };

        Ok(serde_json::json!({
            "depth": queue.entries.len(),
            "pending": pending,
            "assigned": assigned,
            "held": held,
            "throughput_last_hour": throughput,
            "avg_wait_time_secs": avg_wait_time_secs
        }))
    }

    pub async fn queue_reorder(
        &self,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, WebApiError> {
        let request: QueueReorderRequest = parse_json_body(body)?;
        let project_root = &self.context.project_root;

        let updated = reorder_queue(project_root, request.task_ids).map_err(|e| {
            WebApiError::new(
                "internal_error",
                format!("failed to reorder queue: {}", e),
                1,
            )
        })?;

        if updated {
            self.publish_event("queue-reorder", serde_json::json!({ "message": "queue reordered" }));
        }

        Ok(serde_json::json!({ "reordered": updated }))
    }

    pub async fn queue_hold(
        &self,
        task_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, WebApiError> {
        let _request: QueueHoldRequest =
            parse_json_body(body).unwrap_or(QueueHoldRequest {});
        let project_root = &self.context.project_root;

        let updated = hold_queue_entry(project_root, task_id).map_err(|e| {
            WebApiError::new(
                "internal_error",
                format!("failed to hold task: {}", e),
                1,
            )
        })?;

        if updated {
            self.publish_event(
                "queue-hold",
                serde_json::json!({ "task_id": task_id, "held": true }),
            );
        }

        Ok(serde_json::json!({ "held": updated, "task_id": task_id }))
    }

    pub async fn queue_release(
        &self,
        task_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, WebApiError> {
        let _request: QueueReleaseRequest =
            parse_json_body(body).unwrap_or(QueueReleaseRequest { reason: None });
        let project_root = &self.context.project_root;

        let updated = release_queue_entry(project_root, task_id).map_err(|e| {
            WebApiError::new(
                "internal_error",
                format!("failed to release task: {}", e),
                1,
            )
        })?;

        if updated {
            self.publish_event(
                "queue-release",
                serde_json::json!({ "task_id": task_id, "released": true }),
            );
        }

        Ok(serde_json::json!({ "released": updated, "task_id": task_id }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn em_work_queue_entry_status_deserializes_snake_case() {
        let pending: EmWorkQueueEntryStatus = serde_json::from_str("\"pending\"").unwrap();
        assert_eq!(pending, EmWorkQueueEntryStatus::Pending);

        let assigned: EmWorkQueueEntryStatus = serde_json::from_str("\"assigned\"").unwrap();
        assert_eq!(assigned, EmWorkQueueEntryStatus::Assigned);

        let held: EmWorkQueueEntryStatus = serde_json::from_str("\"held\"").unwrap();
        assert_eq!(held, EmWorkQueueEntryStatus::Held);
    }

    #[test]
    fn em_work_queue_entry_status_defaults_to_unknown_for_invalid() {
        let unknown: EmWorkQueueEntryStatus = serde_json::from_str("\"invalid_status\"").unwrap();
        assert_eq!(unknown, EmWorkQueueEntryStatus::Unknown);
    }

    #[test]
    fn em_work_queue_entry_default_status_is_pending() {
        let entry: EmWorkQueueEntry = serde_json::from_str("{\"task_id\": \"TASK-001\"}").unwrap();
        assert_eq!(entry.status, EmWorkQueueEntryStatus::Pending);
        assert_eq!(entry.task_id, "TASK-001");
    }

    #[test]
    fn em_work_queue_state_deserializes_entries_wrapped() {
        let json = r#"{"entries": [
            {"task_id": "TASK-001", "status": "pending"},
            {"task_id": "TASK-002", "status": "assigned"}
        ]}"#;
        let state: EmWorkQueueState = serde_json::from_str(json).unwrap();
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].task_id, "TASK-001");
        assert_eq!(state.entries[1].task_id, "TASK-002");
    }

    #[test]
    fn em_work_queue_state_serializes_and_deserializes_roundtrip() {
        let original = EmWorkQueueState {
            entries: vec![EmWorkQueueEntry {
                task_id: "TASK-001".to_string(),
                status: EmWorkQueueEntryStatus::Pending,
                workflow_id: Some("WF-001".to_string()),
                assigned_at: Some("2025-01-01T00:00:00Z".to_string()),
                held_at: None,
            }],
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: EmWorkQueueState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].task_id, "TASK-001");
        assert_eq!(restored.entries[0].status, EmWorkQueueEntryStatus::Pending);
        assert_eq!(restored.entries[0].workflow_id, Some("WF-001".to_string()));
    }

    #[test]
    fn em_work_queue_state_default_is_empty() {
        let state = EmWorkQueueState::default();
        assert!(state.entries.is_empty());
    }

    impl Default for EmWorkQueueEntry {
        fn default() -> Self {
            Self {
                task_id: String::new(),
                status: EmWorkQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
            }
        }
    }
}
