//! Runtime quotas — per-process caps the daemon enforces to keep itself
//! within a predictable memory / file-descriptor / CPU envelope.
//!
//! Four caps are tracked, all configurable via environment variable. The
//! defaults are picked to give a single-developer workstation enough headroom
//! to drive a healthy queue without letting a misbehaving plugin or runaway
//! event source consume unbounded resources:
//!
//! | Quota | Default | Env override | Enforcement |
//! |-------|---------|--------------|-------------|
//! | Trigger backlog | 1000 events | `ANIMUS_TRIGGER_BACKLOG_MAX` | drop oldest with `tracing::warn!` |
//! | Subscriber buffer memory | 10 MB / subscriber | `ANIMUS_SUBSCRIBER_MEMORY_MAX_MB` | terminate with `subscription/closed` reason=`buffer_full_lagged` |
//! | Plugin process count | 50 concurrent | `ANIMUS_PLUGIN_PROCESS_MAX` | refuse new spawn, return error |
//! | Workflow concurrency | 10 concurrent | `ANIMUS_WORKFLOW_CONCURRENCY_MAX` | queue the new request |
//!
//! The struct is read once at daemon startup; env-overrides apply at that
//! point. Tests construct one explicitly via the field literals.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Trigger backlog cap — maximum number of unprocessed `WebhookEvent`s
/// retained per trigger before the oldest are dropped.
pub const DEFAULT_TRIGGER_BACKLOG_MAX: usize = 1000;

/// Notification subscriber memory cap — approximate cap on the in-memory
/// queue each `workflow/events` subscriber may accumulate before it is
/// terminated. Expressed in megabytes; the broadcaster translates this into
/// a slot count given a representative event size.
pub const DEFAULT_SUBSCRIBER_MEMORY_MAX_MB: usize = 10;

/// Plugin process count cap — maximum number of concurrently-spawned plugin
/// child processes the daemon will hold open. Beyond this, new spawn
/// requests are refused with an error.
pub const DEFAULT_PLUGIN_PROCESS_MAX: usize = 50;

/// Workflow concurrency cap — maximum number of workflow runner subprocesses
/// the daemon will dispatch in parallel. Excess requests sit in the
/// dispatch queue until headroom appears.
pub const DEFAULT_WORKFLOW_CONCURRENCY_MAX: usize = 10;

const ENV_TRIGGER_BACKLOG_MAX: &str = "ANIMUS_TRIGGER_BACKLOG_MAX";
const ENV_SUBSCRIBER_MEMORY_MAX_MB: &str = "ANIMUS_SUBSCRIBER_MEMORY_MAX_MB";
const ENV_PLUGIN_PROCESS_MAX: &str = "ANIMUS_PLUGIN_PROCESS_MAX";
const ENV_WORKFLOW_CONCURRENCY_MAX: &str = "ANIMUS_WORKFLOW_CONCURRENCY_MAX";

/// Effective runtime quotas resolved at daemon startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeQuotas {
    pub trigger_backlog_max: usize,
    pub subscriber_memory_max_mb: usize,
    pub plugin_process_max: usize,
    pub workflow_concurrency_max: usize,
}

impl Default for RuntimeQuotas {
    fn default() -> Self {
        Self {
            trigger_backlog_max: DEFAULT_TRIGGER_BACKLOG_MAX,
            subscriber_memory_max_mb: DEFAULT_SUBSCRIBER_MEMORY_MAX_MB,
            plugin_process_max: DEFAULT_PLUGIN_PROCESS_MAX,
            workflow_concurrency_max: DEFAULT_WORKFLOW_CONCURRENCY_MAX,
        }
    }
}

impl RuntimeQuotas {
    /// Read quotas from the process environment, falling back to defaults
    /// for any var that is unset, empty, or non-numeric. A `0` value is
    /// rejected (treated as default) so an accidental empty override cannot
    /// disable a cap completely.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            trigger_backlog_max: parse_usize_env(ENV_TRIGGER_BACKLOG_MAX, defaults.trigger_backlog_max),
            subscriber_memory_max_mb: parse_usize_env(ENV_SUBSCRIBER_MEMORY_MAX_MB, defaults.subscriber_memory_max_mb),
            plugin_process_max: parse_usize_env(ENV_PLUGIN_PROCESS_MAX, defaults.plugin_process_max),
            workflow_concurrency_max: parse_usize_env(ENV_WORKFLOW_CONCURRENCY_MAX, defaults.workflow_concurrency_max),
        }
    }
}

fn parse_usize_env(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(value) => match value.trim().parse::<usize>() {
            Ok(0) => default,
            Ok(n) => n,
            Err(_) => default,
        },
        Err(_) => default,
    }
}

