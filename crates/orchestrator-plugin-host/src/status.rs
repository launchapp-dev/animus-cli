//! Per-plugin runtime status tracking shared by the daemon and CLI.
//!
//! The daemon constructs a single [`PluginStatusRegistry`] at startup and hands
//! it to every supervised plugin surface (provider session backends, subject
//! router, transport spawner). Each backend reports lifecycle transitions
//! (spawn / exit / restart) and per-RPC liveness so operators can answer the
//! "why does it feel like the agent is stuck?" diagnostic at a glance through
//! `animus plugin status`.
//!
//! Design notes:
//!
//! - The registry never blocks on a `.await` while holding its inner lock.
//!   All operations are O(1) HashMap mutations behind a sync `RwLock`.
//! - Status entries are keyed by the discovered plugin name (the value of
//!   `plugin.name` from `discover_plugins`), which the lockfile and CLI both
//!   surface; this lets the CLI merge discovery rows with live status rows
//!   without a join key indirection.
//! - The registry exposes itself as a [`DispatchObserver`] so wiring it
//!   through `PluginSessionBackend::with_dispatch_observer` automatically
//!   bumps `last_rpc_at` on every successful round-trip.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::session::DispatchObserver;

/// Wire-stable shape returned by the daemon's `plugin/status` control RPC.
///
/// One row per known plugin name. Fields are intentionally serializable and
/// human-readable so the CLI can render either a pretty table or JSON without
/// touching the underlying registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRuntimeStatus {
    pub name: String,
    pub kind: String,
    pub state: PluginRuntimeState,
    pub pid: Option<u32>,
    pub last_rpc_at: Option<DateTime<Utc>>,
    pub last_error: Option<PluginLastError>,
    pub restart_count: u32,
    pub binary_path: Option<String>,
    pub manifest_name: Option<String>,
    /// v0.5.10: `true` while the plugin supervisor has disabled this plugin
    /// after exhausting its restart budget (3 restarts / 60s by default).
    /// Auto-clears once `cooldown_until` passes, matching the supervisor's
    /// own re-enable behavior. Additive serde-default field: payloads from
    /// older daemons deserialize with `false`.
    #[serde(default)]
    pub disabled_by_supervisor: bool,
    /// v0.5.10: when the supervisor cooldown elapses and the plugin becomes
    /// eligible to spawn again. `None` unless `disabled_by_supervisor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntimeState {
    /// Discovered on disk but never spawned in the current daemon session.
    Discovered,
    /// Currently running with a live child process.
    Running,
    /// Spawned previously but the child process has exited.
    Stopped,
    /// Has been restarted at least once (i.e. the supervisor recorded a
    /// retry). The pid + last_rpc_at reflect the latest attempt.
    Restarting,
    /// Discovered manifest entry whose binary could not be located.
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginLastError {
    pub code: String,
    pub message: String,
    pub at: DateTime<Utc>,
}

/// Wire envelope for the `plugin/status` RPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginStatusResponse {
    /// Schema version for the response shape. Bump on breaking changes so
    /// CLIs older than the daemon can refuse cleanly instead of mis-parsing.
    pub protocol_version: u32,
    pub plugins: Vec<PluginRuntimeStatus>,
}

/// Current schema version for [`PluginStatusResponse`]. Increment when the
/// shape changes in a way that older clients cannot parse safely.
pub const PLUGIN_STATUS_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone)]
struct StatusEntry {
    name: String,
    kind: String,
    state: PluginRuntimeState,
    pid: Option<u32>,
    last_rpc: Option<Instant>,
    last_rpc_at: Option<DateTime<Utc>>,
    last_error: Option<PluginLastError>,
    restart_count: u32,
    binary_path: Option<String>,
    manifest_name: Option<String>,
    cooldown_until: Option<DateTime<Utc>>,
}

impl StatusEntry {
    fn to_wire(&self) -> PluginRuntimeStatus {
        // The supervisor silently re-enables a plugin once its cooldown
        // elapses (PluginSupervisor::is_disabled), so the wire view derives
        // "still disabled" from the deadline instead of trusting a stale
        // boolean.
        let disabled_by_supervisor = self.cooldown_until.is_some_and(|deadline| deadline > Utc::now());
        PluginRuntimeStatus {
            name: self.name.clone(),
            kind: self.kind.clone(),
            state: self.state,
            pid: self.pid,
            last_rpc_at: self.last_rpc_at,
            last_error: self.last_error.clone(),
            restart_count: self.restart_count,
            binary_path: self.binary_path.clone(),
            manifest_name: self.manifest_name.clone(),
            disabled_by_supervisor,
            cooldown_until: disabled_by_supervisor.then_some(self.cooldown_until).flatten(),
        }
    }
}

