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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity { dev: metadata.dev(), ino: metadata.ino() })
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> Option<FileIdentity> {
    None
}

/// Follow-mode read position for a single log file: the byte offset of the
/// next unread line plus the identity (dev+ino) of the file that offset
/// refers to, so rotation (rename to `.jsonl.1` + fresh file at the same
/// path) is detected instead of being misread as in-place truncation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FollowCursor {
    offset: u64,
    identity: Option<FileIdentity>,
}

impl FollowCursor {
    pub(super) fn at_end_of(path: &Path, offset: u64) -> Self {
        Self { offset, identity: std::fs::metadata(path).ok().as_ref().and_then(file_identity) }
    }
}

fn rotated_log_path(path: &Path) -> PathBuf {
    let extension = path.extension().map(|value| value.to_string_lossy().to_string()).unwrap_or_default();
    path.with_extension(format!("{extension}.1"))
}

fn nonempty_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn read_all_nonempty_lines(path: &Path, follow_cursor: Option<&mut FollowCursor>) -> Result<Vec<String>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;

    use std::io::Read;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    // In follow mode, only consume up to the last complete
    // newline-terminated line and derive the offset from the bytes
    // actually consumed; a partial tail is re-read once the writer
    // finishes the line.
    let consumed_end = match follow_cursor {
        Some(cursor) => {
            let end = content.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
            *cursor = FollowCursor { offset: end as u64, identity: file_identity(&metadata) };
            end
        }
        None => content.len(),
    };
    Ok(content[..consumed_end].lines().map(str::trim).filter(|line| !line.is_empty()).map(ToOwned::to_owned).collect())
}

/// Drain the rotated sibling (`<name>.jsonl.1`) from the cursor's saved
/// offset. The rotated file is final — no writer will complete a partial
/// tail — so everything through EOF is consumed, including an unterminated
/// last line. When the rotated file is not the one the cursor was reading
/// (a second rotation slipped between polls), re-read it whole: duplicates
/// over loss.
fn drain_rotated_remainder(rotated: &Path, cursor: &FollowCursor) -> Result<Vec<String>> {
    let mut file = match std::fs::File::open(rotated) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    let same_file = match (cursor.identity, file_identity(&metadata)) {
        (Some(stored), Some(current)) => stored == current,
        _ => true,
    };
    let start = if same_file { cursor.offset.min(metadata.len()) } else { 0 };

    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    Ok(nonempty_lines(&buffer))
}

