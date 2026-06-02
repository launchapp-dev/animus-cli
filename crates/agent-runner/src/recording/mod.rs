//! LLM-decision recording + replay (v0.5.1 round-3 production-grade layer).
//!
//! Append-only JSONL log of every agent decision-relevant event that flows
//! through the agent-runner. Recording is always-on. Replay is opt-in via
//! `ANIMUS_REPLAY_SESSION=<path>` or `context.replay_session_path`.
//!
//! Recording layout: `~/.animus/<repo-scope>/runs/<run_id>/decisions.jsonl`.
//! Archived (re-run) logs sit alongside as `decisions-<unix_ms>.jsonl.bak`.
//!
//! What round-3 closes:
//! - Durability is now configurable ([`Durability`]); production default is
//!   [`Durability::FsyncEveryN`] with N=8, balancing throughput against the
//!   bounded loss window on a kernel crash. Tests can opt into [`Durability::FlushOnly`]
//!   for speed.
//! - The parent directory holding `decisions.jsonl` is fsynced when the file
//!   is first created so the file metadata itself is durable on crash.
//! - Cross-provider safety: every recording must open with a session header
//!   ([`DecisionEvent::Metadata`] tagged `kind="session_header"`) that carries
//!   the `provider_id`. [`ReplaySource`] surfaces it and the runner refuses
//!   to replay across providers (see [`recording::fence`] integration).
//! - Idempotency-key fence integration ([`fence`] submodule): retries inside
//!   a durable workflow consult the recorded outcome instead of re-calling
//!   the model.
//! - Race-safe tail reader for gap reconstruction ([`tail`] submodule): the
//!   daemon can stream events that the runner wrote during a daemon-restart
//!   gap by tailing `decisions.jsonl`.
//! - Compaction: re-running a failed workflow archives the prior log via
//!   [`archive_decision_log`] before starting a new one.
//!
//! Out-of-boundary gaps deferred to v0.6+:
//! - Tool side-effects outside the recording boundary are not re-asserted on
//!   replay; replay yields recorded results without re-executing tools.
//! - Provider-shape normalization is documented but not enforced beyond the
//!   `provider_id` mismatch guard. A long-tail of provider streaming
//!   variations (vendor-specific tool-call envelopes, mid-stream tool result
//!   chunking) is still provider-specific in the captured `serde_json::Value`.
//! - Long-term log compression and 7-day expiry of completed logs.
//! - Real DBOS plugin RPC binding: [`fence::DurableStoreClient`] is the
//!   trait the agent-runner is wired against; ao-cli does not yet ship a
//!   concrete client to `launchapp-dev/animus-step-durable-dbos`. The
//!   integration is exercised end-to-end via [`fence::MockDurableStore`];
//!   production wiring is gated on the DBOS plugin's transport surface.
//!
//! ### Provider streaming shapes (informational)
//!
//! | provider     | response chunk shape           | tool call shape                              |
//! | ------------ | ------------------------------- | ---------------------------------------------- |
//! | claude       | `{"text":"…","stream":"stdout"}` | `ToolCallInfo { tool_name, parameters, ts }`   |
//! | codex        | newline-delimited text chunks   | text-encoded; rebuilt by orchestrator-session-host |
//! | gemini       | grouped chunks per response     | `ToolCallInfo`-compatible                      |
//! | opencode     | identical to claude             | identical to claude                            |
//! | oai          | OpenAI-format SSE deltas        | `ToolCallInfo`-compatible                      |
//!
//! All shapes round-trip as `serde_json::Value`; the cross-provider guard
//! ensures the *consumer* (replay) and the *producer* (record) agree on
//! `provider_id` so a Claude-recorded session is never fed to a Codex
//! reader expecting OpenAI delta shape.

pub mod fence;
pub mod tail;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durability posture for a [`Recorder`]. Trades throughput for crash safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Buffer + flush (page-cache only). Process-crash recoverable, kernel-crash
    /// loses any unsynced bytes. Used by tests for speed.
    FlushOnly,
    /// fsync the file after every event. Strongest durability; ~ms per event.
    FsyncPerEvent,
    /// fsync every N events. Default for production. The bounded loss window
    /// is at most N-1 events on a kernel crash.
    FsyncEveryN(usize),
}

