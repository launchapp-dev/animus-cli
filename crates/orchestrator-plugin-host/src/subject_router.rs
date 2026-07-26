use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::{EnvRequirement, RpcError};
use animus_subject_protocol_v2::{
    SubjectCreateRequestV2, SubjectDeleteRequestV2, SubjectFilter, SubjectGetRequestV2, SubjectId,
    SubjectListRequestV2, SubjectRequestContext, SubjectStatusRequestV2, SubjectUpdateRequestV2,
};
use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::host::{PluginNotificationRx, PluginSpawnOptions};
use crate::resident_host_registry::{
    binary_mtime_nanos, global_resident_host_registry, ResidentHostLease, ResidentHostRegistry,
};
use crate::PluginHost;

/// Generous upper bound for a single subject-backend RPC routed through
/// [`SubjectRouter::route_call`]. Subject ops are CRUD against a local
/// store and should complete in milliseconds; the deadline exists so a
/// wedged plugin (alive but not responding) cannot pin a daemon dispatch
/// task forever on the otherwise-untimed request path. Expiry surfaces as
/// an `RpcError` with the protocol's `TIMEOUT` code.
const SUBJECT_ROUTE_TIMEOUT: Duration = Duration::from_mins(2);

/// `subject/watch` JSON-RPC wire method name (animus-subject-protocol
/// `METHOD_SUBJECT_WATCH`). Duplicated here as a literal so the plugin-host
/// crate avoids a dependency on the subject protocol crate.
const SUBJECT_METHOD_WATCH: &str = "subject/watch";

/// `subject/unwatch` JSON-RPC wire method name (animus-subject-protocol
/// `METHOD_SUBJECT_UNWATCH`, v0.1.16+). Spelled as a literal for the same
/// reason as [`SUBJECT_METHOD_WATCH`] — the plugin-host crate stays free of
/// a direct dependency on the subject protocol crate.
const SUBJECT_METHOD_UNWATCH: &str = "subject/unwatch";

/// Subject-kind registration parsed from a plugin's declared
/// `subject_kinds`. A pattern ending in `.*` matches any kind whose dotted
/// prefix matches everything before the trailing `*`.
#[derive(Debug, Clone)]
struct KindPattern {
    /// Raw pattern as declared by the plugin (e.g. `"task"`, `"task.tracked"`,
    /// or `"task.*"`).
    raw: String,
    /// Pattern prefix excluding any trailing `*` (e.g. `"task."` for the glob
    /// `"task.*"`, or the full string for exact matches).
    prefix: String,
    /// Whether the pattern is a glob (`true`) or an exact match (`false`).
    is_glob: bool,
}

impl KindPattern {
    fn parse(raw: &str) -> Self {
        if let Some(stem) = raw.strip_suffix(".*") {
            KindPattern { raw: raw.to_string(), prefix: format!("{stem}."), is_glob: true }
        } else {
            KindPattern { raw: raw.to_string(), prefix: raw.to_string(), is_glob: false }
        }
    }

    fn matches(&self, kind: &str) -> bool {
        if self.is_glob {
            kind.starts_with(&self.prefix) && kind.len() > self.prefix.len()
        } else {
            self.prefix == kind
        }
    }
}

/// Live `subject/watch` subscription handle returned by
/// [`SubjectRouter::start_watch`].
///
/// `notifications` is the plugin host's broadcast receiver; `watch_id` is the
/// JSON-RPC id of the `subject/watch` request, echoed by the runtime in every
/// correlated `subject/changed` notification's `params.id`.
///
/// Dropping the subscription fires a best-effort `subject/unwatch` to the
/// backing plugin (see [`SubjectUnwatchGuard`]) so the plugin can cancel its
/// `backend.watch()` task instead of leaking it until daemon shutdown.
pub struct SubjectWatchSubscription {
    /// Notification receiver for the plugin host backing this watch.
    pub notifications: PluginNotificationRx,
    /// JSON-RPC id allocated for the `subject/watch` request.
    pub watch_id: u64,
    /// Cancel-on-drop guard: sends `subject/unwatch { watch_id }` to the
    /// plugin when the subscription is dropped. Kept private so callers
    /// cannot detach it from the subscription's lifetime by accident.
    unwatch_guard: SubjectUnwatchGuard,
}

impl SubjectWatchSubscription {
    /// Split the subscription into its notification receiver, the watch
    /// request id, and the cancel-on-drop guard.
    ///
    /// Consumers that drive the notification stream by value (e.g. wrapping
    /// `notifications` in a `BroadcastStream`) must keep the returned
    /// [`SubjectWatchGuard`] alive for as long as they want the watch to
    /// stay open — dropping it fires `subject/unwatch` to the plugin. Attach
    /// it to the stream's lifetime so the unwatch fires exactly when the
    /// stream is dropped.
    pub fn into_parts(self) -> (PluginNotificationRx, u64, SubjectWatchGuard) {
        (self.notifications, self.watch_id, SubjectWatchGuard { inner: self.unwatch_guard })
    }
}

/// Opaque cancel-on-drop handle yielded by
/// [`SubjectWatchSubscription::into_parts`]. Holding it keeps the plugin's
/// watch task alive; dropping it fires a best-effort `subject/unwatch`.
pub struct SubjectWatchGuard {
    #[allow(dead_code)]
    inner: SubjectUnwatchGuard,
}

/// RAII guard that emits a best-effort `subject/unwatch` notification when a
/// [`SubjectWatchSubscription`] is dropped.
///
/// The daemon drops a subject watch when the consuming stream ends (client
/// disconnect, scoped subscription teardown, etc.). Before this guard
/// existed, dropping only released the plugin host's broadcast receiver — the
/// plugin kept its `backend.watch()` task alive and emitted discarded
/// notifications until daemon shutdown. The guard closes that leak by telling
/// the plugin to cancel the watch task keyed by `watch_id`.
///
/// Delivery is best-effort and fire-and-forget: `Drop` cannot be async, so it
/// spawns a detached task that sends the notification. Errors are ignored —
/// the plugin may already be gone, and a failed unwatch is no worse than the
/// pre-guard leak it replaces. The `watch_id` is sent as a string to match
/// the protocol's `SubjectUnwatchRequest { watch_id: String }` shape, where it
/// correlates with the id the daemon used for the originating `subject/watch`.
struct SubjectUnwatchGuard {
    host: PluginHost,
    watch_id: u64,
    /// Lease pinning the lazy router's shared cached host against LRU eviction
    /// for the whole watch lifetime. Held purely for its `Drop`: when the guard
    /// drops, the lease drops too, letting the host become eligible for
    /// eviction again. Inert for the eager source (those hosts are router-owned
    /// and never evicted). Never read.
    #[allow(dead_code)]
    lease: HostLease,
}

impl Drop for SubjectUnwatchGuard {
    fn drop(&mut self) {
        let host = self.host.clone();
        let watch_id = self.watch_id;
        // Drop runs inside the daemon's tokio runtime (watch streams are
        // polled there). Spawn a detached best-effort notify rather than
        // blocking the dropping thread on async I/O. If no runtime is active
        // (e.g. a unit test dropping the subscription off-runtime), the
        // `try_current` check skips the send instead of panicking.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::spawn(async move {
            let params = serde_json::json!({ "watch_id": watch_id.to_string() });
            if let Err(error) = host.notify(SUBJECT_METHOD_UNWATCH, Some(params)).await {
                tracing::debug!(
                    watch_id,
                    error = %error,
                    "best-effort subject/unwatch notify failed (plugin may already be gone)",
                );
            }
        });
    }
}

/// Everything the lazy router needs to spawn a subject-backend plugin host
/// on demand, without keeping the process alive ahead of first use.
///
/// Built by the daemon / CLI from a [`crate::DiscoveredPlugin`]: the kind
/// routing table is learned from the manifest's `subject_kind:<kind>`
/// capabilities (alias-translated) so a plugin is never spawned merely to
/// discover which kinds it serves. The spawn parameters
/// (`env_required` / `notification_buffer_size` / `working_dir`) mirror the
/// `PluginSpawnOptions::for_manifest(...).with_*` chain the eager path used.
#[derive(Debug, Clone)]
pub struct SubjectPluginSpec {
    /// Canonical plugin name (the install-time `--name` override when one was
    /// set, else the manifest name). Keys the routing table and the alias map.
    pub name: String,
    /// Path to the plugin binary, passed to [`PluginHost::spawn_with_options`].
    pub path: PathBuf,
    /// Native subject kinds the plugin serves, as declared by its manifest's
    /// `subject_kind:<kind>` capabilities. These are the plugin's *native*
    /// kinds; install-time renames are applied during registration.
    pub native_kinds: Vec<String>,
    /// Manifest `env_required` list — drives the spawn env allowlist.
    pub env_required: Vec<EnvRequirement>,
    /// Manifest notification-buffer hint forwarded to the spawn.
    pub notification_buffer_size: Option<usize>,
    /// Working directory to pin the spawned child to (the project root).
    pub working_dir: Option<PathBuf>,
}

/// Backing source for a kind's plugin host.
///
/// `Eager` holds pre-spawned hosts (tests, one-shot embeds via
/// [`SubjectRouter::from_initialized_hosts`]). `Lazy` holds per-plugin spawn
/// specs and resolves hosts on first route through the shared
/// [`ResidentHostRegistry`] — see [`LazyHosts`].
enum HostSource {
    Eager(HashMap<String, PluginHost>),
    Lazy(Box<LazyHosts>),
}