/// Process-wide handle to the resolved quotas. Read once on first access,
/// then frozen for the lifetime of the process. Set explicitly via
/// [`install_runtime_quotas`] from the daemon startup path; tests can
/// override via that same setter before any subsystem reads it.
static GLOBAL_QUOTAS: OnceLock<RuntimeQuotas> = OnceLock::new();

/// Install the process-wide quotas. Returns `true` if this call won the
/// race, `false` if some prior caller already installed quotas (the prior
/// install wins; subsequent calls are silently ignored).
pub fn install_runtime_quotas(quotas: RuntimeQuotas) -> bool {
    GLOBAL_QUOTAS.set(quotas).is_ok()
}

/// Read the process-wide quotas. If [`install_runtime_quotas`] has not been
/// called yet, lazily initializes from the environment.
pub fn runtime_quotas() -> RuntimeQuotas {
    *GLOBAL_QUOTAS.get_or_init(RuntimeQuotas::from_env)
}

/// Process-wide counter of concurrently-live plugin child processes. Wraps
/// callers around their spawn site via [`acquire_plugin_process_slot`].
static LIVE_PLUGIN_PROCESSES: AtomicUsize = AtomicUsize::new(0);

/// RAII guard that decrements [`LIVE_PLUGIN_PROCESSES`] on drop. Held for
/// the lifetime of a spawned plugin process. The constructor
/// [`acquire_plugin_process_slot`] enforces the
/// [`RuntimeQuotas::plugin_process_max`] cap.
#[must_use = "dropping the guard releases the plugin process slot; bind it to a `let` for the spawn's lifetime"]
#[derive(Debug)]
pub struct PluginProcessSlot {
    _private: (),
}

impl Drop for PluginProcessSlot {
    fn drop(&mut self) {
        LIVE_PLUGIN_PROCESSES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Try to claim a plugin process slot. Returns `Err` if the process count
/// is already at [`RuntimeQuotas::plugin_process_max`]. Callers should hold
/// the returned [`PluginProcessSlot`] for the entire spawn lifetime so the
/// slot is released exactly when the child exits.
pub fn acquire_plugin_process_slot() -> Result<PluginProcessSlot, PluginProcessSlotError> {
    let cap = runtime_quotas().plugin_process_max;
    // Optimistic CAS loop: read, check cap, fetch_add. If we lost the
    // race, retry. Bounded by `cap` so worst-case spins are tiny.
    loop {
        let current = LIVE_PLUGIN_PROCESSES.load(Ordering::SeqCst);
        if current >= cap {
            return Err(PluginProcessSlotError { current, cap });
        }
        match LIVE_PLUGIN_PROCESSES.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(PluginProcessSlot { _private: () }),
            Err(_) => continue,
        }
    }
}

/// Number of currently-live plugin processes, as tracked by
/// [`acquire_plugin_process_slot`]. For metrics / tests.
pub fn live_plugin_process_count() -> usize {
    LIVE_PLUGIN_PROCESSES.load(Ordering::SeqCst)
}

/// Reset the process counter to zero. Test-only — production code must
/// never call this; it would invalidate live `PluginProcessSlot` guards.
#[cfg(test)]
pub fn reset_plugin_process_count_for_tests() {
    LIVE_PLUGIN_PROCESSES.store(0, Ordering::SeqCst);
}

/// Boxed `ProcessSlotGuard` implementation that wraps a
/// [`PluginProcessSlot`] so the plugin-host crate can hold the RAII slot
/// without depending on `orchestrator-daemon-runtime`.
#[derive(Debug)]
struct RuntimeQuotaSlotGuard {
    _inner: PluginProcessSlot,
}

impl orchestrator_plugin_host::ProcessSlotGuard for RuntimeQuotaSlotGuard {}

/// `ProcessSlotFactory` implementation backed by the runtime-quota module.
/// Installed by the daemon startup path so the plugin host's spawn site
/// enforces the configured `RuntimeQuotas::plugin_process_max` cap.
#[derive(Debug, Default)]
pub struct RuntimeQuotaSlotFactory;

impl orchestrator_plugin_host::ProcessSlotFactory for RuntimeQuotaSlotFactory {
    fn acquire(
        &self,
    ) -> Result<orchestrator_plugin_host::BoxedProcessSlotGuard, orchestrator_plugin_host::ProcessSlotError> {
        match acquire_plugin_process_slot() {
            Ok(slot) => Ok(Box::new(RuntimeQuotaSlotGuard { _inner: slot })),
            Err(err) => Err(orchestrator_plugin_host::ProcessSlotError {
                current: err.current,
                cap: err.cap,
                message: err.to_string(),
            }),
        }
    }
}

/// Install the runtime-quota-backed `ProcessSlotFactory` into the plugin
/// host. First-installer-wins (the plugin host's installer ignores
/// subsequent calls), so the daemon startup path can call this
/// unconditionally and tests that pre-install a stub still keep theirs.
pub fn install_runtime_quota_process_slot_factory() -> bool {
    orchestrator_plugin_host::install_process_slot_factory(std::sync::Arc::new(RuntimeQuotaSlotFactory))
}

/// Adapter that bridges the orchestrator-core [`KeyringSecretStore`]
/// behind the plugin-host's [`SecretSnapshotProvider`] hook. The daemon
/// (or any embedder that wants secrets surfaced to plugins) installs
/// one of these at startup; the host then merges the live keychain
/// snapshot into every spawned plugin's env (subject to the 1 MiB cap).
pub struct KeychainSecretSnapshotProvider<S: orchestrator_core::SecretStore + 'static> {
    store: S,
}