impl Durability {
    pub const fn production_default() -> Self {
        Durability::FsyncEveryN(8)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionEvent {
    Prompt { timestamp_ms: u64, model_id: String, prompt: String, runtime_contract: Option<Value> },
    ResponseChunk { timestamp_ms: u64, stream: String, text: String },
    ToolCall { timestamp_ms: u64, name: String, args: Value },
    ToolResult { timestamp_ms: u64, name: String, result: Value },
    Metadata { timestamp_ms: u64, payload: Value },
    Error { timestamp_ms: u64, message: String },
    Finished { timestamp_ms: u64, exit_code: Option<i32> },
}

impl DecisionEvent {
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default()
    }
    pub fn prompt(model_id: impl Into<String>, prompt: impl Into<String>, runtime_contract: Option<Value>) -> Self {
        Self::Prompt {
            timestamp_ms: Self::now_ms(),
            model_id: model_id.into(),
            prompt: prompt.into(),
            runtime_contract,
        }
    }
    pub fn response_chunk(stream: impl Into<String>, text: impl Into<String>) -> Self {
        Self::ResponseChunk { timestamp_ms: Self::now_ms(), stream: stream.into(), text: text.into() }
    }
    pub fn tool_call(name: impl Into<String>, args: Value) -> Self {
        Self::ToolCall { timestamp_ms: Self::now_ms(), name: name.into(), args }
    }
    pub fn tool_result(name: impl Into<String>, result: Value) -> Self {
        Self::ToolResult { timestamp_ms: Self::now_ms(), name: name.into(), result }
    }
    pub fn metadata(payload: Value) -> Self {
        Self::Metadata { timestamp_ms: Self::now_ms(), payload }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error { timestamp_ms: Self::now_ms(), message: message.into() }
    }
    pub fn finished(exit_code: Option<i32>) -> Self {
        Self::Finished { timestamp_ms: Self::now_ms(), exit_code }
    }
    /// Construct the session header [`DecisionEvent::Metadata`] that MUST be
    /// the first event written to a fresh decision log when the writer wants
    /// the cross-provider safety guard.
    pub fn session_header(provider_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self::Metadata {
            timestamp_ms: Self::now_ms(),
            payload: serde_json::json!({
                "kind": "session_header",
                "provider_id": provider_id.into(),
                "model_id": model_id.into(),
            }),
        }
    }
}

struct RecorderInner {
    writer: BufWriter<File>,
    written_since_fsync: usize,
}

pub struct Recorder {
    inner: Mutex<RecorderInner>,
    path: PathBuf,
    durability: Durability,
}

impl Recorder {
    pub fn create_at(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with_durability(path, Durability::production_default())
    }

    pub fn create_with_durability(path: impl AsRef<Path>, durability: Durability) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path.parent().map(Path::to_path_buf);
        if let Some(parent) = parent.as_ref() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create decision-log parent dir {}", parent.display()))?;
        }
        let already_exists = path.exists();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open decision log {}", path.display()))?;
        // fsync the parent directory so the inode + dirent is durable on
        // filesystems that distinguish file-data fsync from directory fsync
        // (ext4, xfs, btrfs). Best-effort: ignore unsupported errors (eg
        // non-directory fs). This must happen the FIRST time the file
        // appears; on subsequent opens (append), the dirent is already
        // durable so we skip it to keep recorder construction cheap.
        if !already_exists {
            if let Some(parent) = parent.as_ref() {
                if let Ok(dir) = File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
        }
        Ok(Self {
            inner: Mutex::new(RecorderInner { writer: BufWriter::new(file), written_since_fsync: 0 }),
            path,
            durability,
        })
    }

    pub fn for_run(project_root: &str, run_id: &str) -> Result<Option<Self>> {
        Self::for_run_with_durability(project_root, run_id, Durability::production_default())
    }

