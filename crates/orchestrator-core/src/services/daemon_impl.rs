use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

/// v0.5.9 in-process freshness cache for [`DaemonHealth`]. Tauri-style
/// flows that call `animus status`, `animus daemon status`, and
/// `animus daemon health` in rapid succession each rebuild the same
/// snapshot from disk. Within a `DAEMON_HEALTH_CACHE_TTL` window the
/// cached value is returned; older entries are rebuilt. Keyed by
/// project root so multiple FileServiceHubs in one process do not
/// cross-pollinate.
const DAEMON_HEALTH_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

fn daemon_health_cache() -> &'static RwLock<HashMap<PathBuf, (Instant, DaemonHealth)>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, (Instant, DaemonHealth)>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

static NO_CACHE_FLAG: AtomicBool = AtomicBool::new(false);

/// Process-global toggle for the in-memory daemon health snapshot
/// cache. Mirrors the `--no-cache` global flag wired in by the CLI.
/// Read paths return `None` when the flag is set so a stale snapshot
/// can never be served on an explicit bypass.
pub fn set_daemon_health_cache_disabled(value: bool) {
    NO_CACHE_FLAG.store(value, Ordering::Relaxed);
}

fn daemon_health_cache_disabled() -> bool {
    NO_CACHE_FLAG.load(Ordering::Relaxed)
}

fn read_daemon_health_cache(project_root: &Path) -> Option<DaemonHealth> {
    if daemon_health_cache_disabled() {
        return None;
    }
    let cache = daemon_health_cache().read().ok()?;
    let (stored_at, value) = cache.get(project_root)?;
    if stored_at.elapsed() < DAEMON_HEALTH_CACHE_TTL {
        Some(value.clone())
    } else {
        None
    }
}

fn write_daemon_health_cache(project_root: &Path, value: &DaemonHealth) {
    if let Ok(mut cache) = daemon_health_cache().write() {
        cache.insert(project_root.to_path_buf(), (Instant::now(), value.clone()));
    }
}

fn invalidate_daemon_health_cache(project_root: &Path) {
    if let Ok(mut cache) = daemon_health_cache().write() {
        cache.remove(project_root);
    }
}

fn daemon_pid_for_status(project_root: &Path) -> Option<u32> {
    #[cfg(test)]
    if let Some(pid) = test_daemon_pid_override() {
        return pid;
    }

    let pm_config_path = crate::daemon_project_config_path(project_root);
    let daemon_dir = pm_config_path.parent()?;
    std::fs::read_to_string(daemon_dir.join("daemon.pid")).ok()?.trim().parse::<u32>().ok()
}

/// Read `(runtime_paused, paused_at)` from the scoped
/// `daemon/daemon-state.json` record. The writer is
/// `orchestrator-daemon-runtime`'s `DaemonRuntimeStateRecord`
/// (`set_runtime_paused`); this crate sits below daemon-runtime in the
/// dependency graph, so the record is read loosely by key name here —
/// keep the key names in sync with the writer.
fn runtime_pause_state_for(project_root: &Path) -> (bool, Option<String>) {
    let path = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".animus"))
        .join("daemon")
        .join("daemon-state.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return (false, None);
    };
    parse_runtime_pause_state(&content)
}

fn parse_runtime_pause_state(content: &str) -> (bool, Option<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return (false, None);
    };
    let paused = value.get("runtime_paused").and_then(serde_json::Value::as_bool).unwrap_or(false);
    if !paused {
        return (false, None);
    }
    let paused_at = value.get("paused_at").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);
    (true, paused_at)
}

fn daemon_process_alive_for_status(pid: u32) -> bool {
    #[cfg(test)]
    if let Some(alive) = test_daemon_process_alive_override() {
        return alive;
    }

    protocol::is_process_alive(pid)
}

async fn mutate_daemon_state<T>(hub: &FileServiceHub, mutator: impl FnOnce(&mut CoreState) -> Result<T>) -> Result<T> {
    #[cfg(test)]
    if test_skip_persist_override() {
        let mut lock = hub.state.write().await;
        return mutator(&mut lock);
    }

    let (output, _) = hub.mutate_persistent_state(mutator).await?;
    Ok(output)
}

/// Fast subset of [`DaemonHealth`] returned by
/// [`load_daemon_status_snapshot_fast`]. Skips the SQLite-backed queued-task
/// count and the provider-plugin executability scan that
/// [`load_daemon_health_snapshot`] does, so callers that only need
/// "is the daemon running?" pay one stat + one pid read + one liveness
/// probe instead of a multi-hundred-millisecond round trip.
#[derive(Debug, Clone)]
pub struct DaemonStatusSnapshot {
    pub status: DaemonStatus,
    pub daemon_pid: Option<u32>,
    pub process_alive: Option<bool>,
}

pub async fn load_daemon_status_snapshot_fast(project_root: &Path) -> Result<DaemonStatusSnapshot> {
    let state_file = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".animus"))
        .join("core-state.json");
    let snapshot = state_store::load_daemon_state_snapshot(&state_file);

    let daemon_pid = daemon_pid_for_status(project_root);
    let process_alive = daemon_pid.map(daemon_process_alive_for_status);

    let mut status = snapshot.daemon_status;
    if matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
        && daemon_pid.is_some()
        && process_alive == Some(false)
    {
        status = DaemonStatus::Crashed;
    }
    if matches!(status, DaemonStatus::Stopped) && process_alive == Some(true) {
        status = DaemonStatus::Running;
    }

    Ok(DaemonStatusSnapshot { status, daemon_pid, process_alive })
}

