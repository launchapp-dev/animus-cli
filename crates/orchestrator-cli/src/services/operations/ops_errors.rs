use crate::cli_types::ErrorsCommand;
use crate::{not_found_error, print_value};
use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{load_errors, save_errors, ErrorRecord, ErrorStore};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use uuid::Uuid;

fn event_matches_project(record: &crate::services::runtime::DaemonEventRecord, canonical: &str) -> bool {
    if let Some(root) = record.project_root.as_deref() {
        return crate::services::runtime::canonicalize_lossy(root) == canonical;
    }
    true
}

fn daemon_log_error_record(record: &crate::services::runtime::DaemonEventRecord) -> Option<ErrorRecord> {
    let level = record.data.get("level").and_then(|value| value.as_str()).unwrap_or("info").to_ascii_lowercase();
    if level != "error" {
        return None;
    }

    let message = record.data.get("message").and_then(|value| value.as_str()).unwrap_or("daemon error").to_string();
    let lower = message.to_ascii_lowercase();
    let recoverable = lower.contains("connection") || lower.contains("timeout") || lower.contains("unavailable");

    Some(ErrorRecord {
        id: format!("ERR-{}", Uuid::new_v4().simple()),
        category: "daemon".to_string(),
        severity: "error".to_string(),
        message,
        task_id: None,
        workflow_id: None,
        recoverable,
        recovered: false,
        created_at: record.timestamp.clone(),
        source_event_id: Some(record.id.clone()),
    })
}

fn notification_data_field(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn notification_error_record(record: &crate::services::runtime::DaemonEventRecord, dead_lettered: bool) -> ErrorRecord {
    let connector_id =
        notification_data_field(&record.data, "connector_id").unwrap_or_else(|| "unknown-connector".to_string());
    let delivery_id =
        notification_data_field(&record.data, "delivery_id").unwrap_or_else(|| "unknown-delivery".to_string());
    let last_error = notification_data_field(&record.data, "last_error")
        .unwrap_or_else(|| "notification delivery failed".to_string());
    let prefix = if dead_lettered { "notification delivery dead-lettered" } else { "notification delivery failed" };

    ErrorRecord {
        id: format!("ERR-{}", Uuid::new_v4().simple()),
        category: "notification".to_string(),
        severity: if dead_lettered { "critical".to_string() } else { "error".to_string() },
        message: format!("{prefix} (connector: {connector_id}, delivery: {delivery_id}): {last_error}"),
        task_id: notification_data_field(&record.data, "task_id"),
        workflow_id: notification_data_field(&record.data, "workflow_id"),
        recoverable: if dead_lettered {
            false
        } else {
            record.data.get("retriable").and_then(Value::as_bool).unwrap_or(false)
        },
        recovered: false,
        created_at: record.timestamp.clone(),
        source_event_id: Some(record.id.clone()),
    }
}

fn daemon_event_to_error(record: &crate::services::runtime::DaemonEventRecord, canonical: &str) -> Option<ErrorRecord> {
    if !event_matches_project(record, canonical) {
        return None;
    }

    match record.event_type.as_str() {
        "log" => daemon_log_error_record(record),
        "notification-delivery-failed" => Some(notification_error_record(record, false)),
        "notification-delivery-dead-lettered" => Some(notification_error_record(record, true)),
        _ => None,
    }
}

fn sync_errors_from_daemon_events(project_root: &str) -> Result<ErrorStore> {
    let canonical = crate::services::runtime::canonicalize_lossy(project_root);
    let mut store = load_errors(project_root)?;
    let mut synced_event_ids: HashSet<String> =
        store.errors.iter().filter_map(|error| error.source_event_id.clone()).collect();
    let path = crate::services::runtime::daemon_events_log_path();
    if !path.exists() {
        return Ok(store);
    }
    let content = fs::read_to_string(path)?;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<crate::services::runtime::DaemonEventRecord>(line) else {
            continue;
        };
        if synced_event_ids.contains(record.id.as_str()) {
            continue;
        }
        let Some(error_record) = daemon_event_to_error(&record, canonical.as_str()) else {
            continue;
        };
        synced_event_ids.insert(record.id);
        store.errors.push(error_record);
    }
    save_errors(project_root, &store)?;
    Ok(store)
}

