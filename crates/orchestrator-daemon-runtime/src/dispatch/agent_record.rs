//! v0.5 P2 Wave C: agent-reattach scaffolding. Full reattach is deferred
//! to v0.6 — today the daemon does NOT attempt to reattach to or interfere
//! with detected orphan agents; operators decide whether to kill them.
//! This module writes a per-spawn record so a future startup-scan can use it,
//! and runs a detect-and-log-only orphan scan on daemon startup. The scan
//! never tries to reattach to or kill a live orphan — it only records the
//! existence of one for operator visibility and deletes stale records whose
//! PID is gone.

use std::path::{Path, PathBuf};

use protocol::SubjectDispatch;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpawnRecord {
    pub agent_session_id: String,
    pub pid: u32,
    pub started_at: String,
    pub subject_id: String,
    pub subject_kind: String,
    pub workflow_ref: String,
    pub task_id: Option<String>,
    pub command_line: Vec<String>,
    pub stdio_socket_path: Option<String>,
    /// v0.5.1 fold-in (item 2): path to the per-agent `decisions.jsonl`
    /// recording. The orphan-scan + reattach path uses this to replay
    /// events that landed during the daemon-restart gap. Optional for
    /// backward-compat with pre-fold-in records — readers should fall back
    /// to the canonical `~/.animus/<scope>/runs/<run_id>/decisions.jsonl`
    /// derivation when this is `None` and `run_id` is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions_jsonl_path: Option<String>,
    /// v0.5.1 fold-in (item 2): byte offset into `decisions_jsonl_path`
    /// at which the daemon last consumed an event. `replay_gap_from_spawn_record`
    /// resumes from here so a daemon restart never double-emits an event
    /// it already broadcast. Defaults to 0 when missing (pre-fold-in
    /// records replay from the start, which is the safer fallback).
    ///
    /// v0.6 gap: currently the offset is only updated during the initial
    /// reattach replay. Events delivered through the live reattach socket
    /// AFTER reattach do NOT advance the persisted offset, so a second
    /// crash + restart will replay (and re-broadcast) those events from
    /// `decisions.jsonl`. Acceptable today because the lifted events are
    /// non-terminal (`agent_finished` / `agent_error`) — subscribers may
    /// see a duplicate but won't double-close any workflow state. The
    /// v0.6 fix is to plumb the live emitter through `update_consumed_offset`
    /// as events flow.
    #[serde(default)]
    pub last_consumed_offset: u64,
}

fn agents_dir(project_root: &std::path::Path) -> Option<PathBuf> {
    let scope = protocol::scoped_state_root(project_root)?;
    let dir = scope.join("runs").join("_pending").join("agents");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    Some(dir)
}

pub fn record_path(project_root: &std::path::Path, agent_session_id: &str) -> Option<PathBuf> {
    let dir = agents_dir(project_root)?;
    Some(dir.join(format!("{agent_session_id}.json")))
}

pub fn write_record(project_root: &std::path::Path, record: &AgentSpawnRecord) -> std::io::Result<()> {
    let Some(path) = record_path(project_root, &record.agent_session_id) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "scoped runtime root unavailable"));
    };
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    if let Ok(file) = std::fs::File::open(&tmp) {
        let _ = file.sync_all();
    }
    std::fs::rename(&tmp, &path)
}

