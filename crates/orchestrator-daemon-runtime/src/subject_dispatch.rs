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
use std::sync::Arc;

use anyhow::{Context, Result};
use orchestrator_plugin_host::{
    discover_plugins, DiscoveredPlugin, KindAliasMap, PluginHost, PluginLockfile, PluginSpawnOptions, SubjectRouter,
};
use serde_json::Value;

use animus_plugin_protocol::{RpcError, PLUGIN_KIND_SUBJECT_BACKEND};

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
    // TODO(codex-p2 round-4 v0.5.7): the candidate set is keyed by
    // `plugin.name` which comes from discovery (plugins.yaml entry name
    // == manifest name). The lockfile entry can be keyed by the
    // `--name` install override when the operator passed one, so a
    // plugin installed as `--name task-archive` with a manifest name of
    // `animus-subject-default` would have its lockfile row dropped here
    // and the daemon would re-register it under its native kind. Fix
    // requires plumbing the install-time name override through discovery
    // (plugins.yaml currently doesn't record it). Today's defaults flow
    // doesn't pass --name so this is dormant.
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

    /// Serialize env-var-touching tests so cargo's parallel runner does
    /// not race on the disable knob. Uses `tokio::sync::Mutex` (rather
    /// than `std::sync::Mutex`) because these tests are async and need
    /// to hold the lock across `.await` points without tripping the
    /// `clippy::await_holding_lock` lint.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(prev) => std::env::set_var(self.key, prev),
                None => std::env::remove_var(self.key),
            }
        }
    }

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

    #[tokio::test]
    async fn discovers_zero_subject_plugins_router_is_empty() {
        let _guard = ENV_LOCK.lock().await;
        let _disable = EnvGuard::unset(SUBJECT_PLUGINS_DISABLE_ENV);
        let _animus_home = EnvGuard::set("ANIMUS_CONFIG_DIR", "/tmp/animus-test-empty-home-subj-xyz123");
        let _plugin_dir = EnvGuard::set("ANIMUS_PLUGIN_DIR", "");

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
        let _guard = ENV_LOCK.lock().await;
        let _disable = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, "1");
        let _animus_home = EnvGuard::set("ANIMUS_CONFIG_DIR", "/tmp/animus-test-empty-home-subj-xyz123");
        let _plugin_dir = EnvGuard::set("ANIMUS_PLUGIN_DIR", "");

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
        let _guard = ENV_LOCK.blocking_lock();

        let _e1 = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, "1");
        assert!(subject_plugins_disable_env_set(), "'1' is truthy");
        drop(_e1);

        let _e2 = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, "0");
        assert!(!subject_plugins_disable_env_set(), "'0' is falsy");
        drop(_e2);

        let _e3 = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, "");
        assert!(!subject_plugins_disable_env_set(), "empty is falsy");
        drop(_e3);

        let _e4 = EnvGuard::unset(SUBJECT_PLUGINS_DISABLE_ENV);
        assert!(!subject_plugins_disable_env_set(), "unset is falsy");
    }
}
