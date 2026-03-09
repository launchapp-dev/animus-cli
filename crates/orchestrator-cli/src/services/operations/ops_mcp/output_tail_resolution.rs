use super::output_tail_types::OutputTailResolution;
use crate::{ensure_safe_run_id, invalid_input_error, not_found_error, run_dir};
use anyhow::{Context, Result};
use orchestrator_core::{OrchestratorWorkflow, WorkflowStateManager, WorkflowStatus};
use protocol::RunId;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(super) fn resolve_output_tail_resolution(
    project_root: &str,
    run_id: Option<String>,
    task_id: Option<String>,
) -> Result<OutputTailResolution> {
    match (run_id, task_id) {
        (Some(run_id), None) => resolve_output_tail_run_id(project_root, run_id),
        (None, Some(task_id)) => resolve_output_tail_task_id(project_root, task_id),
        (Some(_), Some(_)) => Err(invalid_input_error(
            "provide exactly one of run_id or task_id",
        )),
        (None, None) => Err(invalid_input_error(
            "provide exactly one of run_id or task_id",
        )),
    }
}

fn resolve_output_tail_run_id(project_root: &str, run_id: String) -> Result<OutputTailResolution> {
    ensure_safe_run_id(run_id.as_str())?;
    let run_dir =
        crate::services::operations::resolve_run_dir_for_lookup(project_root, run_id.as_str())?
            .ok_or_else(|| not_found_error(format!("run directory not found for {run_id}")))?;
    Ok(OutputTailResolution {
        run_id,
        run_dir,
        resolved_from: "run_id",
    })
}

fn resolve_output_tail_task_id(
    project_root: &str,
    task_id: String,
) -> Result<OutputTailResolution> {
    let workflows = workflow_candidates_for_task(project_root, task_id.as_str())?;
    if workflows.is_empty() {
        return Err(not_found_error(format!(
            "workflow not found for task {task_id}"
        )));
    }

    for workflow in workflows {
        if let Some((run_id, run_dir)) =
            resolve_latest_workflow_run_dir(project_root, workflow.id.as_str())?
        {
            return Ok(OutputTailResolution {
                run_id,
                run_dir,
                resolved_from: "task_id",
            });
        }
    }

    Err(not_found_error(format!(
        "run directory not found for task {task_id}"
    )))
}

fn workflow_candidates_for_task(
    project_root: &str,
    task_id: &str,
) -> Result<Vec<OrchestratorWorkflow>> {
    let manager = WorkflowStateManager::new(project_root);
    let mut workflows: Vec<OrchestratorWorkflow> = manager
        .list()
        .with_context(|| format!("failed to load workflows for task {task_id}"))?
        .into_iter()
        .filter(|workflow| workflow.task_id.eq_ignore_ascii_case(task_id))
        .collect();
    workflows.sort_by(compare_workflow_candidates);
    Ok(workflows)
}

fn compare_workflow_candidates(
    left: &OrchestratorWorkflow,
    right: &OrchestratorWorkflow,
) -> Ordering {
    workflow_status_priority(left.status)
        .cmp(&workflow_status_priority(right.status))
        .then_with(|| workflow_timestamp(right).cmp(&workflow_timestamp(left)))
        .then_with(|| left.id.cmp(&right.id))
}

fn workflow_status_priority(status: WorkflowStatus) -> usize {
    match status {
        WorkflowStatus::Running => 0,
        WorkflowStatus::Escalated => 1,
        WorkflowStatus::Paused => 2,
        WorkflowStatus::Pending => 3,
        WorkflowStatus::Failed => 4,
        WorkflowStatus::Completed => 5,
        WorkflowStatus::Cancelled => 6,
    }
}

fn workflow_timestamp(workflow: &OrchestratorWorkflow) -> i64 {
    workflow
        .completed_at
        .unwrap_or(workflow.started_at)
        .timestamp_millis()
}

fn resolve_latest_workflow_run_dir(
    project_root: &str,
    workflow_id: &str,
) -> Result<Option<(String, PathBuf)>> {
    let run_ids = run_ids_for_workflow(project_root, workflow_id)?;
    let mut candidates = Vec::new();
    for run_id in run_ids {
        let Some(run_dir) =
            crate::services::operations::resolve_run_dir_for_lookup(project_root, run_id.as_str())?
        else {
            continue;
        };
        let events_path = run_dir.join("events.jsonl");
        let has_events = events_path.exists();
        let modified_millis = if has_events {
            path_modified_millis(events_path.as_path())
        } else {
            path_modified_millis(run_dir.as_path())
        };
        candidates.push((run_id, run_dir, has_events, modified_millis));
    }

    candidates.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| left.0.cmp(&right.0))
    });

    Ok(candidates
        .into_iter()
        .next()
        .map(|(run_id, run_dir, _, _)| (run_id, run_dir)))
}

fn run_ids_for_workflow(project_root: &str, workflow_id: &str) -> Result<BTreeSet<String>> {
    let mut run_ids = BTreeSet::new();
    let prefix = format!("wf-{workflow_id}-");
    for runs_root in runs_root_candidates(project_root) {
        if !runs_root.exists() {
            continue;
        }
        for entry in fs::read_dir(&runs_root)
            .with_context(|| format!("failed to read run directory {}", runs_root.display()))?
        {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if !name.starts_with(prefix.as_str()) {
                continue;
            }
            if ensure_safe_run_id(name.as_str()).is_err() {
                continue;
            }
            run_ids.insert(name);
        }
    }
    Ok(run_ids)
}

fn runs_root_candidates(project_root: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(scoped_parent) = run_dir(
        project_root,
        &RunId("output-tail-root-probe".to_string()),
        None,
    )
    .parent()
    {
        candidates.push(scoped_parent.to_path_buf());
    }
    candidates.push(Path::new(project_root).join(".ao").join("runs"));
    candidates.push(
        Path::new(project_root)
            .join(".ao")
            .join("state")
            .join("runs"),
    );

    let mut deduped = Vec::new();
    for candidate in candidates {
        if deduped.iter().all(|existing| existing != &candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn path_modified_millis(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