pub fn delete_record(project_root: &std::path::Path, agent_session_id: &str) {
    if let Some(path) = record_path(project_root, agent_session_id) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn build_record_with_decisions(
    agent_session_id: String,
    pid: u32,
    dispatch: &SubjectDispatch,
    command_line: Vec<String>,
    stdio_socket_path: Option<String>,
    decisions_jsonl_path: Option<String>,
) -> AgentSpawnRecord {
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    AgentSpawnRecord {
        agent_session_id,
        pid,
        started_at,
        subject_id: dispatch.subject_id().unwrap_or_default().to_string(),
        subject_kind: dispatch.subject_kind().unwrap_or_default().to_string(),
        workflow_ref: dispatch.workflow_ref.clone(),
        task_id: dispatch.task_id().map(String::from),
        command_line,
        stdio_socket_path,
        decisions_jsonl_path,
        last_consumed_offset: 0,
    }
}

/// Update the `last_consumed_offset` field on the on-disk spawn record so a
/// subsequent daemon restart can resume gap replay where this daemon left
/// off. Best-effort: a failed read/write is logged and silently ignored;
/// the caller is expected to keep operating from in-memory state, and the
/// next sweep will simply replay an extra (already-broadcast) event in the
/// worst case.
pub fn update_consumed_offset(project_root: &Path, agent_session_id: &str, new_offset: u64) {
    let Some(path) = record_path(project_root, agent_session_id) else {
        return;
    };
    let raw = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!(
                target: "animus.runtime.agent_record",
                %error,
                agent_session_id,
                "skip update_consumed_offset; record not readable"
            );
            return;
        }
    };
    let mut record: AgentSpawnRecord = match serde_json::from_slice(&raw) {
        Ok(rec) => rec,
        Err(error) => {
            tracing::debug!(
                target: "animus.runtime.agent_record",
                %error,
                agent_session_id,
                "skip update_consumed_offset; record not parseable"
            );
            return;
        }
    };
    if record.last_consumed_offset >= new_offset {
        return;
    }
    record.last_consumed_offset = new_offset;
    let tmp = path.with_extension("json.tmp");
    let json = match serde_json::to_vec_pretty(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!(
                target: "animus.runtime.agent_record",
                %error,
                agent_session_id,
                "skip update_consumed_offset; record not serializable"
            );
            return;
        }
    };
    if std::fs::write(&tmp, json).is_err() {
        return;
    }
    if let Ok(file) = std::fs::File::open(&tmp) {
        let _ = file.sync_all();
    }
    let _ = std::fs::rename(&tmp, &path);
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectedOrphan {
    pub agent_session_id: String,
    pub pid: u32,
    pub subject_id: String,
    pub subject_kind: String,
    pub workflow_ref: String,
    pub task_id: Option<String>,
    pub command_line: Vec<String>,
    pub started_at: String,
    pub record_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanedOrphan {
    pub agent_session_id: String,
    pub pid: u32,
    pub record_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnparseableRecord {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OrphanReport {
    pub detected: Vec<DetectedOrphan>,
    pub cleaned: Vec<CleanedOrphan>,
    pub unparseable: Vec<UnparseableRecord>,
}

impl OrphanReport {
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.detected.is_empty() && self.cleaned.is_empty() && self.unparseable.is_empty()
    }
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        protocol::is_process_alive(pid)
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
fn process_command_matches(pid: u32, recorded: &[String]) -> bool {
    let Some(expected_binary) = recorded.first() else {
        return true;
    };
    let output = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]).output();
    let actual = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return false,
    };
    if actual.is_empty() {
        return false;
    }
    let actual_first = actual.split_whitespace().next().unwrap_or("");
    let expected_first_basename =
        std::path::Path::new(expected_binary).file_name().and_then(|s| s.to_str()).unwrap_or(expected_binary.as_str());
    let actual_first_basename =
        std::path::Path::new(actual_first).file_name().and_then(|s| s.to_str()).unwrap_or(actual_first);
    actual_first == expected_binary || actual_first_basename == expected_first_basename
}

