//! Unit tests for the metrics module. We deliberately avoid mutating
//! `HOME` here because [`protocol::scoped_state_root`] reads it
//! process-wide and other parallel tests share the same assumption.
//! Instead, we point the recorder at explicit pending paths through the
//! test-only `for_pending_path` constructor.

use std::fs;

use protocol::test_utils::EnvVarGuard;
use tempfile::tempdir;

use super::events::EventTags;
use super::recorder::MetricsRecorder;
use super::CommandGroup;

#[test]
fn recorder_writes_jsonl_when_opted_in() {
    let dir = tempdir().expect("tempdir");
    let pending = dir.path().join("pending.jsonl");
    let recorder = MetricsRecorder::for_pending_path(pending.clone());
    recorder.record(EventTags::DaemonStarted {});
    recorder.record(EventTags::CliInvoked { command_group: CommandGroup::Daemon });
    let raw = fs::read_to_string(&pending).expect("pending exists");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(first["name"], "daemon_started");
}

#[test]
fn recorder_appends_across_multiple_records() {
    let dir = tempdir().expect("tempdir");
    let pending = dir.path().join("pending.jsonl");
    {
        let recorder = MetricsRecorder::for_pending_path(pending.clone());
        recorder.record(EventTags::DaemonStarted {});
    }
    {
        let recorder = MetricsRecorder::for_pending_path(pending.clone());
        recorder.record(EventTags::DaemonStarted {});
    }
    let raw = fs::read_to_string(&pending).expect("pending exists");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn recorder_clear_pending_removes_file() {
    let dir = tempdir().expect("tempdir");
    let pending = dir.path().join("pending.jsonl");
    let recorder = MetricsRecorder::for_pending_path(pending.clone());
    recorder.record(EventTags::DaemonStarted {});
    assert!(pending.exists());
    recorder.clear_pending();
    assert!(!pending.exists());
}

#[test]
fn env_kill_switch_returns_true_when_set() {
    // ENV_LOCK serialization keeps this from racing with sibling env
    // mutators (shared.rs HOME guards, etc.).
    let _kill = EnvVarGuard::set("ANIMUS_METRICS_DISABLE", Some("1"));
    assert!(protocol::metrics_env_disabled());
}
