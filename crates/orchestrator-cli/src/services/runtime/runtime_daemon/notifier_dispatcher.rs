use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, PluginHost, PluginSpawnOptions};
use protocol::DaemonEventRecord;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

const PLUGIN_KIND_NOTIFIER: &str = "notifier";
const METHOD_NOTIFIER_NOTIFY: &str = "notifier/notify";
const METHOD_NOTIFIER_FLUSH: &str = "notifier/flush";
const NOTIFIER_NOTIFY_TIMEOUT: Duration = Duration::from_secs(15);

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
}

impl NotifierPluginDispatcher {
    pub fn discover(project_root: &str) -> Result<Self> {
        let project_root = PathBuf::from(project_root);
        let plugins = discover_by_kind(&project_root, PLUGIN_KIND_NOTIFIER)
            .with_context(|| format!("failed to discover notifier plugins for {}", project_root.display()))?;
        Ok(Self {
            project_root,
            plugins,
            hosts: Arc::new(AsyncMutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(Vec::new())),
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
        tokio::spawn(async move {
            let params = json!({ "event": event });
            for plugin in &plugins {
                let host = match Self::host_for(&hosts, plugin, &project_root).await {
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
    }

    /// Ask every installed notifier plugin to flush pending deliveries
    /// and return their lifecycle events plus any events accumulated by
    /// background `dispatch` calls.
    pub async fn flush(&self) -> Vec<NotifierLifecycleEvent> {
        let mut lifecycle = self.drain_lifecycle_events();
        if self.plugins.is_empty() {
            return lifecycle;
        }
        let params = json!({});
        for plugin in &self.plugins {
            let host = match Self::host_for(&self.hosts, plugin, &self.project_root).await {
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
    ) -> Result<PluginHost> {
        let mut guard = hosts.lock().await;
        if let Some((_, host)) = guard.iter().find(|(name, _)| name == &plugin.name) {
            return Ok(host.clone());
        }
        let extra_env: Vec<String> = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect();
        let options =
            PluginSpawnOptions::for_manifest(plugin.name.clone(), &plugin.manifest.env_required, extra_env, None)
                .with_working_dir(project_root);
        let host = PluginHost::spawn_with_options(&plugin.path, &[], options).await?;
        guard.push((plugin.name.clone(), host.clone()));
        Ok(host)
    }
}
