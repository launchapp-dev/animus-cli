use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::{
    load_history_store, load_workflow_history_summaries, save_history_store, HistoryExecutionRecord,
    WorkflowHistorySummary, WorkflowStateManager,
};

use crate::cli_types::HistoryCommand;
use crate::{not_found_error, print_value};

#[derive(Debug, Clone)]
struct HistoryRecordCandidate {
    execution_id: String,
    task_id: Option<String>,
    workflow_id: Option<String>,
    status: String,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    stored_record: Option<HistoryExecutionRecord>,
}

impl HistoryRecordCandidate {
    fn from_stored(record: HistoryExecutionRecord) -> Self {
        Self {
            execution_id: record.execution_id.clone(),
            task_id: record.task_id.clone(),
            workflow_id: record.workflow_id.clone(),
            status: record.status.clone(),
            started_at: parse_record_timestamp(record.started_at.as_deref()),
            completed_at: parse_record_timestamp(record.completed_at.as_deref()),
            stored_record: Some(record),
        }
    }

    fn from_workflow_summary(summary: WorkflowHistorySummary) -> Self {
        Self {
            execution_id: summary.workflow_id.clone(),
            task_id: Some(summary.task_id),
            workflow_id: Some(summary.workflow_id),
            status: summary.status,
            started_at: Some(summary.started_at),
            completed_at: summary.completed_at,
            stored_record: None,
        }
    }
}

fn parse_record_timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    value.and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok()).map(|value| value.with_timezone(&Utc))
}

fn workflow_to_history_record(workflow: orchestrator_core::OrchestratorWorkflow) -> HistoryExecutionRecord {
    HistoryExecutionRecord {
        execution_id: workflow.id.clone(),
        task_id: Some(workflow.task_id.clone()),
        workflow_id: Some(workflow.id.clone()),
        run_id: None,
        status: serde_json::to_string(&workflow.status)
            .unwrap_or_else(|_| "\"unknown\"".to_string())
            .trim_matches('"')
            .to_string(),
        started_at: Some(workflow.started_at.to_rfc3339()),
        completed_at: workflow.completed_at.map(|value| value.to_rfc3339()),
        details: serde_json::to_value(&workflow).unwrap_or_else(|_| serde_json::json!({})),
    }
}

fn minimal_history_record(candidate: &HistoryRecordCandidate) -> HistoryExecutionRecord {
    HistoryExecutionRecord {
        execution_id: candidate.execution_id.clone(),
        task_id: candidate.task_id.clone(),
        workflow_id: candidate.workflow_id.clone(),
        run_id: None,
        status: candidate.status.clone(),
        started_at: candidate.started_at.map(|value| value.to_rfc3339()),
        completed_at: candidate.completed_at.map(|value| value.to_rfc3339()),
        details: serde_json::json!({}),
    }
}

fn collect_execution_candidates(project_root: &str) -> Result<Vec<HistoryRecordCandidate>> {
    let store = load_history_store(project_root)?;
    let mut seen = HashSet::with_capacity(store.entries.len());
    let mut candidates = Vec::with_capacity(store.entries.len());

    for record in store.entries {
        seen.insert(record.execution_id.clone());
        candidates.push(HistoryRecordCandidate::from_stored(record));
    }

    for workflow in load_workflow_history_summaries(std::path::Path::new(project_root))? {
        if seen.insert(workflow.workflow_id.clone()) {
            candidates.push(HistoryRecordCandidate::from_workflow_summary(workflow));
        }
    }

    candidates.sort_by(|left, right| {
        right.started_at.cmp(&left.started_at).then_with(|| left.execution_id.cmp(&right.execution_id))
    });
    Ok(candidates)
}

fn load_workflow_records(project_root: &str, workflow_ids: &[String]) -> HashMap<String, HistoryExecutionRecord> {
    let manager = WorkflowStateManager::new(project_root);
    let mut records = HashMap::with_capacity(workflow_ids.len());
    for workflow_id in workflow_ids {
        if let Ok(workflow) = manager.load(workflow_id) {
            records.insert(workflow_id.clone(), workflow_to_history_record(workflow));
        }
    }
    records
}