pub(crate) async fn handle_errors(command: ErrorsCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        ErrorsCommand::List(args) => {
            let mut store = sync_errors_from_daemon_events(project_root)?;
            if let Some(category) = args.category {
                store.errors.retain(|error| error.category.eq_ignore_ascii_case(category.as_str()));
            }
            if let Some(severity) = args.severity {
                store.errors.retain(|error| error.severity.eq_ignore_ascii_case(severity.as_str()));
            }
            if let Some(task_id) = args.task_id {
                store.errors.retain(|error| error.task_id.as_deref() == Some(task_id.as_str()));
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
                .ok_or_else(|| not_found_error(format!("error not found: {}", args.id)))?;
            print_value(error, json)
        }
        ErrorsCommand::Stats => {
            let store = sync_errors_from_daemon_events(project_root)?;
            let mut by_category: HashMap<String, usize> = HashMap::new();
            let mut by_severity: HashMap<String, usize> = HashMap::new();
            let recovered = store.errors.iter().filter(|error| error.recovered).count();
            let recoverable = store.errors.iter().filter(|error| error.recoverable).count();
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
                .ok_or_else(|| not_found_error(format!("error not found: {}", args.id)))?;
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
                    .map(|value: chrono::DateTime<chrono::FixedOffset>| value.with_timezone(&chrono::Utc) >= cutoff)
                    .unwrap_or(true)
            });
            save_errors(project_root, &store)?;
            let removed = before_len.saturating_sub(store.errors.len());
            print_value(serde_json::json!({ "removed": removed }), json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use protocol::test_utils::EnvVarGuard;

    fn daemon_event(
        event_id: &str,
        event_type: &str,
        project_root: Option<String>,
        data: Value,
    ) -> crate::services::runtime::DaemonEventRecord {
        crate::services::runtime::DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: event_id.to_string(),
            seq: 1,
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            project_root,
            data,
        }
    }

    #[test]
    fn sync_errors_ingests_notification_lifecycle_events() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());

        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project_root = TempDir::new().expect("project temp dir");
        let other_project_root = TempDir::new().expect("other project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();
        let other_root_str = other_project_root.path().to_string_lossy().to_string();

        let events = vec![
            daemon_event(
                "evt-log-1",
                "log",
                Some(project_root_str.clone()),
                serde_json::json!({
                    "level": "error",
                    "message": "runner connection unavailable",
                }),
            ),
            daemon_event(
                "evt-notify-1",
                "notification-delivery-failed",
                Some(project_root_str.clone()),
                serde_json::json!({
                    "connector_id": "ops-webhook",
                    "delivery_id": "delivery-1",
                    "last_error": "HTTP 503",
                    "retriable": true,
                    "task_id": "TASK-1",
                    "workflow_id": "WF-1",
                }),
            ),
            daemon_event(
                "evt-notify-2",
                "notification-delivery-dead-lettered",
                Some(project_root_str.clone()),
                serde_json::json!({
                    "connector_id": "ops-webhook",
                    "delivery_id": "delivery-2",
                    "last_error": "HTTP 401",
                }),
            ),
            daemon_event(
                "evt-notify-3",
                "notification-delivery-failed",
                Some(other_root_str),
                serde_json::json!({
                    "connector_id": "other",
                    "delivery_id": "delivery-x",
                    "last_error": "HTTP 500",
                    "retriable": true,
                }),
            ),
            daemon_event(
                "evt-notify-1",
                "notification-delivery-failed",
                Some(project_root_str.clone()),
                serde_json::json!({
                    "connector_id": "ops-webhook",
                    "delivery_id": "delivery-1",
                    "last_error": "HTTP 503",
                    "retriable": true,
                    "task_id": "TASK-1",
                    "workflow_id": "WF-1",
                }),
            ),
        ];

        let events_path = crate::services::runtime::daemon_events_log_path();
        if let Some(parent) = events_path.parent() {
            std::fs::create_dir_all(parent).expect("daemon event directory should be created");
        }
        let content = format!(
            "{}\n",
            events
                .into_iter()
                .map(|record| serde_json::to_string(&record).expect("event should serialize"))
                .collect::<Vec<String>>()
                .join("\n")
        );
        std::fs::write(&events_path, content).expect("daemon events log should be written");

        let store = sync_errors_from_daemon_events(project_root_str.as_str()).expect("error sync should succeed");
        assert_eq!(store.errors.len(), 3);

        let notification_errors: Vec<&ErrorRecord> =
            store.errors.iter().filter(|error| error.category == "notification").collect();
        assert_eq!(notification_errors.len(), 2);

        let failed_error = notification_errors
            .iter()
            .copied()
            .find(|error| error.source_event_id.as_deref() == Some("evt-notify-1"))
            .expect("failed notification error should exist");
        assert_eq!(failed_error.severity, "error");
        assert!(failed_error.recoverable);
        assert_eq!(failed_error.task_id.as_deref(), Some("TASK-1"));
        assert_eq!(failed_error.workflow_id.as_deref(), Some("WF-1"));
        assert!(failed_error.message.contains("ops-webhook"));
        assert!(failed_error.message.contains("HTTP 503"));

        let dead_letter_error = notification_errors
            .iter()
            .copied()
            .find(|error| error.source_event_id.as_deref() == Some("evt-notify-2"))
            .expect("dead-letter notification error should exist");
        assert_eq!(dead_letter_error.severity, "critical");
        assert!(!dead_letter_error.recoverable);
        assert!(dead_letter_error.message.contains("dead-lettered"));

        let store_second_sync =
            sync_errors_from_daemon_events(project_root_str.as_str()).expect("second sync should also succeed");
        assert_eq!(store_second_sync.errors.len(), 3);
    }
}
