use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use protocol::{metrics_env_disabled, Config};

use super::events::{Event, EventTags};
use super::metrics_state_dir;

const PENDING_FILE: &str = "pending.jsonl";
const LAST_SEND_FILE: &str = "last-send.txt";

/// Handle for emitting metrics events into the project's scoped state.
///
/// Construction is fallible-but-quiet: every step that could prevent
/// emission resolves to `None`, so callers can `let _ = recorder
/// .map(|r| r.record(...))` without any branching at the call site.
pub(crate) struct MetricsRecorder {
    pending_path: PathBuf,
}

impl MetricsRecorder {
    /// Build a recorder if telemetry is permitted for the current user.
    /// Returns `None` when:
    ///   - `ANIMUS_METRICS_DISABLE=1`,
    ///   - the **user-global** config (`~/.animus/config.json`) does not
    ///     exist yet (no opt-in possible without a deliberate first-run
    ///     answer),
    ///   - the global config has no `metrics` block,
    ///   - the user opted out,
    ///   - the scoped state root is unavailable.
    ///
    /// Consent lives in the user-global config, NOT the project-local
    /// `.animus/config.json` — project files can be committed and a
    /// cloned repo must never carry someone else's consent or endpoint.
    /// We only **read** the global config here; the recorder never
    /// materializes it as a side effect.
    pub(crate) fn for_project(project_root: &Path) -> Option<Self> {
        if metrics_env_disabled() {
            return None;
        }
        let config = Config::load_global_if_exists()?;
        let metrics = config.metrics.as_ref()?;
        if !metrics.is_enabled() {
            return None;
        }
        let dir = metrics_state_dir(project_root)?;
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        Some(Self { pending_path: dir.join(PENDING_FILE) })
    }

    /// Append an event to the pending queue. Best-effort — IO failures
    /// here must never break the caller.
    pub(crate) fn record(&self, tags: EventTags) {
        let event = Event { recorded_at: Utc::now().to_rfc3339(), tags };
        let Ok(line) = serde_json::to_string(&event) else { return };
        let mut file = match OpenOptions::new().create(true).append(true).open(&self.pending_path) {
            Ok(file) => file,
            Err(_) => return,
        };
        let _ = writeln!(file, "{line}");
    }

    /// Drop the pending queue without sending. Called from tests; the
    /// `metrics disable` CLI handler drops the file directly because the
    /// recorder constructor refuses to build once opt-out is persisted.
    #[cfg(test)]
    pub(crate) fn clear_pending(&self) {
        let _ = fs::remove_file(&self.pending_path);
    }

    /// Test-only constructor that points the recorder at a specific
    /// pending file. Lets the test suite verify accumulation without
    /// touching HOME (which would race against unrelated tests that
    /// share `protocol::scoped_state_root`).
    #[cfg(test)]
    pub(crate) fn for_pending_path(pending_path: std::path::PathBuf) -> Self {
        Self { pending_path }
    }
}

/// Top-level fire-and-forget recorder for callers that don't want to hold
/// a `MetricsRecorder` value. Resolves the recorder, records, drops it.
/// Silently no-ops if telemetry is disabled.
pub(crate) fn record_event(project_root: &Path, tags: EventTags) {
    if let Some(recorder) = MetricsRecorder::for_project(project_root) {
        recorder.record(tags);
    }
}

/// Side-effect-free metrics-block accessor used by the sender + flush
/// paths. Returns the persisted `MetricsConfig` only when the global
/// `~/.animus/config.json` exists and contains a `metrics` block. The
/// `_project_root` parameter is preserved for call-site symmetry with
/// other recorder helpers (and to keep the call shape stable if future
/// telemetry scopes flip back to project-local).
pub(crate) fn read_metrics_block_without_creating(_project_root: &Path) -> Option<protocol::MetricsConfig> {
    Config::load_global_if_exists()?.metrics
}

/// Return the count of buffered events without consuming them.
pub(crate) fn pending_event_count(project_root: &Path) -> usize {
    let Some(dir) = metrics_state_dir(project_root) else { return 0 };
    let path = dir.join(PENDING_FILE);
    let Ok(content) = fs::read_to_string(&path) else { return 0 };
    content.lines().filter(|line| !line.trim().is_empty()).count()
}

