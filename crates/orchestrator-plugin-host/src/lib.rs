//! Stdio hosting, discovery, and routing for Animus-compatible plugins.

mod discovery;
mod host;
pub mod lockfile;
pub mod manifest_cache;
mod registry;
pub mod scope;
pub mod session;
pub mod signature_verifier;
pub mod status;
mod subject_router;
mod transport;

pub use discovery::{
    discover_by_kind, discover_plugins, is_scanned_plugin_name, legacy_plugins_registry_path, plugin_install_dir,
    plugins_registry_path, project_plugin_install_dir, project_plugins_registry_path,
    registered_skip_manifest_check_at_install, registered_skip_manifest_check_at_install_scoped,
    resolve_configured_binary, DiscoveredPlugin, DiscoverySource, DiscoveryWarning, PluginConfigEntry, PluginDiscovery,
};
pub use host::{
    check_protocol_compat, current_secret_snapshot_provider, install_process_slot_factory,
    install_secret_snapshot_provider, BoxedProcessSlotGuard, HostError, PluginHost, PluginHostInner,
    PluginNotificationRx, PluginSpawnOptions, PluginStderrSink, ProcessSlotError, ProcessSlotFactory, ProcessSlotGuard,
    SecretSnapshotProvider, DEFAULT_NOTIFICATION_BROADCAST_CAPACITY, MAX_INJECTED_SECRET_BYTES,
    NOTIFICATION_BROADCAST_CAPACITY_ENV, PLUGIN_BASE_ENV_ALLOWLIST, TRANSPORT_METHOD_SHUTDOWN, TRANSPORT_METHOD_START,
};
#[cfg(any(test, feature = "test-support"))]
pub use host::{
    clear_process_slot_factory_for_test, clear_secret_snapshot_provider_for_test,
    install_process_slot_factory_for_test, install_secret_snapshot_provider_for_test,
};
pub use lockfile::{
    current_target_triple, global_lockfile_path, project_lockfile_path, sha256_of_file, LockEntry, LockVerifyResult,
    PluginLockfile, TargetIntegrity, LOCKFILE_SCHEMA_VERSION,
};
pub use manifest_cache::{CachedEntry, ManifestCache};

/// Crate-wide mutex any test that mutates process-global env vars
/// (notably `ANIMUS_*` env vars consulted by discovery + the manifest
/// cache) MUST hold while running. Cargo runs unit tests on multiple
/// threads in the same process, so without this shared lock the
/// `discovery` and `manifest_cache` modules race each other across the
/// kill-switch env var and produce flaky CI runs. Codex round 4 P2.
#[cfg(test)]
pub(crate) static TEST_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Crate-wide mutex any test that installs a process-global plugin-process
/// slot factory (or spawns real plugin children that consume slots from one)
/// MUST hold while running. The slot factory is process-global, so the
/// `host` cap tests and the `subject_router` lazy-spawn tests would otherwise
/// race: a cap-of-2 factory installed by one test starves the real spawns the
/// other performs. Serializing on this single lock keeps both deterministic.
#[cfg(test)]
pub(crate) static TEST_SLOT_FACTORY_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub use registry::PluginRegistry;
pub use scope::{read_active_flavor, PluginScope, PluginScopeMode, PLUGIN_SCOPE_FILE, PLUGIN_SCOPE_SCHEMA_V1};
pub use signature_verifier::{
    cosign_available, verify_plugin_binary_keyless, verify_plugin_install, PolicyMode, SignaturePolicy,
    TrustedPublisher, VerificationResult, GITHUB_OIDC_ISSUER,
};
pub use status::{
    global_status_registry, install_global_status_registry, PluginLastError, PluginRuntimeState, PluginRuntimeStatus,
    PluginStatusRegistry, PluginStatusResponse, StatusRegistryObserver, PLUGIN_STATUS_PROTOCOL_VERSION,
};
pub use subject_router::{KindAliasMap, SubjectPluginSpec, SubjectRouter, SubjectWatchGuard, SubjectWatchSubscription};
pub use transport::StdioTransport;
