use std::path::{Path, PathBuf};
use std::{fs::OpenOptions, io::Write};

use anyhow::Result;
use chrono::Utc;
use protocol::DaemonEventRecord;
use serde_json::Value;
use uuid::Uuid;

use crate::DaemonEventsPollResponse;

pub struct DaemonEventLog;

impl DaemonEventLog {
    pub fn log_path() -> PathBuf {
        protocol::daemon_events_log_path()
    }

    pub fn read_records(
        limit: Option<usize>,
        project_root_filter: Option<&str>,
    ) -> Result<Vec<DaemonEventRecord>> {
        let path = Self::log_path();
        let canonical_project_root_filter = normalize_project_root_filter(project_root_filter);
        let events = read_all_nonempty_lines(&path)?
            .into_iter()
            .filter_map(|line| serde_json::from_str::<DaemonEventRecord>(&line).ok())
            .filter(|record| {
                matches_project_root_filter(record, canonical_project_root_filter.as_deref())
            })
            .collect();
        Ok(apply_event_limit(events, limit))
    }

    pub fn poll(
        limit: Option<usize>,
        project_root_filter: Option<&str>,
    ) -> Result<DaemonEventsPollResponse> {
        let path = Self::log_path();
        let events = Self::read_records(limit, project_root_filter)?;
        Ok(DaemonEventsPollResponse {
            schema: "ao.daemon.events.poll.v1".to_string(),
            events_path: path.to_string_lossy().to_string(),
            count: events.len(),
            events,
        })
    }

    pub fn next_event(
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

    pub fn append(record: &DaemonEventRecord) -> Result<()> {
        let path = Self::log_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        rotate_if_needed(&path);
        append_line(&path, &serde_json::to_string(record)?)
    }

    pub fn append_fire_and_forget(event_type: &str, project_root: Option<String>, data: Value) {
        let record = DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: Uuid::new_v4().to_string(),
            seq: 0,
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            project_root,
            data,
        };
        let _ = Self::append(&record);
    }
}

const MAX_LOG_SIZE_BYTES: u64 = 5 * 1024 * 1024; // 5 MB

fn rotate_if_needed(path: &Path) {
    let size = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return,
    };
    if size >= MAX_LOG_SIZE_BYTES {
        let rotated = path.with_extension("jsonl.1");
        let _ = std::fs::rename(path, rotated);
    }
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
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

fn normalize_project_root_filter(filter: Option<&str>) -> Option<String> {
    filter
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(canonicalize_lossy)
}

fn matches_project_root_filter(record: &DaemonEventRecord, canonical_filter: Option<&str>) -> bool {
    let Some(filter) = canonical_filter else {
        return true;
    };
    let Some(record_project_root) = record.project_root.as_deref() else {
        return false;
    };
    canonicalize_lossy(record_project_root) == filter
}

fn apply_event_limit(
    mut events: Vec<DaemonEventRecord>,
    limit: Option<usize>,
) -> Vec<DaemonEventRecord> {
    if let Some(limit) = limit {
        if limit == 0 {
            return Vec::new();
        }
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
    }
    events
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate
        .canonicalize()
        .unwrap_or(candidate)
        .to_string_lossy()
        .to_string()
}
