use chrono::{DateTime, Utc};
use orchestrator_core::workflow_ref_for_task;
use orchestrator_daemon_runtime::{
    drop_subject, enqueue_subject_dispatch, hold_subject, queue_snapshot, release_subject, reorder_subjects,
    DispatchQueueEntryStatus, QueueEntrySnapshot, QueueSnapshot,
};
use protocol::{orchestrator::OrchestratorTask, SubjectDispatch};

use super::{
    parsing::parse_json_body,
    requests::{QueueDropRequest, QueueEnqueueRequest, QueueHoldRequest, QueueReleaseRequest, QueueReorderRequest},
    WebApiError, WebApiService,
};

fn throughput_last_hour(snapshot: &QueueSnapshot, now: DateTime<Utc>) -> usize {
    snapshot
        .entries
        .iter()
        .filter_map(|entry| entry.assigned_at.as_deref())
        .filter_map(|assigned_at| DateTime::parse_from_rfc3339(assigned_at).ok())
        .filter(|assigned_at| now.signed_duration_since(assigned_at.with_timezone(&Utc)).num_hours() < 1)
        .count()
}

fn avg_wait_time_secs(snapshot: &QueueSnapshot, now: DateTime<Utc>) -> i64 {
    let mut total_wait_secs = 0i64;
    let mut wait_count = 0usize;

    for entry in &snapshot.entries {
        if entry.status != DispatchQueueEntryStatus::Pending {
            continue;
        }
        let Some(dispatch) = &entry.dispatch else {
            continue;
        };
        total_wait_secs += now.signed_duration_since(dispatch.requested_at).num_seconds().max(0);
        wait_count += 1;
    }

    if wait_count == 0 {
        return 0;
    }

    total_wait_secs / wait_count as i64
}

fn queue_entry_json(
    entry: &QueueEntrySnapshot,
    task_lookup: &std::collections::HashMap<&str, &OrchestratorTask>,
    position: usize,
    now: DateTime<Utc>,
) -> serde_json::Value {
    let task = entry.task_id.as_deref().and_then(|task_id| task_lookup.get(task_id));

    let wait_time =
        entry.dispatch.as_ref().map(|d| now.signed_duration_since(d.requested_at).num_seconds().max(0) as f64);

    serde_json::json!({
        "subject_id": entry.subject_id,
        "task_id": entry.task_id,
        "dispatch": entry.dispatch,
        "status": entry.status,
        "workflow_id": entry.workflow_id,
        "assigned_at": entry.assigned_at,
        "held_at": entry.held_at,
        "position": position,
        "wait_time": wait_time,
        "task": task.map(|t| serde_json::json!({
            "id": t.id,
            "title": t.title,
            "description": t.description,
            "status": t.status,
            "priority": t.priority,
        }))
    })
}

impl WebApiService {
    pub async fn queue_list(&self) -> Result<serde_json::Value, WebApiError> {
        let project_root = &self.context.project_root;
        let snapshot = queue_snapshot(project_root)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to load queue: {}", e), 1))?;

        let tasks = self.context.hub.tasks().list().await.unwrap_or_default();
        let task_lookup =
            tasks.iter().map(|task| (task.id.as_str(), task)).collect::<std::collections::HashMap<_, _>>();

        let now = Utc::now();
        let entries = snapshot
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| queue_entry_json(entry, &task_lookup, i + 1, now))
            .collect::<Vec<_>>();