pub async fn load_daemon_health_snapshot(project_root: &Path) -> Result<DaemonHealth> {
    if let Some(cached) = read_daemon_health_cache(project_root) {
        return Ok(cached);
    }
    let value = load_daemon_health_snapshot_uncached(project_root).await?;
    write_daemon_health_cache(project_root, &value);
    Ok(value)
}

async fn load_daemon_health_snapshot_uncached(project_root: &Path) -> Result<DaemonHealth> {
    let state_file = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".animus"))
        .join("core-state.json");
    let snapshot = state_store::load_daemon_state_snapshot(&state_file);

    let daemon_pid = daemon_pid_for_status(project_root);
    let process_alive = daemon_pid.map(daemon_process_alive_for_status);

    let mut status = snapshot.daemon_status;
    if matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
        && daemon_pid.is_some()
        && process_alive == Some(false)
    {
        status = DaemonStatus::Crashed;
    }
    if matches!(status, DaemonStatus::Stopped) && process_alive == Some(true) {
        status = DaemonStatus::Running;
    }

    let active_agents = snapshot.active_process_count.unwrap_or(0);

    let pool_size = snapshot
        .daemon_pool_size
        .or_else(|| crate::load_daemon_project_config(project_root).ok().and_then(|config| config.pool_size));

    let pool_utilization_percent =
        pool_size.map(|pool_size| if pool_size == 0 { 0.0 } else { (active_agents as f64 / pool_size as f64) * 100.0 });
    let queued_tasks = crate::workflow::count_tasks_with_status(project_root, TaskStatus::Ready)
        .map(|count| count as u32)
        .unwrap_or(0);

    let provider_plugins_healthy = provider_plugins_healthy_for(project_root);
    // `animus daemon stop` also writes runtime_paused=true (as a
    // scheduling guard), so only a live daemon reports as paused —
    // otherwise every stopped daemon would look operator-paused.
    let (runtime_paused, paused_at) = if matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
        runtime_pause_state_for(project_root)
    } else {
        (false, None)
    };

    Ok(DaemonHealth {
        healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused),
        status,
        provider_plugins_healthy,
        active_agents,
        pool_size,
        project_root: Some(project_root.display().to_string()),
        daemon_pid,
        process_alive,
        pool_utilization_percent,
        queued_tasks: Some(queued_tasks),
        total_agents_spawned: None,
        total_agents_completed: None,
        total_agents_failed: None,
        flavor: active_flavor_for_project(project_root),
        runtime_paused,
        paused_at,
        degraded_reasons: degraded_reasons_for(project_root, &status),
    })
}

/// Wave-2: compute actionable degraded-state reasons for a project. This is
/// the proactive surface that turns a silent-but-broken daemon from green
/// into `degraded`. From `orchestrator-core` (which runs both inside and
/// outside the daemon process) it probes the cheap, on-disk condition that
/// the daemon otherwise only logs as `WRN`: the subject router is
/// unroutable (no installed `subject_backend` plugin can serve the
/// `task`/`requirement` kinds the kernel requires).
///
/// The per-process plugin-cap saturation reason is layered in by the
/// live-daemon health path (`control_routing` / `runtime_daemon`), which
/// runs inside the daemon process where the live plugin-process counter and
/// `RuntimeQuotas` cap are populated. From outside that process the counter
/// always reads zero, so probing it here would produce a false negative.
///
/// Only computed for a live daemon (Running/Paused); a stopped daemon is
/// reported via `status`, not as degraded.
fn degraded_reasons_for(project_root: &Path, status: &DaemonStatus) -> Vec<String> {
    if !matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
        return Vec::new();
    }
    let mut reasons = Vec::new();
    if let Some(reason) = subject_router_degraded_reason(project_root) {
        reasons.push(reason);
    }
    reasons
}

/// Env var that, when set to a truthy value, instructs the daemon to run
/// without any subject-backend dispatch. Mirrors
/// `SUBJECT_PLUGINS_DISABLE_ENV` in `orchestrator-daemon-runtime`; kept as
/// a local literal so this crate stays free of an upward dependency on
/// daemon-runtime.
const SUBJECT_PLUGINS_DISABLE_ENV: &str = "ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS";

/// Returns `true` when [`SUBJECT_PLUGINS_DISABLE_ENV`] is set to a truthy
/// value (`1`, `true`, `yes`, `on`, …). Mirrors
/// `orchestrator_daemon_runtime::subject_plugins_disable_env_set`.
fn subject_plugins_disabled() -> bool {
    match std::env::var(SUBJECT_PLUGINS_DISABLE_ENV) {
        Ok(val) => {
            let trimmed = val.trim().to_ascii_lowercase();
            !trimmed.is_empty() && trimmed != "0" && trimmed != "false" && trimmed != "no" && trimmed != "off"
        }
        Err(_) => false,
    }
}