pub fn scan_orphans(records_dir: &Path) -> std::io::Result<OrphanReport> {
    let mut report = OrphanReport::default();

    if !records_dir.exists() {
        return Ok(report);
    }

    #[cfg(not(unix))]
    {
        let _ = records_dir;
        return Ok(report);
    }

    #[cfg(unix)]
    {
        let read_dir = match std::fs::read_dir(records_dir) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(err) => return Err(err),
        };

        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(err) => {
                    report.unparseable.push(UnparseableRecord { path: path.clone(), error: format!("open: {err}") });
                    continue;
                }
            };

            let record: AgentSpawnRecord = match serde_json::from_reader(std::io::BufReader::new(file)) {
                Ok(r) => r,
                Err(err) => {
                    report.unparseable.push(UnparseableRecord { path: path.clone(), error: format!("parse: {err}") });
                    continue;
                }
            };

            // Identity check: a live PID alone is not enough — after a
            // long daemon crash the kernel may have recycled the recorded
            // child PID onto an unrelated process. Treat that as
            // "agent is gone, record is stale" rather than reporting an
            // operator-facing orphan for a process Animus never owned.
            // Mirrors codex round-1 P2 #1 on this fold-in.
            let alive = is_pid_alive(record.pid) && process_command_matches(record.pid, &record.command_line);
            if alive {
                report.detected.push(DetectedOrphan {
                    agent_session_id: record.agent_session_id.clone(),
                    pid: record.pid,
                    subject_id: record.subject_id.clone(),
                    subject_kind: record.subject_kind.clone(),
                    workflow_ref: record.workflow_ref.clone(),
                    task_id: record.task_id.clone(),
                    command_line: record.command_line.clone(),
                    started_at: record.started_at.clone(),
                    record_path: path,
                });
            } else {
                let pid = record.pid;
                let agent_session_id = record.agent_session_id.clone();
                let _ = std::fs::remove_file(&path);
                report.cleaned.push(CleanedOrphan { agent_session_id, pid, record_path: path });
            }
        }

        Ok(report)
    }
}