/// Compiled kind routing tables: exact-match registrations
/// (`kind -> plugin_name`) and glob registrations (`(pattern, plugin_name)`).
/// Produced by [`SubjectRouter::register_kinds`] /
/// [`SubjectRouter::register_kinds_from_specs`].
type KindTables = (HashMap<String, String>, Vec<(KindPattern, String)>, Option<String>);

/// A borrow of a subject-backend host that pins it against eviction for its
/// lifetime. `route_call` holds it across its single RPC; `start_watch` parks it
/// inside the watch subscription so the host stays alive for the whole watch.
/// Dropping it lets the host become eligible for LRU eviction again.
///
/// Cross-role host sharing (0.7 Layer B): lazy hosts live in the process-global
/// [`ResidentHostRegistry`] shared with `config_source` / `workflow_journal`, so
/// a multi-role plugin binary is one shared process. The pin is the registry's
/// own [`ResidentHostLease`]. Eager hosts (test / one-shot embeds) are
/// router-owned and never evicted, so their pin is inert.
pub(crate) struct HostLease {
    host: PluginHost,
    _pin: LeasePin,
}

enum LeasePin {
    /// Eager, router-owned host: never evicted, so nothing to pin.
    Eager,
    /// Lazy host leased from the shared registry: held to pin against eviction.
    Resident(#[allow(dead_code)] ResidentHostLease),
}

impl HostLease {
    fn host(&self) -> &PluginHost {
        &self.host
    }

    /// Wrap a pre-spawned eager host (router-owned, never evicted).
    fn eager(host: PluginHost) -> Self {
        Self { host, _pin: LeasePin::Eager }
    }

    /// Wrap a registry lease, cloning out its host handle while holding the lease
    /// to keep the shared process pinned against eviction.
    fn resident(lease: ResidentHostLease) -> Self {
        Self { host: lease.host().clone(), _pin: LeasePin::Resident(lease) }
    }
}

/// Lazily-spawned subject-backend hosts, resolved through the shared
/// [`ResidentHostRegistry`].
///
/// Spawning is on-demand and at most once per plugin BINARY: the first
/// `route_call` (or watch) that resolves to a plugin spawns + handshakes its
/// host in the registry and caches it there; later calls (routes AND watches,
/// AND other roles resolving the same binary) reuse the same shared process. The
/// registry bounds the concurrently-live set via least-recently-used eviction
/// that skips leased hosts, so neither an in-flight route nor an active watch is
/// ever torn down by cache pressure.
struct LazyHosts {
    /// Spawn specs keyed by plugin name. Immutable after construction.
    specs: HashMap<String, SubjectPluginSpec>,
    /// The process-global resident-host registry (shared across roles). Captured
    /// at construction so a test can install a fresh registry beforehand.
    registry: Arc<ResidentHostRegistry>,
}

impl LazyHosts {
    /// Resolve a leased host for `plugin_name`, spawning + handshaking it once in
    /// the shared registry on first use. The returned [`HostLease`] pins the host
    /// against eviction for the caller's use. The registry's per-binary spawn
    /// lock guarantees exactly-once spawning even under concurrent routes.
    async fn host_for(&self, plugin_name: &str) -> Result<HostLease, RpcError> {
        let Some(spec) = self.specs.get(plugin_name) else {
            return Err(RpcError {
                code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
                message: format!("subject backend '{plugin_name}' is not available"),
                data: None,
            });
        };
        let mtime = binary_mtime_nanos(&spec.path);
        let context = Self::spawn_context(spec);
        let lease = self
            .registry
            .get_or_spawn(&spec.path, mtime, &context, || async {
                // Retry transient spawn/handshake failures inside the registry's
                // per-binary spawn lock so a cold DB connect / fork pressure
                // self-heals rather than surfacing as a hard board error.
                self.spawn_and_handshake(spec).await.map_err(|error| anyhow!(error.message))
            })
            .await
            .map_err(|error| RpcError {
                code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
                message: format!("{error}"),
                data: None,
            })?;
        Ok(HostLease::resident(lease))
    }

    /// Spawn-context fingerprint for a subject spec, matching the effective spawn
    /// options [`Self::spawn_and_handshake_once`] uses: no forwarded env (subject
    /// plugins see only manifest-declared env — the secret trust boundary), the
    /// spec's project-root working directory, and its notification-buffer hint.
    /// This keeps subject hosts in their own registry slot, distinct from the
    /// full-env `config_source` / `workflow_journal` slots.
    fn spawn_context(spec: &SubjectPluginSpec) -> String {
        crate::resident_host_registry::spawn_context_fingerprint(
            &[],
            spec.working_dir.as_deref(),
            spec.notification_buffer_size,
        )
    }