/// Process-wide registry of per-plugin runtime status.
///
/// Construct once at daemon startup, wrap in [`Arc`], hand a clone to every
/// plugin surface that wants to report lifecycle. Cheap to clone (one `Arc`)
/// and safe to share across tasks.
#[derive(Debug, Default)]
pub struct PluginStatusRegistry {
    inner: RwLock<HashMap<String, StatusEntry>>,
}

impl PluginStatusRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register or refresh a discovered plugin entry. Idempotent.
    pub fn record_discovered(
        &self,
        name: &str,
        kind: &str,
        binary_path: Option<String>,
        manifest_name: Option<String>,
    ) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        let entry = guard.entry(name.to_string()).or_insert_with(|| StatusEntry {
            name: name.to_string(),
            kind: kind.to_string(),
            state: PluginRuntimeState::Discovered,
            pid: None,
            last_rpc: None,
            last_rpc_at: None,
            last_error: None,
            restart_count: 0,
            binary_path: binary_path.clone(),
            manifest_name: manifest_name.clone(),
            cooldown_until: None,
        });
        entry.kind = kind.to_string();
        if entry.binary_path.is_none() {
            entry.binary_path = binary_path;
        }
        if entry.manifest_name.is_none() {
            entry.manifest_name = manifest_name;
        }
    }

    /// Mark the plugin as missing on disk (manifest references a binary that
    /// cannot be located).
    pub fn record_missing(&self, name: &str, kind: &str, manifest_name: Option<String>) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        let entry = guard.entry(name.to_string()).or_insert_with(|| StatusEntry {
            name: name.to_string(),
            kind: kind.to_string(),
            state: PluginRuntimeState::Missing,
            pid: None,
            last_rpc: None,
            last_rpc_at: None,
            last_error: None,
            restart_count: 0,
            binary_path: None,
            manifest_name: manifest_name.clone(),
            cooldown_until: None,
        });
        entry.kind = kind.to_string();
        entry.state = PluginRuntimeState::Missing;
        entry.pid = None;
        if entry.manifest_name.is_none() {
            entry.manifest_name = manifest_name;
        }
    }

    /// Record that the named plugin was spawned with `pid`.
    pub fn record_spawn(&self, name: &str, pid: Option<u32>) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        let entry = guard.entry(name.to_string()).or_insert_with(|| StatusEntry {
            name: name.to_string(),
            kind: String::new(),
            state: PluginRuntimeState::Running,
            pid,
            last_rpc: None,
            last_rpc_at: None,
            last_error: None,
            restart_count: 0,
            binary_path: None,
            manifest_name: None,
            cooldown_until: None,
        });
        entry.pid = pid;
        entry.state = PluginRuntimeState::Running;
        // A fresh spawn means the supervisor allowed the plugin to run
        // again, so any previous disable window is over.
        entry.cooldown_until = None;
    }

    /// Record that the supervisor disabled the plugin after exhausting its
    /// restart budget. `cooldown` is how long the supervisor will refuse to
    /// respawn it (`SupervisorConfig::disable_cooldown`).
    pub fn record_supervisor_disabled(&self, name: &str, cooldown: Duration) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        if let Some(entry) = guard.get_mut(name) {
            entry.state = PluginRuntimeState::Stopped;
            entry.pid = None;
            entry.cooldown_until =
                Some(Utc::now() + chrono::Duration::from_std(cooldown).unwrap_or(chrono::Duration::zero()));
        }
    }

    /// Record an exit / connection-lost event. Does NOT bump
    /// `restart_count`; that counter is reserved for supervisor-driven
    /// restart attempts (see [`Self::record_restart`]). The normal path
    /// through `graceful_shutdown` should call this with `error=None` after a
    /// successful dispatch so the entry state flips to Stopped without
    /// polluting the restart counter that operators rely on to spot flapping
    /// plugins.
    pub fn record_exit(&self, name: &str, error: Option<(String, String)>) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        let entry = guard.entry(name.to_string()).or_insert_with(|| StatusEntry {
            name: name.to_string(),
            kind: String::new(),
            state: PluginRuntimeState::Stopped,
            pid: None,
            last_rpc: None,
            last_rpc_at: None,
            last_error: None,
            restart_count: 0,
            binary_path: None,
            manifest_name: None,
            cooldown_until: None,
        });
        entry.state = PluginRuntimeState::Stopped;
        entry.pid = None;
        if let Some((code, message)) = error {
            entry.last_error = Some(PluginLastError { code, message, at: Utc::now() });
        }
    }

    /// Mark the plugin as in the middle of a restart loop. Distinct from
    /// `record_exit` so the CLI can render "restarting" vs "stopped" when the
    /// supervisor is actively attempting a respawn.
    pub fn record_restart(&self, name: &str) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        if let Some(entry) = guard.get_mut(name) {
            entry.restart_count = entry.restart_count.saturating_add(1);
            entry.state = PluginRuntimeState::Restarting;
        }
    }

    /// Bump `last_rpc_at` for the named plugin. Called after every successful
    /// RPC round-trip.
    pub fn record_rpc(&self, name: &str) {
        let mut guard = self.inner.write().expect("plugin status registry poisoned");
        if let Some(entry) = guard.get_mut(name) {
            entry.last_rpc = Some(Instant::now());
            entry.last_rpc_at = Some(Utc::now());
            if matches!(entry.state, PluginRuntimeState::Discovered | PluginRuntimeState::Stopped) {
                entry.state = PluginRuntimeState::Running;
            }
        }
    }

    /// Snapshot every known plugin entry in alphabetical order.
    pub fn snapshot(&self) -> Vec<PluginRuntimeStatus> {
        let guard = self.inner.read().expect("plugin status registry poisoned");
        let mut rows: Vec<PluginRuntimeStatus> = guard.values().map(StatusEntry::to_wire).collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Return one entry by exact name match.
    pub fn get(&self, name: &str) -> Option<PluginRuntimeStatus> {
        let guard = self.inner.read().expect("plugin status registry poisoned");
        guard.get(name).map(StatusEntry::to_wire)
    }

    /// Test helper: how many entries are currently tracked.
    pub fn len(&self) -> usize {
        self.inner.read().expect("plugin status registry poisoned").len()
    }

    /// Test helper: convenience for `len() == 0`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// [`DispatchObserver`] implementation that ticks `last_rpc_at` on every
/// observed round-trip. Wrap the registry in this and hand it to
/// `PluginSessionBackend::with_dispatch_observer` to wire RPC liveness.
#[derive(Debug, Clone)]
pub struct StatusRegistryObserver {
    registry: Arc<PluginStatusRegistry>,
}

impl StatusRegistryObserver {
    pub fn new(registry: Arc<PluginStatusRegistry>) -> Arc<Self> {
        Arc::new(Self { registry })
    }
}

impl DispatchObserver for StatusRegistryObserver {
    fn observe_duration(&self, plugin: &str, _method: &str, _elapsed: Duration) {
        self.registry.record_rpc(plugin);
    }
}

/// Process-global slot holding the active [`PluginStatusRegistry`].
///
/// Set at daemon startup; read by [`PluginSessionBackend::new`] (via
/// [`global_status_registry`]) so newly-constructed backends automatically
/// see the same registry without threading a parameter through every
/// resolver construction site. Stored behind an `RwLock` rather than an
/// `OnceLock` so daemon lifecycle tests (and embedded restarts) can swap in
/// a fresh registry per run without leaving the previous Arc connected to
/// new provider backends.
static GLOBAL_STATUS_REGISTRY: RwLock<Option<Arc<PluginStatusRegistry>>> = RwLock::new(None);

/// Install the process-global plugin status registry. Replaces any previous
/// registry so the slot always reflects the most recently started daemon.
pub fn install_global_status_registry(registry: Arc<PluginStatusRegistry>) {
    let mut guard = GLOBAL_STATUS_REGISTRY.write().expect("global plugin status registry poisoned");
    *guard = Some(registry);
}

/// Return the process-global plugin status registry if one has been
/// installed (e.g. by the daemon at startup). CLI processes that do not
/// install one get `None` here and fall back to per-call discovery only.
pub fn global_status_registry() -> Option<Arc<PluginStatusRegistry>> {
    GLOBAL_STATUS_REGISTRY.read().expect("global plugin status registry poisoned").clone()
}

/// Test helper: clear the global status registry. Lets unit tests run in
/// isolation without polluting each other through the process-global slot.
#[doc(hidden)]
pub fn clear_global_status_registry_for_test() {
    let mut guard = GLOBAL_STATUS_REGISTRY.write().expect("global plugin status registry poisoned");
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_entries_appear_in_snapshot() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered(
            "animus-subject-default",
            "task",
            Some("/bin/foo".into()),
            Some("subject-default".into()),
        );
        let rows = reg.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, PluginRuntimeState::Discovered);
        assert_eq!(rows[0].binary_path.as_deref(), Some("/bin/foo"));
    }

    #[test]
    fn spawn_then_rpc_promotes_state_to_running_and_records_pid() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("animus-provider-claude", "claude", None, None);
        reg.record_spawn("animus-provider-claude", Some(4242));
        reg.record_rpc("animus-provider-claude");
        let row = reg.get("animus-provider-claude").expect("entry");
        assert_eq!(row.state, PluginRuntimeState::Running);
        assert_eq!(row.pid, Some(4242));
        assert!(row.last_rpc_at.is_some(), "last_rpc_at must be set after record_rpc");
    }

    #[test]
    fn exit_preserves_restart_counter_and_stores_last_error() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("animus-queue-default", "queue", None, None);
        reg.record_spawn("animus-queue-default", Some(1));
        reg.record_exit("animus-queue-default", Some(("ConnectionLost".into(), "broken pipe".into())));
        let row = reg.get("animus-queue-default").expect("entry");
        assert_eq!(row.state, PluginRuntimeState::Stopped);
        assert_eq!(row.restart_count, 0, "record_exit must not bump restart_count; restarts come from record_restart");
        assert_eq!(row.pid, None);
        let err = row.last_error.expect("last_error captured");
        assert_eq!(err.code, "ConnectionLost");
    }

    #[test]
    fn restart_bumps_counter_and_flips_state() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("animus-queue-default", "queue", None, None);
        reg.record_spawn("animus-queue-default", Some(1));
        reg.record_restart("animus-queue-default");
        reg.record_restart("animus-queue-default");
        let row = reg.get("animus-queue-default").expect("entry");
        assert_eq!(row.restart_count, 2);
        assert_eq!(row.state, PluginRuntimeState::Restarting);
    }

    #[test]
    fn missing_marks_state_and_preserves_manifest_name() {
        let reg = PluginStatusRegistry::new();
        reg.record_missing("animus-trigger-webhook", "trigger", Some("webhook".into()));
        let row = reg.get("animus-trigger-webhook").expect("entry");
        assert_eq!(row.state, PluginRuntimeState::Missing);
        assert_eq!(row.manifest_name.as_deref(), Some("webhook"));
    }

    #[test]
    fn supervisor_disable_sets_flag_and_cooldown_then_spawn_clears_it() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("animus-subject-default", "task", None, None);
        reg.record_spawn("animus-subject-default", Some(7));
        reg.record_supervisor_disabled("animus-subject-default", Duration::from_mins(5));
        let row = reg.get("animus-subject-default").expect("entry");
        assert!(row.disabled_by_supervisor);
        assert!(row.cooldown_until.is_some(), "cooldown_until must accompany disabled_by_supervisor");
        assert_eq!(row.state, PluginRuntimeState::Stopped);
        assert_eq!(row.pid, None);

        reg.record_spawn("animus-subject-default", Some(8));
        let row = reg.get("animus-subject-default").expect("entry");
        assert!(!row.disabled_by_supervisor, "fresh spawn clears the disable window");
        assert!(row.cooldown_until.is_none());
    }

    #[test]
    fn supervisor_disable_auto_clears_after_cooldown_elapses() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("flappy", "queue", None, None);
        reg.record_supervisor_disabled("flappy", Duration::ZERO);
        let row = reg.get("flappy").expect("entry");
        assert!(!row.disabled_by_supervisor, "elapsed cooldown must read as re-enabled");
        assert!(row.cooldown_until.is_none(), "cooldown_until is withheld once the window passed");
    }

    #[test]
    fn plugin_runtime_status_deserializes_payloads_without_supervisor_fields() {
        // Wire back-compat: rows emitted by pre-v0.5.10 daemons carry no
        // supervisor fields and must default to "not disabled".
        let row: PluginRuntimeStatus = serde_json::from_str(
            r#"{"name":"p","kind":"task","state":"running","pid":null,"last_rpc_at":null,"last_error":null,"restart_count":1,"binary_path":null,"manifest_name":null}"#,
        )
        .expect("old payload deserializes");
        assert!(!row.disabled_by_supervisor);
        assert!(row.cooldown_until.is_none());
    }

    #[test]
    fn snapshot_is_sorted_by_name() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("zeta", "trigger", None, None);
        reg.record_discovered("alpha", "task", None, None);
        reg.record_discovered("middle", "queue", None, None);
        let rows = reg.snapshot();
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["alpha", "middle", "zeta"]);
    }

    #[test]
    fn observer_routes_rpc_through_registry() {
        let reg = PluginStatusRegistry::new();
        reg.record_discovered("foo", "task", None, None);
        reg.record_spawn("foo", Some(1));
        let observer = StatusRegistryObserver::new(reg.clone());
        observer.observe_duration("foo", "task/list", Duration::from_millis(5));
        let row = reg.get("foo").expect("entry");
        assert!(row.last_rpc_at.is_some());
    }
}
