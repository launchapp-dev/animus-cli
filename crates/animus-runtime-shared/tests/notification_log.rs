use std::fs::OpenOptions;
use std::io::Write;

use animus_runtime_shared::notification_log::{workflow_run_dir, NotificationLog};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn notification_log_appends_and_tails_with_seq_correlation() {
    let temp = TempDir::new().expect("tempdir");
    let scoped = temp.path();
    let workflow_id = "wf-001";

    let log = NotificationLog::open(scoped, workflow_id).expect("open log");
    log.append("triage", &json!({"kind": "started", "ts": "t0"})).expect("append 1");
    log.append("triage", &json!({"kind": "text", "delta": "hello"})).expect("append 2");
    log.append("impl", &json!({"kind": "finished", "exit": 0})).expect("append 3");
    drop(log);

    let all = NotificationLog::tail(scoped, workflow_id, 0).expect("tail");
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].seq, 1);
    assert_eq!(all[1].seq, 2);
    assert_eq!(all[2].seq, 3);
    assert_eq!(all[0].phase, "triage");
    assert_eq!(all[2].phase, "impl");

    let from_one = NotificationLog::tail(scoped, workflow_id, 1).expect("tail from seq=1");
    assert_eq!(from_one.len(), 2);
    assert_eq!(from_one[0].seq, 2);
    assert_eq!(from_one[1].seq, 3);

    let from_end = NotificationLog::tail(scoped, workflow_id, 99).expect("tail from far seq");
    assert!(from_end.is_empty());
}

#[test]
fn notification_log_skips_partial_final_line_on_replay() {
    let temp = TempDir::new().expect("tempdir");
    let scoped = temp.path();
    let workflow_id = "wf-partial";

    let log = NotificationLog::open(scoped, workflow_id).expect("open");
    log.append("p1", &json!({"kind": "started"})).expect("append");
    log.append("p1", &json!({"kind": "chunk", "text": "ok"})).expect("append");
    drop(log);

    let path = workflow_run_dir(scoped, workflow_id).join("notifications.jsonl");
    let mut file = OpenOptions::new().append(true).open(&path).expect("reopen");
    file.write_all(b"{\"seq\":99,\"ts\":\"partial\"").expect("partial write");
    file.flush().expect("flush");
    drop(file);

    let tailed = NotificationLog::tail(scoped, workflow_id, 0).expect("tail");
    assert_eq!(tailed.len(), 2, "partial trailing line must be silently dropped on replay");

    let reopened = NotificationLog::open(scoped, workflow_id).expect("reopen log");
    reopened.append("p1", &json!({"kind": "post-crash"})).expect("append post-crash");
    drop(reopened);

    let after = NotificationLog::tail(scoped, workflow_id, 0).expect("tail");
    assert!(after.len() >= 3);
    let last = after.last().expect("last record");
    assert!(last.seq > 2);
    assert_eq!(last.notification.get("kind").and_then(|v| v.as_str()), Some("post-crash"));
}

#[test]
fn notification_log_rotates_at_100mb_boundary() {
    let temp = TempDir::new().expect("tempdir");
    let scoped = temp.path();
    let workflow_id = "wf-rotate";

    let log = NotificationLog::open(scoped, workflow_id).expect("open");
    log.append("p", &json!({"k": "small"})).expect("append small");

    let dir = workflow_run_dir(scoped, workflow_id);
    let primary = dir.join("notifications.jsonl");
    let filler = vec![b'x'; 100 * 1024 * 1024 + 16];
    {
        let mut f = OpenOptions::new().append(true).open(&primary).expect("open primary for filler");
        f.write_all(&filler).expect("filler write");
        f.write_all(b"\n").expect("nl");
        f.flush().expect("flush");
    }

    log.rotate_if_needed().expect("rotate");

    let rotated = dir.join("notifications.1.jsonl");
    assert!(rotated.exists(), "rotated file should exist");
    assert!(primary.exists(), "primary file should exist after rotation");
    let primary_meta = std::fs::metadata(&primary).expect("primary metadata");
    assert!(primary_meta.len() < 100 * 1024 * 1024, "primary should be small after rotation");

    log.append("p", &json!({"k": "post-rotate"})).expect("append post-rotate");
    let after = std::fs::metadata(&primary).expect("primary metadata 2");
    assert!(after.len() > 0);
}