/// Fill `run_id` for records that predate the field (or were synthesized from
/// workflow state). The authoritative source is the per-phase session
/// checkpoints under `runs/<workflow_id>/phases/` — the latest run wins,
/// matching what `animus output read --workflow-id` resolves. Best-effort:
/// records stay `None` when no run is recorded or the latest is ambiguous.
fn enrich_run_ids(project_root: &str, records: &mut [HistoryExecutionRecord]) {
    for record in records.iter_mut() {
        if record.run_id.is_some() {
            continue;
        }
        if let Some(workflow_id) = record.workflow_id.as_deref() {
            record.run_id = super::ops_output::try_latest_run_id_for_workflow(project_root, workflow_id);
        }
    }
}

fn hydrate_candidates(project_root: &str, candidates: Vec<HistoryRecordCandidate>) -> Vec<HistoryExecutionRecord> {
    let workflow_ids: Vec<String> = candidates
        .iter()
        .filter(|candidate| candidate.stored_record.is_none())
        .filter_map(|candidate| candidate.workflow_id.clone())
        .collect();
    let workflow_records = load_workflow_records(project_root, &workflow_ids);

    let mut records: Vec<HistoryExecutionRecord> = candidates
        .into_iter()
        .map(|candidate| {
            if let Some(record) = candidate.stored_record {
                return record;
            }

            candidate
                .workflow_id
                .as_ref()
                .and_then(|workflow_id| workflow_records.get(workflow_id).cloned())
                .unwrap_or_else(|| minimal_history_record(&candidate))
        })
        .collect();
    enrich_run_ids(project_root, &mut records);
    records
}

fn candidate_matches_filters(
    candidate: &HistoryRecordCandidate,
    task_id: Option<&str>,
    workflow_id: Option<&str>,
    status: Option<&str>,
    started_after: Option<DateTime<Utc>>,
    started_before: Option<DateTime<Utc>>,
) -> bool {
    if let Some(task_id) = task_id {
        if candidate.task_id.as_deref() != Some(task_id) {
            return false;
        }
    }
    if let Some(workflow_id) = workflow_id {
        if candidate.workflow_id.as_deref() != Some(workflow_id) {
            return false;
        }
    }
    if let Some(status) = status {
        if !candidate.status.eq_ignore_ascii_case(status) {
            return false;
        }
    }
    if let Some(started_after) = started_after {
        if candidate.started_at.map(|value| value < started_after).unwrap_or(true) {
            return false;
        }
    }
    if let Some(started_before) = started_before {
        if candidate.started_at.map(|value| value > started_before).unwrap_or(true) {
            return false;
        }
    }

    true
}

