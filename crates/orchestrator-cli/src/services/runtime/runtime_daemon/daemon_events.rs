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
            if json {
                println!("{line}");
            } else if let Ok(record) = serde_json::from_str::<DaemonEventRecord>(line) {
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
