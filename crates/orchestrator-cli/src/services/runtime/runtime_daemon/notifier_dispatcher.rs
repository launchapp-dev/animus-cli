use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, PluginHost, PluginSpawnOptions};
use protocol::DaemonEventRecord;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tracing::warn;

const PLUGIN_KIND_NOTIFIER: &str = "notifier";
const METHOD_NOTIFIER_NOTIFY: &str = "notifier/notify";
const METHOD_NOTIFIER_FLUSH: &str = "notifier/flush";
const NOTIFIER_NOTIFY_TIMEOUT: Duration = Duration::from_secs(15);
const NOTIFIER_INIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct NotifierLifecycleEvent {
    pub event_type: String,
    pub project_root: Option<String>,
    pub data: Value,
}

#[derive(Clone)]
pub struct NotifierPluginDispatcher {
    project_root: PathBuf,
    plugins: Vec<DiscoveredPlugin>,
    hosts: Arc<AsyncMutex<Vec<(String, PluginHost)>>>,
    pending: Arc<Mutex<Vec<NotifierLifecycleEvent>>>,
    // Track in-flight dispatch tasks so shutdown waits for them before
    // exit. Closes codex round-1 P2: fire-and-forget tasks could be
    // cancelled before the plugin receives the final status event when the
    // daemon runs --once or shuts down soon after emitting. Regular flush
    // does NOT await these (daemon tick stays responsive — codex round-4 P2).
    in_flight: Arc<Mutex<Vec<JoinHandle<()>>>>,
    // Env vars referenced by the project's notification_config (any
    // `*_env` string value, e.g. `url_env`, `headers_env`, `bearer_token_env`).
    // Computed once at discover() and forwarded to the notifier plugin spawn.
    // Closes codex round-4 P2 (credential env regression) without
    // re-leaking the daemon's full environment (codex round-1 P1).
    notifier_env_allowlist: Vec<String>,
}