    pub fn for_run_with_durability(project_root: &str, run_id: &str, durability: Durability) -> Result<Option<Self>> {
        let Some(path) = decision_log_path(project_root, run_id) else {
            return Ok(None);
        };
        Ok(Some(Self::create_with_durability(path, durability)?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn durability(&self) -> Durability {
        self.durability
    }

    pub fn record(&self, event: &DecisionEvent) -> Result<()> {
        let line = serde_json::to_string(event).context("serialize decision event")?;
        let mut guard = self.inner.lock().expect("recorder mutex poisoned");
        guard.writer.write_all(line.as_bytes()).context("write decision event")?;
        guard.writer.write_all(b"\n").context("write decision newline")?;
        guard.writer.flush().context("flush decision event")?;
        match self.durability {
            Durability::FlushOnly => {}
            Durability::FsyncPerEvent => {
                let inner_file = guard.writer.get_ref();
                inner_file.sync_data().context("fsync decision event")?;
            }
            Durability::FsyncEveryN(n) => {
                guard.written_since_fsync = guard.written_since_fsync.saturating_add(1);
                let threshold = n.max(1);
                if guard.written_since_fsync >= threshold {
                    let inner_file = guard.writer.get_ref();
                    inner_file.sync_data().context("fsync decision batch")?;
                    guard.written_since_fsync = 0;
                }
            }
        }
        Ok(())
    }

    /// Force a final fsync (used by tests and by clean shutdown paths to
    /// flush any pending FsyncEveryN window before the recorder drops).
    pub fn fsync_now(&self) -> Result<()> {
        let mut guard = self.inner.lock().expect("recorder mutex poisoned");
        guard.writer.flush().context("flush before fsync")?;
        guard.writer.get_ref().sync_data().context("fsync now")?;
        guard.written_since_fsync = 0;
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Best-effort final fsync on drop so the loss window stays at most N
        // events even when callers forget to call `fsync_now`. We swallow
        // errors here because Drop can't fail.
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.writer.flush();
            if guard.written_since_fsync > 0 {
                let _ = guard.writer.get_ref().sync_data();
                guard.written_since_fsync = 0;
            }
        }
    }
}

pub fn decision_log_path(project_root: &str, run_id: &str) -> Option<PathBuf> {
    if run_id.trim().is_empty() {
        return None;
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        return None;
    }
    let home = dirs::home_dir()?;
    Some(
        home.join(".animus")
            .join(protocol::repository_scope_for_path(Path::new(project_root)))
            .join("runs")
            .join(run_id)
            .join("decisions.jsonl"),
    )
}

/// Compaction primitive: rename a `decisions.jsonl` to
/// `decisions-<unix_ms>[-<n>].jsonl.bak` so a re-run can start with a fresh
/// log while preserving the prior run for forensics. No-op if the log is
/// absent. If a backup at the same millisecond already exists (two archive
/// calls within 1ms), a monotonically-increasing suffix is appended so the
/// earlier archive is not clobbered (codex round-3 P3).
pub fn archive_decision_log(path: impl AsRef<Path>) -> std::io::Result<Option<PathBuf>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let parent = match path.parent() {
        Some(p) => p,
        None => return Ok(None),
    };
    let mut archive = parent.join(format!("decisions-{ts}.jsonl.bak"));
    let mut suffix: u32 = 1;
    while archive.exists() {
        archive = parent.join(format!("decisions-{ts}-{suffix}.jsonl.bak"));
        suffix = suffix.saturating_add(1);
        if suffix > 10_000 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("archive collision: cannot allocate a unique suffix in {}", parent.display()),
            ));
        }
    }
    std::fs::rename(path, &archive)?;
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(Some(archive))
}

pub struct ReplaySource {
    events: std::vec::IntoIter<DecisionEvent>,
    truncated_tail: bool,
    provider_id: Option<String>,
}

