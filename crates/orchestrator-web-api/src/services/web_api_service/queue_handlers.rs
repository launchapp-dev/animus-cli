use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    parsing::parse_json_body,
    requests::{QueueHoldRequest, QueueReorderRequest, QueueReleaseRequest},
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmWorkQueueEntry {
    task_id: String,
    #[serde(default)]
    status: EmWorkQueueEntryStatus,
    #[serde(default)]
    workflow_id: Option<String>,
    #[serde(default)]
    queued_at: Option<String>,
    #[serde(default)]
    assigned_at: Option<String>,
    #[serde(default)]
    held_at: Option<String>,
    #[serde(default)]
    hold_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EmWorkQueueState {
    #[serde(default)]
    entries: Vec<EmWorkQueueEntry>,
}

fn em_work_queue_state_path(project_root: &str) -> Result<PathBuf> {
    let path = PathBuf::from(project_root)
        .join(".ao")
        .join("runtime")
        .join("scheduler")
        .join(EM_WORK_QUEUE_STATE_FILE);
    Ok(path)
}

fn load_em_work_queue_state(project_root: &str) -> Result<Option<EmWorkQueueState>> {
    let path = em_work_queue_state_path(project_root)?;
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

fn save_em_work_queue_state(project_root: &str, state: &EmWorkQueueState) -> Result<()> {
    let path = em_work_queue_state_path(project_root)?;
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

impl WebApiService {
    pub async fn queue_list(&self) -> Result<serde_json::Value, WebApiError> {
        let project_root = &self.context.project_root;

        let queue_state = load_em_work_queue_state(project_root)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?;

        let entries = queue_state
            .map(|state| state.entries)
            .unwrap_or_default();

        let enriched_entries: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|entry| {
                json!({
                    "task_id": entry.task_id,
                    "status": match entry.status {
                        EmWorkQueueEntryStatus::Pending => "pending",
                        EmWorkQueueEntryStatus::Assigned => "assigned",
                        EmWorkQueueEntryStatus::Held => "held",
                        EmWorkQueueEntryStatus::Unknown => "unknown",
                    },
                    "workflow_id": entry.workflow_id,
                    "queued_at": entry.queued_at,
                    "assigned_at": entry.assigned_at,
                    "held_at": entry.held_at,
                    "hold_reason": entry.hold_reason,
                })
            })
            .collect();

        let total = enriched_entries.len();
        Ok(json!({
            "entries": enriched_entries,
            "total": total,
        }))
    }

    pub async fn queue_stats(&self) -> Result<serde_json::Value, WebApiError> {
        let project_root = &self.context.project_root;

        let queue_state = load_em_work_queue_state(project_root)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?;

        let entries = queue_state
            .map(|state| state.entries)
            .unwrap_or_default();

        let pending = entries
            .iter()
            .filter(|e| e.status == EmWorkQueueEntryStatus::Pending)
            .count();
        let assigned = entries
            .iter()
            .filter(|e| e.status == EmWorkQueueEntryStatus::Assigned)
            .count();
        let held = entries
            .iter()
            .filter(|e| e.status == EmWorkQueueEntryStatus::Held)
            .count();

        Ok(json!({
            "depth": entries.len(),
            "pending": pending,
            "assigned": assigned,
            "held": held,
            "ready": pending,
        }))
    }

    pub async fn queue_reorder(&self, body: serde_json::Value) -> Result<serde_json::Value, WebApiError> {
        let request: QueueReorderRequest = parse_json_body(body)?;
        let project_root = &self.context.project_root;

        let Some(mut state) = load_em_work_queue_state(project_root)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?
        else {
            return Err(WebApiError::new(
                "not_found",
                "queue not initialized",
                3,
            ));
        };

        let mut new_entries = Vec::new();
        for task_id in &request.task_ids {
            if let Some(entry) = state.entries.iter().find(|e| &e.task_id == task_id) {
                new_entries.push(entry.clone());
            }
        }

        for entry in &state.entries {
            if !request.task_ids.contains(&entry.task_id) {
                new_entries.push(entry.clone());
            }
        }

        state.entries = new_entries;

        save_em_work_queue_state(project_root, &state)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?;

        self.publish_event("queue-reorder", json!({}));

        Ok(json!({ "message": "queue reordered successfully" }))
    }

    pub async fn queue_hold(
        &self,
        task_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, WebApiError> {
        let request: QueueHoldRequest = parse_json_body(body)?;
        let project_root = &self.context.project_root;

        let Some(mut state) = load_em_work_queue_state(project_root)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?
        else {
            return Err(WebApiError::new(
                "not_found",
                "queue not initialized",
                3,
            ));
        };

        let mut found = false;
        for entry in &mut state.entries {
            if entry.task_id != task_id {
                continue;
            }
            if entry.status != EmWorkQueueEntryStatus::Pending {
                return Err(WebApiError::new(
                    "conflict",
                    format!("task {} is not in pending state", task_id),
                    4,
                ));
            }
            entry.status = EmWorkQueueEntryStatus::Held;
            entry.held_at = Some(Utc::now().to_rfc3339());
            entry.hold_reason = request.reason.clone();
            found = true;
            break;
        }

        if !found {
            return Err(WebApiError::new(
                "not_found",
                format!("task {} not found in queue", task_id),
                3,
            ));
        }

        save_em_work_queue_state(project_root, &state)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?;

        self.publish_event(
            "queue-hold",
            json!({ "task_id": task_id, "reason": request.reason }),
        );

        Ok(json!({ "message": "task held successfully", "task_id": task_id }))
    }

    pub async fn queue_release(
        &self,
        task_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, WebApiError> {
        let _request: QueueReleaseRequest = parse_json_body(body)?;
        let project_root = &self.context.project_root;

        let Some(mut state) = load_em_work_queue_state(project_root)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?
        else {
            return Err(WebApiError::new(
                "not_found",
                "queue not initialized",
                3,
            ));
        };

        let mut found = false;
        for entry in &mut state.entries {
            if entry.task_id != task_id {
                continue;
            }
            if entry.status != EmWorkQueueEntryStatus::Held {
                return Err(WebApiError::new(
                    "conflict",
                    format!("task {} is not in held state", task_id),
                    4,
                ));
            }
            entry.status = EmWorkQueueEntryStatus::Pending;
            entry.held_at = None;
            entry.hold_reason = None;
            found = true;
            break;
        }

        if !found {
            return Err(WebApiError::new(
                "not_found",
                format!("task {} not found in queue", task_id),
                3,
            ));
        }

        save_em_work_queue_state(project_root, &state)
            .map_err(|e| WebApiError::new("internal", e.to_string(), 1))?;

        self.publish_event("queue-release", json!({ "task_id": task_id }));

        Ok(json!({ "message": "task released successfully", "task_id": task_id }))
    }
}