impl<S: orchestrator_core::SecretStore + 'static> KeychainSecretSnapshotProvider<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: orchestrator_core::SecretStore + 'static> orchestrator_plugin_host::SecretSnapshotProvider
    for KeychainSecretSnapshotProvider<S>
{
    fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
        self.store.snapshot_for_spawn().unwrap_or_default()
    }

    fn snapshot_filtered(&self, requested: &[String]) -> std::collections::BTreeMap<String, String> {
        // The per-scope index is the source of truth: a key that is
        // not in `list_keys()` is not considered stored, even if the
        // OS keychain still has a stale entry. Intersect `requested`
        // with the index before calling `get` so a write that lost
        // its index update is invisible to the spawn path.
        // (codex round-6 P2.)
        let indexed: std::collections::BTreeSet<String> =
            self.store.list_keys().unwrap_or_default().into_iter().collect();
        let mut out = std::collections::BTreeMap::new();
        for key in requested {
            if !indexed.contains(key) {
                continue;
            }
            if let Ok(Some(value)) = self.store.get(key) {
                out.insert(key.clone(), value);
            }
        }
        out
    }
}

/// Install the keychain-backed [`SecretSnapshotProvider`] into the
/// plugin host. First-installer-wins. Safe to call repeatedly.
pub fn install_keychain_secret_provider_for(project_root: &std::path::Path) -> bool {
    let Some(scoped_root) = protocol::repository_scope::scoped_state_root(project_root) else {
        return false;
    };
    let scope = scope_label_for_scoped_root(project_root, &scoped_root);
    let store = orchestrator_core::build_secret_store(&scope, scoped_root);
    orchestrator_plugin_host::install_secret_snapshot_provider(std::sync::Arc::new(
        KeychainSecretSnapshotProvider::new(store),
    ))
}

/// Adapter that bridges a [`orchestrator_core::SecretStore`] behind the
/// workflow-YAML `WorkflowSecretResolver` hook. Lets `${VAR}`
/// interpolation fall back to the keychain when an env var is not set
/// in the daemon's process environment.
pub struct KeychainWorkflowResolver<S: orchestrator_core::SecretStore + 'static> {
    store: S,
}

impl<S: orchestrator_core::SecretStore + 'static> KeychainWorkflowResolver<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: orchestrator_core::SecretStore + 'static> orchestrator_config::WorkflowSecretResolver
    for KeychainWorkflowResolver<S>
{
    fn resolve(&self, key: &str) -> Option<String> {
        // Index is authoritative — a key that's missing from the
        // per-scope index counts as not-stored even if the OS
        // keychain still has a stale entry. (codex round-6 P2.)
        let indexed: std::collections::BTreeSet<String> = self.store.list_keys().ok()?.into_iter().collect();
        if !indexed.contains(key) {
            return None;
        }
        self.store.get(key).ok().flatten()
    }
}

/// Install the keychain-backed `WorkflowSecretResolver` so workflow YAML
/// `${VAR}` falls back to the keychain when the env var is unset.
/// First-installer-wins.
pub fn install_keychain_workflow_resolver_for(project_root: &std::path::Path) -> bool {
    let Some(scoped_root) = protocol::repository_scope::scoped_state_root(project_root) else {
        return false;
    };
    let scope = scope_label_for_scoped_root(project_root, &scoped_root);
    let store = orchestrator_core::build_secret_store(&scope, scoped_root);
    orchestrator_config::install_workflow_secret_resolver(std::sync::Arc::new(KeychainWorkflowResolver::new(store)))
}

/// Prefer the adopted scope directory name when it's present and UTF-8;
/// fall back to a freshly-derived `repo-scope`. Keeps the keychain
/// service name aligned with the index file even when
/// `scoped_state_root` adopted an existing scope by git-origin.
/// (codex round-1 P2.)
fn scope_label_for_scoped_root(project_root: &std::path::Path, scoped_root: &std::path::Path) -> String {
    if let Some(name) = scoped_root.file_name().and_then(|s| s.to_str()) {
        return name.to_string();
    }
    protocol::repository_scope::repository_scope_for_path(project_root)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginProcessSlotError {
    pub current: usize,
    pub cap: usize,
}

impl std::fmt::Display for PluginProcessSlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin process cap reached ({} live, max {}); refusing new spawn", self.current, self.cap)
    }
}

