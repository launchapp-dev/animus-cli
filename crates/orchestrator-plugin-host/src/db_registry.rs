//! DB-registry discovery source: the desired plugin set read from the
//! Postgres `plugin_registry` table served by the animus-postgres BaaS.
//!
//! # Bootstrap paradox
//!
//! The plugin that reads the registry is itself the DB-backend plugin, so it
//! cannot be gated on the registry it serves. The DB tier is therefore
//! OPT-IN: the daemon wires a [`PluginRegistrySource`] into
//! [`crate::PluginDiscovery`] only AFTER the bootstrap DB-backend plugin has
//! completed its handshake (kernel + animus-postgres + `DATABASE_URL` arrive
//! from the thin-image bootstrap tier). Until a source is wired, discovery
//! runs the file/dir tiers alone and the DB tier is a no-op — so a fresh boot
//! never deadlocks waiting on a registry that isn't reachable yet.
//!
//! # Seam
//!
//! `orchestrator-plugin-host` takes NO Postgres dependency. The actual SQL
//! (and the exact `plugin_registry` column types, still being finalized on the
//! BaaS side) live behind [`PluginRegistrySource`] in a daemon-side adapter.
//! This module only defines the stable in-kernel contract the discovery tier
//! consumes plus a [`StaticRegistrySource`] used by tests and as the pre-schema
//! stub adapter.

use anyhow::Result;

/// One desired-plugin row from the Postgres `plugin_registry` table.
///
/// Mirrors the BaaS schema `{name, version, source, sha256, target, enabled,
/// scope}`. Resolution keys off `name` (resolved against the binary present on
/// the volume) and `enabled`; the remaining fields carry provenance/integrity
/// metadata the adapter and any future verification pass can act on without a
/// second registry read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbRegistryEntry {
    /// Canonical plugin name, matching the installed binary file name in the
    /// plugin install dir (e.g. `animus-subject-default`, `animus-postgres`).
    pub name: String,
    /// Pinned release version, when the registry records one.
    pub version: Option<String>,
    /// Install source descriptor (e.g. a `git+tag` release coordinate).
    pub source: Option<String>,
    /// Expected sha256 of the installed artifact, when recorded.
    pub sha256: Option<String>,
    /// Target triple the row pins, or `noarch`/`any` for a platform-independent
    /// bundle. `None` means "whatever is installed on the volume".
    pub target: Option<String>,
    /// Whether the plugin should be loaded. Disabled rows are skipped by the
    /// discovery tier.
    pub enabled: bool,
    /// Optional plugin scope tag (flavor / project scope) carried through from
    /// the registry row. Advisory; the scope FILTER still applies downstream.
    pub scope: Option<String>,
}

impl DbRegistryEntry {
    /// Construct an enabled entry with only a name set — the common shape for
    /// tests and for a minimal registry row.
    pub fn enabled(name: impl Into<String>) -> Self {
        Self { name: name.into(), version: None, source: None, sha256: None, target: None, enabled: true, scope: None }
    }
}

/// Read side of the DB-backed plugin registry.
///
/// Implemented by a daemon-side adapter that queries the Postgres
/// `plugin_registry` through the already-running DB-backend plugin. Kept as a
/// trait so the SQL/schema can evolve behind this seam without touching
/// discovery, and so `orchestrator-plugin-host` stays Postgres-free.
pub trait PluginRegistrySource: Send + Sync {
    /// Return the desired plugin set. An error propagates to the discovery
    /// caller as a [`crate::DiscoveryWarning`]: the DB tier degrades to a
    /// no-op rather than sinking the whole discovery run.
    fn desired_plugins(&self) -> Result<Vec<DbRegistryEntry>>;
}

/// In-memory [`PluginRegistrySource`] backing tests and the pre-schema stub
/// adapter. Holds a fixed row set (or a canned error) so the discovery-tier
/// wiring can be exercised without a live Postgres.
#[derive(Debug, Clone)]
pub struct StaticRegistrySource {
    result: std::result::Result<Vec<DbRegistryEntry>, String>,
}

impl StaticRegistrySource {
    /// A source that returns `entries` on every read.
    pub fn new(entries: Vec<DbRegistryEntry>) -> Self {
        Self { result: Ok(entries) }
    }

    /// A source whose read always fails with `message` — models the DB being
    /// unreachable so the graceful-degrade path can be tested.
    pub fn failing(message: impl Into<String>) -> Self {
        Self { result: Err(message.into()) }
    }
}

impl PluginRegistrySource for StaticRegistrySource {
    fn desired_plugins(&self) -> Result<Vec<DbRegistryEntry>> {
        self.result.clone().map_err(|message| anyhow::anyhow!(message))
    }
}