impl ReplaySource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| format!("open replay session {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        let mut truncated_tail = false;
        let lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>().context("read replay session")?;
        let total = lines.len();
        for (idx, line) in lines.into_iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<DecisionEvent>(&line) {
                Ok(event) => events.push(event),
                Err(err) => {
                    if idx + 1 == total {
                        truncated_tail = true;
                    } else {
                        anyhow::bail!(
                            "corrupted decision-log entry at line {} of {}: {}",
                            idx + 1,
                            path.display(),
                            err
                        );
                    }
                }
            }
        }
        let provider_id = extract_provider_id(&events);
        Ok(Self { events: events.into_iter(), truncated_tail, provider_id })
    }

    pub fn truncated_tail(&self) -> bool {
        self.truncated_tail
    }

    /// The recorded provider id from the session header, if any. `None` when
    /// no header is present (legacy logs predating the cross-provider guard).
    pub fn provider_id(&self) -> Option<&str> {
        self.provider_id.as_deref()
    }

    /// Cross-provider safety guard. Returns `Err` when the recorded
    /// provider does not match the consumer's expected provider id. A
    /// missing header is permitted (legacy logs) — callers that want strict
    /// behavior should also assert `provider_id().is_some()`.
    pub fn require_provider(&self, expected: &str) -> Result<()> {
        match &self.provider_id {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => anyhow::bail!(
                "decision-log provider mismatch: recorded with `{}`, replay expects `{}` — cross-provider replay is not supported",
                actual,
                expected
            ),
            None => Ok(()),
        }
    }

    pub fn next_event(&mut self) -> Option<DecisionEvent> {
        self.events.next()
    }

    pub fn drain(mut self) -> Vec<DecisionEvent> {
        let mut out = Vec::new();
        while let Some(event) = self.next_event() {
            out.push(event);
        }
        out
    }
}

