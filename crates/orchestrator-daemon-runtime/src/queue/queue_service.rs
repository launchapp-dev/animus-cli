use anyhow::Result;
use chrono::Utc;
use protocol::SubjectDispatch;
use serde::Serialize;

use crate::{
    load_dispatch_queue_state, save_dispatch_queue_state, DispatchQueueEntry, DispatchQueueEntryStatus,
    DispatchQueueState,
};

#[derive(Debug, Clone, Serialize)]
pub struct QueueEntrySnapshot {
    pub subject_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<SubjectDispatch>,
    pub status: DispatchQueueEntryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueStats {
    pub total: usize,
    pub pending: usize,
    pub assigned: usize,
    pub held: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueSnapshot {
    pub entries: Vec<QueueEntrySnapshot>,
    pub stats: QueueStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueEnqueueResult {
    pub enqueued: bool,
    pub subject_id: String,
}

pub fn queue_snapshot(project_root: &str) -> Result<QueueSnapshot> {
    let state = load_dispatch_queue_state(project_root)?.unwrap_or_default();
    Ok(snapshot_from_state(&state))
}

pub fn queue_stats(project_root: &str) -> Result<QueueStats> {
    Ok(queue_snapshot(project_root)?.stats)
}

pub fn enqueue_subject_dispatch(project_root: &str, dispatch: SubjectDispatch) -> Result<QueueEnqueueResult> {
    let mut state = load_dispatch_queue_state(project_root)?.unwrap_or_default();
    let subject_id = dispatch.subject_key();

    if state.entries.iter().any(|entry| {
        entry.subject_id() == subject_id
            && entry.status != DispatchQueueEntryStatus::Unknown
            && if let Some(existing) = entry.dispatch.as_ref() {
                existing.workflow_ref == dispatch.workflow_ref
            } else {
                match (entry.task_id(), dispatch.task_id()) {
                    (Some(existing), Some(incoming)) => existing == incoming,
                    _ => false,
                }
            }
    }) {
        return Ok(QueueEnqueueResult { enqueued: false, subject_id });
    }

    state.entries.push(DispatchQueueEntry::from_dispatch(dispatch));
    save_dispatch_queue_state(project_root, &state)?;
    Ok(QueueEnqueueResult { enqueued: true, subject_id })
}

pub fn hold_subject(project_root: &str, subject_id: &str) -> Result<bool> {
    let Some(mut state) = load_dispatch_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut updated = false;
    for entry in &mut state.entries {
        if entry.subject_id() != subject_id || entry.status != DispatchQueueEntryStatus::Pending {
            continue;
        }
        entry.status = DispatchQueueEntryStatus::Held;
        entry.held_at = Some(Utc::now().to_rfc3339());
        updated = true;
        break;
    }

    if updated {
        save_dispatch_queue_state(project_root, &state)?;
    }
    Ok(updated)
}

pub fn release_subject(project_root: &str, subject_id: &str) -> Result<bool> {
    let Some(mut state) = load_dispatch_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut updated = false;
    for entry in &mut state.entries {
        if entry.subject_id() != subject_id || entry.status != DispatchQueueEntryStatus::Held {
            continue;
        }
        entry.status = DispatchQueueEntryStatus::Pending;
        entry.held_at = None;
        updated = true;
        break;
    }

    if updated {
        save_dispatch_queue_state(project_root, &state)?;
    }
    Ok(updated)
}

pub fn reorder_subjects(project_root: &str, subject_ids: Vec<String>) -> Result<bool> {
    let Some(mut state) = load_dispatch_queue_state(project_root)? else {
        return Ok(false);
    };

    let original_order = state.entries.iter().map(|entry| entry.subject_id().to_string()).collect::<Vec<_>>();
    let mut reordered = Vec::new();
    let mut consumed = vec![false; state.entries.len()];

    for subject_id in &subject_ids {
        for (index, entry) in state.entries.iter().enumerate() {
            if consumed[index] || entry.subject_id() != subject_id {
                continue;
            }
            consumed[index] = true;
            reordered.push(entry.clone());
        }
    }

    for (index, entry) in state.entries.iter().enumerate() {
        if !consumed[index] {
            reordered.push(entry.clone());
        }
    }

    let reordered_order = reordered.iter().map(|entry| entry.subject_id().to_string()).collect::<Vec<_>>();
    if reordered_order == original_order {
        return Ok(false);
    }

    state.entries = reordered;
    save_dispatch_queue_state(project_root, &state)?;
    Ok(true)
}

fn snapshot_from_state(state: &DispatchQueueState) -> QueueSnapshot {
    let entries = state
        .entries
        .iter()
        .map(|entry| QueueEntrySnapshot {
            subject_id: entry.subject_id().to_string(),
            task_id: entry.task_id().map(ToOwned::to_owned),
            dispatch: entry.dispatch.clone(),
            status: entry.status,
            workflow_id: entry.workflow_id.clone(),
            assigned_at: entry.assigned_at.clone(),
            held_at: entry.held_at.clone(),
        })
        .collect::<Vec<_>>();

    let stats = QueueStats {
        total: state.entries.len(),
        pending: state.entries.iter().filter(|entry| entry.status == DispatchQueueEntryStatus::Pending).count(),
        assigned: state.entries.iter().filter(|entry| entry.status == DispatchQueueEntryStatus::Assigned).count(),
        held: state.entries.iter().filter(|entry| entry.status == DispatchQueueEntryStatus::Held).count(),
    };

    QueueSnapshot { entries, stats }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use protocol::SubjectRef;

    #[test]
    fn enqueue_subject_dispatch_is_idempotent_for_same_task_pipeline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().to_string();
        let dispatch = SubjectDispatch::for_task_with_metadata(
            "TASK-1",
            "standard",
            "manual-queue-enqueue",
            Utc.with_ymd_and_hms(2026, 3, 7, 23, 0, 0).unwrap(),
        );

        let first = enqueue_subject_dispatch(&project_root, dispatch.clone()).expect("enqueue");
        let second = enqueue_subject_dispatch(&project_root, dispatch).expect("enqueue");

        assert!(first.enqueued);
        assert!(!second.enqueued);
        let snapshot = queue_snapshot(&project_root).expect("snapshot");
        assert_eq!(snapshot.stats.total, 1);
        assert_eq!(snapshot.entries[0].subject_id, "TASK-1");
    }

    #[test]
    fn hold_release_and_reorder_use_subject_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().to_string();
        enqueue_subject_dispatch(&project_root, SubjectDispatch::for_task("TASK-1", "standard"))
            .expect("enqueue first");
        enqueue_subject_dispatch(&project_root, SubjectDispatch::for_task("TASK-2", "standard"))
            .expect("enqueue second");

        assert!(hold_subject(&project_root, "TASK-2").expect("hold"));
        assert!(release_subject(&project_root, "TASK-2").expect("release"));
        assert!(reorder_subjects(&project_root, vec!["TASK-2".into(), "TASK-1".into()]).expect("reorder"));

        let snapshot = queue_snapshot(&project_root).expect("snapshot");
        assert_eq!(snapshot.entries[0].subject_id, "TASK-2");
        assert_eq!(snapshot.entries[1].subject_id, "TASK-1");
    }

    #[test]
    fn enqueue_subject_dispatch_accepts_non_task_subjects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().to_string();

        let result = enqueue_subject_dispatch(
            &project_root,
            SubjectDispatch::for_requirement("REQ-39", "planning", "manual-queue-enqueue")
                .with_input(Some(json!({"scope":"shared-ingress"}))),
        )
        .expect("enqueue");

        assert!(result.enqueued);
        let snapshot = queue_snapshot(&project_root).expect("snapshot");
        assert_eq!(snapshot.stats.total, 1);
        assert_eq!(snapshot.entries[0].subject_id, "REQ-39");
        assert!(snapshot.entries[0].task_id.is_none());
        assert_eq!(
            snapshot.entries[0].dispatch.as_ref().map(|dispatch| dispatch.workflow_ref.as_str()),
            Some("planning")
        );
    }

