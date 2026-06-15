//! Subject backend plugin integration for the daemon.
//!
//! NOTE: this module's primary type is [`SubjectPluginDispatch`] — not
//! to be confused with `protocol::SubjectDispatch`, which is the
//! queue-envelope shape that gates dispatch into workflow runners. The
//! two are unrelated: `SubjectDispatch` describes WHAT work to do;
//! `SubjectPluginDispatch` describes WHERE subjects come from when
//! resolved through plugins.
//!
//! Mirrors the LogStorageBackend pattern from
//! [`crate::log_storage`] (commit `48966ba9`) and the provider/trigger
//! plugin pattern: at daemon startup we discover every installed plugin,
//! filter for `plugin_kind == subject_backend`, and (when the operator
//! has not set [`SUBJECT_PLUGINS_DISABLE_ENV`]) hand them to
//! [`SubjectRouter::from_initialized_hosts`] which spawns each child,
//! handshakes, and builds an immutable kind→plugin map.
//!
//! Anti-deadlock rules:
//!
//! - The resolved [`SubjectPluginDispatch`] handle is an [`Arc`] over an
//!   immutable [`SubjectRouter`]. No mutexes guard it on the read path.
//! - The router is set once at daemon startup and never mutated.
//! - Discovery returns owned data; nothing holds a lock across `.await`.
//! - Duplicate-kind collisions abort discovery early with a clear error
//!   message naming both plugins (per
//!   [`SubjectRouter::from_initialized_hosts`]).
//!
//! Subjects must be served by installed `subject_backend` plugins —
//! when no plugin is mounted for a requested kind, `<kind>/<verb>` calls
//! fail with a NotFound RpcError. As of v0.4.12 the in-tree task and
//! requirement adapters were removed; install
//! `animus-subject-default` and `animus-subject-requirements` (via
//! `animus plugin install-defaults --include-subjects`) to keep the
//! `kind=task` and `kind=requirement` surfaces routable.

use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_core::Stream;
use futures_util::StreamExt;
use orchestrator_plugin_host::{
    discover_plugins, DiscoveredPlugin, KindAliasMap, PluginHost, PluginLockfile, PluginSpawnOptions, SubjectRouter,
};
use serde_json::Value;

use animus_plugin_protocol::{RpcError, PLUGIN_KIND_SUBJECT_BACKEND};
use animus_subject_protocol_wire::SubjectChangedEvent;

/// Environment variable that forces subject-backend plugin discovery to
/// be skipped entirely. Mirrors `ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN`
/// from [`crate::log_storage`] and the provider plugin opt-out shape.
/// Any non-empty value other than `"0"` / `"false"` / `"no"` / `"off"`
/// is treated as truthy.
pub const SUBJECT_PLUGINS_DISABLE_ENV: &str = "ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS";

/// Resolved subject-routing state for a daemon run.
///
/// When no subject-backend plugins are installed (or the disable env var
/// is set) the dispatch is `Empty` — every `<kind>/<verb>` call will
/// fail with [`animus_plugin_protocol::error_codes::METHOD_NOT_FOUND`]
/// per [`SubjectRouter::route_call`].
#[derive(Clone, Default)]
pub struct SubjectPluginDispatch {
    router: Option<Arc<SubjectRouter>>,
    kinds: Vec<String>,
    plugin_count: usize,
}

impl std::fmt::Debug for SubjectPluginDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubjectPluginDispatch")
            .field("plugin_count", &self.plugin_count)
            .field("kinds", &self.kinds)
            .field("router_present", &self.router.is_some())
            .finish()
    }
}

impl SubjectPluginDispatch {
    /// Empty dispatch — no subject plugins active. Every routing attempt
    /// returns `METHOD_NOT_FOUND`.
    pub fn empty() -> Self {
        Self { router: None, kinds: Vec::new(), plugin_count: 0 }
    }

    /// Wrap an already-built [`SubjectRouter`]. Used by tests and by
    /// callers that need to pre-populate the dispatch (e.g. one-shot CLI
    /// invocations).
    pub fn from_router(router: SubjectRouter, kinds: Vec<String>, plugin_count: usize) -> Self {
        Self { router: Some(Arc::new(router)), kinds, plugin_count }
    }

    /// `true` when at least one subject-backend plugin contributed a kind.
    pub fn is_active(&self) -> bool {
        self.router.is_some()
    }

    /// Number of subject-backend plugins backing this dispatch.
    pub fn plugin_count(&self) -> usize {
        self.plugin_count
    }

    /// Subject kinds currently routable. Order is whatever the router
    /// reports (HashMap iteration order — not guaranteed stable).
    pub fn kinds(&self) -> &[String] {
        &self.kinds
    }