pub(crate) async fn handle_history(command: HistoryCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        HistoryCommand::Task(args) => {
            let mut candidates = collect_execution_candidates(project_root)?;
            candidates.retain(|candidate| candidate.task_id.as_deref() == Some(args.task_id.as_str()));
            if let Some(limit) = args.limit {
                candidates.truncate(limit);
            }
            print_value(hydrate_candidates(project_root, candidates), json)
        }
        HistoryCommand::Get(args) => {
            let store = load_history_store(project_root)?;
            if let Some(record) = store.entries.into_iter().find(|record| record.execution_id == args.id) {
                let mut records = [record];
                enrich_run_ids(project_root, &mut records);
                let [record] = records;
                return print_value(record, json);
            }

            let workflow = WorkflowStateManager::new(project_root)
                .load(&args.id)
                .map(workflow_to_history_record)
                .map_err(|_| not_found_error(format!("execution not found: {}", args.id)))?;
            let mut records = [workflow];
            enrich_run_ids(project_root, &mut records);
            let [workflow] = records;
            print_value(workflow, json)
        }
        HistoryCommand::Recent(args) => {
            let mut candidates = collect_execution_candidates(project_root)?;
            candidates.truncate(args.limit.unwrap_or(100));
            print_value(hydrate_candidates(project_root, candidates), json)
        }
        HistoryCommand::Search(args) => {
            // --since <DURATION> is a relative spelling of --started-after;
            // clap marks them mutually exclusive. Windows too large for
            // chrono (or underflowing the epoch) degrade to "no lower
            // bound", which is what an enormous lookback means anyway.
            let started_after = match args.since {
                Some(secs) => chrono::Duration::try_seconds(secs.min(i64::MAX as u64) as i64)
                    .and_then(|window| Utc::now().checked_sub_signed(window)),
                None => args
                    .started_after
                    .as_deref()
                    .map(chrono::DateTime::parse_from_rfc3339)
                    .transpose()?
                    .map(|value| value.with_timezone(&Utc)),
            };
            let started_before = args
                .started_before
                .as_deref()
                .map(chrono::DateTime::parse_from_rfc3339)
                .transpose()?
                .map(|value| value.with_timezone(&Utc));

            let mut candidates = collect_execution_candidates(project_root)?;
            candidates.retain(|candidate| {
                candidate_matches_filters(
                    candidate,
                    args.task_id.as_deref(),
                    args.workflow_id.as_deref(),
                    args.status.as_deref(),
                    started_after,
                    started_before,
                )
            });

            let offset = args.offset.unwrap_or(0);
            let limit = args.limit.unwrap_or(candidates.len());
            let result: Vec<_> = candidates.into_iter().skip(offset).take(limit).collect();
            print_value(hydrate_candidates(project_root, result), json)
        }
        HistoryCommand::Cleanup(args) => {
            let cutoff = Utc::now() - chrono::Duration::days(args.days.max(0));
            let mut store = load_history_store(project_root)?;
            let before_len = store.entries.len();
            store.entries.retain(|entry| {
                entry
                    .completed_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc) >= cutoff)
                    .unwrap_or(true)
            });
            save_history_store(project_root, &store)?;
            let removed = before_len.saturating_sub(store.entries.len());
            print_value(serde_json::json!({ "removed": removed }), json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;
    use protocol::RunId;

    #[test]
    fn enrich_run_ids_resolves_latest_run_from_session_checkpoints() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let root = project_root.to_string_lossy().to_string();

        let workflow_run_dir = crate::run_dir(&root, &RunId("wf-hist".to_string()), None);
        let phases_dir = workflow_run_dir.join("phases");
        std::fs::create_dir_all(&phases_dir).expect("phases dir should be created");
        std::fs::write(
            phases_dir.join("implementation.session.json"),
            serde_json::json!({
                "workflow_id": "wf-hist",
                "phase_id": "implementation",
                "provider": "claude",
                "run_id": "run-hist-latest",
                "status": "completed",
                "started_at": "2026-06-01T00:00:00Z",
            })
            .to_string(),
        )
        .expect("checkpoint should be written");

        let mut records = [
            HistoryExecutionRecord {
                execution_id: "wf-hist".to_string(),
                task_id: Some("TASK-1".to_string()),
                workflow_id: Some("wf-hist".to_string()),
                run_id: None,
                status: "completed".to_string(),
                started_at: None,
                completed_at: None,
                details: serde_json::json!({}),
            },
            HistoryExecutionRecord {
                execution_id: "wf-unknown".to_string(),
                task_id: None,
                workflow_id: Some("wf-unknown".to_string()),
                run_id: None,
                status: "failed".to_string(),
                started_at: None,
                completed_at: None,
                details: serde_json::json!({}),
            },
            HistoryExecutionRecord {
                execution_id: "wf-preset".to_string(),
                task_id: None,
                workflow_id: Some("wf-hist".to_string()),
                run_id: Some("run-already-set".to_string()),
                status: "completed".to_string(),
                started_at: None,
                completed_at: None,
                details: serde_json::json!({}),
            },
        ];
        enrich_run_ids(&root, &mut records);
        assert_eq!(records[0].run_id.as_deref(), Some("run-hist-latest"));
        assert_eq!(records[1].run_id, None, "unresolvable workflow stays None");
        assert_eq!(records[2].run_id.as_deref(), Some("run-already-set"), "stored run_id is preserved");
    }
}