/// Probe whether the configured subject-backend plugins can form a routable
/// [`orchestrator_plugin_host::SubjectRouter`]. Returns `None` (healthy) when:
///
/// - subject plugins are deliberately disabled via
///   `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS`; or
/// - at least one `subject_backend` plugin is installed AND its kind routing
///   table builds without conflicts (duplicate kind claims, etc.).
///
/// Returns a degraded reason when:
///
/// - plugin discovery itself fails (e.g. a malformed `plugins.yaml`);
/// - no `subject_backend` plugins are installed; or
/// - two or more installed plugins claim the same subject kind, preventing
///   the router from building.
///
/// Consolidated backends (e.g. `animus-postgres`) whose binary name does NOT
/// begin with `animus-subject-` are correctly recognized when registered in
/// `plugins.yaml` with `plugin_kind = "subject_backend"` — kind-based
/// detection never applies filename heuristics.
///
/// Install-time kind renames recorded in `plugins.lock` are applied before
/// the duplicate-kind check, matching the logic in `resolve_subject_dispatch`.
fn subject_router_degraded_reason(project_root: &Path) -> Option<String> {
    use animus_plugin_protocol::PLUGIN_KIND_SUBJECT_BACKEND;
    use orchestrator_plugin_host::{discover_by_kind, SubjectPluginSpec, SubjectRouter};

    if subject_plugins_disabled() {
        return None;
    }

    let plugins = match discover_by_kind(project_root, PLUGIN_KIND_SUBJECT_BACKEND) {
        Ok(p) => p,
        Err(err) => {
            return Some(format!(
                "subject_backend unroutable: plugin discovery failed ({err:#}) — \
                 run `animus plugin status` to diagnose"
            ));
        }
    };

    if plugins.is_empty() {
        return Some(
            "subject_backend unroutable: no executable subject-backend plugin installed — \
             run `animus plugin install-defaults --include-subjects`"
                .to_string(),
        );
    }

    // Build the kind-routing table (no spawning) to detect conflicts such as
    // two installed plugins claiming the same subject kind. Mirrors the logic
    // in `orchestrator_daemon_runtime::resolve_subject_dispatch`.
    let aliases = load_subject_kind_aliases(project_root, &plugins);
    let specs: Vec<SubjectPluginSpec> = plugins
        .iter()
        .map(|p| SubjectPluginSpec {
            name: p.name.clone(),
            path: p.path.clone(),
            native_kinds: p
                .manifest
                .capabilities
                .iter()
                .filter_map(|cap| cap.strip_prefix("subject_kind:"))
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect(),
            env_required: p.manifest.env_required.clone(),
            notification_buffer_size: p.manifest.notification_buffer_size,
            working_dir: None,
        })
        .collect();

    // Detect the case where all installed subject_backend plugins declare no
    // subject kinds — the router would build successfully but serve nothing.
    // This is distinct from "no plugins installed": plugins exist but none
    // claims any subject_kind:* capability, so no request could ever be routed.
    if specs.iter().all(|s| s.native_kinds.is_empty()) {
        return Some(
            "subject_backend unroutable: no installed subject-backend plugin declares any subject kinds — \
             run `animus plugin status` to diagnose kind registrations"
                .to_string(),
        );
    }

    match SubjectRouter::from_lazy_specs(specs, aliases) {
        Ok(_) => None,
        Err(err) => Some(format!(
            "subject_backend unroutable: router construction failed ({err}) — \
             run `animus plugin status` to diagnose conflicting kind registrations"
        )),
    }
}

/// Load install-time kind renames from `plugins.lock` for the degraded-reason
/// router check. Mirrors `load_kind_aliases_from_lockfile` in
/// `orchestrator_daemon_runtime::subject_dispatch` but lives here to keep
/// `orchestrator-core` free of an upward dependency on daemon-runtime.
fn load_subject_kind_aliases(
    project_root: &Path,
    candidates: &[orchestrator_plugin_host::DiscoveredPlugin],
) -> orchestrator_plugin_host::KindAliasMap {
    use orchestrator_plugin_host::{KindAliasMap, PluginLockfile};
    let mut aliases = KindAliasMap::default();
    let Ok(lock) = PluginLockfile::load_default(Some(project_root)) else {
        return aliases;
    };
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

/// v0.5: probe the working tree for an `animus.flavor.v1` manifest and
/// return its `id`. v0.5 always emits `"default"` when the canonical
/// manifest at `flavors/default.toml` is found; absent installs return
/// `None` so consumers can distinguish "not configured" from "default".
fn active_flavor_for_project(project_root: &Path) -> Option<String> {
    crate::flavor::load_flavor_in(project_root, crate::flavor::DEFAULT_FLAVOR_ID).ok().flatten().map(|m| m.id)
}

/// v0.5.3: discover installed provider plugins and report whether at
/// least one binary is present **and executable** on disk. The lookup
/// is intentionally cheap (no spawn / no probe) so health calls stay
/// sub-second even when many providers are configured. A path that
/// exists but has lost its execute bit fails to spawn at run time, so
/// surfacing it as healthy here would produce a false-green; match the
/// `animus plugin status` executability semantics exactly.
///
/// v0.5.9: previously delegated to `discover_provider_plugins`, which
/// ran the full `discover_plugins` pipeline — including a `--manifest`
/// probe on every installed plugin. With 30+ subject-backend plugins
/// installed, `animus daemon status` was spending ~3s probing binaries
/// it didn't care about just to confirm one provider was alive. Walk
/// the plugin install dir directly: any `animus-provider-*` entry that's
/// executable counts. No spawn, no probe, no manifest read.
fn provider_plugins_healthy_for(_project_root: &Path) -> bool {
    use orchestrator_plugin_host::plugin_install_dir;
    let dir = plugin_install_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with("animus-provider-") {
            continue;
        }
        let path = entry.path();
        let candidate = if path.is_dir() { path.join(name_str) } else { path };
        if is_binary_executable(&candidate) {
            return true;
        }
    }
    false
}