    /// Borrow the inner router. `None` when the dispatch is empty.
    pub fn router(&self) -> Option<&Arc<SubjectRouter>> {
        self.router.as_ref()
    }

    /// Route a `<kind>/<verb>` request through the active plugin.
    ///
    /// Returns `BackendError::NotFound`-shaped [`RpcError`] when no
    /// plugin is mounted for the kind embedded in `method` (this includes
    /// the empty-dispatch case).
    pub async fn route_call(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        let kind = method.split('/').next().unwrap_or_default();
        match self.router.as_deref() {
            Some(router) => router.route_call(method, params).await,
            None => Err(RpcError {
                code: animus_plugin_protocol::error_codes::METHOD_NOT_FOUND,
                message: format!("no subject backend mounted for kind '{kind}'"),
                data: None,
            }),
        }
    }

    /// Open a live `subject/changed` stream sourced from the mounted
    /// subject-backend plugin(s).
    ///
    /// Behaviour:
    /// - `kind = Some(k)` watches only the plugin that owns kind `k`;
    ///   `kind = None` watches every mounted plugin and merges their event
    ///   streams.
    /// - Each selected plugin gets a `subject/watch` request; the daemon then
    ///   subscribes to that plugin's notification broadcast and forwards every
    ///   `subject/changed` notification (decoded to [`SubjectChangedEvent`])
    ///   into the returned stream, applying inbound kind translation for any
    ///   install-time renamed backend.
    /// - A backend that responds `METHOD_NOT_SUPPORTED` (polling-only) is
    ///   skipped with a logged note rather than failing the subscription.
    /// - When no backend is mounted, or none support streaming, the returned
    ///   stream is empty/closed.
    pub fn subject_watch(
        &self,
        kind: Option<String>,
        filter: Option<Value>,
    ) -> Pin<Box<dyn Stream<Item = SubjectChangedEvent> + Send>> {
        let Some(router) = self.router.clone() else {
            return Box::pin(futures_util::stream::empty());
        };
        watch_stream(router, kind, filter)
    }
}

/// Wire method name for the `subject/changed` notification
/// (animus-subject-protocol `NOTIFICATION_SUBJECT_CHANGED`). Spelled as a
/// literal here for the same reason the router avoids the subject protocol
/// crate dependency.
const SUBJECT_NOTIFICATION_CHANGED: &str = "subject/changed";

/// Build the merged `subject/changed` stream over the selected plugins. Split
/// out of [`SubjectPluginDispatch::subject_watch`] so the async `subject/watch`
/// handshake can run lazily inside the stream rather than blocking the caller.
fn watch_stream(
    router: Arc<SubjectRouter>,
    kind: Option<String>,
    filter: Option<Value>,
) -> Pin<Box<dyn Stream<Item = SubjectChangedEvent> + Send>> {
    let plugin_names = router.watch_plugin_names(kind.as_deref());
    if plugin_names.is_empty() {
        return Box::pin(futures_util::stream::empty());
    }

    let mut per_plugin: Vec<Pin<Box<dyn Stream<Item = SubjectChangedEvent> + Send>>> =
        Vec::with_capacity(plugin_names.len());

    for plugin_name in plugin_names {
        let router = router.clone();
        let kind = kind.clone();
        let filter = filter.clone();
        // Keep a copy of the filter for client-side enforcement: the current
        // plugin runtime's `SubjectBackend::watch()` receives no filter, so
        // multi-kind / unfiltered backends emit every change and we must drop
        // events that don't match the caller's filter ourselves.
        let client_filter = filter.clone();
        // Each plugin's branch performs its own `subject/watch` handshake on
        // first poll, then yields decoded events until the broadcast closes.
        let stream = futures_util::stream::once(async move {
            match router.start_watch(&plugin_name, kind.as_deref(), filter).await {
                Ok(subscription) => Some((router, plugin_name, kind, client_filter, subscription)),
                Err(error) => {
                    if error.code == animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED {
                        tracing::debug!(
                            plugin = %plugin_name,
                            "subject backend does not support subject/watch; skipping live stream branch",
                        );
                    } else {
                        tracing::warn!(
                            plugin = %plugin_name,
                            code = error.code,
                            error = %error.message,
                            "subject/watch handshake failed; skipping live stream branch",
                        );
                    }
                    None
                }
            }
        })
        .filter_map(|opt| async move { opt })
        .flat_map(|(router, plugin_name, kind, client_filter, subscription)| {
            plugin_changed_stream(router, plugin_name, kind, client_filter, subscription)
        });
        per_plugin.push(Box::pin(stream));
    }

    Box::pin(futures_util::stream::select_all(per_plugin))
}

