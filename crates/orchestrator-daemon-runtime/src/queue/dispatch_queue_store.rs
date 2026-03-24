use std::fs::{self, File};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use fs2::FileExt;
use protocol::SubjectDispatch;
use tracing::warn;
use uuid::Uuid;

use crate::{DispatchQueueEntry, DispatchQueueEntryStatus, DispatchQueueState};

const DISPATCH_QUEUE_STATE_FILE: &str = "dispatch-queue.json";
const DISPATCH_QUEUE_LOCK_FILE: &str = "dispatch-queue.lock";

fn acquire_queue_lock(project_root: &str) -> Result<File> {
    let lock_path = dispatch_queue_state_path(project_root)?.with_file_name(DISPATCH_QUEUE_LOCK_FILE);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(&lock_path)?;
    file.lock_exclusive()
        .with_context(|| format!("failed to acquire dispatch queue lock at {}", lock_path.display()))?;
    Ok(file)
}

pub fn dispatch_queue_state_path(project_root: &str) -> Result<PathBuf> {
    let runtime_root = protocol::scoped_state_root(std::path::Path::new(project_root))
        .ok_or_else(|| anyhow!("failed to resolve scoped state root for {project_root}"))?;
    Ok(runtime_root.join("scheduler").join(DISPATCH_QUEUE_STATE_FILE))
}

pub fn load_dispatch_queue_state(project_root: &str) -> Result<Option<DispatchQueueState>> {
    let path = dispatch_queue_state_path(project_root)?;
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read dispatch queue state file at {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(Some(DispatchQueueState::default()));
    }

    serde_json::from_str::<DispatchQueueState>(&content)
        .map(Some)
        .or_else(|_| {
            serde_json::from_str::<Vec<DispatchQueueEntry>>(&content)
                .map(|entries| Some(DispatchQueueState { entries }))
        })
        .with_context(|| format!("failed to parse dispatch queue state file at {}", path.display()))
}

pub fn save_dispatch_queue_state(project_root: &str, state: &DispatchQueueState) -> Result<()> {
    let path = dispatch_queue_state_path(project_root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if state.entries.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    let payload = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|value| value.to_str()).unwrap_or(DISPATCH_QUEUE_STATE_FILE),
        Uuid::new_v4()
    ));
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

pub fn mark_dispatch_queue_entry_assigned(
    project_root: &str,
    dispatch: &SubjectDispatch,
    workflow_id: Option<&str>,
) -> Result<bool> {
    let _lock = acquire_queue_lock(project_root)?;
    let Some(mut state) = load_dispatch_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut updated = false;
    for entry in &mut state.entries {
        if entry.status != DispatchQueueEntryStatus::Pending {
            continue;
        }
        if entry.subject_id() != dispatch.subject_key() {
            continue;
        }
        if entry.dispatch.as_ref().is_some_and(|existing| existing.workflow_ref != dispatch.workflow_ref) {
            continue;
        }
        if entry.dispatch.is_none() && entry.task_id() != dispatch.task_id() {
            continue;
        }
        entry.status = DispatchQueueEntryStatus::Assigned;
        if let Some(workflow_id) = workflow_id {
            entry.workflow_id = Some(workflow_id.to_string());
        }
        entry.assigned_at = Some(Utc::now().to_rfc3339());
        updated = true;
        break;
    }

    if updated {
        save_dispatch_queue_state(project_root, &state)?;
    }

    Ok(updated)
}

fn remove_terminal_dispatch_queue_entry(
    project_root: &str,
    subject_id: &str,
    workflow_ref: Option<&str>,
    workflow_id: Option<&str>,
) -> Result<usize> {
    let _lock = acquire_queue_lock(project_root)?;
    let Some(mut state) = load_dispatch_queue_state(project_root)? else {
        return Ok(0);
    };

    let before = state.entries.len();
    state.entries.retain(|entry| {
        if entry.subject_id() != subject_id {
            return true;
        }
        if entry.status != DispatchQueueEntryStatus::Assigned {
            return true;
        }
        if let Some(workflow_ref) = workflow_ref {
            if entry.dispatch.as_ref().is_some_and(|dispatch| dispatch.workflow_ref != workflow_ref) {
                return true;
            }
        }
        if let Some(workflow_id) = workflow_id {
            if entry.workflow_id.as_deref().is_some_and(|entry_workflow_id| entry_workflow_id != workflow_id) {
                return true;
            }
        }
        false
    });
    let removed = before.saturating_sub(state.entries.len());
    if removed > 0 {
        save_dispatch_queue_state(project_root, &state)?;
    }
    Ok(removed)
}

pub fn remove_terminal_dispatch_queue_entry_non_fatal(
    project_root: &str,
    subject_id: &str,
    workflow_ref: Option<&str>,
    workflow_id: Option<&str>,
) {
    if let Err(error) = remove_terminal_dispatch_queue_entry(project_root, subject_id, workflow_ref, workflow_id) {
        warn!(
            actor = protocol::ACTOR_DAEMON,
            subject_id,
            workflow_ref,
            workflow_id,
            error = %error,
            "failed to remove terminal dispatch queue entry"
        );
    }
}
