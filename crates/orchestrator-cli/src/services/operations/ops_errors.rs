use crate::cli_types::ErrorsCommand;
use crate::print_value;
use anyhow::{anyhow, Result};
use chrono::Utc;
use orchestrator_core::{load_errors, save_errors, ErrorRecord, ErrorStore};
use std::collections::HashMap;
use std::fs;
use uuid::Uuid;

fn sync_errors_from_daemon_events(project_root: &str) -> Result<ErrorStore> {
    let canonical = crate::services::runtime::canonicalize_lossy(project_root);
    let mut store = load_errors(project_root)?;
    let path = crate::services::runtime::daemon_events_log_path();
    if !path.exists() {
        return Ok(store);
    }
    let content = fs::read_to_string(path)?;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<crate::services::runtime::DaemonEventRecord>(line)
        else {
            continue;
        };
        if record.event_type != "log" {
            continue;
        }
        if let Some(root) = record.project_root.as_deref() {
            if crate::services::runtime::canonicalize_lossy(root) != canonical {
                continue;
            }
        }
        let level = record
            .data
            .get("level")
            .and_then(|value| value.as_str())
            .unwrap_or("info")
            .to_ascii_lowercase();
        if level != "error" {
            continue;
        }
        if store
            .errors
            .iter()
            .any(|error| error.source_event_id.as_deref() == Some(record.id.as_str()))
        {
            continue;
        }
        let message = record
            .data
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("daemon error")
            .to_string();
        let lower = message.to_ascii_lowercase();
        let recoverable = lower.contains("connection")
            || lower.contains("timeout")
            || lower.contains("unavailable");
        store.errors.push(ErrorRecord {
            id: format!("ERR-{}", Uuid::new_v4().simple()),
            category: "daemon".to_string(),
            severity: "error".to_string(),
            message,
            task_id: None,
            workflow_id: None,
            recoverable,
            recovered: false,
            created_at: record.timestamp,
            source_event_id: Some(record.id),
        });
    }
    save_errors(project_root, &store)?;
    Ok(store)
}

pub(crate) async fn handle_errors(
    command: ErrorsCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        ErrorsCommand::List(args) => {
            let mut store = sync_errors_from_daemon_events(project_root)?;
            if let Some(category) = args.category {
                store
                    .errors
                    .retain(|error| error.category.eq_ignore_ascii_case(category.as_str()));
            }
            if let Some(severity) = args.severity {
                store
                    .errors
                    .retain(|error| error.severity.eq_ignore_ascii_case(severity.as_str()));
            }
            if let Some(task_id) = args.task_id {
                store
                    .errors
                    .retain(|error| error.task_id.as_deref() == Some(task_id.as_str()));
            }
            if let Some(limit) = args.limit {
                if store.errors.len() > limit {
                    store.errors = store.errors.split_off(store.errors.len() - limit);
                }
            }
            print_value(store.errors, json)
        }
        ErrorsCommand::Get(args) => {
            let store = sync_errors_from_daemon_events(project_root)?;
            let error = store
                .errors
                .into_iter()
                .find(|error| error.id == args.id)
                .ok_or_else(|| anyhow!("error not found: {}", args.id))?;
            print_value(error, json)
        }
        ErrorsCommand::Stats => {
            let store = sync_errors_from_daemon_events(project_root)?;
            let mut by_category: HashMap<String, usize> = HashMap::new();
            let mut by_severity: HashMap<String, usize> = HashMap::new();
            let recovered = store.errors.iter().filter(|error| error.recovered).count();
            let recoverable = store
                .errors
                .iter()
                .filter(|error| error.recoverable)
                .count();
            for error in &store.errors {
                *by_category.entry(error.category.clone()).or_insert(0) += 1;
                *by_severity.entry(error.severity.clone()).or_insert(0) += 1;
            }
            print_value(
                serde_json::json!({
                    "total": store.errors.len(),
                    "recovered": recovered,
                    "recoverable": recoverable,
                    "by_category": by_category,
                    "by_severity": by_severity,
                }),
                json,
            )
        }
        ErrorsCommand::Retry(args) => {
            let mut store = sync_errors_from_daemon_events(project_root)?;
            let error = store
                .errors
                .iter_mut()
                .find(|error| error.id == args.id)
                .ok_or_else(|| anyhow!("error not found: {}", args.id))?;
            if error.recoverable {
                error.recovered = true;
            }
            let result = serde_json::json!({
                "error_id": error.id,
                "can_recover": error.recoverable,
                "recovered": error.recovered,
            });
            save_errors(project_root, &store)?;
            print_value(result, json)
        }
        ErrorsCommand::Cleanup(args) => {
            let cutoff = Utc::now() - chrono::Duration::days(args.days as i64);
            let mut store = sync_errors_from_daemon_events(project_root)?;
            let before_len = store.errors.len();
            store.errors.retain(|error| {
                error
                    .created_at
                    .parse::<chrono::DateTime<chrono::FixedOffset>>()
                    .map(|value: chrono::DateTime<chrono::FixedOffset>| {
                        value.with_timezone(&chrono::Utc) >= cutoff
                    })
                    .unwrap_or(true)
            });
            save_errors(project_root, &store)?;
            let removed = before_len.saturating_sub(store.errors.len());
            print_value(serde_json::json!({ "removed": removed }), json)
        }
    }
}