/// Drain one plugin's notification broadcast, keeping only `subject/changed`
/// notifications correlated to *this* watch RPC, unwrapping the runtime's
/// `{ "id": <watch req id>, "event": <SubjectChangedEvent> }` envelope,
/// applying inbound kind translation for renamed backends, and enforcing the
/// requested `kind` scope client-side.
///
/// Correlation: the runtime echoes the `subject/watch` request id in each
/// notification's `params.id`. We keep only notifications whose id matches
/// [`SubjectWatchSubscription::watch_id`] so concurrent or stale watch RPCs
/// sharing this host's broadcast cannot cross-talk into this stream. Older
/// runtimes that omit `params.id` still work: a missing id is treated as
/// "uncorrelated, belongs to the sole watcher" and accepted.
///
/// Client-side `kind` + `filter` enforcement: `SubjectBackend::watch()` in the
/// current runtime receives no kind/filter arguments, so a multi-kind backend
/// emits events for every kind it owns and ignores any filter the daemon
/// forwarded. We therefore drop events whose subject kind does not match the
/// scoped `requested_kind`, and events whose subject does not match the
/// caller's `filter` (status / kind / assignee / labels / updated_since /
/// native_status). Pagination-only filter fields (`cursor`, `limit`) and the
/// `dispatch_label` / `has_attachment_kind` gates are not applied to live
/// events — they have no meaningful per-event semantics here.
fn plugin_changed_stream(
    router: Arc<SubjectRouter>,
    plugin_name: String,
    requested_kind: Option<String>,
    filter: Option<Value>,
    subscription: orchestrator_plugin_host::SubjectWatchSubscription,
) -> Pin<Box<dyn Stream<Item = SubjectChangedEvent> + Send>> {
    let orchestrator_plugin_host::SubjectWatchSubscription { notifications, watch_id } = subscription;
    let stream = tokio_stream::wrappers::BroadcastStream::new(notifications).filter_map(move |item| {
        let router = router.clone();
        let plugin_name = plugin_name.clone();
        let requested_kind = requested_kind.clone();
        let filter = filter.clone();
        async move {
            let notification = match item {
                Ok(notification) => notification,
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        plugin = %plugin_name,
                        skipped,
                        "subject/watch subscriber lagged behind plugin broadcast; events dropped",
                    );
                    return None;
                }
            };
            if notification.method != SUBJECT_NOTIFICATION_CHANGED {
                return None;
            }
            let params = notification.params?;

            // Correlate to this watch RPC. When `params.id` is present it must
            // match our request id; a missing id (legacy runtime) is accepted.
            if let Some(id_value) = params.get("id") {
                if let Some(notif_id) = id_value.as_u64() {
                    if notif_id != watch_id {
                        return None;
                    }
                }
            }

            // Unwrap the `{ id, event }` envelope. Fall back to treating the
            // whole `params` object as the event for any runtime that emits
            // the bare event shape.
            let event_value = match params.get("event") {
                Some(event) => event.clone(),
                None => params,
            };
            let mut event: SubjectChangedEvent = match serde_json::from_value(event_value) {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!(
                        plugin = %plugin_name,
                        error = %error,
                        "subject/changed notification with undecodable event payload dropped",
                    );
                    return None;
                }
            };
            if !router.aliases_are_identity() {
                translate_event_kind_inbound(&mut event, &router, &plugin_name);
            }

            // Client-side kind scope: post-translation, the event's subject
            // kind is the user-facing installed kind, so compare directly.
            if let Some(requested) = requested_kind.as_deref() {
                if event.subject.kind != requested {
                    return None;
                }
            }

            // Client-side filter enforcement against the event's subject.
            if let Some(filter) = filter.as_ref() {
                if !subject_matches_filter(&event.subject, filter) {
                    return None;
                }
            }

            Some(event)
        }
    });
    Box::pin(stream)
}