impl NotifierPluginDispatcher {
    pub fn discover(project_root: &str) -> Result<Self> {
        let project_root = PathBuf::from(project_root);
        let plugins = discover_by_kind(&project_root, PLUGIN_KIND_NOTIFIER)
            .with_context(|| format!("failed to discover notifier plugins for {}", project_root.display()))?;
        let notifier_env_allowlist = read_notifier_env_allowlist(&project_root);
        Ok(Self {
            project_root,
            plugins,
            hosts: Arc::new(AsyncMutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::new(Mutex::new(Vec::new())),
            notifier_env_allowlist,
        })
    }

    pub fn has_notifiers(&self) -> bool {
        !self.plugins.is_empty()
    }

    /// Drain accumulated lifecycle events produced by background notifier
    /// dispatches. Called on each tick by `flush_notifications`.
    pub fn drain_lifecycle_events(&self) -> Vec<NotifierLifecycleEvent> {
        let mut guard = self.pending.lock().expect("notifier pending mutex poisoned");
        std::mem::take(&mut *guard)
    }

    /// Fire-and-forget dispatch of one event to every installed notifier
    /// plugin. Lifecycle events returned by the plugins land in the
    /// `pending` queue and surface on the next `flush_notifications`
    /// tick. Errors are logged via `tracing::warn!` and never propagate
    /// back into the daemon's event loop.
    pub fn dispatch(&self, event: DaemonEventRecord) {
        if self.plugins.is_empty() {
            return;
        }
        let plugins = self.plugins.clone();
        let project_root = self.project_root.clone();
        let hosts = self.hosts.clone();
        let pending = self.pending.clone();
        let in_flight = self.in_flight.clone();
        let env_allowlist = self.notifier_env_allowlist.clone();
        let handle = tokio::spawn(async move {
            let params = json!({ "event": event });
            for plugin in &plugins {
                let host = match Self::host_for(&hosts, plugin, &project_root, &env_allowlist).await {
                    Ok(host) => host,
                    Err(error) => {
                        warn!(plugin = %plugin.name, %error, "failed to spawn notifier plugin");
                        continue;
                    }
                };
                match host
                    .request_with_timeout(METHOD_NOTIFIER_NOTIFY, Some(params.clone()), NOTIFIER_NOTIFY_TIMEOUT)
                    .await
                {
                    Ok(value) => Self::capture_lifecycle(&pending, &value),
                    Err(error) => {
                        warn!(plugin = %plugin.name, code = error.code, message = %error.message, "notifier/notify failed");
                    }
                }
            }
        });
        // Register the handle so flush + shutdown await it. Codex round-1 P2.
        let mut guard = in_flight.lock().expect("notifier in_flight mutex poisoned");
        // Drop already-completed handles so the vec doesn't grow unbounded
        // during long daemon runs.
        guard.retain(|h| !h.is_finished());
        guard.push(handle);
    }

    /// Ask every installed notifier plugin to flush pending deliveries
    /// and return their lifecycle events plus any events accumulated by
    /// background `dispatch` calls. Called every daemon tick; does NOT
    /// await in-flight dispatch tasks (codex round-4 P2: slow notifier
    /// would otherwise stall the scheduler). Use `shutdown_drain()` from
    /// the shutdown path to wait for the final status events.
    pub async fn flush(&self) -> Vec<NotifierLifecycleEvent> {
        let mut lifecycle = self.drain_lifecycle_events();
        if self.plugins.is_empty() {
            return lifecycle;
        }
        let params = json!({});
        for plugin in &self.plugins {
            let host = match Self::host_for(&self.hosts, plugin, &self.project_root, &self.notifier_env_allowlist).await
            {
                Ok(host) => host,
                Err(error) => {
                    warn!(plugin = %plugin.name, %error, "failed to spawn notifier plugin");
                    continue;
                }
            };
            match host.request_with_timeout(METHOD_NOTIFIER_FLUSH, Some(params.clone()), NOTIFIER_NOTIFY_TIMEOUT).await
            {
                Ok(value) => {
                    let pending = self.pending.clone();
                    Self::capture_lifecycle(&pending, &value);
                    lifecycle.extend(self.drain_lifecycle_events());
                }
                Err(error) => {
                    warn!(plugin = %plugin.name, code = error.code, message = %error.message, "notifier/flush failed");
                }
            }
        }
        lifecycle
    }

    /// Wait for every in-flight `dispatch` task to complete. Called by the
    /// daemon's shutdown path (and the `--once` exit path) so the final
    /// status events reach the plugin before the Tokio runtime exits.
    /// Codex round-1 P2 + codex round-4 P2 (don't block regular ticks).
    pub async fn shutdown_drain(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut guard = self.in_flight.lock().expect("notifier in_flight mutex poisoned");
            std::mem::take(&mut *guard)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }

    fn capture_lifecycle(pending: &Arc<Mutex<Vec<NotifierLifecycleEvent>>>, value: &Value) {
        let Some(events) = value.get("lifecycle_events").and_then(|v| v.as_array()) else {
            return;
        };
        let mut guard = pending.lock().expect("notifier pending mutex poisoned");
        for raw in events {
            let event_type = raw.get("event_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if event_type.is_empty() {
                continue;
            }
            let project_root = raw.get("project_root").and_then(|v| v.as_str()).map(ToOwned::to_owned);
            let data = raw.get("data").cloned().unwrap_or_else(|| json!({}));
            guard.push(NotifierLifecycleEvent { event_type, project_root, data });
        }
    }

    async fn host_for(
        hosts: &Arc<AsyncMutex<Vec<(String, PluginHost)>>>,
        plugin: &DiscoveredPlugin,
        project_root: &PathBuf,
        env_allowlist: &[String],
    ) -> Result<PluginHost> {
        let mut guard = hosts.lock().await;
        if let Some((_, host)) = guard.iter().find(|(name, _)| name == &plugin.name) {
            return Ok(host.clone());
        }
        // Codex round-1 P1 + round-4 P2: forward ONLY the env var names the
        // project's notification_config explicitly references (any `*_env`
        // string value: `url_env`, `headers_env`, `bearer_token_env`, etc.).
        // Provider API keys, daemon-internal tokens, and other unrelated
        // secrets stay out of the notifier subprocess.
        let options = PluginSpawnOptions::for_manifest(
            plugin.name.clone(),
            &plugin.manifest.env_required,
            env_allowlist.to_vec(),
            None,
        )
        .with_working_dir(project_root);
        let host = PluginHost::spawn_with_options(&plugin.path, &[], options).await?;

        // Codex round-1 P2: run the standard initialize/initialized handshake
        // before invoking role methods. Matches the plugin_clients.rs pattern
        // (project_binding + memory_mcp_stdio_command) so notifier plugins
        // behave like every other plugin host in the codebase.
        let repo_scope = protocol::repository_scope_for_path(project_root);
        let mut init_extensions = serde_json::Map::new();
        init_extensions.insert(
            "project_binding".to_string(),
            json!({
                "project_root": project_root.to_string_lossy(),
                "repo_scope": repo_scope,
            }),
        );
        if let Ok(self_path) = std::env::current_exe() {
            init_extensions
                .insert("memory_mcp_stdio_command".to_string(), json!({ "command": self_path.to_string_lossy() }));
        }
        let init_params = json!({
            "protocol_version": "1.1.0",
            "host_info": { "name": "animus", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "streaming": false, "progress": false, "cancellation": false },
            "init_extensions": init_extensions,
        });
        host.request_typed_with_timeout("initialize", Some(init_params), NOTIFIER_INIT_TIMEOUT)
            .await
            .with_context(|| format!("notifier plugin '{}' initialize failed", plugin.name))?;
        host.notify("initialized", None)
            .await
            .with_context(|| format!("notifier plugin '{}' initialized notification failed", plugin.name))?;

        guard.push((plugin.name.clone(), host.clone()));
        Ok(host)
    }
}

/// Read the daemon project config (`pm-config.json` under the scoped
/// daemon config dir — NOT under `<project>/.animus/`) and collect every
/// env var name referenced inside `notification_config` via a `*_env`
/// string OR a `*_env` map whose VALUES are env-var-name strings (e.g.
/// `"headers_env": { "Authorization": "ANIMUS_NOTIFY_WEBHOOK_BEARER" }`).
/// Returned names become the additional env-var allowlist for notifier
/// plugin spawns. Missing config / parse failure → empty Vec (the daemon
/// simply won't forward any extra env to the notifier).
/// Closes codex v0.5.3 Task D round-5 P2 (wrong path) + P2 (headers_env).
fn read_notifier_env_allowlist(project_root: &std::path::Path) -> Vec<String> {
    let pm_config_path = orchestrator_core::daemon_project_config_path(project_root);
    let Ok(bytes) = std::fs::read(&pm_config_path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let Some(notification_config) = value.get("notification_config") else {
        return Vec::new();
    };
    let mut allowlist: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    collect_env_refs(notification_config, &mut allowlist);
    allowlist.into_iter().collect()
}

/// Walk a JSON value and collect env-var names referenced by every
/// `*_env` key. A `*_env` key whose value is a string contributes that
/// string. A `*_env` key whose value is an object contributes every
/// string value inside that object (the `headers_env` shape:
/// `{ "<header-name>": "<env-var-name>" }`). Recursively descends into
/// nested arrays and objects so config compositions still work.
fn collect_env_refs(value: &Value, out: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                if key.ends_with("_env") {
                    match val {
                        Value::String(name) => {
                            let trimmed = name.trim();
                            if !trimmed.is_empty() {
                                out.insert(trimmed.to_string());
                            }
                        }
                        Value::Object(inner) => {
                            for inner_val in inner.values() {
                                if let Some(name) = inner_val.as_str() {
                                    let trimmed = name.trim();
                                    if !trimmed.is_empty() {
                                        out.insert(trimmed.to_string());
                                    }
                                }
                            }
                        }
                        Value::Array(arr) => {
                            for item in arr {
                                if let Some(name) = item.as_str() {
                                    let trimmed = name.trim();
                                    if !trimmed.is_empty() {
                                        out.insert(trimmed.to_string());
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                collect_env_refs(val, out);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_env_refs(item, out);
            }
        }
        _ => {}
    }
}