impl std::error::Error for PluginProcessSlotError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that mutate `GLOBAL_QUOTAS` race each other under `cargo test`
    /// because the OnceLock is process-global. The lock here serializes
    /// the (small) set of tests that touch the global; the construction /
    /// parsing tests below use local `RuntimeQuotas` values and never
    /// touch the global.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let prior = std::env::var(key).ok();
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prior.as_deref() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn defaults_match_documented_values() {
        let q = RuntimeQuotas::default();
        assert_eq!(q.trigger_backlog_max, 1000);
        assert_eq!(q.subscriber_memory_max_mb, 10);
        assert_eq!(q.plugin_process_max, 50);
        assert_eq!(q.workflow_concurrency_max, 10);
    }

    #[test]
    fn env_overrides_apply_when_numeric() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _g1 = EnvGuard::set(ENV_TRIGGER_BACKLOG_MAX, Some("2500"));
        let _g2 = EnvGuard::set(ENV_SUBSCRIBER_MEMORY_MAX_MB, Some("32"));
        let _g3 = EnvGuard::set(ENV_PLUGIN_PROCESS_MAX, Some("200"));
        let _g4 = EnvGuard::set(ENV_WORKFLOW_CONCURRENCY_MAX, Some("25"));
        let q = RuntimeQuotas::from_env();
        assert_eq!(q.trigger_backlog_max, 2500);
        assert_eq!(q.subscriber_memory_max_mb, 32);
        assert_eq!(q.plugin_process_max, 200);
        assert_eq!(q.workflow_concurrency_max, 25);
    }

    #[test]
    fn plugin_process_count_refuses_spawn_when_at_cap() {
        // We can't easily isolate the process-wide GLOBAL_QUOTAS without
        // racing other tests in this binary, so we drive the cap by
        // claiming slots up to whatever the current process-wide cap
        // resolves to. The contract under test is that the (cap+1)-th
        // acquire returns an error; that holds regardless of cap value.
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        reset_plugin_process_count_for_tests();

        let cap = runtime_quotas().plugin_process_max;
        let mut held = Vec::new();
        for _ in 0..cap {
            held.push(acquire_plugin_process_slot().expect("slot under cap should be granted"));
        }
        assert_eq!(live_plugin_process_count(), cap);

        let denied = acquire_plugin_process_slot();
        assert!(denied.is_err(), "spawn at cap must be refused");
        let err = denied.unwrap_err();
        assert_eq!(err.cap, cap);
        assert_eq!(err.current, cap);

        // Drop one slot; a fresh acquire should now succeed.
        held.pop();
        let recovered = acquire_plugin_process_slot().expect("slot freed by drop should be reusable");
        assert_eq!(live_plugin_process_count(), cap);
        drop(recovered);
        held.clear();
        assert_eq!(live_plugin_process_count(), 0, "all slots should be released after drop");
    }

    #[test]
    fn runtime_quota_factory_refuses_when_cap_reached() {
        use orchestrator_plugin_host::ProcessSlotFactory as _;

        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        reset_plugin_process_count_for_tests();

        let factory = RuntimeQuotaSlotFactory;
        let cap = runtime_quotas().plugin_process_max;

        let mut held = Vec::with_capacity(cap);
        for _ in 0..cap {
            held.push(factory.acquire().expect("factory should grant slot under cap"));
        }
        assert_eq!(live_plugin_process_count(), cap);

        let denied = factory.acquire();
        assert!(denied.is_err(), "factory must refuse at cap");
        let err = denied.unwrap_err();
        assert_eq!(err.cap, cap);
        assert_eq!(err.current, cap);

        // Drop one boxed guard; counter releases via the inner
        // `PluginProcessSlot::drop` => fresh acquire succeeds.
        held.pop();
        let recovered = factory.acquire().expect("released slot should be reusable");
        drop(recovered);
        held.clear();
        assert_eq!(live_plugin_process_count(), 0);
    }

    #[test]
    fn env_overrides_ignored_when_unparseable() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _g1 = EnvGuard::set(ENV_TRIGGER_BACKLOG_MAX, Some("garbage"));
        let _g2 = EnvGuard::set(ENV_SUBSCRIBER_MEMORY_MAX_MB, Some(""));
        let _g3 = EnvGuard::set(ENV_PLUGIN_PROCESS_MAX, Some("0"));
        let _g4 = EnvGuard::set(ENV_WORKFLOW_CONCURRENCY_MAX, None);
        let q = RuntimeQuotas::from_env();
        assert_eq!(q, RuntimeQuotas::default());
    }
}