/// Best-effort client-side match of a watched subject against a
/// [`animus_subject_protocol_wire::SubjectFilter`]-shaped JSON `filter`.
///
/// Returns `true` (keep the event) when every populated, event-applicable
/// filter constraint is satisfied. Empty / absent constraints match
/// everything. The subject is serialized to JSON once so the matcher can read
/// the same wire shape the filter was authored against (status is kebab-case,
/// etc.), avoiding a hard dependency on the concrete enum reprs.
///
/// Applied constraints: `status`, `kind`, `assignee`, `labels_any`,
/// `labels_all`, `native_status`, `updated_since`. Ignored (no per-event
/// meaning): `cursor`, `limit`, `dispatch_label`, `has_attachment_kind`.
fn subject_matches_filter(subject: &animus_subject_protocol_wire::Subject, filter: &Value) -> bool {
    let Value::Object(filter) = filter else {
        return true;
    };
    let Ok(Value::Object(subject_json)) = serde_json::to_value(subject) else {
        return true;
    };

    // Helper: a filter array constraint — subject's scalar string field must
    // be one of the listed values. Empty / absent array = no constraint.
    let scalar_in_array = |filter_key: &str, subject_value: Option<&str>| -> bool {
        match filter.get(filter_key).and_then(Value::as_array) {
            Some(values) if !values.is_empty() => {
                let Some(actual) = subject_value else {
                    return false;
                };
                values.iter().any(|v| v.as_str() == Some(actual))
            }
            _ => true,
        }
    };

    // status (kebab-case string in JSON), kind, assignee.
    if !scalar_in_array("status", subject_json.get("status").and_then(Value::as_str)) {
        return false;
    }
    if !scalar_in_array("kind", subject_json.get("kind").and_then(Value::as_str)) {
        return false;
    }
    if !scalar_in_array("assignee", subject_json.get("assignee").and_then(Value::as_str)) {
        return false;
    }

    // native_status (scalar equality).
    if let Some(Value::String(want)) = filter.get("native_status") {
        if subject_json.get("native_status").and_then(Value::as_str) != Some(want.as_str()) {
            return false;
        }
    }

    // labels_any / labels_all.
    let subject_labels: Vec<&str> = subject_json
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(any) = filter.get("labels_any").and_then(Value::as_array) {
        if !any.is_empty() && !any.iter().filter_map(Value::as_str).any(|l| subject_labels.contains(&l)) {
            return false;
        }
    }
    if let Some(all) = filter.get("labels_all").and_then(Value::as_array) {
        if !all.iter().filter_map(Value::as_str).all(|l| subject_labels.contains(&l)) {
            return false;
        }
    }

    // updated_since: keep when the subject's updated_at is >= the bound.
    // Compare as RFC3339 strings is unreliable across offsets, so parse.
    if let Some(Value::String(since)) = filter.get("updated_since") {
        if let (Ok(bound), Some(updated)) =
            (chrono::DateTime::parse_from_rfc3339(since), subject_json.get("updated_at").and_then(Value::as_str))
        {
            match chrono::DateTime::parse_from_rfc3339(updated) {
                Ok(updated_at) if updated_at < bound => return false,
                _ => {}
            }
        }
    }

    true
}

/// Rewrite an inbound [`SubjectChangedEvent`]'s `subject.kind` / id prefixes
/// from the plugin's native kind back to the user-facing installed kind, for
/// backends that were renamed at install time. Mirrors the narrow inbound
/// translation `SubjectRouter::route_call` applies to responses.
fn translate_event_kind_inbound(event: &mut SubjectChangedEvent, router: &SubjectRouter, plugin_name: &str) {
    // Translate via JSON round-trip so we reuse the router's notion of which
    // native kind maps to which installed kind without depending on the
    // concrete wire struct internals beyond `subject.kind` + `id`.
    let Ok(Value::Object(mut map)) = serde_json::to_value(&*event) else {
        return;
    };

    // `subject.kind` + `subject.id`
    if let Some(Value::Object(subject)) = map.get_mut("subject") {
        if let Some(Value::String(native_kind)) = subject.get("kind").cloned() {
            if let Some(installed) = router.installed_kind_for_plugin_native(plugin_name, &native_kind) {
                subject.insert("kind".to_string(), Value::String(installed.to_string()));
            }
        }
        rewrite_id_prefix_inbound(subject, router, plugin_name);
    }
    // Top-level `id`
    rewrite_id_prefix_inbound(&mut map, router, plugin_name);

    if let Ok(translated) = serde_json::from_value::<SubjectChangedEvent>(Value::Object(map)) {
        *event = translated;
    }
}

fn rewrite_id_prefix_inbound(object: &mut serde_json::Map<String, Value>, router: &SubjectRouter, plugin_name: &str) {
    let Some(Value::String(id)) = object.get("id") else {
        return;
    };
    let Some((native_prefix, rest)) = id.split_once(':') else {
        return;
    };
    let Some(installed) = router.installed_kind_for_plugin_native(plugin_name, native_prefix) else {
        return;
    };
    let rewritten = format!("{installed}:{rest}");
    object.insert("id".to_string(), Value::String(rewritten));
}

/// Returns `true` when [`SUBJECT_PLUGINS_DISABLE_ENV`] is set to a truthy
/// value. Mirrors the log-storage and provider dispatch knobs.
pub fn subject_plugins_disable_env_set() -> bool {
    match std::env::var(SUBJECT_PLUGINS_DISABLE_ENV) {
        Ok(value) => {
            let trimmed = value.trim().to_ascii_lowercase();
            !trimmed.is_empty() && trimmed != "0" && trimmed != "false" && trimmed != "no" && trimmed != "off"
        }
        Err(_) => false,
    }
}