#[cfg(unix)]
fn is_binary_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_binary_executable(path: &Path) -> bool {
    path.exists()
}

#[async_trait]
impl DaemonServiceApi for InMemoryServiceHub {
    async fn start(&self, config: DaemonStartConfig) -> Result<()> {
        let pool_size = config.pool_size;
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Running;
        if let Some(ps) = pool_size {
            lock.daemon_pool_size = Some(ps);
        }
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: match pool_size {
                Some(ps) => format!("daemon started (pool_size: {ps})"),
                None => "daemon started".to_string(),
            },
        });
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Stopped;
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: "daemon stopped".to_string(),
        });
        Ok(())
    }

    async fn pause(&self) -> Result<()> {
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Paused;
        lock.logs.push(LogEntry { timestamp: Utc::now(), level: LogLevel::Info, message: "daemon paused".to_string() });
        Ok(())
    }

    async fn resume(&self) -> Result<()> {
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Running;
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: "daemon resumed".to_string(),
        });
        Ok(())
    }

    async fn status(&self) -> Result<DaemonStatus> {
        Ok(self.state.read().await.daemon_status)
    }

    async fn health(&self) -> Result<DaemonHealth> {
        let lock = self.state.read().await;
        Ok(DaemonHealth {
            healthy: matches!(lock.daemon_status, DaemonStatus::Running | DaemonStatus::Paused),
            status: lock.daemon_status,
            provider_plugins_healthy: false,
            active_agents: 0,
            pool_size: lock.daemon_pool_size,
            project_root: None,
            daemon_pid: None,
            process_alive: None,
            pool_utilization_percent: lock.daemon_pool_size.map(|_| 0.0),
            queued_tasks: Some(0),
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
            flavor: crate::flavor::load_flavor(crate::flavor::DEFAULT_FLAVOR_ID).ok().flatten().map(|m| m.id),
            runtime_paused: matches!(lock.daemon_status, DaemonStatus::Paused),
            paused_at: None,
            degraded_reasons: Vec::new(),
        })
    }

    async fn logs(&self, limit: Option<usize>) -> Result<Vec<LogEntry>> {
        let lock = self.state.read().await;
        let mut logs = lock.logs.clone();
        if let Some(limit) = limit {
            if logs.len() > limit {
                logs = logs.split_off(logs.len() - limit);
            }
        }
        Ok(logs)
    }

    async fn clear_logs(&self) -> Result<()> {
        self.state.write().await.logs.clear();
        Ok(())
    }

    async fn active_agents(&self) -> Result<usize> {
        Ok(0)
    }

    async fn set_active_process_count(&self, count: usize) -> Result<()> {
        self.state.write().await.active_process_count = Some(count);
        Ok(())
    }
}

