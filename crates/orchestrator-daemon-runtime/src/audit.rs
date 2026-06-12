//! Append-only audit log for security-relevant events.
//!
//! The log lives at `<scoped_state_root>/audit.jsonl` and rotates at
//! [`AUDIT_LOG_MAX_BYTES`] bytes to `audit.<N>.jsonl`. Every line is one JSON
//! object:
//!
//! ```json
//! {"ts": "2026-05-26T...", "actor": "user", "event": "plugin_install",
//!  "details": {"repo": "launchapp-dev/animus-provider-claude",
//!              "version": "v0.2.2", "sha256": "abc...",
//!              "signature_status": "verified"}}
//! ```
//!
//! [`Audit::log_event`] is fire-and-forget: any IO error is logged via
//! `tracing::warn!` but never propagated, so install / verify paths that
//! call it can never fail because audit went wrong.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use fs2::FileExt;
use serde::Serialize;
use serde_json::{json, Value};

/// Default rotation threshold (10 MiB). Configurable via
/// [`Audit::with_max_bytes`] in tests.
pub const AUDIT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Where on disk the audit log file lives, given a scoped state root.
#[must_use]
pub fn audit_log_path(scoped_root: &Path) -> PathBuf {
    scoped_root.join("audit.jsonl")
}

/// Who performed the action being logged.
///
/// v0.5.8 small core kept the legacy `User`/`Daemon` variants for
/// back-compat: every existing reader still parses lines with
/// `"actor": "user"` and `"actor": "daemon"`. New call sites can pass
/// [`AuditActor::Principal`] to additionally stamp typed `principal.id`
/// and `principal.kind` fields on the JSON record. The legacy `actor`
/// string still maps to `"user"` / `"daemon"` / `"service_account"` so
/// SIEM filters keep working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditActor {
    /// Legacy "some OS user did this" actor. Preserved bit-identical for
    /// existing call sites (no `principal.*` fields emitted).
    User,
    /// Legacy daemon-initiated event.
    Daemon,
    /// v0.5.8: typed principal carrying `id` + `kind` for the audit
    /// record. The legacy `actor` field is derived from `kind` so
    /// existing readers keep working.
    Principal {
        /// Stable principal id (matches `Principal::id`).
        id: String,
        /// Principal kind label (matches `Principal::kind`).
        kind: &'static str,
    },
}

impl AuditActor {
    /// Stable string label written to the legacy `actor` field. Returns
    /// `"user"` / `"daemon"` / `"service_account"` per the design doc.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Daemon => "daemon",
            Self::Principal { kind, .. } => kind,
        }
    }

    /// Convenience: build a [`Self::Principal`] actor from an
    /// `orchestrator_core::Principal`.
    #[must_use]
    pub fn from_principal(principal: &orchestrator_core::Principal) -> Self {
        Self::Principal { id: principal.id().to_string(), kind: principal.kind() }
    }

    fn principal_fields(&self) -> Option<(String, &'static str)> {
        match self {
            Self::Principal { id, kind } => Some((id.clone(), kind)),
            Self::User | Self::Daemon => None,
        }
    }
}

/// Stable event labels. Add new variants here when a new sink starts
/// emitting events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEventKind {
    PluginInstall,
    PluginUninstall,
    PluginUpgrade,
    SignatureSkipped,
    SignatureInvalid,
    TrustKeyAdded,
    TrustPublisherAdded,
    TrustOrgRevoked,
    PolicyOverride,
    LockfileMismatch,
}

impl AuditEventKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PluginInstall => "plugin_install",
            Self::PluginUninstall => "plugin_uninstall",
            Self::PluginUpgrade => "plugin_upgrade",
            Self::SignatureSkipped => "signature_skipped",
            Self::SignatureInvalid => "signature_invalid",
            Self::TrustKeyAdded => "trust_key_added",
            Self::TrustPublisherAdded => "trust_publisher_added",
            Self::TrustOrgRevoked => "trust_org_revoked",
            Self::PolicyOverride => "policy_override",
            Self::LockfileMismatch => "lockfile_mismatch",
        }
    }
}

/// One audit log record. The wire shape is always the JSON object described
/// in the module docs.
///
/// v0.5.8 small core adds the optional `principal` field: when the actor
/// is [`AuditActor::Principal`], serialization emits a sibling
/// `principal: {id, kind}` object next to the legacy `actor` string. The
/// legacy string field is kept so existing audit-log readers (history
/// CLI, external SIEM exports) keep parsing unchanged — see the design
/// doc §Risks #6.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    /// RFC 3339 timestamp.
    pub ts: String,
    /// Legacy actor string (`"user"` / `"daemon"` / `"service_account"`).
    pub actor: &'static str,
    /// Stable event label (matches [`AuditEventKind::as_str`]).
    pub event: &'static str,
    /// Free-form event payload.
    pub details: Value,
    /// v0.5.8 typed principal carrier. Emitted only when the actor was
    /// constructed via [`AuditActor::Principal`] / [`AuditActor::from_principal`].
    /// Absent on legacy `User`/`Daemon` actors so existing audit-log
    /// fixtures keep parsing bit-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<AuditPrincipal>,
}

