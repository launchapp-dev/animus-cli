#[cfg(test)]
use super::canonicalize_lossy;
use crate::cli_types::DaemonEventsArgs;
use crate::print_value;
use anyhow::Result;
use orchestrator_daemon_runtime::{DaemonEventLog, DaemonEventsPollResponse};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;

pub(crate) use protocol::DaemonEventRecord;

pub(crate) fn daemon_events_log_path() -> PathBuf {
    DaemonEventLog::log_path()
}

fn read_all_nonempty_lines(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().map(str::trim).filter(|line| !line.is_empty()).map(ToOwned::to_owned).collect())
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

    Ok(buffer.lines().map(str::trim).filter(|line| !line.is_empty()).map(ToOwned::to_owned).collect())
}

#[cfg(test)]
pub(crate) fn read_daemon_event_records(
    limit: Option<usize>,
    project_root_filter: Option<&str>,
) -> Result<Vec<DaemonEventRecord>> {
    DaemonEventLog::read_records(limit, project_root_filter)
}

pub(crate) fn poll_daemon_events(
    limit: Option<usize>,
    project_root_filter: Option<&str>,
) -> Result<DaemonEventsPollResponse> {
    DaemonEventLog::poll(limit, project_root_filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use protocol::test_utils::EnvVarGuard;

    fn sample_event(seq: u64, event_type: &str, project_root: Option<&str>) -> DaemonEventRecord {
        DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: format!("event-{seq}"),
            seq,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            event_type: event_type.to_string(),
            project_root: project_root.map(ToOwned::to_owned),
            data: serde_json::json!({ "seq": seq }),
        }
    }

    fn write_events_log(path: &Path, lines: &[String]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("daemon events parent should be created");
        }
        let content = lines.iter().map(|line| format!("{line}\n")).collect::<String>();
        std::fs::write(path, content).expect("daemon events log should be written");
    }

    #[test]
    fn read_daemon_event_records_returns_ordered_tail_and_skips_invalid_lines() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let root_a = TempDir::new().expect("project A");
        let root_b = TempDir::new().expect("project B");
        let root_a_path = canonicalize_lossy(root_a.path().to_string_lossy().as_ref());
        let root_b_path = canonicalize_lossy(root_b.path().to_string_lossy().as_ref());

        let path = daemon_events_log_path();
        write_events_log(
            &path,
            &[
                serde_json::to_string(&sample_event(1, "queue", Some(root_a_path.as_str()))).expect("event json"),
                "{not-json".to_string(),
                serde_json::to_string(&sample_event(2, "workflow", Some(root_b_path.as_str()))).expect("event json"),
                serde_json::to_string(&sample_event(3, "log", Some(root_a_path.as_str()))).expect("event json"),
            ],
        );

        let events = read_daemon_event_records(Some(2), None).expect("records should be readable");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 2);
        assert_eq!(events[1].seq, 3);
        assert_eq!(events[0].event_type, "workflow");
        assert_eq!(events[1].event_type, "log");
    }

    #[test]
    fn read_daemon_event_records_filters_by_project_root() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let root_a = TempDir::new().expect("project A");
        let root_b = TempDir::new().expect("project B");
        let root_a_path = canonicalize_lossy(root_a.path().to_string_lossy().as_ref());
        let root_b_path = canonicalize_lossy(root_b.path().to_string_lossy().as_ref());

        let path = daemon_events_log_path();
        write_events_log(
            &path,
            &[
                serde_json::to_string(&sample_event(1, "queue", Some(root_a_path.as_str()))).expect("event json"),
                serde_json::to_string(&sample_event(2, "queue", Some(root_b_path.as_str()))).expect("event json"),
                serde_json::to_string(&sample_event(3, "workflow", Some(root_a_path.as_str()))).expect("event json"),
            ],
        );

        let events =
            read_daemon_event_records(Some(10), Some(root_a_path.as_str())).expect("records should be readable");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.project_root.as_deref() == Some(root_a_path.as_str())));
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 3);

        let padded_filter = format!("  {root_a_path}  ");
        let padded =
            read_daemon_event_records(Some(10), Some(padded_filter.as_str())).expect("records should be readable");
        assert_eq!(padded.len(), 2);
        assert!(padded.iter().all(|event| event.project_root.as_deref() == Some(root_a_path.as_str())));

        let empty = read_daemon_event_records(Some(10), Some("/does/not/exist")).expect("records should be readable");
        assert!(empty.is_empty());
    }

    #[test]
    fn poll_daemon_events_returns_metadata_and_count() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let root = TempDir::new().expect("project");
        let root_path = canonicalize_lossy(root.path().to_string_lossy().as_ref());
        let path = daemon_events_log_path();
        write_events_log(
            &path,
            &[serde_json::to_string(&sample_event(7, "queue", Some(root_path.as_str()))).expect("event json")],
        );

        let response = poll_daemon_events(Some(10), Some(root_path.as_str())).expect("poll should succeed");
        assert_eq!(response.schema, "ao.daemon.events.poll.v1");
        assert_eq!(response.count, 1);
        assert_eq!(response.events.len(), 1);
        assert!(response.events_path.ends_with("daemon-events.jsonl"));
    }
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
            offset = std::fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
            lines
        } else {
            read_nonempty_lines_since(&path, &mut offset)?
        };

        for line in &lines {
            if json {
                println!("{line}");
            } else if let Ok(record) = serde_json::from_str::<DaemonEventRecord>(line) {
                let project = record.project_root.as_deref().map(|value| format!(" [{value}]")).unwrap_or_default();
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