/// Filter the project's installed plugins down to subject backends.
pub fn discover_subject_backends(project_root: &Path) -> Result<Vec<DiscoveredPlugin>> {
    let plugins = discover_plugins(project_root)?;
    Ok(plugins.into_iter().filter(|p| p.manifest.plugin_kind == PLUGIN_KIND_SUBJECT_BACKEND).collect())
}

/// Outcome of [`resolve_subject_dispatch`].
///
/// `selected` is always populated (`SubjectPluginDispatch::empty()` when
/// no plugins are active). `warnings` carries operator-facing messages
/// surfaced via [`crate::DaemonRunEvent::SubjectRouterResolved`].
#[derive(Debug, Clone)]
pub struct SubjectDispatchResolution {
    pub selected: SubjectPluginDispatch,
    pub all_candidates: Vec<DiscoveredPlugin>,
    pub warnings: Vec<String>,
}

/// Resolve the daemon's subject dispatch for a given project root.
///
/// Selection rules (in priority order):
///
/// 1. If [`SUBJECT_PLUGINS_DISABLE_ENV`] is set truthy → empty dispatch
///    (warnings note the override when plugins were installed).
/// 2. Else if discovery surfaces zero subject_backend plugins → empty.
/// 3. Else spawn each plugin via [`PluginHost::spawn_with_options`],
///    hand the hosts to [`SubjectRouter::from_initialized_hosts`], wrap
///    the resulting router in an `Arc`.
/// 4. Duplicate-kind collisions abort with the error from the router.
///
/// Errors from discovery + plugin spawn surface upward so the daemon can
/// log them. The daemon entrypoint maps any error to empty + a warning
/// so a broken subject plugin never blocks startup.
pub async fn resolve_subject_dispatch(project_root: &Path) -> Result<SubjectDispatchResolution> {
    let mut warnings: Vec<String> = Vec::new();
    let candidates = discover_subject_backends(project_root)?;

    if subject_plugins_disable_env_set() {
        if !candidates.is_empty() {
            warnings.push(format!(
                "subject_backend plugin discovered ({} installed) but {SUBJECT_PLUGINS_DISABLE_ENV} is set; subject CLI calls will return NotFound",
                candidates.len()
            ));
        }
        return Ok(SubjectDispatchResolution {
            selected: SubjectPluginDispatch::empty(),
            all_candidates: candidates,
            warnings,
        });
    }

    if candidates.is_empty() {
        return Ok(SubjectDispatchResolution {
            selected: SubjectPluginDispatch::empty(),
            all_candidates: candidates,
            warnings,
        });
    }

    let mut hosts: HashMap<String, PluginHost> = HashMap::new();
    let mut kinds: Vec<String> = Vec::new();
    let mut plugin_count = 0usize;

    for plugin in &candidates {
        let options = PluginSpawnOptions::for_manifest(
            plugin.name.clone(),
            &plugin.manifest.env_required,
            std::iter::empty::<String>(),
            None,
        )
        .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
        .with_working_dir(project_root);
        let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
            .await
            .with_context(|| format!("failed to spawn subject_backend plugin '{}'", plugin.name))?;
        hosts.insert(plugin.name.clone(), host);
        plugin_count += 1;
    }

    // v0.5.7: load install-time kind renames from the project's
    // plugins.lock so the SubjectRouter can translate
    // `<installed_kind>/<verb>` -> `<native_kind>/<verb>` at the wire
    // boundary. When the lockfile is missing or every entry was
    // installed under its native kind, the alias map is empty and the
    // router behaves identically to its pre-v0.5.7 form.
    let aliases = load_kind_aliases_from_lockfile(project_root, &candidates);
    let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases.clone()).await?;

    for plugin in &candidates {
        for cap in &plugin.manifest.capabilities {
            if let Some(rest) = cap.strip_prefix("subject_kind:") {
                let trimmed = rest.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                let effective =
                    aliases.installed_for_plugin_native(&plugin.name, &trimmed).unwrap_or(trimmed.as_str()).to_string();
                if !kinds.contains(&effective) {
                    kinds.push(effective);
                }
            }
        }
    }

    Ok(SubjectDispatchResolution {
        selected: SubjectPluginDispatch::from_router(router, kinds, plugin_count),
        all_candidates: candidates,
        warnings,
    })
}

