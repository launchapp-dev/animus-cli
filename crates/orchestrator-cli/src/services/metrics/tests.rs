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
fn recorder_drops_events_when_pending_is_oversized() {
    // A stalled/failed flush must never let the buffer grow without bound.
    // Once pending is past the cap, new events are dropped rather than
    // appended (this is the guard against the multi-GB runaway).
    let dir = tempdir().expect("tempdir");
    let pending = dir.path().join("pending.jsonl");
    // Write a buffer just over the 8 MiB record-time cap.
    let big = "x".repeat(8 * 1024 * 1024 + 1024);
    fs::write(&pending, &big).expect("seed oversized pending");
    let before = fs::metadata(&pending).expect("meta").len();

    let recorder = MetricsRecorder::for_pending_path(pending.clone());
    recorder.record(EventTags::DaemonStarted {});

    let after = fs::metadata(&pending).expect("meta").len();
    assert_eq!(after, before, "oversized pending must not grow");
}

#[test]
fn cleanup_removes_oversized_and_stale_flushing_and_truncates_pending() {
    use super::recorder::cleanup_metrics_dir;
    let dir = tempdir().expect("tempdir");
    let p = dir.path();
    // An oversized flushing snapshot (>16 MiB) — pathological, must go.
    fs::write(p.join("flushing-1-a.jsonl"), "y".repeat(17 * 1024 * 1024)).unwrap();
    // A small, fresh flushing snapshot — kept when not stale.
    fs::write(p.join("flushing-2-b.jsonl"), "{}\n").unwrap();
    // An oversized pending buffer (>8 MiB) — dropped.
    fs::write(p.join("pending.jsonl"), "z".repeat(9 * 1024 * 1024)).unwrap();

    // Wide stale window: only the oversized flushing + oversized pending go.
    let r1 = cleanup_metrics_dir(p, 100_000);
    assert_eq!(r1.flushing_removed, 1, "oversized flushing removed");
    assert!(r1.pending_truncated, "oversized pending dropped");
    assert!(r1.bytes_reclaimed > 16 * 1024 * 1024);
    assert!(p.join("flushing-2-b.jsonl").exists(), "fresh flushing kept");
    assert!(!p.join("pending.jsonl").exists(), "oversized pending gone");

    // Zero stale window: the remaining flushing file is now treated as stale.
    let r2 = cleanup_metrics_dir(p, 0);
    assert_eq!(r2.flushing_removed, 1, "stale flushing removed");
    assert!(!p.join("flushing-2-b.jsonl").exists());
}

#[test]
fn env_kill_switch_returns_true_when_set() {
    // ENV_LOCK serialization keeps this from racing with sibling env
    // mutators (shared.rs HOME guards, etc.).
    let _kill = EnvVarGuard::set("ANIMUS_METRICS_DISABLE", Some("1"));
    assert!(protocol::metrics_env_disabled());
}
