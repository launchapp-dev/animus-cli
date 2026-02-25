use crate::cli_types::DaemonEventsArgs;
use crate::print_value;
use crate::shared::append_line;
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DaemonEventRecord {
    pub(crate) schema: String,
    pub(crate) id: String,
    pub(crate) seq: u64,
    pub(crate) timestamp: String,
    pub(crate) event_type: String,
    pub(crate) project_root: Option<String>,
    pub(crate) data: Value,
}

pub(crate) fn daemon_events_log_path() -> PathBuf {
    protocol::Config::global_config_dir().join("daemon-events.jsonl")
}

fn event_matches_filter(value: Option<&str>, expected: Option<&str>) -> bool {
    match expected {
        Some(expected) if !expected.is_empty() => value
            .map(|candidate| candidate.eq_ignore_ascii_case(expected))
            .unwrap_or(false),
        _ => true,
    }
}

fn event_record_matches_args(record: &DaemonEventRecord, args: &DaemonEventsArgs) -> bool {
    let event_type = args
        .event_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let workflow_id = args
        .workflow_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let task_id = args
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let phase_id = args
        .phase
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    event_matches_filter(Some(record.event_type.as_str()), event_type)
        && event_matches_filter(
            record.data.get("workflow_id").and_then(Value::as_str),
            workflow_id,
        )
        && event_matches_filter(record.data.get("task_id").and_then(Value::as_str), task_id)
        && event_matches_filter(
            record.data.get("phase_id").and_then(Value::as_str),
            phase_id,
        )
}

fn read_all_nonempty_lines(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn read_nonempty_lines_since(path: &Path, offset: &mut u64) -> Result<Vec<String>> {
    if !path.exists() {
        *offset = 0;
        return Ok(Vec::new());
    }

    let mut file = std::fs::OpenOptions::new().read(true).open(path)?;
    let len = file.metadata()?.len();
    if *offset > len {
        *offset = 0;
    }

    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(*offset))?;
    let mut buffer = String::new();
    file.read_to_string(&mut buffer)?;
    *offset = len;

    Ok(buffer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub(super) fn next_daemon_event(
    seq: &mut u64,
    event_type: &str,
    project_root: Option<String>,
    data: Value,
) -> DaemonEventRecord {
    *seq = seq.saturating_add(1);
    DaemonEventRecord {
        schema: "ao.daemon.event.v1".to_string(),
        id: Uuid::new_v4().to_string(),
        seq: *seq,
        timestamp: Utc::now().to_rfc3339(),
        event_type: event_type.to_string(),
        project_root,
        data,
    }
}

fn append_daemon_event(record: &DaemonEventRecord) -> Result<()> {
    let path = daemon_events_log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    append_line(&path, &serde_json::to_string(record)?)
}

pub(super) fn emit_daemon_event(record: &DaemonEventRecord, json: bool) -> Result<()> {
    append_daemon_event(record)?;
    if json {
        println!("{}", serde_json::to_string(record)?);
    } else {
        let project = record
            .project_root
            .as_deref()
            .map(|value| format!(" [{value}]"))
            .unwrap_or_default();
        println!("{}{} {}", record.event_type, project, record.timestamp);
    }
    Ok(())
}

pub(super) async fn handle_daemon_events_impl(args: DaemonEventsArgs, json: bool) -> Result<()> {
    let path = daemon_events_log_path();
    if !path.exists() {
        print_value(
            serde_json::json!({
                "schema": "ao.daemon.events.v1",
                "events_path": path,
                "events": [],
            }),
            json,
        )?;
        return Ok(());
    }

    let mut offset = 0u64;
    let mut first_iteration = true;

    loop {
        let lines = if first_iteration {
            let mut lines = read_all_nonempty_lines(&path)?;
            if let Some(limit) = args.limit {
                if lines.len() > limit {
                    lines = lines.split_off(lines.len() - limit);
                }
            }
            offset = std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            lines
        } else {
            read_nonempty_lines_since(&path, &mut offset)?
        };

        for line in &lines {
            let parsed = serde_json::from_str::<DaemonEventRecord>(line).ok();
            if let Some(record) = parsed.as_ref() {
                if !event_record_matches_args(record, &args) {
                    continue;
                }
            } else if args.event_type.is_some()
                || args.workflow_id.is_some()
                || args.task_id.is_some()
                || args.phase.is_some()
            {
                continue;
            }

            if json {
                println!("{line}");
            } else if let Some(record) = parsed {
                let project = record
                    .project_root
                    .as_deref()
                    .map(|value| format!(" [{value}]"))
                    .unwrap_or_default();
                println!("{}{} {}", record.event_type, project, record.timestamp);
            } else {
                println!("{line}");
            }
        }

        first_iteration = false;
        if !args.follow {
            break;
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = sleep(Duration::from_millis(500)) => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(event_type: &str) -> DaemonEventRecord {
        DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: "evt-1".to_string(),
            seq: 1,
            timestamp: "2026-02-25T00:00:00Z".to_string(),
            event_type: event_type.to_string(),
            project_root: Some("/tmp/project".to_string()),
            data: serde_json::json!({
                "workflow_id": "wf-1",
                "task_id": "TASK-023",
                "phase_id": "implementation"
            }),
        }
    }

    #[test]
    fn daemon_event_filters_match_when_unset() {
        let args = DaemonEventsArgs {
            limit: None,
            follow: false,
            event_type: None,
            workflow_id: None,
            task_id: None,
            phase: None,
        };
        assert!(event_record_matches_args(
            &record("workflow-phase-model-failover"),
            &args
        ));
    }

    #[test]
    fn daemon_event_filters_match_all_requested_fields() {
        let args = DaemonEventsArgs {
            limit: None,
            follow: false,
            event_type: Some("workflow-phase-model-failover".to_string()),
            workflow_id: Some("wf-1".to_string()),
            task_id: Some("TASK-023".to_string()),
            phase: Some("implementation".to_string()),
        };
        assert!(event_record_matches_args(
            &record("workflow-phase-model-failover"),
            &args
        ));
    }

    #[test]
    fn daemon_event_filters_exclude_non_matching_phase() {
        let args = DaemonEventsArgs {
            limit: None,
            follow: false,
            event_type: Some("workflow-phase-model-failover".to_string()),
            workflow_id: Some("wf-1".to_string()),
            task_id: Some("TASK-023".to_string()),
            phase: Some("testing".to_string()),
        };
        assert!(!event_record_matches_args(
            &record("workflow-phase-model-failover"),
            &args
        ));
    }
}