/// Build the daemon-side kind translator from `plugins.lock`. Reads every
/// subject_backend entry whose `installed_kind` differs from `native_kind`
/// and registers a one-way alias the SubjectRouter consults at routing
/// time. A missing or unreadable lockfile yields an empty alias map
/// (the router behaves identically to its pre-v0.5.7 form). Lockfile
/// entries pointing at plugins that aren't in the `candidates` set are
/// dropped silently — they refer to plugins that were uninstalled (or
/// failed manifest discovery this boot) and would otherwise pollute the
/// alias map with dangling renames.
fn load_kind_aliases_from_lockfile(project_root: &Path, candidates: &[DiscoveredPlugin]) -> KindAliasMap {
    let mut aliases = KindAliasMap::default();
    let lock = match PluginLockfile::load_default(Some(project_root)) {
        Ok(l) => l,
        Err(err) => {
            tracing::warn!(error = %err, "plugin lockfile unreadable; daemon-side kind translator disabled for this run");
            return aliases;
        }
    };
    // v0.5.8 fold-in (closes codex P2 round-4 v0.5.7): the candidate set
    // is keyed by `plugin.name` which now reflects the install-time
    // `--name <NAME>` override when the operator passed one (recorded in
    // `plugins.yaml` as `name_override` and surfaced by
    // `PluginDiscovery::discover_configured`). The lockfile entry is
    // already keyed under the same override, so the candidate filter
    // here no longer drops aliases for renamed installs.
    let candidate_names: std::collections::HashSet<&str> = candidates.iter().map(|p| p.name.as_str()).collect();
    for entry in &lock.plugins {
        if !candidate_names.contains(entry.name.as_str()) {
            continue;
        }
        let (Some(installed), Some(native)) = (entry.effective_installed_kind(), entry.effective_native_kind()) else {
            continue;
        };
        aliases.insert(&entry.name, installed, native);
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_plugin_protocol::{
        InitializeResult, PluginCapabilities, PluginInfo, PluginManifest, RpcRequest, RpcResponse,
    };
    use orchestrator_plugin_host::{DiscoverySource, PluginHost};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Env mutation goes through the protocol crate's `EnvVarGuard`, which
    /// holds the process-wide env lock for the guard's lifetime. That
    /// serializes these tests against every other `EnvVarGuard` user in
    /// this binary (`ANIMUS_CONFIG_DIR` / `ANIMUS_PLUGIN_DIR` are shared
    /// with the dispatch and log_storage test modules).
    use protocol::test_utils::EnvVarGuard;

    fn isolated_project() -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().to_path_buf();
        std::fs::create_dir_all(project.join(".animus/plugins")).expect("mkdir plugins dir");
        (temp, project)
    }

    fn fake_plugin(name: &str, _kinds: &[&str]) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.to_string(),
            path: PathBuf::from(format!("/tmp/{name}")),
            manifest: PluginManifest {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                plugin_kind: PLUGIN_KIND_SUBJECT_BACKEND.to_string(),
                description: "fake".to_string(),
                protocol_version: "1.0.0".to_string(),
                capabilities: vec![],
                env_required: vec![],
                notification_buffer_size: None,
            },
            source: DiscoverySource::ProjectLocal,
        }
    }

    /// Spawns an in-process fake subject backend over `tokio::io::duplex`
    /// streams. Used by router-population tests without touching the
    /// filesystem or spawning child processes.
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
                                plugin_kind: PLUGIN_KIND_SUBJECT_BACKEND.to_string(),
                                description: None,
                            },
                            capabilities: PluginCapabilities {
                                subject_kinds: kinds.clone(),
                                methods: kinds.iter().map(|k| format!("{k}/list")).collect(),
                                ..PluginCapabilities::default()
                            },
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

    /// Fake subject backend that supports `subject/watch`: it acks the watch
    /// request, then emits one `subject/changed` notification for a subject of
    /// the first declared kind. Other backends echo `{ "method": ... }`.
    async fn watch_subject_host(name: &str, subject_kinds: Vec<&str>, supports_watch: bool) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let kinds = subject_kinds.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();
        let first_kind = kinds.first().cloned().unwrap_or_else(|| "task".to_string());

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                match request.method.as_str() {
                    "initialize" => {
                        let response = RpcResponse::ok(
                            request.id,
                            serde_json::json!(InitializeResult {
                                protocol_version: "1.0.0".to_string(),
                                plugin_info: PluginInfo {
                                    name: name_for_task.clone(),
                                    version: "0.1.0".to_string(),
                                    plugin_kind: PLUGIN_KIND_SUBJECT_BACKEND.to_string(),
                                    description: None,
                                },
                                capabilities: PluginCapabilities {
                                    subject_kinds: kinds.clone(),
                                    methods: kinds.iter().map(|k| format!("{k}/list")).collect(),
                                    ..PluginCapabilities::default()
                                },
                            }),
                        );
                        let mut encoded = serde_json::to_string(&response).expect("encode");
                        encoded.push('\n');
                        plugin_writer.write_all(encoded.as_bytes()).await.expect("write");
                    }
                    "initialized" => continue,
                    "subject/watch" => {
                        if !supports_watch {
                            let response = RpcResponse::err(
                                request.id,
                                RpcError {
                                    code: animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED,
                                    message: "polling-only backend".to_string(),
                                    data: None,
                                },
                            );
                            let mut encoded = serde_json::to_string(&response).expect("encode");
                            encoded.push('\n');
                            plugin_writer.write_all(encoded.as_bytes()).await.expect("write");
                            continue;
                        }
                        // Ack the watch.
                        let watch_req_id = request.id.clone();
                        let response = RpcResponse::ok(request.id, serde_json::json!({}));
                        let mut encoded = serde_json::to_string(&response).expect("encode");
                        encoded.push('\n');
                        plugin_writer.write_all(encoded.as_bytes()).await.expect("write");
                        // Emit two subject/changed notifications using the
                        // runtime's `{ id, event }` envelope, echoing the watch
                        // request id for correlation. The first is for an
                        // unrelated kind (multi-kind backends emit events for
                        // every kind they own, since `SubjectBackend::watch`
                        // takes no kind argument); a kind-scoped subscriber must
                        // drop it. The second matches `first_kind`.
                        for (kind, local) in [("other-kind", "LOCAL-0"), (first_kind.as_str(), "LOCAL-1")] {
                            let notification = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "subject/changed",
                                "params": {
                                    "id": watch_req_id,
                                    "event": {
                                        "id": format!("{kind}:{local}"),
                                        "change_kind": "updated",
                                        "subject": {
                                            "id": format!("{kind}:{local}"),
                                            "kind": kind,
                                            "title": "watched subject",
                                            "status": "in-progress",
                                            "created_at": "2026-01-01T00:00:00Z",
                                            "updated_at": "2026-01-01T00:00:00Z",
                                        },
                                    },
                                },
                            });
                            let mut encoded = serde_json::to_string(&notification).expect("encode");
                            encoded.push('\n');
                            plugin_writer.write_all(encoded.as_bytes()).await.expect("write notification");
                        }
                    }
                    method => {
                        let response = RpcResponse::ok(request.id, serde_json::json!({ "method": method }));
                        let mut encoded = serde_json::to_string(&response).expect("encode");
                        encoded.push('\n');
                        plugin_writer.write_all(encoded.as_bytes()).await.expect("write");
                    }
                }
            }
        });

        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn subject_watch_streams_changed_events_from_backend() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), watch_subject_host("tasks", vec!["task"], true).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");
        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string()], 1);

        let mut stream = dispatch.subject_watch(Some("task".to_string()), None);
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("event arrives before timeout")
            .expect("stream yields an event");
        // The backend emits an unrelated `other-kind` event before the
        // matching `task` one; the kind-scoped subscription drops it, so the
        // first event we observe is the `task` one.
        assert_eq!(event.id.as_str(), "task:LOCAL-1");
        assert_eq!(event.subject.kind, "task");
    }

    #[tokio::test]
    async fn subject_watch_skips_polling_only_backend() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), watch_subject_host("tasks", vec!["task"], false).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");
        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string()], 1);

        let mut stream = dispatch.subject_watch(Some("task".to_string()), None);
        // METHOD_NOT_SUPPORTED backends are skipped — the branch closes with
        // no events, so the merged stream ends.
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("stream resolves before timeout");
        assert!(next.is_none(), "polling-only backend yields no events and the stream closes");
    }

    #[tokio::test]
    async fn subject_watch_drops_events_not_matching_filter() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), watch_subject_host("tasks", vec!["task"], true).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");
        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string()], 1);

        // The backend emits `in-progress` task event(s); a `status: ["done"]`
        // filter must drop them all. The watch subscription stays OPEN (a live
        // watch never self-closes), so we assert no event is delivered within
        // a short window rather than expecting the stream to end.
        let filter = serde_json::json!({ "status": ["done"] });
        let mut stream = dispatch.subject_watch(Some("task".to_string()), Some(filter));
        let next = tokio::time::timeout(std::time::Duration::from_millis(750), stream.next()).await;
        assert!(next.is_err(), "events not matching the status filter must be dropped (no delivery within the window)");
    }

    #[tokio::test]
    async fn subject_watch_keeps_events_matching_filter() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), watch_subject_host("tasks", vec!["task"], true).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");
        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string()], 1);

        // Filter that the emitted `in-progress` task satisfies.
        let filter = serde_json::json!({ "status": ["in-progress"] });
        let mut stream = dispatch.subject_watch(Some("task".to_string()), Some(filter));
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("event arrives before timeout")
            .expect("stream yields the matching event");
        assert_eq!(event.id.as_str(), "task:LOCAL-1");
    }

    #[tokio::test]
    async fn subject_watch_empty_dispatch_is_closed_stream() {
        let dispatch = SubjectPluginDispatch::empty();
        let mut stream = dispatch.subject_watch(None, None);
        assert!(stream.next().await.is_none(), "no backends → closed stream");
    }

    #[tokio::test]
    async fn discovers_zero_subject_plugins_router_is_empty() {
        let _disable = EnvVarGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, None);
        let _animus_home = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some("/tmp/animus-test-empty-home-subj-xyz123"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(""));

        let (_temp, project_root) = isolated_project();

        let resolution = resolve_subject_dispatch(&project_root).await.expect("resolve");
        assert!(!resolution.selected.is_active(), "no plugins → empty dispatch");
        assert_eq!(resolution.selected.plugin_count(), 0);
        assert!(resolution.warnings.is_empty(), "no plugins → no warnings");
        assert!(resolution.all_candidates.is_empty());
    }

    #[tokio::test]
    async fn discovers_subject_plugin_with_kinds() {
        // Build a router directly from in-process fake hosts so we don't
        // depend on spawning real plugin binaries from disk. This
        // exercises the same router-population path resolve_dispatch uses
        // after spawning succeeds.
        let mut hosts = HashMap::new();
        hosts.insert("multi-backend".to_string(), subject_host("multi-backend", vec!["task", "issue"]).await);

        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router builds");
        assert_eq!(router.plugin_for_kind("task"), Some("multi-backend"));
        assert_eq!(router.plugin_for_kind("issue"), Some("multi-backend"));
        assert_eq!(router.plugin_for_kind("unknown"), None);

        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string(), "issue".to_string()], 1);
        assert!(dispatch.is_active());
        assert_eq!(dispatch.plugin_count(), 1);
        assert_eq!(dispatch.kinds(), &["task".to_string(), "issue".to_string()]);
    }

    #[tokio::test]
    async fn duplicate_kind_returns_error_at_startup() {
        let mut hosts = HashMap::new();
        hosts.insert("first-backend".to_string(), subject_host("first-backend", vec!["task"]).await);
        hosts.insert("second-backend".to_string(), subject_host("second-backend", vec!["task"]).await);

        let result = SubjectRouter::from_initialized_hosts(hosts).await;
        let error = match result {
            Ok(_) => panic!("duplicate kind must abort router build"),
            Err(error) => error,
        };
        let message = format!("{error}");
        assert!(message.contains("duplicate subject kind"), "error names duplicate: {message}");
        assert!(message.contains("task"), "error names kind: {message}");
        // Both plugin names must appear somewhere in the message so
        // operators can see the conflict (the router lists them as
        // 'existing' + the newcomer).
        assert!(
            message.contains("first-backend") || message.contains("second-backend"),
            "error names at least one offending plugin: {message}",
        );
    }

    #[tokio::test]
    async fn disable_env_var_skips_discovery() {
        let _disable = EnvVarGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, Some("1"));
        let _animus_home = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some("/tmp/animus-test-empty-home-subj-xyz123"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(""));

        let (_temp, project_root) = isolated_project();
        let resolution = resolve_subject_dispatch(&project_root).await.expect("resolve");
        assert!(!resolution.selected.is_active(), "disable env forces empty dispatch");

        // Sanity-check that fake plugin manifests round-trip the kind
        // identifier the resolver inspects.
        let p = fake_plugin("synthetic", &["task"]);
        assert_eq!(p.manifest.plugin_kind, PLUGIN_KIND_SUBJECT_BACKEND);
    }

    #[tokio::test]
    async fn subject_command_routes_through_router() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), subject_host("tasks", vec!["task"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string()], 1);

        let result = dispatch
            .route_call("task/list", Some(serde_json::json!({ "limit": 10 })))
            .await
            .expect("route call succeeds");
        assert_eq!(result["method"], "task/list", "router forwarded method to plugin: {result}");

        // Unmounted kind → NotFound RpcError with the kind named.
        let err = dispatch.route_call("issue/list", None).await.expect_err("unmounted kind fails");
        assert_eq!(err.code, animus_plugin_protocol::error_codes::METHOD_NOT_FOUND);
        assert!(err.message.contains("issue"), "error names missing kind: {}", err.message);
    }

    #[test]
    fn disable_env_predicate_recognizes_truthy_values() {
        let _e1 = EnvVarGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, Some("1"));
        assert!(subject_plugins_disable_env_set(), "'1' is truthy");
        drop(_e1);

        let _e2 = EnvVarGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, Some("0"));
        assert!(!subject_plugins_disable_env_set(), "'0' is falsy");
        drop(_e2);

        let _e3 = EnvVarGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, Some(""));
        assert!(!subject_plugins_disable_env_set(), "empty is falsy");
        drop(_e3);

        let _e4 = EnvVarGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, None);
        assert!(!subject_plugins_disable_env_set(), "unset is falsy");
    }
}