        Ok(serde_json::json!({
            "entries": entries,
            "stats": {
                "total": snapshot.stats.total,
                "pending": snapshot.stats.pending,
                "assigned": snapshot.stats.assigned,
                "held": snapshot.stats.held
            }
        }))
    }

    pub async fn queue_stats(&self) -> Result<serde_json::Value, WebApiError> {
        let project_root = &self.context.project_root;
        let snapshot = queue_snapshot(project_root)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to load queue: {}", e), 1))?;
        let now = Utc::now();

        Ok(serde_json::json!({
            "depth": snapshot.stats.total,
            "pending": snapshot.stats.pending,
            "assigned": snapshot.stats.assigned,
            "held": snapshot.stats.held,
            "throughput_last_hour": throughput_last_hour(&snapshot, now),
            "avg_wait_time_secs": avg_wait_time_secs(&snapshot, now),
        }))
    }

    pub async fn queue_reorder(&self, body: serde_json::Value) -> Result<serde_json::Value, WebApiError> {
        let request: QueueReorderRequest = parse_json_body(body)?;
        let project_root = &self.context.project_root;

        let updated = reorder_subjects(project_root, request.subject_ids)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to reorder queue: {}", e), 1))?;

        if updated {
            self.publish_event("queue-reorder", serde_json::json!({ "message": "queue reordered" }));
        }

        Ok(serde_json::json!({ "reordered": updated }))
    }

    pub async fn queue_hold(&self, task_id: &str, body: serde_json::Value) -> Result<serde_json::Value, WebApiError> {
        let _request: QueueHoldRequest = parse_json_body(body).unwrap_or(QueueHoldRequest {});
        let project_root = &self.context.project_root;

        let updated = hold_subject(project_root, task_id)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to hold task: {}", e), 1))?;

        if updated {
            self.publish_event("queue-hold", serde_json::json!({ "task_id": task_id, "held": true }));
        }

        Ok(serde_json::json!({ "held": updated, "task_id": task_id }))
    }

    pub async fn queue_release(
        &self,
        task_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, WebApiError> {
        let request: QueueReleaseRequest = parse_json_body(body).unwrap_or(QueueReleaseRequest { reason: None });
        let project_root = &self.context.project_root;

        let updated = release_subject(project_root, task_id)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to release task: {}", e), 1))?;

        if updated {
            let mut payload = serde_json::json!({ "task_id": task_id, "released": true });
            if let Some(reason) = request.reason.as_deref() {
                payload["reason"] = serde_json::Value::String(reason.to_string());
            }
            self.publish_event("queue-release", payload);
        }

        let mut response = serde_json::json!({ "released": updated, "task_id": task_id });
        if let Some(reason) = request.reason.as_deref() {
            response["reason"] = serde_json::Value::String(reason.to_string());
        }

        Ok(response)
    }

    pub async fn queue_enqueue(&self, body: serde_json::Value) -> Result<serde_json::Value, WebApiError> {
        let request: QueueEnqueueRequest = parse_json_body(body)?;
        let project_root = &self.context.project_root;

        let task = self
            .context
            .hub
            .tasks()
            .get(&request.task_id)
            .await
            .map_err(|e| WebApiError::new("not_found", format!("task not found: {}", e), 1))?;

        let workflow_ref = request.workflow_ref.unwrap_or_else(|| workflow_ref_for_task(&task));
        let dispatch =
            SubjectDispatch::for_task_with_metadata(task.id.clone(), workflow_ref, "web-queue-enqueue", Utc::now());

        let result = enqueue_subject_dispatch(project_root, dispatch)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to enqueue task: {}", e), 1))?;

        if result.enqueued {
            self.publish_event("queue-enqueue", serde_json::json!({ "task_id": task.id, "enqueued": true }));

            if let Some(position) = request.position {
                let snapshot = queue_snapshot(project_root)
                    .map_err(|e| WebApiError::new("internal_error", format!("failed to load queue: {}", e), 1))?;
                let mut ids: Vec<String> =
                    snapshot.entries.iter().map(|e| e.subject_id.clone()).collect();
                if let Some(current_pos) = ids.iter().position(|id| id == &task.id) {
                    let id = ids.remove(current_pos);
                    let insert_at = position.saturating_sub(1).min(ids.len());
                    ids.insert(insert_at, id);
                    reorder_subjects(project_root, ids)
                        .map_err(|e| WebApiError::new("internal_error", format!("failed to reorder: {}", e), 1))?;
                }
            }
        }

        Ok(serde_json::json!({ "enqueued": result.enqueued, "task_id": task.id }))
    }

    pub async fn queue_drop(&self, task_id: &str, body: serde_json::Value) -> Result<serde_json::Value, WebApiError> {
        let _request: QueueDropRequest = parse_json_body(body).unwrap_or(QueueDropRequest {});
        let project_root = &self.context.project_root;

        let removed = drop_subject(project_root, task_id)
            .map_err(|e| WebApiError::new("internal_error", format!("failed to drop task: {}", e), 1))?;

        if removed > 0 {
            self.publish_event("queue-drop", serde_json::json!({ "task_id": task_id, "dropped": true }));
        }

        Ok(serde_json::json!({ "dropped": removed > 0, "task_id": task_id }))
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use protocol::SubjectDispatch;

    use super::*;

    #[test]
    fn throughput_last_hour_counts_recent_assignments() {
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 0, 30, 0).unwrap();
        let snapshot = QueueSnapshot {
            entries: vec![
                QueueEntrySnapshot {
                    subject_id: "TASK-1".into(),
                    task_id: Some("TASK-1".into()),
                    dispatch: None,
                    status: DispatchQueueEntryStatus::Assigned,
                    workflow_id: None,
                    assigned_at: Some(now.to_rfc3339()),
                    held_at: None,
                },
                QueueEntrySnapshot {
                    subject_id: "TASK-2".into(),
                    task_id: Some("TASK-2".into()),
                    dispatch: None,
                    status: DispatchQueueEntryStatus::Assigned,
                    workflow_id: None,
                    assigned_at: Some(Utc.with_ymd_and_hms(2026, 3, 7, 20, 30, 0).unwrap().to_rfc3339()),
                    held_at: None,
                },
            ],
            stats: orchestrator_daemon_runtime::QueueStats { total: 2, pending: 0, assigned: 2, held: 0 },
        };

        assert_eq!(throughput_last_hour(&snapshot, now), 1);
    }

    #[test]
    fn avg_wait_time_uses_dispatch_requested_at_for_pending_entries() {
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 0, 30, 0).unwrap();
        let snapshot = QueueSnapshot {
            entries: vec![QueueEntrySnapshot {
                subject_id: "TASK-1".into(),
                task_id: Some("TASK-1".into()),
                dispatch: Some(SubjectDispatch::for_task_with_metadata(
                    "TASK-1",
                    "ops",
                    "queue-test",
                    Utc.with_ymd_and_hms(2026, 3, 8, 0, 20, 0).unwrap(),
                )),
                status: DispatchQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
            }],
            stats: orchestrator_daemon_runtime::QueueStats { total: 1, pending: 1, assigned: 0, held: 0 },
        };

        assert_eq!(avg_wait_time_secs(&snapshot, now), 600);
    }
}