/// Typed principal carrier serialized as a sibling of the legacy
/// `actor` string. Mirrors `orchestrator_core::Principal::{id,kind}`.
#[derive(Debug, Clone, Serialize)]
pub struct AuditPrincipal {
    /// Stable principal id.
    pub id: String,
    /// Principal kind (`user` / `daemon` / `service_account`).
    pub kind: &'static str,
}

impl AuditEvent {
    /// Build an event with `ts` set to "now".
    #[must_use]
    pub fn new(actor: AuditActor, event: AuditEventKind, details: Value) -> Self {
        let principal = actor.principal_fields().map(|(id, kind)| AuditPrincipal { id, kind });
        Self { ts: Utc::now().to_rfc3339(), actor: actor.as_str(), event: event.as_str(), details, principal }
    }
}

/// Audit log writer. Cheap to construct (just two paths); the file is opened
/// (and rotated, if needed) once per [`Self::log_event`] call.
#[derive(Debug, Clone)]
pub struct Audit {
    path: PathBuf,
    max_bytes: u64,
}

impl Audit {
    /// Bind a writer to `audit.jsonl` under the supplied scoped state root.
    #[must_use]
    pub fn at_scoped_root(scoped_root: &Path) -> Self {
        Self { path: audit_log_path(scoped_root), max_bytes: AUDIT_LOG_MAX_BYTES }
    }

    /// Bind a writer to a specific path. Used by tests.
    #[must_use]
    pub fn at_path(path: PathBuf) -> Self {
        Self { path, max_bytes: AUDIT_LOG_MAX_BYTES }
    }

    /// Override the rotation threshold (used by tests).
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Resolved log path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Fire-and-forget log call.
    ///
    /// Errors are downgraded to `tracing::warn!` so a broken audit log can
    /// never fail an install or verify. Callers MUST treat this as
    /// best-effort and never branch on success.
    pub fn log_event(&self, event: AuditEvent) {
        if let Err(err) = self.try_log(&event) {
            tracing::warn!(
                path = %self.path.display(),
                event = event.event,
                error = %err,
                "audit log write failed; dropping event",
            );
        }
    }

    /// Convenience: build + log in one call.
    pub fn log(&self, actor: AuditActor, kind: AuditEventKind, details: Value) {
        self.log_event(AuditEvent::new(actor, kind, details));
    }

    fn try_log(&self, event: &AuditEvent) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
        line.push('\n');
        for _ in 0..AUDIT_ROTATION_RETRIES {
            let mut handle = OpenOptions::new().create(true).append(true).open(&self.path)?;
            handle.lock_exclusive()?;
            if !locked_file_still_at_path(&handle, &self.path) {
                let _ = handle.unlock();
                continue;
            }
            if handle.metadata()?.len() >= self.max_bytes {
                let rotated = next_rotation_path(&self.path);
                if fs::rename(&self.path, rotated).is_ok() {
                    let _ = handle.unlock();
                    continue;
                }
            }
            handle.write_all(line.as_bytes())?;
            handle.flush()?;
            let _ = handle.unlock();
            return Ok(());
        }

        let mut handle = OpenOptions::new().create(true).append(true).open(&self.path)?;
        handle.lock_exclusive()?;
        handle.write_all(line.as_bytes())?;
        handle.flush()?;
        let _ = handle.unlock();
        Ok(())
    }
}

const AUDIT_ROTATION_RETRIES: usize = 5;

#[cfg(unix)]
fn locked_file_still_at_path(file: &fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(held), Ok(current)) => held.ino() == current.ino() && held.dev() == current.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn locked_file_still_at_path(_file: &fs::File, _path: &Path) -> bool {
    true
}

/// Pick the next free `audit.<N>.jsonl` rotation slot next to `path`.
fn next_rotation_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("audit");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("jsonl");
    for n in 1..1_000_000_u32 {
        let candidate = parent.join(format!("{stem}.{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem}.overflow.{ext}"))
}

/// Convenience for callers that don't want to construct an [`Audit`] up
/// front: build one bound to `scoped_root` and emit a single event.
pub fn log_event(scoped_root: &Path, event: AuditEvent) {
    Audit::at_scoped_root(scoped_root).log_event(event);
}