#[async_trait]
impl DaemonServiceApi for FileServiceHub {
    async fn start(&self, config: DaemonStartConfig) -> Result<()> {
        let pool_size = config.pool_size;

        let result = mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Running;
            if let Some(ps) = pool_size {
                state.daemon_pool_size = Some(ps);
            }
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: match pool_size {
                    Some(ps) => format!("daemon started (pool_size: {ps})"),
                    None => "daemon started".to_string(),
                },
            });
            Ok(())
        })
        .await;
        invalidate_daemon_health_cache(&self.project_root);
        result
    }

    async fn stop(&self) -> Result<()> {
        let result = mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Stopped;
            state.active_process_count = None;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon stopped".to_string(),
            });
            Ok(())
        })
        .await;
        invalidate_daemon_health_cache(&self.project_root);
        result
    }

    async fn pause(&self) -> Result<()> {
        let result = mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Paused;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon paused".to_string(),
            });
            Ok(())
        })
        .await;
        invalidate_daemon_health_cache(&self.project_root);
        result
    }

    async fn resume(&self) -> Result<()> {
        let result = mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Running;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon resumed".to_string(),
            });
            Ok(())
        })
        .await;
        invalidate_daemon_health_cache(&self.project_root);
        result
    }

    async fn status(&self) -> Result<DaemonStatus> {
        let daemon_pid = daemon_pid_for_status(&self.project_root);
        let daemon_process_alive = daemon_pid.map(daemon_process_alive_for_status);

        let (status, should_mark_crashed) = {
            let lock = self.state.read().await;
            let should_mark_crashed = matches!(lock.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                && daemon_pid.is_some()
                && daemon_process_alive == Some(false);
            (lock.daemon_status, should_mark_crashed)
        };

        if should_mark_crashed {
            let result = mutate_daemon_state(self, |state| {
                if matches!(state.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                    && daemon_pid.is_some()
                    && daemon_process_alive == Some(false)
                {
                    state.daemon_status = DaemonStatus::Crashed;
                    state.logs.push(LogEntry {
                        timestamp: Utc::now(),
                        level: LogLevel::Error,
                        message: "daemon pid liveness check failed while daemon was active".to_string(),
                    });
                }
                Ok(state.daemon_status)
            })
            .await;
            invalidate_daemon_health_cache(&self.project_root);
            return result;
        }

        Ok(status)
    }

    async fn health(&self) -> Result<DaemonHealth> {
        if let Some(cached) = read_daemon_health_cache(&self.project_root) {
            return Ok(cached);
        }
        let daemon_pid = daemon_pid_for_status(&self.project_root);
        let process_alive = daemon_pid.map(daemon_process_alive_for_status);
        let (mut status, should_mark_crashed) = {
            let lock = self.state.read().await;
            let should_mark_crashed = matches!(lock.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                && daemon_pid.is_some()
                && process_alive == Some(false);
            (lock.daemon_status, should_mark_crashed)
        };

        if should_mark_crashed {
            status = mutate_daemon_state(self, |state| {
                if matches!(state.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                    && daemon_pid.is_some()
                    && process_alive == Some(false)
                {
                    state.daemon_status = DaemonStatus::Crashed;
                    state.logs.push(LogEntry {
                        timestamp: Utc::now(),
                        level: LogLevel::Error,
                        message: "daemon pid liveness check failed while daemon was active".to_string(),
                    });
                }
                Ok(state.daemon_status)
            })
            .await?;
        }

        let persisted_process_count = self.state.read().await.active_process_count;
        let active_agents = persisted_process_count.unwrap_or(0);

        let lock = self.state.read().await;

        let pool_size = lock
            .daemon_pool_size
            .or_else(|| crate::load_daemon_project_config(&self.project_root).ok().and_then(|c| c.pool_size));

        if matches!(status, DaemonStatus::Stopped) && process_alive == Some(true) {
            status = DaemonStatus::Running;
        }

        let pool_utilization_percent =
            pool_size.map(|ps| if ps == 0 { 0.0 } else { (active_agents as f64 / ps as f64) * 100.0 });
        let queued_tasks = crate::workflow::count_tasks_with_status(&self.project_root, TaskStatus::Ready)
            .map(|count| count as u32)
            .unwrap_or(0);
        let provider_plugins_healthy = provider_plugins_healthy_for(&self.project_root);
        // See load_daemon_health_snapshot_uncached: stop also sets the
        // runtime_paused guard, so only a live daemon reports paused.
        let (runtime_paused, paused_at) = if matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
            runtime_pause_state_for(&self.project_root)
        } else {
            (false, None)
        };

        let value = DaemonHealth {
            healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused),
            status,
            provider_plugins_healthy,
            active_agents,
            pool_size,
            project_root: Some(self.project_root.display().to_string()),
            daemon_pid,
            process_alive,
            pool_utilization_percent,
            queued_tasks: Some(queued_tasks),
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
            flavor: active_flavor_for_project(&self.project_root),
            runtime_paused,
            paused_at,
            degraded_reasons: degraded_reasons_for(&self.project_root, &status),
        };
        write_daemon_health_cache(&self.project_root, &value);
        Ok(value)
    }

    async fn logs(&self, limit: Option<usize>) -> Result<Vec<LogEntry>> {
        load_logs(&self.logs_file, limit)
    }

    async fn clear_logs(&self) -> Result<()> {
        mutate_daemon_state(self, |state| {
            state.logs.clear();
            Ok(())
        })
        .await?;
        clear_logs_file(&self.logs_file)
    }

    async fn active_agents(&self) -> Result<usize> {
        let lock = self.state.read().await;
        Ok(lock.active_process_count.unwrap_or(0))
    }

    async fn set_active_process_count(&self, count: usize) -> Result<()> {
        let result = mutate_daemon_state(self, |state| {
            state.active_process_count = Some(count);
            Ok(())
        })
        .await;
        invalidate_daemon_health_cache(&self.project_root);
        result
    }
}

#[cfg(test)]
fn test_daemon_pid_override() -> Option<Option<u32>> {
    None
}

#[cfg(test)]
fn test_daemon_process_alive_override() -> Option<bool> {
    None
}

#[cfg(test)]
fn test_skip_persist_override() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_file_hub(temp: &TempDir) -> FileServiceHub {
        let state_file = temp.path().join(".animus").join("core-state.json");
        let logs_file = state_store::logs_file_for_state_file(&state_file);
        std::fs::create_dir_all(state_file.parent().expect("state file should have a parent directory"))
            .expect("state dir should exist");
        FileServiceHub {
            state: std::sync::Arc::new(tokio::sync::RwLock::new(CoreState::default_with_stopped())),
            state_file,
            logs_file,
            project_root: temp.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn file_hub_start_sets_running() {
        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, Default::default()).await.expect("daemon start should succeed");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
    }

    #[tokio::test]
    async fn fast_snapshot_returns_stopped_for_empty_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let snapshot = load_daemon_status_snapshot_fast(temp.path())
            .await
            .expect("fast snapshot should succeed for empty project");
        assert_eq!(snapshot.status, DaemonStatus::Stopped);
        assert!(snapshot.daemon_pid.is_none());
        assert!(snapshot.process_alive.is_none());
    }

    #[tokio::test]
    async fn fast_snapshot_matches_full_snapshot_status_for_empty_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fast = load_daemon_status_snapshot_fast(temp.path()).await.expect("fast snapshot");
        let full = load_daemon_health_snapshot(temp.path()).await.expect("full snapshot");
        assert_eq!(fast.status, full.status, "fast snapshot must report the same DaemonStatus as the full snapshot");
        assert_eq!(fast.daemon_pid, full.daemon_pid);
        assert_eq!(fast.process_alive, full.process_alive);
    }

    #[tokio::test]
    async fn fast_snapshot_does_not_open_sqlite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scoped = protocol::scoped_state_root(temp.path()).unwrap_or_else(|| temp.path().join(".animus"));
        let _ = load_daemon_status_snapshot_fast(temp.path()).await.expect("fast snapshot");
        // The full snapshot opens the per-project SQLite store
        // (count_tasks_with_status). The fast snapshot must not — if it
        // did, the scoped store directory would carry the typical
        // `tasks.db`, `tasks.db-wal`, or `tasks.db-shm` files. Assert
        // none materialized.
        let store_dir = scoped.join("store");
        if store_dir.exists() {
            for entry in std::fs::read_dir(&store_dir).expect("read store dir") {
                let entry = entry.expect("dir entry");
                let name = entry.file_name();
                let name = name.to_string_lossy();
                assert!(
                    !name.ends_with(".db") && !name.ends_with(".db-wal") && !name.ends_with(".db-shm"),
                    "fast snapshot must not open SQLite; found {name}"
                );
            }
        }
    }

    #[tokio::test]
    async fn file_hub_start_does_not_spawn_runner_sidecar() {
        // v0.5.3: there is no runner sidecar; starting the daemon must
        // succeed without it.
        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, DaemonStartConfig::default())
            .await
            .expect("daemon start should succeed without starting runner");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
    }

    fn sample_health(status: DaemonStatus) -> DaemonHealth {
        DaemonHealth {
            healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused),
            status,
            provider_plugins_healthy: false,
            active_agents: 0,
            pool_size: None,
            project_root: Some("/probe".to_string()),
            daemon_pid: None,
            process_alive: None,
            pool_utilization_percent: None,
            queued_tasks: Some(0),
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
            flavor: None,
            runtime_paused: false,
            paused_at: None,
            degraded_reasons: Vec::new(),
        }
    }

    #[test]
    fn parse_runtime_pause_state_reads_paused_record() {
        let (paused, at) =
            parse_runtime_pause_state(r#"{"runtime_paused":true,"paused_at":"2026-06-11T00:00:00+00:00"}"#);
        assert!(paused);
        assert_eq!(at.as_deref(), Some("2026-06-11T00:00:00+00:00"));
    }

    #[test]
    fn parse_runtime_pause_state_defaults_when_unpaused_or_invalid() {
        assert_eq!(parse_runtime_pause_state(r#"{"runtime_paused":false}"#), (false, None));
        // paused_at is ignored unless runtime_paused is true.
        assert_eq!(parse_runtime_pause_state(r#"{"paused_at":"2026-01-01T00:00:00Z"}"#), (false, None));
        assert_eq!(parse_runtime_pause_state(r#"{"runtime_paused":true}"#), (true, None));
        assert_eq!(parse_runtime_pause_state("not-json"), (false, None));
        assert_eq!(parse_runtime_pause_state("{}"), (false, None));
    }

    #[test]
    fn daemon_health_cache_returns_stored_value_within_ttl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let value = sample_health(DaemonStatus::Running);
        write_daemon_health_cache(temp.path(), &value);
        let read = read_daemon_health_cache(temp.path()).expect("should hit");
        assert_eq!(read.status, DaemonStatus::Running);
        assert_eq!(read.project_root.as_deref(), Some("/probe"));
    }

    #[test]
    fn daemon_health_cache_keys_isolate_by_project_root() {
        let temp_a = tempfile::tempdir().expect("tempdir a");
        let temp_b = tempfile::tempdir().expect("tempdir b");
        write_daemon_health_cache(temp_a.path(), &sample_health(DaemonStatus::Running));
        write_daemon_health_cache(temp_b.path(), &sample_health(DaemonStatus::Paused));
        assert_eq!(read_daemon_health_cache(temp_a.path()).unwrap().status, DaemonStatus::Running);
        assert_eq!(read_daemon_health_cache(temp_b.path()).unwrap().status, DaemonStatus::Paused);
    }

    #[tokio::test]
    async fn daemon_health_cache_invalidated_by_lifecycle_mutations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        write_daemon_health_cache(&hub.project_root, &sample_health(DaemonStatus::Running));
        assert!(read_daemon_health_cache(&hub.project_root).is_some(), "precondition: cache populated");

        DaemonServiceApi::start(&hub, Default::default()).await.expect("start");
        assert!(read_daemon_health_cache(&hub.project_root).is_none(), "start must invalidate the daemon health cache");

        write_daemon_health_cache(&hub.project_root, &sample_health(DaemonStatus::Running));
        DaemonServiceApi::stop(&hub).await.expect("stop");
        assert!(read_daemon_health_cache(&hub.project_root).is_none(), "stop must invalidate the daemon health cache");

        write_daemon_health_cache(&hub.project_root, &sample_health(DaemonStatus::Running));
        DaemonServiceApi::pause(&hub).await.expect("pause");
        assert!(read_daemon_health_cache(&hub.project_root).is_none(), "pause must invalidate the daemon health cache");

        write_daemon_health_cache(&hub.project_root, &sample_health(DaemonStatus::Running));
        DaemonServiceApi::resume(&hub).await.expect("resume");
        assert!(
            read_daemon_health_cache(&hub.project_root).is_none(),
            "resume must invalidate the daemon health cache"
        );

        write_daemon_health_cache(&hub.project_root, &sample_health(DaemonStatus::Running));
        DaemonServiceApi::set_active_process_count(&hub, 3).await.expect("set count");
        assert!(
            read_daemon_health_cache(&hub.project_root).is_none(),
            "set_active_process_count must invalidate the daemon health cache"
        );
    }

    #[test]
    fn daemon_health_cache_expires_after_ttl() {
        let temp = tempfile::tempdir().expect("tempdir");
        // Simulate stale by manually pushing an old timestamp into the cache.
        {
            let mut cache = daemon_health_cache().write().unwrap();
            let stale = Instant::now()
                .checked_sub(DAEMON_HEALTH_CACHE_TTL + std::time::Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            cache.insert(temp.path().to_path_buf(), (stale, sample_health(DaemonStatus::Running)));
        }
        assert!(read_daemon_health_cache(temp.path()).is_none(), "stale entry should not be returned");
    }

    // ---- subject_router_degraded_reason regression tests ----------------
    //
    // These tests manipulate process-global env vars that drive plugin
    // discovery (`ANIMUS_CONFIG_DIR`, `ANIMUS_PLUGIN_DIR`,
    // `ANIMUS_PLUGIN_PATH`). They must run serially so env mutations from
    // one test don't race another.

    static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value.as_ref());
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// Redirect the three env vars that drive `discover_by_kind` to isolated
    /// temp dirs. Returns guards that restore the originals on drop.
    fn isolate_discovery_env(config_dir: &Path, plugin_dir: &Path) -> (EnvGuard, EnvGuard, EnvGuard) {
        (
            EnvGuard::set("ANIMUS_CONFIG_DIR", config_dir),
            EnvGuard::set("ANIMUS_PLUGIN_DIR", plugin_dir),
            EnvGuard::set("ANIMUS_PLUGIN_PATH", ""),
        )
    }

    /// Regression test for TASK-211: a consolidated backend whose binary name
    /// does NOT start with `animus-subject-` (e.g. `animus-postgres`) must not
    /// produce a degraded reason when it declares `plugin_kind = "subject_backend"`
    /// in its manifest and is registered in `plugins.yaml`.
    #[cfg(unix)]
    #[test]
    fn subject_router_not_degraded_for_consolidated_backend_registered_as_subject_backend() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");

        let config_dir = temp.path().join("animus-home");
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&config_dir).expect("mkdir config_dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");

        // A fake `animus-postgres` binary: its name does not start with
        // `animus-subject-`, so an old filename-prefix probe would have ignored
        // it and falsely reported no subject_backend installed.
        let bin = temp.path().join("animus-postgres");
        let manifest = serde_json::json!({
            "name": "animus-postgres",
            "version": "0.1.0",
            "plugin_kind": "subject_backend",
            "description": "Consolidated PostgreSQL subject backend",
            "protocol_version": "1.0.0",
            "capabilities": ["subject_kind:task"]
        });
        std::fs::write(&bin, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        let mut perms = std::fs::metadata(&bin).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).expect("chmod");

        // Register the consolidated backend in the global plugins.yaml.
        std::fs::write(
            config_dir.join("plugins.yaml"),
            format!("plugins:\n  animus-postgres:\n    binary: {}\n", bin.display()),
        )
        .expect("write plugins.yaml");

        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let _env = isolate_discovery_env(&config_dir, &plugin_dir);
        let reason = subject_router_degraded_reason(&project_root);
        assert!(
            reason.is_none(),
            "consolidated backend `animus-postgres` declared as subject_backend must not produce a degraded reason, got: {reason:?}"
        );
    }

    /// A project with no subject_backend plugins at all must produce a degraded
    /// reason so operators see actionable feedback rather than a silent failure.
    #[test]
    fn subject_router_degraded_when_no_subject_backend_plugins_installed() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");

        let config_dir = temp.path().join("animus-home");
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&config_dir).expect("mkdir config_dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");
        // No plugins.yaml, no binaries — discover_by_kind returns an empty Vec.

        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let _env = isolate_discovery_env(&config_dir, &plugin_dir);
        let reason = subject_router_degraded_reason(&project_root);
        assert!(reason.is_some(), "no subject_backend plugins must produce a degraded reason");
        let r = reason.unwrap();
        assert!(
            r.contains("subject_backend unroutable"),
            "degraded reason must mention `subject_backend unroutable`, got: {r}"
        );
    }

    /// When `discover_by_kind` itself returns an error (e.g. a malformed
    /// `plugins.yaml`), the probe must report degraded rather than silently
    /// returning `None` (false-healthy). This was the residual bug after the
    /// `.ok()?` pattern was left in place post-refactor.
    #[test]
    fn subject_router_degraded_on_discovery_failure_not_false_healthy() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");

        let config_dir = temp.path().join("animus-home");
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&config_dir).expect("mkdir config_dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");

        // A bare integer is valid YAML but cannot be deserialized as
        // PluginsConfig (a struct), so load_plugins_config returns Err and
        // discover_by_kind propagates it.
        std::fs::write(config_dir.join("plugins.yaml"), "12345\n").expect("write invalid yaml");

        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let _env = isolate_discovery_env(&config_dir, &plugin_dir);
        let reason = subject_router_degraded_reason(&project_root);
        assert!(
            reason.is_some(),
            "plugin discovery failure must produce a degraded reason, not false-healthy (None)"
        );
        let reason = reason.unwrap();
        assert!(
            reason.contains("plugins.yaml"),
            "discovery failure must retain the underlying configuration error, got: {reason}"
        );
    }

    /// When `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS` is set to a truthy value,
    /// the operator has deliberately disabled subject-backend dispatch — this
    /// is not a degraded state. The probe must return `None` even when no
    /// subject_backend plugins are installed (which would otherwise trigger the
    /// "no plugin installed" degraded reason).
    #[test]
    fn subject_router_not_degraded_when_subject_plugins_disabled_by_env() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");

        let config_dir = temp.path().join("animus-home");
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&config_dir).expect("mkdir config_dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");
        // No plugins.yaml, no binaries — would normally degrade as "no subject_backend installed".

        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let _env = isolate_discovery_env(&config_dir, &plugin_dir);
        let _disable = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, "1");

        let reason = subject_router_degraded_reason(&project_root);
        assert!(
            reason.is_none(),
            "ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1 must suppress the degraded reason, got: {reason:?}"
        );
    }

    /// `subject_plugins_disabled` must agree with the real boot parser in
    /// `orchestrator-daemon-runtime` for every canonical falsy value, including
    /// mixed-case variants. `"False"`, `"FALSE"`, `"No"`, `"Off"`, and `"0"`
    /// must all be treated as falsy (plugins NOT disabled), and `"true"`,
    /// `"True"`, `"TRUE"`, `"1"`, `"yes"`, `"on"` as truthy (plugins disabled).
    #[test]
    fn subject_plugins_disabled_matches_boot_parser_for_mixed_case_falsy_values() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let falsy = ["", "0", "false", "False", "FALSE", "no", "No", "NO", "off", "Off", "OFF"];
        for val in &falsy {
            let _g = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, val);
            assert!(
                !subject_plugins_disabled(),
                "expected subject_plugins_disabled() == false for {SUBJECT_PLUGINS_DISABLE_ENV}={val:?}"
            );
        }

        let truthy = ["1", "true", "True", "TRUE", "yes", "Yes", "YES", "on", "On", "ON"];
        for val in &truthy {
            let _g = EnvGuard::set(SUBJECT_PLUGINS_DISABLE_ENV, val);
            assert!(
                subject_plugins_disabled(),
                "expected subject_plugins_disabled() == true for {SUBJECT_PLUGINS_DISABLE_ENV}={val:?}"
            );
        }
    }

    /// A `subject_backend` plugin that declares NO `subject_kind:*` capabilities
    /// must produce a degraded reason. The router it would build serves nothing —
    /// any call would fail with "no backend registered for kind". This guards the
    /// case where `discover_by_kind` returns plugins but `from_lazy_specs` would
    /// succeed with an empty routing table (false-healthy).
    #[cfg(unix)]
    #[test]
    fn subject_router_degraded_when_subject_backend_declares_no_kinds() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");

        let config_dir = temp.path().join("animus-home");
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&config_dir).expect("mkdir config_dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");

        let bin = temp.path().join("animus-subject-nokind");
        let manifest = serde_json::json!({
            "name": "animus-subject-nokind",
            "version": "0.1.0",
            "plugin_kind": "subject_backend",
            "description": "subject backend with no subject kinds",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        std::fs::write(&bin, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        let mut perms = std::fs::metadata(&bin).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).expect("chmod");

        std::fs::write(
            config_dir.join("plugins.yaml"),
            format!("plugins:\n  animus-subject-nokind:\n    binary: {}\n", bin.display()),
        )
        .expect("write plugins.yaml");

        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let _env = isolate_discovery_env(&config_dir, &plugin_dir);
        let reason = subject_router_degraded_reason(&project_root);
        assert!(
            reason.is_some(),
            "subject_backend with no subject kinds must produce a degraded reason (false-healthy guard)"
        );
        let r = reason.unwrap();
        assert!(
            r.contains("subject_backend unroutable"),
            "degraded reason must mention `subject_backend unroutable`, got: {r}"
        );
    }

    /// Two `subject_backend` plugins that both declare `subject_kind:task` cause
    /// `SubjectRouter::from_lazy_specs` to fail with a duplicate-kind error.
    /// The probe must surface this as a degraded reason so the operator can
    /// diagnose the conflicting installations instead of silently routing to
    /// whichever plugin happened to be discovered first.
    #[cfg(unix)]
    #[test]
    fn subject_router_degraded_on_duplicate_kind_conflict() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");

        let config_dir = temp.path().join("animus-home");
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&config_dir).expect("mkdir config_dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");

        let make_plugin = |name: &str| -> std::path::PathBuf {
            let bin = temp.path().join(name);
            let manifest = serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "plugin_kind": "subject_backend",
                "description": format!("fake {name}"),
                "protocol_version": "1.0.0",
                "capabilities": ["subject_kind:task"]
            });
            std::fs::write(&bin, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
            let mut perms = std::fs::metadata(&bin).expect("meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).expect("chmod");
            bin
        };

        let bin_a = make_plugin("fake-subject-a");
        let bin_b = make_plugin("fake-subject-b");

        std::fs::write(
            config_dir.join("plugins.yaml"),
            format!(
                "plugins:\n  fake-subject-a:\n    binary: {}\n  fake-subject-b:\n    binary: {}\n",
                bin_a.display(),
                bin_b.display()
            ),
        )
        .expect("write plugins.yaml");

        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let _env = isolate_discovery_env(&config_dir, &plugin_dir);
        let reason = subject_router_degraded_reason(&project_root);
        assert!(
            reason.is_some(),
            "duplicate subject_kind:task claims must produce a degraded reason"
        );
        let r = reason.unwrap();
        assert!(
            r.contains("subject_backend unroutable"),
            "degraded reason must mention `subject_backend unroutable`, got: {r}"
        );
        assert!(
            r.contains("router construction failed") || r.contains("duplicate"),
            "degraded reason must mention the router construction failure or duplicate kind, got: {r}"
        );
    }
}