pub fn scan_orphans_for_project(project_root: &Path) -> std::io::Result<OrphanReport> {
    let Some(scope) = protocol::scoped_state_root(project_root) else {
        return Ok(OrphanReport::default());
    };
    let dir = scope.join("runs").join("_pending").join("agents");
    scan_orphans(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::SubjectDispatchExt;

    // Pins PATH so concurrent tests that clobber PATH cannot break the
    // `ps`/`sleep`/`true` spawns these tests rely on. Takes the dispatch-wide
    // env lock first (the build_runner_command_from_dispatch tests mutate
    // PATH under that lock) and then the protocol env lock via EnvVarGuard,
    // so both PATH-mutating domains in this binary are excluded.
    #[cfg(unix)]
    fn pin_path_for_process_spawns() -> (std::sync::MutexGuard<'static, ()>, protocol::test_utils::EnvVarGuard) {
        let lock = crate::dispatch::test_env::lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (lock, protocol::test_utils::EnvVarGuard::set("PATH", Some("/bin:/usr/bin")))
    }

    fn write_record_into(dir: &std::path::Path, record: &AgentSpawnRecord) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", record.agent_session_id));
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    #[test]
    fn pre_foldin_record_deserializes_with_default_decisions_and_offset() {
        // Older records (written before v0.5.1 fold-in item 2) do not have
        // `decisions_jsonl_path` or `last_consumed_offset` fields. They MUST
        // still deserialize cleanly, with sensible defaults — otherwise an
        // existing on-disk record from a pre-fold-in daemon would crash the
        // post-fold-in orphan scan.
        let raw = serde_json::json!({
            "agent_session_id": "agent-old",
            "pid": 4242,
            "started_at": "2025-01-01T00:00:00Z",
            "subject_id": "TASK-OLD",
            "subject_kind": "task",
            "workflow_ref": "standard",
            "task_id": "TASK-OLD",
            "command_line": ["/bin/echo"],
            "stdio_socket_path": null,
        });
        let parsed: AgentSpawnRecord = serde_json::from_value(raw).expect("backward compat parse");
        assert!(parsed.decisions_jsonl_path.is_none());
        assert_eq!(parsed.last_consumed_offset, 0);
    }

    fn update_consumed_offset_at(path: &std::path::Path, new_offset: u64) {
        // Pure-IO variant of `update_consumed_offset` for unit tests that
        // want to avoid the `scoped_state_root` side-effect of writing
        // under `~/.animus/`. Mirrors the production helper's contract:
        // no-op on lowering, atomic write via rename.
        let raw = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut record: AgentSpawnRecord = match serde_json::from_slice(&raw) {
            Ok(r) => r,
            Err(_) => return,
        };
        if record.last_consumed_offset >= new_offset {
            return;
        }
        record.last_consumed_offset = new_offset;
        let tmp = path.with_extension("json.tmp");
        let json = match serde_json::to_vec_pretty(&record) {
            Ok(b) => b,
            Err(_) => return,
        };
        if std::fs::write(&tmp, json).is_err() {
            return;
        }
        let _ = std::fs::rename(&tmp, path);
    }

    #[test]
    fn update_consumed_offset_helper_writes_higher_offset_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let dispatch = protocol::SubjectDispatch::for_task("TASK-OFFSET", "standard");
        let record = build_record_with_decisions(
            "agent-offset".to_string(),
            99,
            &dispatch,
            vec!["/bin/echo".into()],
            None,
            None,
        );
        let path = write_record_into(temp.path(), &record).expect("write");
        update_consumed_offset_at(&path, 1024);
        let parsed: AgentSpawnRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed.last_consumed_offset, 1024);
    }

    #[test]
    fn update_consumed_offset_helper_never_lowers_a_higher_persisted_value() {
        let temp = tempfile::tempdir().unwrap();
        let dispatch = protocol::SubjectDispatch::for_task("TASK-OFFSET2", "standard");
        let mut record = build_record_with_decisions(
            "agent-offset2".to_string(),
            99,
            &dispatch,
            vec!["/bin/echo".into()],
            None,
            None,
        );
        record.last_consumed_offset = 4096;
        let path = write_record_into(temp.path(), &record).expect("write");
        update_consumed_offset_at(&path, 1024);
        let parsed: AgentSpawnRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed.last_consumed_offset, 4096, "offset must never go backward");
    }

    #[test]
    fn record_roundtrips_via_json_in_isolated_dir() {
        let temp = tempfile::tempdir().unwrap();
        let dispatch = protocol::SubjectDispatch::for_task("TASK-1", "standard");
        let record =
            build_record_with_decisions("agent-xyz".to_string(), 4242, &dispatch, vec!["/bin/echo".into()], None, None);

        let path = write_record_into(temp.path(), &record).expect("write");
        let raw = std::fs::read(&path).unwrap();
        let back: AgentSpawnRecord = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back.agent_session_id, "agent-xyz");
        assert_eq!(back.pid, 4242);
        assert_eq!(back.subject_id, dispatch.subject_id().unwrap_or_default());
        assert_eq!(back.workflow_ref, "standard");
        assert_eq!(back.command_line, vec!["/bin/echo".to_string()]);

        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_empty_dir_returns_empty_report() {
        let temp = tempfile::tempdir().unwrap();
        let report = scan_orphans(temp.path()).expect("scan empty dir");
        assert!(report.is_empty());
        assert!(report.detected.is_empty());
        assert!(report.cleaned.is_empty());
        assert!(report.unparseable.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_missing_dir_returns_empty_report() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        let report = scan_orphans(&missing).expect("scan missing dir");
        assert!(report.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_alive_pid_is_detected_and_record_preserved() {
        let _path = pin_path_for_process_spawns();
        let temp = tempfile::tempdir().unwrap();

        let mut child = std::process::Command::new("sleep").arg("60").spawn().expect("spawn sleep");
        let alive_pid = child.id();

        let dispatch = protocol::SubjectDispatch::for_task("TASK-ALIVE", "standard");
        let record = build_record_with_decisions(
            "agent-alive".to_string(),
            alive_pid,
            &dispatch,
            vec!["sleep".into(), "60".into()],
            None,
            None,
        );
        let path = write_record_into(temp.path(), &record).expect("write record");

        let report = scan_orphans(temp.path()).expect("scan");
        assert_eq!(report.detected.len(), 1, "alive pid should be detected");
        assert!(report.cleaned.is_empty(), "alive pid must NOT be cleaned");
        assert!(report.unparseable.is_empty());

        let detected = &report.detected[0];
        assert_eq!(detected.agent_session_id, "agent-alive");
        assert_eq!(detected.pid, alive_pid);
        assert_eq!(detected.subject_id, dispatch.subject_id().unwrap_or_default());
        assert_eq!(detected.workflow_ref, "standard");
        assert_eq!(detected.command_line, vec!["sleep".to_string(), "60".to_string()]);

        assert!(path.exists(), "record file must be preserved when pid is alive");

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_dead_pid_is_cleaned_and_record_deleted() {
        let _path = pin_path_for_process_spawns();
        let temp = tempfile::tempdir().unwrap();

        let mut child = std::process::Command::new("true").spawn().expect("spawn true");
        let dead_pid = child.id();
        let _ = child.wait();

        for _ in 0..50 {
            if !protocol::is_process_alive(dead_pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!protocol::is_process_alive(dead_pid), "child pid should have exited");

        let dispatch = protocol::SubjectDispatch::for_task("TASK-DEAD", "standard");
        let record =
            build_record_with_decisions("agent-dead".to_string(), dead_pid, &dispatch, vec!["true".into()], None, None);
        let path = write_record_into(temp.path(), &record).expect("write record");

        let report = scan_orphans(temp.path()).expect("scan");
        assert!(report.detected.is_empty(), "dead pid must NOT be detected as live");
        assert_eq!(report.cleaned.len(), 1, "dead pid should be cleaned");
        assert!(report.unparseable.is_empty());

        let cleaned = &report.cleaned[0];
        assert_eq!(cleaned.agent_session_id, "agent-dead");
        assert_eq!(cleaned.pid, dead_pid);
        assert!(!path.exists(), "record file should have been deleted");
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_unparseable_record_is_reported_and_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("garbage.json");
        std::fs::write(&path, b"{not json at all").unwrap();

        let report = scan_orphans(temp.path()).expect("scan");
        assert!(report.detected.is_empty());
        assert!(report.cleaned.is_empty());
        assert_eq!(report.unparseable.len(), 1, "unparseable record must be reported");
        assert_eq!(report.unparseable[0].path, path);
        assert!(path.exists(), "unparseable record must be preserved for operator inspection");
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_alive_pid_with_mismatched_command_is_cleaned_as_recycled() {
        let _path = pin_path_for_process_spawns();
        let temp = tempfile::tempdir().unwrap();

        let mut child = std::process::Command::new("sleep").arg("60").spawn().expect("spawn sleep");
        let alive_pid = child.id();

        let dispatch = protocol::SubjectDispatch::for_task("TASK-REUSED", "standard");
        let record = build_record_with_decisions(
            "agent-reused".to_string(),
            alive_pid,
            &dispatch,
            vec!["/usr/local/bin/animus-completely-other-binary".into()],
            None,
            None,
        );
        let path = write_record_into(temp.path(), &record).expect("write record");

        let report = scan_orphans(temp.path()).expect("scan");
        assert!(report.detected.is_empty(), "PID reused by unrelated process must NOT be reported as orphan");
        assert_eq!(report.cleaned.len(), 1, "stale record for recycled PID should be cleaned");
        assert!(!path.exists(), "record file should be deleted when command mismatches");

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphans_is_idempotent_for_alive_pid() {
        let _path = pin_path_for_process_spawns();
        let temp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep").arg("60").spawn().expect("spawn sleep");
        let alive_pid = child.id();

        let dispatch = protocol::SubjectDispatch::for_task("TASK-IDEMPOTENT", "standard");
        let record = build_record_with_decisions(
            "agent-idem".to_string(),
            alive_pid,
            &dispatch,
            vec!["sleep".into(), "60".into()],
            None,
            None,
        );
        write_record_into(temp.path(), &record).expect("write record");

        let first = scan_orphans(temp.path()).expect("scan 1");
        let second = scan_orphans(temp.path()).expect("scan 2");

        assert_eq!(first.detected.len(), 1);
        assert_eq!(second.detected.len(), 1, "alive orphan should be re-reported, not de-duped");

        let _ = child.kill();
        let _ = child.wait();
    }
}