/// Convenience that just wraps `(actor, kind, details)` into an
/// [`AuditEvent`] and calls [`log_event`].
pub fn log(scoped_root: &Path, actor: AuditActor, kind: AuditEventKind, details: Value) {
    log_event(scoped_root, AuditEvent::new(actor, kind, details));
}

/// Convenience helper used by install pipelines: emit a `plugin_install` /
/// `plugin_upgrade` line with the standard `{repo, version, sha256,
/// signature_status}` shape.
pub fn log_plugin_install(
    scoped_root: &Path,
    kind: AuditEventKind,
    repo: &str,
    version: &str,
    sha256: &str,
    signature_status: &str,
) {
    log(
        scoped_root,
        AuditActor::User,
        kind,
        json!({
            "repo": repo,
            "version": version,
            "sha256": sha256,
            "signature_status": signature_status,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as JsonValue;
    use tempfile::tempdir;

    fn read_lines(path: &Path) -> Vec<JsonValue> {
        let body = fs::read_to_string(path).unwrap();
        body.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn log_event_writes_one_line_per_call() {
        let dir = tempdir().unwrap();
        let audit = Audit::at_scoped_root(dir.path());
        audit.log_event(AuditEvent::new(
            AuditActor::User,
            AuditEventKind::PluginInstall,
            json!({"repo": "x/y", "version": "v1", "sha256": "abc", "signature_status": "verified"}),
        ));
        audit.log_event(AuditEvent::new(
            AuditActor::Daemon,
            AuditEventKind::SignatureSkipped,
            json!({"repo": "x/y", "reason": "warn-mode"}),
        ));

        let lines = read_lines(audit.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "plugin_install");
        assert_eq!(lines[0]["actor"], "user");
        assert_eq!(lines[0]["details"]["signature_status"], "verified");
        assert_eq!(lines[1]["event"], "signature_skipped");
        assert_eq!(lines[1]["actor"], "daemon");
    }

    #[test]
    fn audit_log_rotates_at_threshold() {
        let dir = tempdir().unwrap();
        let path = audit_log_path(dir.path());
        // Tiny threshold so a couple of writes trip rotation.
        let audit = Audit::at_path(path.clone()).with_max_bytes(200);
        for i in 0..10 {
            audit.log_event(AuditEvent::new(
                AuditActor::User,
                AuditEventKind::PluginInstall,
                json!({"i": i, "padding": "x".repeat(100)}),
            ));
        }
        // The active file exists.
        assert!(path.exists());
        // At least one rotated file exists too.
        let rotated = dir.path().join("audit.1.jsonl");
        assert!(
            rotated.exists(),
            "expected rotation to audit.1.jsonl, dir contents: {:?}",
            fs::read_dir(dir.path()).unwrap().collect::<Vec<_>>()
        );
    }

    #[test]
    fn log_event_does_not_panic_on_unwritable_path() {
        // A path under a nonexistent root that we can still try to create.
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        let audit = Audit::at_path(nested.join("audit.jsonl"));
        audit.log_event(AuditEvent::new(AuditActor::User, AuditEventKind::PolicyOverride, json!({"flag": "--force"})));
        // The nested directory should have been created and the line written.
        let lines = read_lines(audit.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["event"], "policy_override");
    }

    #[test]
    fn next_rotation_path_skips_existing_slots() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("audit.jsonl");
        fs::write(&base, "").unwrap();
        fs::write(dir.path().join("audit.1.jsonl"), "").unwrap();
        fs::write(dir.path().join("audit.2.jsonl"), "").unwrap();
        let next = next_rotation_path(&base);
        assert_eq!(next.file_name().and_then(|s| s.to_str()), Some("audit.3.jsonl"));
    }

    #[test]
    fn event_kinds_have_stable_string_labels() {
        assert_eq!(AuditEventKind::PluginInstall.as_str(), "plugin_install");
        assert_eq!(AuditEventKind::PluginUninstall.as_str(), "plugin_uninstall");
        assert_eq!(AuditEventKind::PluginUpgrade.as_str(), "plugin_upgrade");
        assert_eq!(AuditEventKind::SignatureSkipped.as_str(), "signature_skipped");
        assert_eq!(AuditEventKind::SignatureInvalid.as_str(), "signature_invalid");
        assert_eq!(AuditEventKind::TrustKeyAdded.as_str(), "trust_key_added");
        assert_eq!(AuditEventKind::TrustPublisherAdded.as_str(), "trust_publisher_added");
        assert_eq!(AuditEventKind::TrustOrgRevoked.as_str(), "trust_org_revoked");
        assert_eq!(AuditEventKind::PolicyOverride.as_str(), "policy_override");
        assert_eq!(AuditEventKind::LockfileMismatch.as_str(), "lockfile_mismatch");
    }
}
