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

fn hydrate_candidates(project_root: &str, candidates: Vec<HistoryRecordCandidate>) -> Vec<HistoryExecutionRecord> {
    let workflow_ids: Vec<String> = candidates
        .iter()
        .filter(|candidate| candidate.stored_record.is_none())
        .filter_map(|candidate| candidate.workflow_id.clone())
        .collect();
    let workflow_records = load_workflow_records(project_root, &workflow_ids);

    candidates
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
        .collect()
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
                return print_value(record, json);
            }

            let workflow = WorkflowStateManager::new(project_root)
                .load(&args.id)
                .map(workflow_to_history_record)
                .map_err(|_| not_found_error(format!("execution not found: {}", args.id)))?;
            print_value(workflow, json)
        }
        HistoryCommand::Recent(args) => {
            let mut candidates = collect_execution_candidates(project_root)?;
            candidates.truncate(args.limit.unwrap_or(100));
            print_value(hydrate_candidates(project_root, candidates), json)
        }
        HistoryCommand::Search(args) => {
            let started_after = args
                .started_after
                .as_deref()
                .map(chrono::DateTime::parse_from_rfc3339)
                .transpose()?
                .map(|value| value.with_timezone(&Utc));
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