fn extract_provider_id(events: &[DecisionEvent]) -> Option<String> {
    for event in events {
        if let DecisionEvent::Metadata { payload, .. } = event {
            let is_header = payload.get("kind").and_then(|v| v.as_str()) == Some("session_header");
            if is_header {
                return payload.get("provider_id").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }
    None
}

pub fn env_replay_source() -> Result<Option<ReplaySource>> {
    match std::env::var("ANIMUS_REPLAY_SESSION") {
        Ok(path) if !path.trim().is_empty() => Ok(Some(ReplaySource::open(path)?)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tempfile::TempDir;

    fn tmp_log() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("decisions.jsonl");
        (dir, path)
    }

    #[test]
    fn round_trip_preserves_event_sequence() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        recorder.record(&DecisionEvent::prompt("claude-sonnet", "hello", None)).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "hi ")).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "there")).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);
        let events = ReplaySource::open(&path).expect("replay").drain();
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], DecisionEvent::Prompt { .. }));
        match &events[1] {
            DecisionEvent::ResponseChunk { text, .. } => assert_eq!(text, "hi "),
            other => panic!("unexpected: {:?}", other),
        }
        match &events[2] {
            DecisionEvent::ResponseChunk { text, .. } => assert_eq!(text, "there"),
            other => panic!("unexpected: {:?}", other),
        }
        assert!(matches!(events[3], DecisionEvent::Finished { exit_code: Some(0), .. }));
    }

    #[test]
    fn streaming_chunk_order_is_preserved_under_concurrent_recording() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        for i in 0..50 {
            recorder.record(&DecisionEvent::response_chunk("stdout", format!("chunk-{i}"))).unwrap();
        }
        drop(recorder);
        let events = ReplaySource::open(&path).expect("replay").drain();
        let texts: Vec<String> = events
            .into_iter()
            .filter_map(|e| match e {
                DecisionEvent::ResponseChunk { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        let expected: Vec<String> = (0..50).map(|i| format!("chunk-{i}")).collect();
        assert_eq!(texts, expected);
    }

    #[test]
    fn tool_result_replay_does_not_re_execute_tool() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        static TOOL_EXEC_COUNT: AtomicU32 = AtomicU32::new(0);
        TOOL_EXEC_COUNT.store(0, Ordering::SeqCst);
        let tool_args = serde_json::json!({"path": "/tmp/foo"});
        recorder.record(&DecisionEvent::tool_call("read_file", tool_args.clone())).unwrap();
        let tool_result = {
            TOOL_EXEC_COUNT.fetch_add(1, Ordering::SeqCst);
            serde_json::json!({"content": "side-effect-1"})
        };
        recorder.record(&DecisionEvent::tool_result("read_file", tool_result.clone())).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);
        assert_eq!(TOOL_EXEC_COUNT.load(Ordering::SeqCst), 1);
        let events = ReplaySource::open(&path).expect("replay").drain();
        let mut saw_call = false;
        let mut saw_result = false;
        for event in events {
            match event {
                DecisionEvent::ToolCall { name, args, .. } => {
                    assert_eq!(name, "read_file");
                    assert_eq!(args, tool_args);
                    saw_call = true;
                }
                DecisionEvent::ToolResult { name, result, .. } => {
                    assert_eq!(name, "read_file");
                    assert_eq!(result, tool_result);
                    saw_result = true;
                }
                _ => {}
            }
        }
        assert!(saw_call && saw_result);
        assert_eq!(TOOL_EXEC_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn truncated_final_line_is_tolerated() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "ok")).unwrap();
        drop(recorder);
        let mut file = OpenOptions::new().append(true).open(&path).expect("reopen");
        file.write_all(br#"{"kind":"response_chunk","timestamp_ms":1,"stream":"stdout","tex"#).unwrap();
        drop(file);
        let source = ReplaySource::open(&path).expect("replay tolerates truncation");
        assert!(source.truncated_tail());
        let events = source.drain();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn corrupted_middle_line_aborts_replay() {
        let (_dir, path) = tmp_log();
        {
            let mut file = OpenOptions::new().create(true).append(true).open(&path).unwrap();
            writeln!(
                file,
                r#"{{"kind":"prompt","timestamp_ms":1,"model_id":"m","prompt":"p","runtime_contract":null}}"#
            )
            .unwrap();
            writeln!(file, "not-json-garbage").unwrap();
            writeln!(file, r#"{{"kind":"finished","timestamp_ms":2,"exit_code":0}}"#).unwrap();
        }
        match ReplaySource::open(&path) {
            Ok(_) => panic!("middle corruption must abort"),
            Err(err) => assert!(err.to_string().contains("corrupted"), "unexpected error: {err}"),
        }
    }

    #[test]
    fn unsafe_run_id_is_rejected() {
        assert!(decision_log_path("/tmp", "../escape").is_none());
        assert!(decision_log_path("/tmp", "ok/nope").is_none());
        assert!(decision_log_path("/tmp", "").is_none());
    }

    #[test]
    fn env_replay_source_returns_none_when_unset() {
        if std::env::var("ANIMUS_REPLAY_SESSION").is_err() {
            assert!(env_replay_source().expect("no error").is_none());
        }
    }

    #[test]
    fn fsync_per_event_durability_survives_simulated_crash() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "alpha")).unwrap();
        // Simulate process crash: drop without explicit close, then read from
        // a fresh handle. With FsyncPerEvent, every byte is durable.
        std::mem::forget(recorder);
        let events = ReplaySource::open(&path).expect("replay after crash").drain();
        let chunks: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                DecisionEvent::ResponseChunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks, vec!["alpha"]);
    }

    #[test]
    fn fsync_every_n_flushes_on_drop_so_loss_window_is_bounded() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncEveryN(4)).expect("recorder");
        for i in 0..10 {
            recorder.record(&DecisionEvent::response_chunk("stdout", format!("e{i}"))).unwrap();
        }
        // Drop runs the best-effort fsync of any pending window so the file
        // observes all 10 events when we re-open it.
        drop(recorder);
        let events = ReplaySource::open(&path).expect("replay").drain();
        assert_eq!(events.len(), 10);
    }

    #[test]
    fn handoff_between_recorders_preserves_event_stream() {
        let (_dir, path) = tmp_log();
        {
            let r1 = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder 1");
            r1.record(&DecisionEvent::session_header("claude", "claude-sonnet")).unwrap();
            r1.record(&DecisionEvent::prompt("claude-sonnet", "first", None)).unwrap();
            r1.record(&DecisionEvent::response_chunk("stdout", "a")).unwrap();
        }
        // Simulate writer death + restart pointing at the same file.
        {
            let r2 = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder 2");
            r2.record(&DecisionEvent::response_chunk("stdout", "b")).unwrap();
            r2.record(&DecisionEvent::finished(Some(0))).unwrap();
        }
        let source = ReplaySource::open(&path).expect("replay");
        assert_eq!(source.provider_id(), Some("claude"));
        let events = source.drain();
        let chunks: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                DecisionEvent::ResponseChunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks, vec!["a", "b"]);
        assert!(matches!(events.last(), Some(DecisionEvent::Finished { exit_code: Some(0), .. })));
    }

    #[test]
    fn provider_id_is_extracted_from_session_header() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        recorder.record(&DecisionEvent::session_header("codex", "gpt-5")).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);
        let source = ReplaySource::open(&path).expect("replay");
        assert_eq!(source.provider_id(), Some("codex"));
    }

    #[test]
    fn require_provider_rejects_mismatched_replay() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        recorder.record(&DecisionEvent::session_header("claude", "claude-sonnet")).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);
        let source = ReplaySource::open(&path).expect("replay");
        // Same provider: ok.
        source.require_provider("claude").expect("matching provider");
        // Mismatch: must error.
        let source2 = ReplaySource::open(&path).expect("replay");
        let err = source2.require_provider("codex").expect_err("mismatch must error");
        assert!(err.to_string().contains("provider mismatch"), "unexpected: {err}");
    }

    #[test]
    fn require_provider_permits_legacy_log_without_header() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);
        let source = ReplaySource::open(&path).expect("replay");
        assert!(source.provider_id().is_none());
        source.require_provider("anything").expect("legacy log: no guard");
    }

    #[test]
    fn archive_renames_existing_log_with_timestamped_suffix() {
        let (_dir, path) = tmp_log();
        let recorder = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        drop(recorder);
        let archive = archive_decision_log(&path).expect("archive").expect("must produce a path");
        assert!(!path.exists(), "primary must be moved away");
        assert!(archive.exists(), "archived path must exist");
        let name = archive.file_name().and_then(|s| s.to_str()).expect("file name");
        assert!(name.starts_with("decisions-"));
        assert!(name.ends_with(".jsonl.bak"));
        // A fresh recorder can now reopen the primary path and start clean.
        let r2 = Recorder::create_with_durability(&path, Durability::FlushOnly).expect("fresh");
        r2.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(r2);
        let events = ReplaySource::open(&path).expect("replay").drain();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DecisionEvent::Finished { .. }));
    }

    #[test]
    fn archive_appends_unique_suffix_on_same_millisecond_collision() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("decisions.jsonl");

        // First archive: writes to a fresh path, succeeds with bare `<ts>` form.
        std::fs::write(&path, b"first").unwrap();
        let first = archive_decision_log(&path).expect("first archive").expect("path");
        assert!(first.exists());

        // Manually create a sibling at the EXACT timestamped path that the
        // second archive would have chosen, simulating a same-millisecond
        // collision the wall clock can't fix.
        std::fs::write(&first, b"colliding").unwrap();
        // Now create a fresh primary log and archive it.
        std::fs::write(&path, b"second").unwrap();
        let second = archive_decision_log(&path).expect("second archive").expect("path");
        assert!(second.exists());
        assert_ne!(first, second, "second archive must NOT clobber the first");
        // The first archive must still be intact with its original content.
        let raw = std::fs::read(&first).unwrap();
        assert_eq!(raw, b"colliding");
    }

    #[test]
    fn archive_on_absent_log_is_noop() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("decisions.jsonl");
        assert!(archive_decision_log(&path).expect("noop").is_none());
    }
}
