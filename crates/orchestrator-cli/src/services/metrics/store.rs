//! Pending event persistence under `~/.animus/metrics/`.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use protocol::Config;
use serde::{Deserialize, Serialize};

use super::events::{Event, EventName};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEvent {
    pub name: EventName,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tags: std::collections::BTreeMap<String, String>,
    pub recorded_at: DateTime<Utc>,
}

impl PendingEvent {
    pub fn from_event(event: &Event) -> Self {
        let mut tags = std::collections::BTreeMap::new();
        if let Some((key, value)) = event.tag() {
            tags.insert(key.to_string(), value.to_string());
        }
        Self { name: event.name(), tags, recorded_at: Utc::now() }
    }
}

pub fn metrics_dir() -> PathBuf {
    Config::global_config_dir().join("metrics")
}

pub fn pending_path() -> PathBuf {
    metrics_dir().join("pending.jsonl")
}

pub fn last_send_path() -> PathBuf {
    metrics_dir().join("last-send.json")
}

pub fn append(event: &PendingEvent) -> Result<()> {
    let dir = metrics_dir();
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create metrics dir {}", dir.display()))?;
    let path = pending_path();
    let mut file =
        OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("open {}", path.display()))?;
    let line = serde_json::to_string(event)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn read_all() -> Result<Vec<PendingEvent>> {
    let path = pending_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<PendingEvent>(trimmed) {
            events.push(parsed);
        }
    }
    Ok(events)
}

pub fn clear() -> Result<()> {
    let path = pending_path();
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LastSendStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

pub fn read_last_send() -> LastSendStatus {
    let path = last_send_path();
    if !path.exists() {
        return LastSendStatus::default();
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str::<LastSendStatus>(&content).ok())
        .unwrap_or_default()
}

pub fn write_last_send(status: &LastSendStatus) -> Result<()> {
    let dir = metrics_dir();
    fs::create_dir_all(&dir)?;
    let path = last_send_path();
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(status)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}