    #[test]
    fn reorder_subjects_keeps_all_entries_for_same_subject() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().to_string();
        enqueue_subject_dispatch(&project_root, SubjectDispatch::for_task("TASK-1", "standard"))
            .expect("enqueue standard");
        enqueue_subject_dispatch(&project_root, SubjectDispatch::for_task("TASK-2", "standard"))
            .expect("enqueue second");
        enqueue_subject_dispatch(&project_root, SubjectDispatch::for_task("TASK-1", "ops")).expect("enqueue ops");

        assert!(reorder_subjects(&project_root, vec!["TASK-1".into()]).expect("reorder"));

        let snapshot = queue_snapshot(&project_root).expect("snapshot");
        assert_eq!(snapshot.stats.total, 3);
        assert_eq!(snapshot.entries[0].subject_id, "TASK-1");
        assert_eq!(
            snapshot.entries[0].dispatch.as_ref().map(|dispatch| dispatch.workflow_ref.as_str()),
            Some("standard")
        );
        assert_eq!(snapshot.entries[1].subject_id, "TASK-1");
        assert_eq!(snapshot.entries[1].dispatch.as_ref().map(|dispatch| dispatch.workflow_ref.as_str()), Some("ops"));
        assert_eq!(snapshot.entries[2].subject_id, "TASK-2");
    }

    #[test]
    fn generic_subjects_use_kind_qualified_queue_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().to_string();
        let dispatch = SubjectDispatch::for_subject_with_metadata(
            SubjectRef::new("pack.review", "REV-7"),
            "review",
            "manual-queue-enqueue",
            Utc.with_ymd_and_hms(2026, 3, 8, 8, 0, 0).unwrap(),
        );

        let result = enqueue_subject_dispatch(&project_root, dispatch).expect("enqueue");

        assert!(result.enqueued);
        assert_eq!(result.subject_id, "pack.review::REV-7");
        let snapshot = queue_snapshot(&project_root).expect("snapshot");
        assert_eq!(snapshot.entries[0].subject_id, "pack.review::REV-7");
        assert!(snapshot.entries[0].task_id.is_none());
    }
}
