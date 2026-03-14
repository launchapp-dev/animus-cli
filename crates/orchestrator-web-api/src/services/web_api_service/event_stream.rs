use std::path::PathBuf;

use anyhow::Context;
use chrono::Utc;
use orchestrator_web_contracts::DaemonEventRecord;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::models::WebApiError;

use super::EVENT_SCHEMA;

pub(super) fn daemon_events_log_path() -> PathBuf {
    protocol::Config::global_config_dir().join("daemon-events.jsonl")
}

pub(super) fn read_max_seq_for_project(project_root: &str) -> Result<u64, WebApiError> {
    let records = read_events_for_project(project_root)?;
    Ok(records.iter().map(|record| record.seq).max().unwrap_or(0))
}

pub(super) fn read_events_for_project(
    project_root: &str,
) -> Result<Vec<DaemonEventRecord>, WebApiError> {
    let path = daemon_events_log_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read daemon events: {}", path.display()))?;

    let mut parsed_records = Vec::new();

    for (line_number, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let fallback_seq = (line_number as u64).saturating_add(1);

        let mut record = match serde_json::from_str::<DaemonEventRecord>(trimmed) {
            Ok(record) => record,
            Err(_) => match serde_json::from_str::<Value>(trimmed) {
                Ok(raw) => value_to_event_record(raw, fallback_seq),
                Err(_) => continue,
            },
        };

        if record.seq == 0 {
            record.seq = fallback_seq;
        }

        if record.schema.trim().is_empty() {
            record.schema = EVENT_SCHEMA.to_string();
        }

        if record.id.trim().is_empty() {
            record.id = Uuid::new_v4().to_string();
        }

        if record.timestamp.trim().is_empty() {
            record.timestamp = Utc::now().to_rfc3339();
        }

        if record.event_type.trim().is_empty() {
            record.event_type = "unknown".to_string();
        }

        if record
            .project_root
            .as_ref()
            .map(|root| root == project_root)
            .unwrap_or(true)
        {
            parsed_records.push(record);
        }
    }

    parsed_records.sort_by_key(|record| record.seq);
    Ok(parsed_records)
}

pub(super) fn value_to_event_record(value: Value, fallback_seq: u64) -> DaemonEventRecord {
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or(EVENT_SCHEMA)
        .to_string();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let seq = value
        .get("seq")
        .and_then(Value::as_u64)
        .unwrap_or(fallback_seq);
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let event_type = value
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let project_root = value
        .get("project_root")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let data = value.get("data").cloned().unwrap_or_else(|| json!({}));

    DaemonEventRecord {
        schema,
        id,
        seq,
        timestamp,
        event_type,
        project_root,
        data,
    }
}