    /// Spawn + handshake a subject-backend plugin, retrying on transient
    /// failures (a cold DB connect, fork/handshake pressure under load) with
    /// backoff. Previously a single transient "connection lost" on (re)spawn
    /// surfaced as a hard board error; a couple of quick retries let the daemon
    /// self-heal instead. Backend bugs still fail fast after the attempts.
    async fn spawn_and_handshake(&self, spec: &SubjectPluginSpec) -> Result<PluginHost, RpcError> {
        const MAX_ATTEMPTS: usize = 3;
        // Backoff before each retry (not before the first attempt).
        const BACKOFF_MS: [u64; 2] = [150, 600];
        let mut last_err: Option<RpcError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            match self.spawn_and_handshake_once(spec).await {
                Ok(host) => return Ok(host),
                Err(error) => {
                    if let Some(delay) = BACKOFF_MS.get(attempt).copied() {
                        tracing::warn!(
                            plugin = %spec.name,
                            attempt = attempt + 1,
                            "subject_backend spawn/handshake failed ({}); retrying in {delay}ms",
                            error.message
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                    last_err = Some(error);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| RpcError {
            code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
            message: format!("subject_backend plugin '{}' spawn failed after {MAX_ATTEMPTS} attempts", spec.name),
            data: None,
        }))
    }

    async fn spawn_and_handshake_once(&self, spec: &SubjectPluginSpec) -> Result<PluginHost, RpcError> {
        let mut options =
            PluginSpawnOptions::for_manifest(spec.name.clone(), &spec.env_required, std::iter::empty::<String>(), None)
                .with_notification_buffer_hint(spec.notification_buffer_size);
        if let Some(dir) = spec.working_dir.as_ref() {
            options = options.with_working_dir(dir.clone());
        }
        let host = PluginHost::spawn_with_options(&spec.path, &[], options).await.map_err(|error| RpcError {
            code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
            message: format!("failed to spawn subject_backend plugin '{}': {error}", spec.name),
            data: None,
        })?;
        // Drive the initialize handshake so the plugin is ready before the
        // first real RPC; a handshake failure tears the half-spawned child
        // down rather than leaking it.
        if let Err(error) = host.handshake().await {
            let _ = host.clone().shutdown().await;
            return Err(RpcError {
                code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
                message: format!("subject_backend plugin '{}' failed its initialize handshake: {error}", spec.name),
                data: None,
            });
        }
        Ok(host)
    }

    /// Shut down every resident host in the shared registry. Called at
    /// router/daemon shutdown. Because the registry is shared across roles, this
    /// also reaps `config_source` / `workflow_journal` hosts — desirable at
    /// teardown, and idempotent. There is no production caller mid-life; the
    /// daemon reaps via the role-specific `shutdown_resident_hosts` teardown.
    async fn shutdown(&self) {
        self.registry.shutdown_all().await;
    }

    /// Test helper: whether the shared registry currently holds a live host for
    /// `plugin_name` (by its binary path + mtime).
    #[cfg(test)]
    fn is_cached(&self, plugin_name: &str) -> bool {
        match self.specs.get(plugin_name) {
            Some(spec) => {
                self.registry.contains(&spec.path, binary_mtime_nanos(&spec.path), &Self::spawn_context(spec))
            }
            None => false,
        }
    }

    /// Test helper: number of concurrently-live hosts in the shared registry.
    #[cfg(test)]
    fn live_len(&self) -> usize {
        self.registry.live_len()
    }
}

pub struct SubjectRouter {
    /// Exact-kind registrations keyed by the declared kind string.
    exact_kinds: HashMap<String, String>,
    /// Glob registrations stored as (pattern, plugin_name) pairs.
    glob_kinds: Vec<(KindPattern, String)>,
    /// Catch-all backend (declared via a bare `*` `subject_kind`). Resolves
    /// ONLY when no exact or glob pattern claims the kind, so a dynamic-kind
    /// backend (e.g. `subject-postgres` serving runtime-declared kinds) can
    /// receive any unclaimed kind without re-declaring its manifest. At most
    /// one catch-all may be registered.
    catch_all: Option<String>,
    /// Backing host source: eager (pre-spawned) or lazy (spawn-on-route).
    hosts: HostSource,
    /// Daemon-side translator state: maps user-facing `installed_kind` to
    /// the plugin's hardcoded `native_kind`. Outbound `route_call` rewrites
    /// `<installed_kind>/<verb>` to `<native_kind>/<verb>` before forwarding
    /// to the plugin's stdio; inbound responses have their top-level `kind`
    /// field rewritten from native back to installed.
    ///
    /// Empty when no installed plugin was renamed at install time —
    /// translation is a no-op and the router behaves identically to its
    /// pre-v0.5.7 form.
    aliases: KindAliasMap,
}

/// Per-plugin install-time rename map. Each entry pairs the user-facing
/// `installed_kind` (the prefix the SubjectRouter dispatches against) with
/// the plugin's hardcoded `native_kind` (the prefix the plugin actually
/// implements on the wire).
///
/// Built by the install pipeline from the v0.5.7 `plugins.lock` schema;
/// passed into [`SubjectRouter::from_initialized_hosts_with_aliases`] so
/// the router can register the installed_kind variant and translate at
/// the wire boundary.
#[derive(Debug, Clone, Default)]
pub struct KindAliasMap {
    /// Map from `installed_kind` -> `native_kind`. Only populated for
    /// plugins where the two values differ; identity mappings are
    /// represented by absence.
    installed_to_native: HashMap<String, String>,
    /// Map from `native_kind` -> `installed_kind`, scoped per plugin name
    /// so two plugins claiming the same native kind (each with its own
    /// installed_kind) can both round-trip inbound responses correctly.
    /// Lookups join on the plugin name produced at routing time.
    by_plugin: HashMap<String, HashMap<String, String>>,
}

impl KindAliasMap {
    /// Register an `(installed_kind, native_kind)` pair for `plugin_name`.
    /// Identity pairs (installed == native) are dropped: the translator
    /// only needs to track real renames.
    pub fn insert(&mut self, plugin_name: &str, installed_kind: &str, native_kind: &str) {
        if installed_kind == native_kind {
            return;
        }
        self.installed_to_native.insert(installed_kind.to_string(), native_kind.to_string());
        self.by_plugin
            .entry(plugin_name.to_string())
            .or_default()
            .insert(native_kind.to_string(), installed_kind.to_string());
    }

    /// Resolve the plugin-native kind that `installed_kind` maps to, if any.
    pub fn native_for_installed(&self, installed_kind: &str) -> Option<&str> {
        self.installed_to_native.get(installed_kind).map(String::as_str)
    }

    /// Resolve the user-facing kind a plugin's native kind should be
    /// rewritten to before returning a response, if any.
    pub fn installed_for_plugin_native(&self, plugin_name: &str, native_kind: &str) -> Option<&str> {
        self.by_plugin.get(plugin_name).and_then(|m| m.get(native_kind)).map(String::as_str)
    }

    /// `true` when no renames are registered. Lets the router short-circuit
    /// the inbound walker for the common case where every install uses its
    /// native kind.
    pub fn is_empty(&self) -> bool {
        self.installed_to_native.is_empty()
    }
}

impl SubjectRouter {
    pub async fn from_initialized_hosts(hosts: HashMap<String, PluginHost>) -> Result<Self> {
        Self::from_initialized_hosts_with_aliases(hosts, KindAliasMap::default()).await
    }

    /// Build the router and apply install-time kind renames. When `aliases`
    /// contains a `(plugin_name, native_kind) -> installed_kind` entry, the
    /// router registers the `installed_kind` against that plugin instead of
    /// the manifest-declared `native_kind`. This is the load-bearing piece
    /// of the v0.5.7 daemon-side translator: plugins keep emitting
    /// `task/list`, the router exposes it as `archive/list`, and outbound /
    /// inbound translation in [`Self::route_call`] keeps the wire boundary
    /// consistent.
    pub async fn from_initialized_hosts_with_aliases(
        hosts: HashMap<String, PluginHost>,
        aliases: KindAliasMap,
    ) -> Result<Self> {
        match Self::register_kinds(&hosts, &aliases).await {
            Ok((exact_kinds, glob_kinds, catch_all)) => {
                Ok(Self { exact_kinds, glob_kinds, catch_all, hosts: HostSource::Eager(hosts), aliases })
            }
            Err(error) => {
                // We own the spawned hosts; dropping them without shutdown
                // would orphan every already-live plugin child the moment
                // one plugin fails its handshake or claims a duplicate kind.
                for (_, host) in hosts {
                    let _ = host.shutdown().await;
                }
                Err(error)
            }
        }
    }

    /// Build a router that spawns subject-backend plugins lazily, on first
    /// route to one of their kinds, rather than eagerly at construction.
    ///
    /// The kind routing table is built entirely from each spec's
    /// `native_kinds` (the plugin manifest's `subject_kind:<kind>`
    /// capabilities), alias-translated exactly as the eager path translates
    /// handshake-declared kinds — so NO plugin process is spawned merely to
    /// learn which kinds it serves. Duplicate-kind collisions and glob/exact
    /// precedence are validated up front, identically to the eager path.
    ///
    /// This is the production daemon / CLI constructor: a project that only
    /// uses `kind=task` spawns at most the one `task` backend (plus whatever
    /// other kinds it actually routes to), keeping the live subject-host set
    /// far below the runtime plugin-process cap even when dozens of
    /// data-source subject plugins are installed globally.
    pub fn from_lazy_specs(specs: Vec<SubjectPluginSpec>, aliases: KindAliasMap) -> Result<Self> {
        let (exact_kinds, glob_kinds, catch_all) = Self::register_kinds_from_specs(&specs, &aliases)?;
        let specs_by_name: HashMap<String, SubjectPluginSpec> =
            specs.into_iter().map(|spec| (spec.name.clone(), spec)).collect();
        let lazy = LazyHosts { specs: specs_by_name, registry: global_resident_host_registry() };
        Ok(Self { exact_kinds, glob_kinds, catch_all, hosts: HostSource::Lazy(Box::new(lazy)), aliases })
    }

    /// Build the exact/glob kind tables from manifest-declared native kinds,
    /// applying install-time renames. Mirrors [`Self::register_kinds`] but
    /// reads kinds from the spec list instead of a live handshake, so it is
    /// synchronous and spawns nothing.
    fn register_kinds_from_specs(specs: &[SubjectPluginSpec], aliases: &KindAliasMap) -> Result<KindTables> {
        let mut exact_kinds: HashMap<String, String> = HashMap::new();
        let mut glob_kinds: Vec<(KindPattern, String)> = Vec::new();
        let mut catch_all: Option<String> = None;

        for spec in specs {
            for raw_kind in &spec.native_kinds {
                // A bare `*` declares the catch-all backend: it claims any kind
                // no specific pattern matches. At most one may be registered.
                if raw_kind == "*" {
                    if let Some(existing) = &catch_all {
                        return Err(anyhow!(
                            "duplicate subject catch-all '*' claimed by '{}' and '{}'",
                            existing,
                            spec.name
                        ));
                    }
                    catch_all = Some(spec.name.clone());
                    continue;
                }
                let pattern = KindPattern::parse(raw_kind);
                let (registered_pattern, registered_raw) = if pattern.is_glob {
                    (pattern, raw_kind.clone())
                } else if let Some(installed) = aliases.installed_for_plugin_native(&spec.name, &pattern.raw) {
                    (KindPattern::parse(installed), installed.to_string())
                } else {
                    (pattern, raw_kind.clone())
                };

                if registered_pattern.is_glob {
                    if let Some((existing_pattern, existing_name)) =
                        glob_kinds.iter().find(|(p, _)| p.prefix == registered_pattern.prefix && p.is_glob)
                    {
                        return Err(anyhow!(
                            "duplicate subject kind glob '{}' claimed by '{}' and '{}'",
                            existing_pattern.raw,
                            existing_name,
                            spec.name
                        ));
                    }
                    glob_kinds.push((registered_pattern, spec.name.clone()));
                } else if let Some(existing) = exact_kinds.get(&registered_raw) {
                    return Err(anyhow!(
                        "duplicate subject kind '{}' claimed by '{}' and '{}'",
                        registered_raw,
                        existing,
                        spec.name
                    ));
                } else {
                    exact_kinds.insert(registered_raw, spec.name.clone());
                }
            }
        }

        Ok((exact_kinds, glob_kinds, catch_all))
    }

    /// Resolve a leased host for `plugin_name`, spawning + handshaking it on
    /// first use under the lazy source. The returned [`HostLease`] pins the host
    /// against LRU eviction for as long as it is held, so the caller can drive
    /// its RPC — or park the lease in a watch subscription — without the host
    /// being torn down out from under it by cache pressure.
    ///
    /// Anti-deadlock: the slow `spawn_with_options().await` runs while holding
    /// only a *per-plugin* spawn lock — never the shared cache lock and never a
    /// lock spanning a different plugin — so concurrent routes to other kinds
    /// proceed unblocked, and two concurrent routes to the *same* not-yet-live
    /// plugin spawn it exactly once (the loser observes the cache populated
    /// when it re-checks under the spawn lock).
    ///
    /// For the eager source the pre-spawned shared host is leased directly
    /// (eager hosts are router-owned and never evicted, so the lease token is
    /// inert there).
    async fn host_for(&self, plugin_name: &str) -> Result<HostLease, RpcError> {
        match &self.hosts {
            HostSource::Eager(hosts) => hosts.get(plugin_name).cloned().map(HostLease::eager).ok_or_else(|| RpcError {
                code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
                message: format!("subject backend '{plugin_name}' is not available"),
                data: None,
            }),
            HostSource::Lazy(lazy) => lazy.host_for(plugin_name).await,
        }
    }

    /// Gracefully shut down every live subject-backend host this router owns.
    /// For the lazy source only the currently-spawned (cached) hosts are
    /// touched — plugins that were never routed to were never spawned and need
    /// no teardown. Idempotent and safe to call once at daemon shutdown.
    pub async fn shutdown(&self) {
        match &self.hosts {
            HostSource::Eager(hosts) => {
                for host in hosts.values() {
                    let _ = host.clone().shutdown().await;
                }
            }
            HostSource::Lazy(lazy) => lazy.shutdown().await,
        }
    }

    async fn register_kinds(hosts: &HashMap<String, PluginHost>, aliases: &KindAliasMap) -> Result<KindTables> {
        let mut exact_kinds: HashMap<String, String> = HashMap::new();
        let mut glob_kinds: Vec<(KindPattern, String)> = Vec::new();
        let mut catch_all: Option<String> = None;
        let names = hosts.keys().cloned().collect::<Vec<_>>();

        for name in names {
            let host = hosts.get(&name).ok_or_else(|| anyhow!("plugin host disappeared during routing setup"))?;
            let result = host.handshake().await?;
            for raw_kind in result.capabilities.subject_kinds {
                // A bare `*` declares the catch-all backend (see the lazy path).
                if raw_kind == "*" {
                    if let Some(existing) = &catch_all {
                        return Err(anyhow!(
                            "duplicate subject catch-all '*' claimed by '{}' and '{}'",
                            existing,
                            name
                        ));
                    }
                    catch_all = Some(name.clone());
                    continue;
                }
                let pattern = KindPattern::parse(&raw_kind);
                // Apply install-time rename: register the installed_kind
                // instead of the native one for this plugin if an alias
                // was recorded at install time. Glob patterns are
                // currently passed through unrenamed — the v0.5.7
                // translator only covers exact kinds, matching the
                // mission's scope of `task -> task-2` style renames.
                let (registered_pattern, registered_raw) = if pattern.is_glob {
                    (pattern, raw_kind.clone())
                } else if let Some(installed) = aliases.installed_for_plugin_native(&name, &pattern.raw) {
                    let renamed = KindPattern::parse(installed);
                    let renamed_raw = installed.to_string();
                    (renamed, renamed_raw)
                } else {
                    (pattern, raw_kind.clone())
                };

                if registered_pattern.is_glob {
                    if let Some((existing_pattern, existing_name)) =
                        glob_kinds.iter().find(|(p, _)| p.prefix == registered_pattern.prefix && p.is_glob)
                    {
                        return Err(anyhow!(
                            "duplicate subject kind glob '{}' claimed by '{}' and '{}'",
                            existing_pattern.raw,
                            existing_name,
                            name
                        ));
                    }
                    glob_kinds.push((registered_pattern, name.clone()));
                } else if let Some(existing) = exact_kinds.get(&registered_raw) {
                    return Err(anyhow!(
                        "duplicate subject kind '{}' claimed by '{}' and '{}'",
                        registered_raw,
                        existing,
                        name
                    ));
                } else {
                    exact_kinds.insert(registered_raw, name.clone());
                }
            }
        }

        Ok((exact_kinds, glob_kinds, catch_all))
    }

    /// Resolve the plugin name responsible for `kind`.
    ///
    /// Precedence rules:
    ///
    /// 1. Exact-match registration (e.g. `task.tracked` beats `task.*`).
    /// 2. Longest matching glob prefix wins (`task.tracked.*` beats `task.*`
    ///    when resolving `task.tracked.foo`).
    /// 3. If two globs of equal prefix length both match, the resolution is
    ///    ambiguous and `None` is returned. (Equal-prefix duplicates are
    ///    already rejected at registration time, so this is defensive.)
    pub fn plugin_for_kind(&self, kind: &str) -> Option<&str> {
        if let Some(name) = self.exact_kinds.get(kind) {
            return Some(name.as_str());
        }
        let mut best: Option<(usize, &str)> = None;
        let mut ambiguous = false;
        for (pattern, plugin) in &self.glob_kinds {
            if !pattern.matches(kind) {
                continue;
            }
            let len = pattern.prefix.len();
            match best {
                None => best = Some((len, plugin.as_str())),
                Some((cur_len, _cur_plugin)) => {
                    if len > cur_len {
                        best = Some((len, plugin.as_str()));
                        ambiguous = false;
                    } else if len == cur_len {
                        ambiguous = true;
                    }
                }
            }
        }
        if ambiguous {
            return None;
        }
        // A specific (exact/glob) match always wins; the catch-all backend is
        // consulted only when no specific pattern claims the kind.
        best.map(|(_, plugin)| plugin).or(self.catch_all.as_deref())
    }

    /// `true` when `method`'s kind prefix is EXPLICITLY registered (exact or
    /// glob). Deliberately excludes the `*` catch-all: the catch-all routes any
    /// kind via [`Self::plugin_for_kind`]/[`Self::route_call`], but a method
    /// classifier must not report every `x/y` method as a subject method, or it
    /// would mis-claim non-subject methods (`config/load`, `journal/record`).
    pub fn is_subject_method(&self, method: &str) -> bool {
        method.split('/').next().is_some_and(|kind| {
            self.exact_kinds.contains_key(kind) || self.glob_kinds.iter().any(|(p, _)| p.matches(kind))
        })
    }

    pub async fn route_call(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        let installed_kind = method.split('/').next().unwrap_or_default();
        let Some(plugin_name) = self.plugin_for_kind(installed_kind) else {
            return Err(RpcError {
                code: animus_plugin_protocol::error_codes::METHOD_NOT_FOUND,
                message: format!("no subject backend registered for kind '{installed_kind}'"),
                data: None,
            });
        };
        let plugin_name = plugin_name.to_string();
        // Lazy spawn-on-route: resolve (and on first use spawn + handshake) the
        // backing host for this kind's plugin. The lease pins the host against
        // LRU eviction for the duration of the RPC below, and the RPC runs after
        // every router-internal lock is released.
        let lease = self.host_for(&plugin_name).await?;
        let host = lease.host();

        let native_kind_opt = self.aliases.native_for_installed(installed_kind);

        // Outbound translation:
        //  - rewrite `<installed_kind>/<verb>` to `<native_kind>/<verb>` so
        //    the plugin sees the prefix it actually implements.
        //  - rewrite any top-level `id` / `subject_id` field in `params`
        //    whose `<kind>:` prefix matches the installed_kind so the
        //    plugin's local store can resolve native IDs (subject IDs are
        //    encoded `<kind>:<local-id>` per
        //    `extract_kind_from_subject_id` in control/dispatch.rs).
        let translated_method = match native_kind_opt {
            Some(native_kind) => match method.split_once('/') {
                Some((_, rest)) => format!("{native_kind}/{rest}"),
                None => native_kind.to_string(),
            },
            None => method.to_string(),
        };
        let translated_params = match (native_kind_opt, params) {
            (Some(native_kind), Some(value)) => Some(rewrite_outbound_id_prefix(value, installed_kind, native_kind)),
            (_, other) => other,
        };

        let mut response =
            host.request_with_timeout(&translated_method, translated_params, SUBJECT_ROUTE_TIMEOUT).await?;

        // Inbound translation: rewrite the top-level `kind` field AND the
        // `<kind>:` prefix in `id` fields so callers continue to see the
        // installed_kind they sent. Walker scope is intentionally narrow —
        // only Subject.kind/.id, SubjectList.subjects[*].kind/.id, and
        // SubjectEvent.subject.kind/.id — to avoid taking on full schema
        // knowledge inside the host crate. Deep-nested `kind` fields
        // (inside `metadata`, `tags`, etc.) are out of scope for v0.5.7.
        // See `docs/architecture/plugin-kind-translator-v0.5.7.md`.
        if !self.aliases.is_empty() {
            rewrite_response_kind(&mut response, &plugin_name, &self.aliases);
        }

        Ok(response)
    }

    /// Route an authenticated application call through the actor-scoped v2
    /// subject protocol. This is deliberately separate from
    /// [`Self::route_call`], the explicit legacy v1 edge.
    pub async fn route_actor_call(
        &self,
        method: &str,
        params: Option<Value>,
        actor: &animus_actor::Actor,
    ) -> Result<Value, RpcError> {
        let invalid = |message: String| RpcError {
            code: animus_plugin_protocol::error_codes::INVALID_PARAMS,
            message,
            data: None,
        };
        let v2_actor = serde_json::from_value(serde_json::to_value(actor).map_err(|error| invalid(error.to_string()))?)
            .map_err(|error| invalid(format!("failed to convert authenticated actor: {error}")))?;
        let context = SubjectRequestContext::for_actor(v2_actor);
        let (kind, verb) = method
            .split_once('/')
            .ok_or_else(|| invalid(format!("actor-scoped subject method must be '<kind>/<verb>', got '{method}'")))?;
        let raw = params.unwrap_or_else(|| serde_json::json!({}));
        let request = match verb {
            "list" => {
                let filter_value = raw.get("filter").cloned().unwrap_or(raw);
                let filter: SubjectFilter = serde_json::from_value(filter_value)
                    .map_err(|error| invalid(format!("invalid subject list filter: {error}")))?;
                serde_json::to_value(SubjectListRequestV2 { context, filter })
            }
            "get" => {
                let id = required_subject_id(&raw, verb).map_err(invalid)?;
                serde_json::to_value(SubjectGetRequestV2 { context, id })
            }
            "create" => {
                serde_json::to_value(SubjectCreateRequestV2 { context, kind: Some(kind.to_string()), payload: raw })
            }
            "update" => {
                let id = required_subject_id(&raw, verb).map_err(invalid)?;
                let patch = raw.get("patch").cloned().unwrap_or_else(|| {
                    let mut patch = raw;
                    if let Value::Object(map) = &mut patch {
                        map.remove("id");
                    }
                    patch
                });
                serde_json::to_value(SubjectUpdateRequestV2 { context, id, patch })
            }
            "status" => {
                let id = required_subject_id(&raw, verb).map_err(invalid)?;
                let status = raw
                    .get("status")
                    .and_then(Value::as_str)
                    .filter(|status| !status.trim().is_empty())
                    .ok_or_else(|| invalid("subject status requires `status`".into()))?
                    .to_string();
                serde_json::to_value(SubjectStatusRequestV2 { context, id, status })
            }
            "delete" => {
                let id = required_subject_id(&raw, verb).map_err(invalid)?;
                serde_json::to_value(SubjectDeleteRequestV2 { context, id })
            }
            other => {
                return Err(RpcError {
                    code: animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED,
                    message: format!("subject actor protocol v2 does not support '{other}'"),
                    data: None,
                })
            }
        }
        .map_err(|error| invalid(format!("failed to encode subject v2 request: {error}")))?;
        self.route_call(&format!("{kind}/v2/{verb}"), Some(request)).await
    }

    pub async fn resolve_subject(&self, subject_kind: &str, subject_id: &str) -> Result<Value, RpcError> {
        self.route_call(&format!("{subject_kind}/get"), Some(serde_json::json!({ "id": subject_id }))).await
    }

    /// Distinct plugin names that should be asked to watch for changes when a
    /// caller subscribes for `installed_kind`.
    ///
    /// - `Some(kind)` → the single plugin registered for that exact/glob kind
    ///   (empty when no backend is mounted for it).
    /// - `None` → every distinct subject-backend plugin currently mounted, so
    ///   an unscoped watch receives events across all kinds.
    ///
    /// The names are de-duplicated: a plugin claiming several kinds is only
    /// watched once.
    pub fn watch_plugin_names(&self, installed_kind: Option<&str>) -> Vec<String> {
        match installed_kind {
            Some(kind) => self.plugin_for_kind(kind).map(|name| vec![name.to_string()]).unwrap_or_default(),
            None => {
                let mut names: Vec<String> = Vec::new();
                // Include the catch-all (`*`) backend alongside the exact/glob
                // plugins: an unscoped watch must receive events for every
                // mounted backend, including the wildcard that serves
                // runtime-declared dynamic kinds (e.g. a portal `declare_kind`).
                // Omitting it dropped all catch-all-served kinds from the merged
                // `subject/changed` stream.
                for name in
                    self.exact_kinds.values().chain(self.glob_kinds.iter().map(|(_, n)| n)).chain(self.catch_all.iter())
                {
                    if !names.iter().any(|existing| existing == name) {
                        names.push(name.clone());
                    }
                }
                names
            }
        }
    }

    /// Open a `subject/watch` subscription against a single mounted plugin.
    ///
    /// Subscribes to the plugin host's notification broadcast *before* issuing
    /// the `subject/watch` request so no early `subject/changed` notification
    /// is missed, then forwards the watch request (with outbound kind
    /// translation applied to `installed_kind` and any nested `filter.kind`).
    ///
    /// Returns the live notification receiver on success. Callers filter the
    /// stream down to `subject/changed` notifications themselves and are
    /// responsible for inbound kind
    /// translation via [`Self::installed_kind_for_plugin_native`].
    ///
    /// `Err` is returned for transport / RPC failures, including the
    /// `METHOD_NOT_SUPPORTED` code a polling-only backend returns — callers
    /// should treat that as "this backend cannot stream" and degrade rather
    /// than fail the whole subscription.
    ///
    /// On success the returned [`SubjectWatchSubscription`] carries both the
    /// notification receiver and the JSON-RPC request id the host used for the
    /// `subject/watch` call. The runtime echoes that id in every
    /// `subject/changed` notification's `params.id`, so a subscriber can
    /// demultiplex its own events from any concurrent / stale watch RPC that
    /// shares the same plugin host's notification broadcast.
    pub async fn start_watch(
        &self,
        plugin_name: &str,
        installed_kind: Option<&str>,
        filter: Option<Value>,
    ) -> Result<SubjectWatchSubscription, RpcError> {
        // Lazy spawn-on-watch: lease the shared cached host (spawning it on
        // first use). The lease pins the host against LRU eviction; it is parked
        // inside the returned subscription so the host stays alive for the
        // whole watch — no dedicated per-watch child, and no eviction can tear
        // an active watch down. `&host` below borrows the leased clone.
        let lease = self.host_for(plugin_name).await?;
        let host = lease.host();

        // Subscribe before the request so notifications emitted between the
        // plugin acking `subject/watch` and us returning are not dropped.
        let rx = host.subscribe_notifications();

        // Outbound kind translation: the watch request carries the
        // user-facing `installed_kind`; the plugin only understands its
        // native kind.
        let native_kind = installed_kind.and_then(|kind| {
            self.aliases.native_for_installed(kind).map(str::to_string).or_else(|| Some(kind.to_string()))
        });
        let mut params = serde_json::Map::new();
        if let Some(kind) = native_kind.as_deref() {
            params.insert("kind".to_string(), Value::String(kind.to_string()));
        }
        if let Some(filter) = filter {
            // Translate `filter.kind` from installed -> native when a rename
            // is in effect, mirroring `route_call`'s outbound handling.
            let translated = match (installed_kind, native_kind.as_deref()) {
                (Some(installed), Some(native)) if installed != native => {
                    let mut wrapped = serde_json::Map::new();
                    wrapped.insert("filter".to_string(), filter);
                    let rewritten = rewrite_outbound_id_prefix(Value::Object(wrapped), installed, native);
                    rewritten.get("filter").cloned().unwrap_or(Value::Null)
                }
                _ => filter,
            };
            params.insert("filter".to_string(), translated);
        }
        let watch_params = if params.is_empty() { None } else { Some(Value::Object(params)) };

        // `subject/watch` wire method name (animus-subject-protocol). Spelled
        // as a literal so the plugin-host crate need not pull in the subject
        // protocol crate (and its potentially divergent version).
        //
        // Capture the request id: the runtime echoes it in every
        // `subject/changed` notification's `params.id` so the subscriber can
        // filter out notifications belonging to other watch RPCs sharing this
        // host's broadcast. The same id keys the `subject/unwatch` the drop
        // guard sends below so the plugin cancels the right watch task.
        let (watch_id, result) =
            host.request_with_timeout_capturing_id(SUBJECT_METHOD_WATCH, watch_params, SUBJECT_ROUTE_TIMEOUT).await;
        // On a watch failure (e.g. a polling-only backend answering
        // METHOD_NOT_SUPPORTED) we simply return: the lease drops here, leaving
        // the shared cached host alive for reuse by other routes/watches — no
        // child to reap, since there is no dedicated per-watch process.
        result?;

        // Clone the host handle for the unwatch guard, then move the lease into
        // the guard so the host stays pinned against eviction for the whole
        // watch lifetime.
        let guard_host = host.clone();

        // Per-watch cancellation: when the daemon drops this subscription
        // (client disconnect, scoped teardown), the guard fires
        // `subject/unwatch { watch_id }` so the plugin cancels its
        // `backend.watch()` task instead of leaking it until daemon shutdown
        // (animus-subject-protocol v0.1.16+; older backends treat the
        // notification as an unknown no-op method).
        let unwatch_guard = SubjectUnwatchGuard { host: guard_host, watch_id, lease };

        Ok(SubjectWatchSubscription { notifications: rx, watch_id, unwatch_guard })
    }

    /// Translate a plugin's native subject kind back to the user-facing
    /// installed kind for `plugin_name`, if an install-time rename is in
    /// effect. Returns `None` when there is no rename (identity).
    pub fn installed_kind_for_plugin_native(&self, plugin_name: &str, native_kind: &str) -> Option<&str> {
        self.aliases.installed_for_plugin_native(plugin_name, native_kind)
    }

    /// `true` when no install-time kind renames are registered. Lets watch
    /// callers skip inbound translation in the common identity case.
    pub fn aliases_are_identity(&self) -> bool {
        self.aliases.is_empty()
    }
}

fn required_subject_id(params: &Value, verb: &str) -> std::result::Result<SubjectId, String> {
    params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(SubjectId::new)
        .ok_or_else(|| format!("subject {verb} requires `id`"))
}

/// Rewrite the top-level `kind` field on known response shapes from the
/// plugin's `native_kind` back to the user-facing `installed_kind`.
///
/// Supported shapes (matches the v0.5 SubjectRouter response surface):
///
/// - `Subject` — `{ "kind": "<native>", ... }` (top-level object).
/// - `SubjectList` — `{ "subjects": [{ "kind": "<native>", ... }, ...] }`.
/// - `SubjectEvent` — `{ "subject": { "kind": "<native>", ... }, ... }`.
///
/// The walker is intentionally narrow: it only inspects the top-level
/// object plus the two named collections above. Deep-nested `kind` fields
/// (inside `metadata`, `tags`, freeform plugin-defined payloads) are left
/// alone — rewriting them would require host-side schema knowledge that
/// belongs in the protocol crate, not the router. See
/// `docs/architecture/plugin-kind-translator-v0.5.7.md` for the explicit
/// deferral.
fn rewrite_response_kind(value: &mut Value, plugin_name: &str, aliases: &KindAliasMap) {
    let Value::Object(map) = value else {
        return;
    };
    rewrite_kind_in_object(map, plugin_name, aliases);
    if let Some(Value::Object(subject)) = map.get_mut("subject") {
        rewrite_kind_in_object(subject, plugin_name, aliases);
    }
    if let Some(Value::Array(subjects)) = map.get_mut("subjects") {
        for entry in subjects {
            if let Value::Object(item) = entry {
                rewrite_kind_in_object(item, plugin_name, aliases);
            }
        }
    }
}

fn rewrite_kind_in_object(object: &mut serde_json::Map<String, Value>, plugin_name: &str, aliases: &KindAliasMap) {
    if let Some(Value::String(kind)) = object.get("kind") {
        if let Some(installed) = aliases.installed_for_plugin_native(plugin_name, kind) {
            object.insert("kind".to_string(), Value::String(installed.to_string()));
        }
    }
    for id_field in ["id", "subject_id"] {
        let Some(Value::String(id)) = object.get(id_field) else {
            continue;
        };
        let Some((native_prefix, rest)) = id.split_once(':') else {
            continue;
        };
        let Some(installed) = aliases.installed_for_plugin_native(plugin_name, native_prefix) else {
            continue;
        };
        let rewritten = format!("{installed}:{rest}");
        object.insert(id_field.to_string(), Value::String(rewritten));
    }
}

/// Translate outbound params before forwarding to the plugin's stdio.
/// Rewrites the following fields when their value matches `installed_kind`:
///
/// - top-level `kind` (string or array of strings) — used by
///   `subject/create` payloads and the CLI's `subject list` shape.
/// - top-level `id` / `subject_id` — `<installed_kind>:<local-id>` is
///   rewritten to `<native_kind>:<local-id>`.
/// - top-level `filter.kind` (array of strings) — used by the daemon's
///   `subject/list` dispatch.
/// - nested `subject.kind` + `subject.id` — used by event-shaped params.
///
/// Recurses into the top-level object only — same narrow scope as
/// [`rewrite_response_kind`] — and is a no-op when the supplied JSON
/// isn't an object.
fn rewrite_outbound_id_prefix(mut value: Value, installed_kind: &str, native_kind: &str) -> Value {
    if let Value::Object(map) = &mut value {
        rewrite_outbound_in_object(map, installed_kind, native_kind);
        if let Some(Value::Object(subject)) = map.get_mut("subject") {
            rewrite_outbound_in_object(subject, installed_kind, native_kind);
        }
        if let Some(Value::Object(filter)) = map.get_mut("filter") {
            rewrite_outbound_kind_field(filter, installed_kind, native_kind);
        }
    }
    value
}

fn rewrite_outbound_in_object(object: &mut serde_json::Map<String, Value>, installed_kind: &str, native_kind: &str) {
    rewrite_outbound_kind_field(object, installed_kind, native_kind);
    for id_field in ["id", "subject_id"] {
        let Some(Value::String(id)) = object.get(id_field) else {
            continue;
        };
        let Some((prefix, rest)) = id.split_once(':') else {
            continue;
        };
        if prefix != installed_kind {
            continue;
        }
        let rewritten = format!("{native_kind}:{rest}");
        object.insert(id_field.to_string(), Value::String(rewritten));
    }
}

/// Rewrite the `kind` field on a JSON object when its value matches
/// `installed_kind`. Supports both string (`"kind": "archive"`) and
/// array-of-strings (`"kind": ["archive", "other"]`) shapes — the CLI
/// emits the array form for `subject/list` and the daemon control
/// dispatch reads `filter.kind` as an array.
fn rewrite_outbound_kind_field(object: &mut serde_json::Map<String, Value>, installed_kind: &str, native_kind: &str) {
    match object.get("kind") {
        Some(Value::String(kind)) if kind == installed_kind => {
            object.insert("kind".to_string(), Value::String(native_kind.to_string()));
        }
        Some(Value::Array(items)) => {
            let rewritten: Vec<Value> = items
                .iter()
                .map(|item| match item {
                    Value::String(k) if k == installed_kind => Value::String(native_kind.to_string()),
                    other => other.clone(),
                })
                .collect();
            object.insert("kind".to_string(), Value::Array(rewritten));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    async fn subject_host(name: &str, subject_kinds: Vec<&str>) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let kinds = subject_kinds.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: "subject_backend".to_string(),
                                plugin_kinds: Vec::new(),
                                description: None,
                            },
                            capabilities: PluginCapabilities {
                                subject_kinds: kinds.clone(),
                                methods: kinds.iter().map(|kind| format!("{kind}/get")).collect(),
                                ..PluginCapabilities::default()
                            },
                            kind_capabilities: std::collections::HashMap::new(),
                        }),
                    ),
                    "initialized" => continue,
                    method => RpcResponse::ok(request.id, serde_json::json!({ "method": method })),
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");
            }
        });

        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn routes_by_subject_kind_prefix() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), subject_host("tasks", vec!["task"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        let result = router.route_call("task/get", Some(serde_json::json!({ "id": "TASK-1" }))).await.expect("route");

        assert_eq!(result["method"], "task/get");
        assert_eq!(router.plugin_for_kind("task"), Some("tasks"));
    }

    #[tokio::test]
    async fn glob_kind_matches_dotted_subkinds() {
        let mut hosts = HashMap::new();
        hosts.insert("all-tasks".to_string(), subject_host("all-tasks", vec!["task.*"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        // Glob matches both kinds.
        assert_eq!(router.plugin_for_kind("task.tracked"), Some("all-tasks"));
        assert_eq!(router.plugin_for_kind("task.untracked"), Some("all-tasks"));
        // The glob does not match the bare prefix itself.
        assert_eq!(router.plugin_for_kind("task"), None);
        // And the route_call path also accepts the dotted method.
        let result = router.route_call("task.tracked/list", Some(serde_json::json!({}))).await.expect("route");
        assert_eq!(result["method"], "task.tracked/list");
    }

    #[tokio::test]
    async fn exact_match_beats_glob() {
        let mut hosts = HashMap::new();
        hosts.insert("any-task".to_string(), subject_host("any-task", vec!["task.*"]).await);
        hosts.insert("tracked".to_string(), subject_host("tracked", vec!["task.tracked"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        assert_eq!(router.plugin_for_kind("task.tracked"), Some("tracked"));
        assert_eq!(router.plugin_for_kind("task.untracked"), Some("any-task"));
    }

    #[tokio::test]
    async fn longest_glob_prefix_wins() {
        let mut hosts = HashMap::new();
        hosts.insert("any-task".to_string(), subject_host("any-task", vec!["task.*"]).await);
        hosts.insert("nested".to_string(), subject_host("nested", vec!["task.tracked.*"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        assert_eq!(router.plugin_for_kind("task.tracked.high"), Some("nested"));
        assert_eq!(router.plugin_for_kind("task.untracked.low"), Some("any-task"));
    }

    /// Spawns a fake subject backend that round-trips the inbound method
    /// and params back to the caller. Used by translator tests so the test
    /// can assert what the plugin actually saw (post outbound rewrite).
    async fn echo_subject_host(name: &str, subject_kinds: Vec<&str>) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let kinds = subject_kinds.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: "subject_backend".to_string(),
                                plugin_kinds: Vec::new(),
                                description: None,
                            },
                            capabilities: PluginCapabilities {
                                subject_kinds: kinds.clone(),
                                methods: kinds.iter().map(|kind| format!("{kind}/get")).collect(),
                                ..PluginCapabilities::default()
                            },
                            kind_capabilities: std::collections::HashMap::new(),
                        }),
                    ),
                    "initialized" => continue,
                    method => {
                        // Echo what the plugin saw, plus a `subject` payload
                        // whose `kind` matches the native prefix it received.
                        // Inbound translation should rewrite that `kind` back
                        // to the installed_kind before returning to the caller.
                        // IDs are emitted in the canonical `<kind>:<local-id>`
                        // shape so tests can assert outbound + inbound ID
                        // translation alongside the `kind` field rewrite.
                        let prefix = method.split('/').next().unwrap_or_default().to_string();
                        let saw_params = request.params.clone().unwrap_or(serde_json::Value::Null);
                        RpcResponse::ok(
                            request.id,
                            serde_json::json!({
                                "plugin_saw_method": method,
                                "plugin_saw_params": saw_params,
                                "kind": prefix.clone(),
                                "subject": {
                                    "kind": prefix.clone(),
                                    "id": format!("{prefix}:LOCAL-1"),
                                },
                                "subjects": [
                                    { "kind": prefix.clone(), "id": format!("{prefix}:LOCAL-A") },
                                    {
                                        "kind": prefix.clone(),
                                        "id": format!("{prefix}:LOCAL-B"),
                                        "metadata": { "kind": "untouched" },
                                    }
                                ]
                            }),
                        )
                    }
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");
            }
        });

        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn outbound_method_rewrites_installed_kind_to_native() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        assert_eq!(router.plugin_for_kind("archive"), Some("archive"));
        assert_eq!(router.plugin_for_kind("task"), None, "native kind must NOT be routable after rename");

        let result = router.route_call("archive/list", None).await.expect("route call");
        assert_eq!(result["plugin_saw_method"], "task/list", "plugin must receive native-kind method");
    }

    #[tokio::test]
    async fn outbound_method_is_unchanged_when_alias_is_identity() {
        let mut hosts = HashMap::new();
        hosts.insert("default".to_string(), echo_subject_host("default", vec!["task"]).await);
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, KindAliasMap::default())
            .await
            .expect("router builds");

        let result = router.route_call("task/list", None).await.expect("route call");
        assert_eq!(result["plugin_saw_method"], "task/list");
    }

    #[tokio::test]
    async fn inbound_response_rewrites_top_level_subject_and_subjects_kind() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let result = router.route_call("archive/list", None).await.expect("route call");
        assert_eq!(result["kind"], "archive", "top-level kind rewritten to installed");
        assert_eq!(result["subject"]["kind"], "archive", "Subject.kind rewritten");
        assert_eq!(result["subjects"][0]["kind"], "archive", "SubjectList.subjects[0].kind rewritten");
        assert_eq!(result["subjects"][1]["kind"], "archive", "SubjectList.subjects[1].kind rewritten");
        // IDs must travel through the translator alongside the `kind`
        // field so subsequent control-plane round-trips that extract the
        // kind from `<kind>:<local-id>` land back on the same plugin.
        assert_eq!(result["subject"]["id"], "archive:LOCAL-1");
        assert_eq!(result["subjects"][0]["id"], "archive:LOCAL-A");
        assert_eq!(result["subjects"][1]["id"], "archive:LOCAL-B");
        // Deep nesting under `metadata` is explicitly out of scope.
        assert_eq!(
            result["subjects"][1]["metadata"]["kind"], "untouched",
            "deep-nested kind fields must be left alone in v0.5.7"
        );
    }

    #[tokio::test]
    async fn outbound_params_rewrite_id_prefix_to_native_kind() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "id": "archive:LOCAL-X" });
        let result = router.route_call("archive/get", Some(params)).await.expect("route call");
        assert_eq!(
            result["plugin_saw_params"]["id"], "task:LOCAL-X",
            "outbound id prefix must be translated to native_kind before forwarding"
        );
    }

    #[tokio::test]
    async fn outbound_params_rewrite_top_level_kind_string() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "kind": "archive", "title": "demo" });
        let result = router.route_call("archive/create", Some(params)).await.expect("route call");
        assert_eq!(
            result["plugin_saw_params"]["kind"], "task",
            "create's top-level kind must be translated to the native kind"
        );
    }

    #[tokio::test]
    async fn outbound_params_rewrite_filter_kind_array() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "filter": { "kind": ["archive", "other"] } });
        let result = router.route_call("archive/list", Some(params)).await.expect("route call");
        let kinds = &result["plugin_saw_params"]["filter"]["kind"];
        assert_eq!(kinds[0], "task", "matching installed_kind in array must be translated");
        assert_eq!(kinds[1], "other", "unrelated kinds in array must be preserved");
    }

    #[tokio::test]
    async fn outbound_params_leave_unrelated_id_prefixes_alone() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "id": "other:UNTOUCHED" });
        let result = router.route_call("archive/get", Some(params)).await.expect("route call");
        assert_eq!(
            result["plugin_saw_params"]["id"], "other:UNTOUCHED",
            "non-matching id prefixes must be forwarded verbatim"
        );
    }

    #[tokio::test]
    async fn duplicate_glob_kinds_are_rejected_at_registration() {
        let mut hosts = HashMap::new();
        hosts.insert("a".to_string(), subject_host("a", vec!["task.*"]).await);
        hosts.insert("b".to_string(), subject_host("b", vec!["task.*"]).await);

        let outcome = SubjectRouter::from_initialized_hosts(hosts).await;
        let err = match outcome {
            Err(e) => e,
            Ok(_) => panic!("router should reject duplicate glob kinds"),
        };
        assert!(format!("{err:?}").contains("duplicate subject kind glob"));
    }

    // --- Lazy spawn-on-route tests ---------------------------------------
    //
    // These exercise the real spawn path: each fake backend is a tiny shell
    // script that (a) appends its name to a shared spawn-log on startup so a
    // test can assert which plugins were actually spawned, and (b) speaks just
    // enough JSON-RPC (`initialize` + echoing `<kind>/<verb>`) for the router.
    // Spawning real children means these are unix-only, mirroring the existing
    // `write_env_dump_plugin` host tests.

    #[cfg(unix)]
    mod lazy {
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};

        use super::*;

        /// Write an executable subject-backend plugin script that logs its
        /// `--name`-derived label to `spawn_log` on every spawn and answers the
        /// JSON-RPC handshake + an echoing `<kind>/<verb>` for `kind`.
        fn write_lazy_subject_plugin(dir: &Path, name: &str, kind: &str, spawn_log: &Path) -> PathBuf {
            let plugin = dir.join(name);
            // The script records the spawn (so tests can count + identify
            // spawns), then loops reading one JSON-RPC line at a time. It
            // answers `initialize` with a manifest declaring `kind`, ignores
            // the `initialized` notification, and echoes any other method.
            // `printf '%s\n'` flushes per line so the host reader sees frames
            // promptly. `id` is hardcoded to 1 because the host issues
            // `initialize` first (id 1) and these tests never pipeline.
            // The host correlates responses by request id, so the script must
            // echo back each request's own id. `id` is extracted from the JSON
            // line with sed (numeric ids only — the host always allocates
            // numeric ids). The `initialized` notification carries no id and is
            // skipped.
            let script = format!(
                r#"#!/bin/sh
echo "{name}" >> "{log}"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{{\"protocol_version\":\"1.0.0\",\"plugin_info\":{{\"name\":\"{name}\",\"version\":\"0.1.0\",\"plugin_kind\":\"subject_backend\"}},\"capabilities\":{{\"subject_kinds\":[\"{kind}\"],\"methods\":[\"{kind}/list\"]}}}}}}"
      ;;
    *'"method":"initialized"'*)
      : ;;
    *'"method":"shutdown"'*)
      printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":null}}"
      ;;
    *'"id"'*)
      printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{{\"routed\":\"{name}\"}}}}"
      ;;
  esac
done
"#,
                name = name,
                kind = kind,
                log = spawn_log.display(),
            );
            std::fs::write(&plugin, script).expect("write lazy subject plugin");
            let mut perms = std::fs::metadata(&plugin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&plugin, perms).unwrap();
            plugin
        }

        fn spec(dir: &Path, name: &str, kind: &str, spawn_log: &Path) -> SubjectPluginSpec {
            let path = write_lazy_subject_plugin(dir, name, kind, spawn_log);
            SubjectPluginSpec {
                name: name.to_string(),
                path,
                native_kinds: vec![kind.to_string()],
                env_required: vec![],
                notification_buffer_size: None,
                working_dir: None,
            }
        }

        fn spawn_log_lines(spawn_log: &Path) -> Vec<String> {
            match std::fs::read_to_string(spawn_log) {
                Ok(body) => body.lines().map(ToString::to_string).collect(),
                Err(_) => Vec::new(),
            }
        }

        /// Serialize against the process-global slot factory (shared with the
        /// host cap tests), since each lazy test spawns real plugin children.
        /// The same lock also serializes access to the process-global resident
        /// host registry these tests install via [`fresh_registry`].
        fn slot_guard() -> std::sync::MutexGuard<'static, ()> {
            crate::TEST_SLOT_FACTORY_GUARD.lock().unwrap_or_else(|p| p.into_inner())
        }

        /// Install a fresh process-global resident-host registry with the given
        /// soft cap, isolating this test from any hosts a prior test left behind.
        /// Cross-role host sharing (0.7 Layer B) moved the lazy host cache out of
        /// the per-router `LazyHosts` into this shared registry, so a per-test
        /// reset (under `slot_guard`) replaces the old per-router env-cap knob.
        fn fresh_registry(max_live: usize) {
            crate::resident_host_registry::install_resident_host_registry_for_test(max_live);
        }

        #[test]
        fn catch_all_resolves_only_unclaimed_kinds() {
            // from_lazy_specs spawns nothing; plugin_for_kind is a pure lookup.
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![spec(dir.path(), "tasks", "task", &log), spec(dir.path(), "baas", "*", &log)];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");
            // A specific (exact) backend wins over the catch-all.
            assert_eq!(router.plugin_for_kind("task"), Some("tasks"));
            // Any kind no specific backend claims falls to the catch-all.
            assert_eq!(router.plugin_for_kind("blog"), Some("baas"));
            assert_eq!(router.plugin_for_kind("knowledge"), Some("baas"));
            // is_subject_method reflects EXPLICIT registration only — it must
            // NOT report catch-all-routed kinds (or it would mis-classify
            // non-subject methods like config/load as subject methods).
            assert!(router.is_subject_method("task/list"));
            assert!(!router.is_subject_method("blog/list"));
            assert!(!router.is_subject_method("config/load"));
        }

        #[test]
        fn unscoped_watch_includes_catch_all_backend() {
            // An unscoped (`kind = None`) watch must ask every mounted backend
            // to watch — including the `*` catch-all that serves runtime dynamic
            // kinds — so the merged `subject/changed` stream does not silently
            // drop catch-all-served kinds.
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![spec(dir.path(), "tasks", "task", &log), spec(dir.path(), "baas", "*", &log)];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");

            let mut names = router.watch_plugin_names(None);
            names.sort();
            assert_eq!(
                names,
                vec!["baas".to_string(), "tasks".to_string()],
                "unscoped watch must include the catch-all"
            );

            // A scoped watch is unchanged: it resolves the single owning plugin.
            assert_eq!(router.watch_plugin_names(Some("task")), vec!["tasks".to_string()]);
        }

        #[test]
        fn unscoped_watch_dedupes_multi_kind_backend() {
            // A backend claiming several kinds is watched only once, and the
            // catch-all is not double-counted when it is the same plugin name.
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![spec(dir.path(), "tasks", "task", &log), spec(dir.path(), "reqs", "requirement", &log)];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");
            let mut names = router.watch_plugin_names(None);
            names.sort();
            assert_eq!(names, vec!["reqs".to_string(), "tasks".to_string()]);
        }

        #[test]
        fn duplicate_catch_all_is_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![spec(dir.path(), "a", "*", &log), spec(dir.path(), "b", "*", &log)];
            assert!(SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).is_err());
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn no_plugin_spawned_until_a_matching_kind_is_routed() {
            let _slot = slot_guard();
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            fresh_registry(8);
            let specs = vec![spec(dir.path(), "tasks", "task", &log), spec(dir.path(), "issues", "issue", &log)];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");

            // Routing table is populated from manifests, with zero spawns.
            assert_eq!(router.plugin_for_kind("task"), Some("tasks"));
            assert_eq!(router.plugin_for_kind("issue"), Some("issues"));
            assert!(spawn_log_lines(&log).is_empty(), "construction must not spawn any plugin");

            // First route to `task` spawns ONLY the tasks plugin.
            let result = router.route_call("task/list", None).await.expect("route task");
            assert_eq!(result["routed"], "tasks");
            let spawns = spawn_log_lines(&log);
            assert_eq!(spawns, vec!["tasks".to_string()], "only the routed plugin spawns; got {spawns:?}");

            router.shutdown().await;
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn routing_one_kind_never_spawns_unrelated_plugins() {
            let _slot = slot_guard();
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            fresh_registry(8);
            let specs = vec![
                spec(dir.path(), "tasks", "task", &log),
                spec(dir.path(), "issues", "issue", &log),
                spec(dir.path(), "docs", "doc", &log),
            ];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");

            router.route_call("task/list", None).await.expect("route task");
            // A second route to the same kind reuses the cached host (no
            // re-spawn).
            router.route_call("task/get", None).await.expect("route task again");

            let spawns = spawn_log_lines(&log);
            assert_eq!(spawns, vec!["tasks".to_string()], "only `task` plugin spawned, exactly once; got {spawns:?}");
            assert!(!spawns.contains(&"issues".to_string()), "unrelated `issue` plugin must not spawn");
            assert!(!spawns.contains(&"docs".to_string()), "unrelated `doc` plugin must not spawn");

            router.shutdown().await;
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn alias_translation_routes_installed_kind_to_native_plugin_lazily() {
            let _slot = slot_guard();
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            // Plugin natively serves `task`, installed under `archive`.
            fresh_registry(8);
            let specs = vec![spec(dir.path(), "archive", "task", &log)];
            let mut aliases = KindAliasMap::default();
            aliases.insert("archive", "archive", "task");
            let router = SubjectRouter::from_lazy_specs(specs, aliases).expect("router builds");

            // Routing table registers the installed kind, hides the native one.
            assert_eq!(router.plugin_for_kind("archive"), Some("archive"));
            assert_eq!(router.plugin_for_kind("task"), None, "native kind is not routable after rename");
            assert!(spawn_log_lines(&log).is_empty(), "no spawn from registration");

            let result = router.route_call("archive/list", None).await.expect("route archive");
            assert_eq!(result["routed"], "archive");
            assert_eq!(spawn_log_lines(&log), vec!["archive".to_string()]);

            router.shutdown().await;
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn glob_kind_routes_and_spawns_lazily() {
            let _slot = slot_guard();
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            fresh_registry(8);
            let specs = vec![spec(dir.path(), "any-task", "task.*", &log)];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");

            assert_eq!(router.plugin_for_kind("task.tracked"), Some("any-task"));
            assert_eq!(router.plugin_for_kind("task"), None, "glob does not match bare prefix");
            assert!(spawn_log_lines(&log).is_empty(), "no spawn from registration");

            let result = router.route_call("task.tracked/list", None).await.expect("route dotted kind");
            assert_eq!(result["routed"], "any-task");
            assert_eq!(spawn_log_lines(&log), vec!["any-task".to_string()]);

            router.shutdown().await;
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn active_watch_host_is_pinned_against_lru_eviction() {
            let _slot = slot_guard();
            // Cap the shared registry at 1 so route LRU pressure is maximal; a
            // watch on `wk` must NOT be evicted by routing to other kinds, because
            // the watch holds a lease that pins its cached host against eviction.
            fresh_registry(1);
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![
                spec(dir.path(), "watcher", "wk", &log),
                spec(dir.path(), "other-a", "oa", &log),
                spec(dir.path(), "other-b", "ob", &log),
            ];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");

            // Open a watch: spawns + caches the host, and the subscription holds
            // a lease pinning it.
            let sub = router.start_watch("watcher", Some("wk"), None).await.expect("watch starts");
            let HostSource::Lazy(lazy) = &router.hosts else { panic!("expected lazy") };
            assert!(lazy.is_cached("watcher"), "watch host is cached and shared");

            // Drive route LRU churn well past the soft cap of 1; the leased
            // watch host must survive because eviction skips leased hosts.
            router.route_call("oa/list", None).await.expect("route oa");
            router.route_call("ob/list", None).await.expect("route ob");

            assert!(lazy.is_cached("watcher"), "leased watch host must survive route-driven eviction pressure");
            // The watcher spawned exactly once and was never re-spawned.
            let watcher_spawns = spawn_log_lines(&log).iter().filter(|n| *n == "watcher").count();
            assert_eq!(watcher_spawns, 1, "watch host spawned once and survived eviction");

            // After the watch drops, its lease drops, so the (formerly pinned)
            // watch host is finally evictable again. A single subsequent route
            // through ANY cached host triggers the opportunistic over-cap drain,
            // which reclaims the now-idle hosts back to the soft cap of 1.
            drop(sub);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let result = router.route_call("oa/list", None).await.expect("route oa again (fast-path drain)");
            assert_eq!(result["routed"], "other-a");
            assert!(lazy.live_len() <= 1, "once all leases drop, a single route drains the cache back to the soft cap");

            router.shutdown().await;
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn over_cap_overshoot_is_drained_after_leases_drop() {
            // Reproduces codex's concern: when more than `max_live` hosts are all
            // leased at once (e.g. many concurrent watches), the cache overshoots
            // the cap; once the leases drop, the next route — even a fast-path
            // cache hit — must drain the cache back to the cap rather than leave
            // idle hosts alive forever.
            let _slot = slot_guard();
            fresh_registry(1);
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![
                spec(dir.path(), "a", "ka", &log),
                spec(dir.path(), "b", "kb", &log),
                spec(dir.path(), "c", "kc", &log),
            ];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");
            let HostSource::Lazy(lazy) = &router.hosts else { panic!("expected lazy") };

            // Hold three concurrent leases (simulating three live watches) — all
            // over the cap of 1, so eviction can reclaim none of them.
            let l1 = lazy.host_for("a").await.expect("lease a");
            let l2 = lazy.host_for("b").await.expect("lease b");
            let l3 = lazy.host_for("c").await.expect("lease c");
            assert_eq!(lazy.live_len(), 3, "all three leased hosts are cached, over the cap");

            // Drop the leases: the hosts are now idle but still cached.
            drop((l1, l2, l3));
            assert_eq!(lazy.live_len(), 3, "dropping leases alone does not evict");

            // A single fast-path route to an already-cached plugin drains the
            // overshoot back to the soft cap of 1.
            router.route_call("ka/list", None).await.expect("route ka (fast-path)");
            assert_eq!(lazy.live_len(), 1, "fast-path drain reclaims idle over-cap hosts down to the soft cap");

            router.shutdown().await;
        }

        #[tokio::test]
        #[allow(clippy::await_holding_lock)] // intentional: serializes real-spawn tests across awaits
        async fn live_host_set_stays_bounded_under_the_cap() {
            let _slot = slot_guard();
            // Force a shared-registry cap of 2; route to three distinct kinds and
            // assert the live host count never exceeds 2.
            fresh_registry(2);
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("spawns.log");
            let specs = vec![
                spec(dir.path(), "a", "ka", &log),
                spec(dir.path(), "b", "kb", &log),
                spec(dir.path(), "c", "kc", &log),
            ];
            let router = SubjectRouter::from_lazy_specs(specs, KindAliasMap::default()).expect("router builds");

            router.route_call("ka/list", None).await.expect("route ka");
            router.route_call("kb/list", None).await.expect("route kb");
            // Third distinct route triggers an LRU eviction of `a`.
            router.route_call("kc/list", None).await.expect("route kc");

            let HostSource::Lazy(lazy) = &router.hosts else {
                panic!("expected lazy host source");
            };
            let live = lazy.live_len();
            assert!(live <= 2, "live host count {live} must stay within the cap of 2");
            // All three were spawned over time; the cap bounds CONCURRENT live
            // hosts, not the cumulative spawn count.
            let mut spawned = spawn_log_lines(&log);
            spawned.sort();
            assert_eq!(spawned, vec!["a".to_string(), "b".to_string(), "c".to_string()]);

            router.shutdown().await;
        }
    }
}
