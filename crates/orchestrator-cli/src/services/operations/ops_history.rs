use crate::cli_types::HistoryCommand;
use crate::{not_found_error, print_value};
use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{load_history_store, save_history_store, HistoryExecutionRecord, ServiceHub};
use std::sync::Arc;

async fn collect_execution_records(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<Vec<HistoryExecutionRecord>> {
    let mut combined = load_history_store(project_root)?.entries;
    let workflows = hub.workflows().list().await.unwrap_or_default();
    for workflow in workflows {
        if combined.iter().any(|entry| entry.execution_id == workflow.id) {
            continue;
        }
        combined.push(HistoryExecutionRecord {
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
        });
    }

    combined.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(combined)
}

pub(crate) async fn handle_history(
    command: HistoryCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        HistoryCommand::Task(args) => {
            let mut records = collect_execution_records(hub, project_root).await?;
            records.retain(|record| record.task_id.as_deref() == Some(args.task_id.as_str()));
            if let Some(limit) = args.limit {
                records.truncate(limit);
            }
            print_value(records, json)
        }
        HistoryCommand::Get(args) => {
            let records = collect_execution_records(hub, project_root).await?;
            let record = records
                .into_iter()
                .find(|record| record.execution_id == args.id)
                .ok_or_else(|| not_found_error(format!("execution not found: {}", args.id)))?;
            print_value(record, json)
        }
        HistoryCommand::Recent(args) => {
            let mut records = collect_execution_records(hub, project_root).await?;
            records.truncate(args.limit.unwrap_or(100));
            print_value(records, json)
        }
        HistoryCommand::Search(args) => {
            let mut records = collect_execution_records(hub, project_root).await?;
            if let Some(task_id) = args.task_id {
                records.retain(|record| record.task_id.as_deref() == Some(task_id.as_str()));
            }
            if let Some(workflow_id) = args.workflow_id {
                records.retain(|record| record.workflow_id.as_deref() == Some(workflow_id.as_str()));
            }
            if let Some(status) = args.status {
                records.retain(|record| record.status.eq_ignore_ascii_case(status.as_str()));
            }
            if let Some(started_after) = args.started_after {
                let after =
                    chrono::DateTime::parse_from_rfc3339(&started_after).map(|value| value.with_timezone(&Utc))?;
                records.retain(|record| {
                    record
                        .started_at
                        .as_deref()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| value.with_timezone(&Utc) >= after)
                        .unwrap_or(false)
                });
            }
            if let Some(started_before) = args.started_before {
                let before =
                    chrono::DateTime::parse_from_rfc3339(&started_before).map(|value| value.with_timezone(&Utc))?;
                records.retain(|record| {
                    record
                        .started_at
                        .as_deref()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| value.with_timezone(&Utc) <= before)
                        .unwrap_or(false)
                });
            }
            let offset = args.offset.unwrap_or(0);
            let limit = args.limit.unwrap_or(records.len());
            let result: Vec<_> = records.into_iter().skip(offset).take(limit).collect();
            print_value(result, json)
        }
        HistoryCommand::Cleanup(args) => {
            let cutoff = Utc::now() - chrono::Duration::days(args.days.max(0));
            let mut store = load_history_store(project_root)?;
            let before_len = store.entries.len();
            store.entries.retain(|entry| {
                let keep = entry
                    .completed_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc) >= cutoff)
                    .unwrap_or(true);
                keep
            });
            save_history_store(project_root, &store)?;
            let removed = before_len.saturating_sub(store.entries.len());
            print_value(serde_json::json!({ "removed": removed }), json)
        }
    }
}