/// Read the complete lines appended since the cursor's last position,
/// surviving log rotation: when the file at `path` is no longer the one the
/// cursor was reading (or vanished mid-rotation), the remainder of the
/// rotated `.jsonl.1` sibling is drained first, then reading continues from
/// the start of the fresh file. Ambiguous corners prefer duplicates over
/// loss.
pub(super) fn read_new_complete_lines(path: &Path, cursor: &mut FollowCursor) -> Result<Vec<String>> {
    let rotated = rotated_log_path(path);
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The live file vanished. If we had consumed bytes the writer
            // likely rotated and has not recreated the file yet: drain the
            // rotated remainder so those records are not lost.
            if cursor.offset > 0 || cursor.identity.is_some() {
                let lines = drain_rotated_remainder(&rotated, cursor)?;
                *cursor = FollowCursor::default();
                return Ok(lines);
            }
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };

    let metadata = file.metadata()?;
    let identity = file_identity(&metadata);
    let len = metadata.len();
    let rotation_detected = match (cursor.identity, identity) {
        (Some(stored), Some(current)) => stored != current,
        _ => len < cursor.offset,
    };

    let mut lines = Vec::new();
    if rotation_detected {
        lines.extend(drain_rotated_remainder(&rotated, cursor)?);
        cursor.offset = 0;
    } else if cursor.offset > len {
        // Same file truncated in place: re-read from the start
        // (duplicates over loss).
        cursor.offset = 0;
    }
    cursor.identity = identity;

    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    if let Some(newline_idx) = buffer.iter().rposition(|&b| b == b'\n') {
        let consumed = &buffer[..=newline_idx];
        cursor.offset += consumed.len() as u64;
        lines.extend(nonempty_lines(consumed));
    }
    Ok(lines)
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
            schema: "animus.daemon.event.v1".to_string(),
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

    fn append_lines(path: &Path, content: &str) {
        use std::io::Write;
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(path).expect("log should open for append");
        file.write_all(content.as_bytes()).expect("log should append");
    }

    #[test]
    fn read_new_complete_lines_tails_appends_and_defers_partial_tail() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("daemon-events.jsonl");
        std::fs::write(&path, "a\nb\n").expect("log should be written");

        let mut cursor = FollowCursor::default();
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["a", "b"]);

        append_lines(&path, "c\npartial");
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["c"]);

        append_lines(&path, "-done\n");
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["partial-done"]);
        assert!(read_new_complete_lines(&path, &mut cursor).expect("read").is_empty());
    }

    #[test]
    fn read_new_complete_lines_drains_rotated_tail_before_fresh_file() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("daemon-events.jsonl");
        std::fs::write(&path, "a\nb\n").expect("log should be written");

        let mut cursor = FollowCursor::default();
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["a", "b"]);

        // Records appended between the last poll and rotation must not be
        // lost when the writer rotates to `.jsonl.1` and starts fresh.
        append_lines(&path, "c\n");
        std::fs::rename(&path, temp.path().join("daemon-events.jsonl.1")).expect("rotation rename");
        std::fs::write(&path, "d\ne\n").expect("fresh log should be written");

        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["c", "d", "e"]);

        append_lines(&path, "f\n");
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["f"]);
    }

    #[test]
    fn read_new_complete_lines_drains_rotated_tail_when_fresh_file_not_yet_created() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("daemon-events.jsonl");
        std::fs::write(&path, "a\n").expect("log should be written");

        let mut cursor = FollowCursor::default();
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["a"]);

        append_lines(&path, "b\n");
        std::fs::rename(&path, temp.path().join("daemon-events.jsonl.1")).expect("rotation rename");
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["b"]);

        std::fs::write(&path, "c\n").expect("fresh log should be written");
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["c"]);
    }

    #[test]
    fn read_new_complete_lines_detects_rotation_when_fresh_file_outgrows_old_offset() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("daemon-events.jsonl");
        std::fs::write(&path, "aa\n").expect("log should be written");

        let mut cursor = FollowCursor::default();
        assert_eq!(read_new_complete_lines(&path, &mut cursor).expect("read"), vec!["aa"]);

        // Rotate, then make the fresh file longer than the saved offset.
        // A length-only check would resume mid-file in the fresh log.
        append_lines(&path, "bb\n");
        std::fs::rename(&path, temp.path().join("daemon-events.jsonl.1")).expect("rotation rename");
        std::fs::write(&path, "long-line-one\nlong-line-two\n").expect("fresh log should be written");

        assert_eq!(
            read_new_complete_lines(&path, &mut cursor).expect("read"),
            vec!["bb", "long-line-one", "long-line-two"]
        );
    }

    #[test]
    fn read_daemon_event_records_returns_ordered_tail_and_skips_invalid_lines() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
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
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
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
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let root = TempDir::new().expect("project");
        let root_path = canonicalize_lossy(root.path().to_string_lossy().as_ref());
        let path = daemon_events_log_path();
        write_events_log(
            &path,
            &[serde_json::to_string(&sample_event(7, "queue", Some(root_path.as_str()))).expect("event json")],
        );

        let response = poll_daemon_events(Some(10), Some(root_path.as_str())).expect("poll should succeed");
        assert_eq!(response.schema, "animus.daemon.events.poll.v1");
        assert_eq!(response.count, 1);
        assert_eq!(response.events.len(), 1);
        assert!(response.events_path.ends_with("daemon-events.jsonl"));
    }
}

pub(super) async fn handle_daemon_events_impl(args: DaemonEventsArgs, json: bool) -> Result<()> {
    let path = daemon_events_log_path();
    if !path.exists() {
        if !args.follow {
            print_value(
                serde_json::json!({
                    "schema": "animus.daemon.events.v1",
                    "events_path": path,
                    "events": [],
                }),
                json,
            )?;
            return Ok(());
        }
        // Follow mode: the daemon may not have emitted anything yet. Poll
        // for the file to appear instead of exiting immediately.
        while !path.exists() {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(()),
                _ = sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    let mut cursor = FollowCursor::default();
    let mut first_iteration = true;

    loop {
        let lines = if first_iteration {
            let follow_cursor = args.follow.then_some(&mut cursor);
            let mut lines = read_all_nonempty_lines(&path, follow_cursor)?;
            if let Some(limit) = args.limit {
                if lines.len() > limit {
                    lines = lines.split_off(lines.len() - limit);
                }
            }
            lines
        } else {
            read_new_complete_lines(&path, &mut cursor)?
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
