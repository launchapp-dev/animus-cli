//! Stdio hosting, discovery, and routing for Animus-compatible plugins.

mod discovery;
mod host;
pub mod lockfile;
mod registry;
pub mod session;
pub mod signature_verifier;
pub mod status;
mod subject_router;
mod transport;

pub use discovery::{
    discover_by_kind, discover_plugins, legacy_plugins_registry_path, plugin_install_dir, plugins_registry_path,
    registered_skip_manifest_check_at_install, DiscoveredPlugin, DiscoverySource, DiscoveryWarning, PluginConfigEntry,
    PluginDiscovery,
};
pub use host::{
    check_protocol_compat, install_process_slot_factory, BoxedProcessSlotGuard, HostError, PluginHost, PluginHostInner,
    PluginNotificationRx, PluginSpawnOptions, PluginStderrSink, ProcessSlotError, ProcessSlotFactory, ProcessSlotGuard,
    DEFAULT_NOTIFICATION_BROADCAST_CAPACITY, NOTIFICATION_BROADCAST_CAPACITY_ENV, PLUGIN_BASE_ENV_ALLOWLIST,
    TRANSPORT_METHOD_SHUTDOWN, TRANSPORT_METHOD_START,
};
#[cfg(any(test, feature = "test-support"))]
pub use host::{clear_process_slot_factory_for_test, install_process_slot_factory_for_test};
pub use lockfile::{
    global_lockfile_path, sha256_of_file, LockEntry, LockVerifyResult, PluginLockfile, LOCKFILE_SCHEMA_VERSION,
};
pub use registry::PluginRegistry;
pub use signature_verifier::{
    cosign_available, verify_plugin_binary_keyless, verify_plugin_install, PolicyMode, SignaturePolicy,
    TrustedPublisher, VerificationResult, GITHUB_OIDC_ISSUER,
};
pub use status::{
    global_status_registry, install_global_status_registry, PluginLastError, PluginRuntimeState, PluginRuntimeStatus,
    PluginStatusRegistry, PluginStatusResponse, StatusRegistryObserver, PLUGIN_STATUS_PROTOCOL_VERSION,
};
pub use subject_router::{KindAliasMap, SubjectRouter};
pub use transport::StdioTransport;