/// Returns the last-send marker as an RFC 3339 string, if present.
pub(crate) fn last_send_timestamp(project_root: &Path) -> Option<String> {
    let dir = metrics_state_dir(project_root)?;
    let path = dir.join(LAST_SEND_FILE);
    let raw = fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Records the timestamp of the most recent successful send.
pub(crate) fn write_last_send_timestamp(project_root: &Path, ts: &str) {
    let Some(dir) = metrics_state_dir(project_root) else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = fs::write(dir.join(LAST_SEND_FILE), ts);
}

/// Returns the pending events WITHOUT removing or rotating the file.
/// Inspection helper kept alongside [`rotate_and_read_pending`] (the
/// flush-path primitive) so future debug surfaces can probe the buffer
/// non-destructively.
#[allow(dead_code)]
pub(crate) fn drain_pending(project_root: &Path) -> Vec<Event> {
    let Some(dir) = metrics_state_dir(project_root) else { return Vec::new() };
    read_events_from(&dir.join(PENDING_FILE))
}

/// Atomically rotates `pending.jsonl` to a uniquely-named flushing
/// file and returns the parsed events alongside the rotated path. Any
/// CLI processes that append to `pending.jsonl` after the rotation see
/// a fresh empty file and their events survive into the next flush.
///
/// Also recovers any stale `flushing-*` snapshots left behind by a
/// previous flush that was cancelled (e.g. the opportunistic 2s CLI
/// exit timeout firing mid-send) by folding them back into the
/// rotated batch — so events are neither stranded nor lost.
///
/// Returns `None` when there is no pending file *and* no stale
/// snapshots. The caller is responsible for deleting the rotated file
/// on success (via [`delete_rotated`]) or restoring it on hard
/// failure (via [`restore_rotated`]).
pub(crate) fn rotate_and_read_pending(project_root: &Path) -> Option<(PathBuf, Vec<Event>)> {
    let dir = metrics_state_dir(project_root)?;
    let src = dir.join(PENDING_FILE);
    let stale = recover_stale_flushing(&dir);
    if !src.exists() && stale.is_empty() {
        return None;
    }
    let ts = Utc::now().format("%Y%m%dT%H%M%S%.6f").to_string();
    let pid = std::process::id();
    let rotated = dir.join(format!("flushing-{pid}-{ts}.jsonl"));
    if src.exists() && fs::rename(&src, &rotated).is_err() {
        return None;
    }
    let mut events = read_events_from(&rotated);
    if !stale.is_empty() {
        let mut combined = String::new();
        for path in &stale {
            if let Ok(content) = fs::read_to_string(path) {
                combined.push_str(&content);
                if !content.ends_with('\n') && !content.is_empty() {
                    combined.push('\n');
                }
            }
        }
        if let Ok(existing) = fs::read_to_string(&rotated) {
            combined.push_str(&existing);
        }
        let _ = fs::write(&rotated, combined);
        for path in &stale {
            let _ = fs::remove_file(path);
        }
        events = read_events_from(&rotated);
    }
    Some((rotated, events))
}

/// Lists prior `flushing-*` snapshots that another flush abandoned
/// (e.g. via the 2s CLI-exit timeout). The current rotation creates a
/// new uniquely-named file, so anything pre-existing was stranded.
fn recover_stale_flushing(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let mut stale = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("flushing-") && name.ends_with(".jsonl") {
                stale.push(path);
            }
        }
    }
    stale
}

fn read_events_from(path: &Path) -> Vec<Event> {
    let Ok(content) = fs::read_to_string(path) else { return Vec::new() };
    let mut events = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Event>(trimmed) {
            events.push(event);
        }
    }
    events
}

/// Removes a previously-rotated flushing file after a successful send.
pub(crate) fn delete_rotated(rotated: &Path) {
    let _ = fs::remove_file(rotated);
}

/// Restores a rotated flushing file back into `pending.jsonl` after a
/// hard failure, merging with anything new that arrived during the
/// flush. If a new `pending.jsonl` was created in the meantime, the
/// rotated contents are prepended (oldest-first) so the next flush
/// sees both.
pub(crate) fn restore_rotated(project_root: &Path, rotated: &Path) {
    let Some(dir) = metrics_state_dir(project_root) else {
        let _ = fs::remove_file(rotated);
        return;
    };
    let pending = dir.join(PENDING_FILE);
    let rotated_content = fs::read_to_string(rotated).unwrap_or_default();
    let appended = fs::read_to_string(&pending).unwrap_or_default();
    let mut merged = String::with_capacity(rotated_content.len() + appended.len());
    merged.push_str(&rotated_content);
    if !rotated_content.ends_with('\n') && !rotated_content.is_empty() {
        merged.push('\n');
    }
    merged.push_str(&appended);
    let _ = fs::write(&pending, merged);
    let _ = fs::remove_file(rotated);
}

/// Removes the pending file in-place. Reserved for explicit opt-out
/// flows that bypass the rotate/restore dance.
#[allow(dead_code)]
pub(crate) fn delete_pending(project_root: &Path) {
    let Some(dir) = metrics_state_dir(project_root) else { return };
    let _ = fs::remove_file(dir.join(PENDING_FILE));
}

/// Rewrites the pending queue from a slice of events. Used when a send
/// returns a hard rejection and we want to drop just the rejected
/// batch while preserving anything else that arrived during the send.
/// Currently unused since [`drain_pending`] leaves the file alone, but
/// kept here as a primitive for future flush strategies.
#[allow(dead_code)]
pub(crate) fn rewrite_pending(project_root: &Path, events: &[Event]) {
    let Some(dir) = metrics_state_dir(project_root) else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(PENDING_FILE);
    let mut lines = String::new();
    for event in events {
        if let Ok(line) = serde_json::to_string(event) {
            lines.push_str(&line);
            lines.push('\n');
        }
    }
    let _ = fs::write(path, lines);
}
