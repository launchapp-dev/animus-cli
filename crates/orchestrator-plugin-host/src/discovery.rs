use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use animus_plugin_protocol::PluginManifest;
use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::host::PLUGIN_BASE_ENV_ALLOWLIST;
use crate::scope::PluginScope;

/// Hard ceiling on how long a plugin gets to print its manifest before the
/// host kills the child and surfaces a discovery warning. Manifests are
/// static metadata — anything beyond a second is pathological.
const MANIFEST_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard ceiling on bytes the host will buffer from a plugin's stdout during
/// the `--manifest` probe. A manifest is JSON in the kilobytes; anything
/// over 1 MiB is either a bug or an attack.
const MANIFEST_PROBE_MAX_STDOUT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    ExplicitConfig,
    ProjectLocal,
    PluginPath,
    SystemPath,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredPlugin {
    pub name: String,
    pub path: PathBuf,
    pub manifest: PluginManifest,
    pub source: DiscoverySource,
}

/// A plugin that was located on disk but could not be loaded — typically because
/// its `--manifest` probe failed. Surfaced alongside successful discoveries so
/// callers can tell users *why* an installed plugin disappeared instead of
/// silently dropping it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryWarning {
    pub name: String,
    pub path: PathBuf,
    pub source: DiscoverySource,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginConfigEntry {
    pub binary: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Install-time `--name <NAME>` override recorded in `plugins.yaml`.
    /// When set, discovery uses this as the canonical [`DiscoveredPlugin::name`]
    /// instead of the manifest-declared name or the table key. Keeps the
    /// lockfile entry, discovery, and the daemon's SubjectRouter alias map
    /// agreed on the same logical name when the operator installed the
    /// plugin with `--name`. v0.5.8+.
    #[serde(default)]
    pub name_override: Option<String>,
    /// Persisted audit trail: `true` when the plugin was installed with the
    /// `--skip-manifest-check` flag. Discovery emits a `warn!` on every probe
    /// for plugins flagged this way so operators don't lose track of a plugin
    /// whose manifest health is intentionally untrusted.
    #[serde(default)]
    pub skip_manifest_check_at_install: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
struct PluginsConfig {
    #[serde(default)]
    plugins: HashMap<String, PluginConfigEntry>,
    #[serde(default)]
    providers: HashMap<String, PluginConfigEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct PluginDiscovery {
    project_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    include_system_path: bool,
    scope: Option<PluginScope>,
}

impl PluginDiscovery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        self.project_root = Some(project_root.into());
        self
    }

    pub fn with_config_path(mut self, config_path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(config_path.into());
        self
    }

    /// Opt in to scanning `$PATH` for `animus-plugin-*` / `animus-provider-*` binaries.
    ///
    /// Defaults to `false`. When enabled, [`PluginDiscovery::discover`] will
    /// execute every matching binary on `$PATH` with `--manifest` to fetch its
    /// manifest. This runs arbitrary executables found on the user's `$PATH`
    /// during discovery, so only enable when the caller explicitly trusts that
    /// surface.
    pub fn include_system_path(mut self, include_system_path: bool) -> Self {
        self.include_system_path = include_system_path;
        self
    }

    /// Apply a [`PluginScope`] filter to the discovered set. Plugins
    /// outside the scope are removed from the returned `discovered`
    /// list AFTER the manifest probe completes — warnings for failed
    /// probes still surface so operators retain diagnostic info on
    /// out-of-scope plugins whose manifest is broken.
    ///
    /// When this builder method is not called, [`PluginScopeMode::All`]
    /// semantics apply (the v0.5.8 behavior).
    pub fn with_scope(mut self, scope: PluginScope) -> Self {
        self.scope = Some(scope);
        self
    }

    pub fn discover(&self) -> Result<Vec<DiscoveredPlugin>> {
        Ok(self.discover_with_warnings()?.0)
    }

    /// Like [`PluginDiscovery::discover`], but also returns a list of
    /// [`DiscoveryWarning`]s for plugins that were located but could not be
    /// loaded (e.g. their `--manifest` probe failed). Warnings are also emitted
    /// at `warn` level via `tracing`.
    ///
    /// # Precedence
    ///
    /// Discovery walks the following sources in order, and a plugin name (or
    /// binary file name when scanning directories) is locked in by the first
    /// source that yields it. Later sources can never override an earlier one
    /// — duplicates from lower-precedence sources are silently skipped:
    ///
    /// 1. Explicit registry config (`~/.animus/plugins.yaml`, or the path
    ///    supplied to [`PluginDiscovery::with_config_path`]).
    /// 2. Project-local install dir (`<project_root>/.animus/plugins/`).
    /// 3. Global install dir (`$ANIMUS_PLUGIN_DIR` when set, otherwise
    ///    `~/.animus/plugins/`) — the canonical destination for
    ///    `animus plugin install`. Scanned unconditionally so a binary
    ///    dropped into this directory by hand is still discovered even when
    ///    the registry yaml is missing or stale.
    /// 4. `$ANIMUS_PLUGIN_PATH` (PATH-style, colon-separated additional
    ///    directories, appended after the global install dir).
    /// 5. Operating-system `$PATH` (only when
    ///    [`PluginDiscovery::include_system_path`] is opted in).
    ///
    /// Entries are deduplicated by name to keep the precedence chain
    /// deterministic regardless of underlying file-system iteration order.
    pub fn discover_with_warnings(&self) -> Result<(Vec<DiscoveredPlugin>, Vec<DiscoveryWarning>)> {
        let mut discovered = Vec::new();
        let mut warnings = Vec::new();
        let mut seen = HashSet::new();

        self.discover_configured(&mut discovered, &mut warnings, &mut seen)?;

        if let Some(project_root) = &self.project_root {
            scan_dir(
                &project_root.join(".animus/plugins"),
                DiscoverySource::ProjectLocal,
                &mut discovered,
                &mut warnings,
                &mut seen,
            );
        }

        // Scan the global plugin install dir unconditionally. This is the
        // canonical destination for `animus plugin install` and the
        // historical "user dropped a binary here by hand" location. When
        // `$ANIMUS_PLUGIN_DIR` is set, [`plugin_install_dir`] returns that
        // override; otherwise it resolves to `~/.animus/plugins/`
        // (honoring `$ANIMUS_CONFIG_DIR` for hermetic tests).
        scan_dir(&plugin_install_dir(), DiscoverySource::PluginPath, &mut discovered, &mut warnings, &mut seen);

        if let Ok(plugin_path) = std::env::var("ANIMUS_PLUGIN_PATH") {
            for raw_dir in plugin_path.split(':') {
                if !raw_dir.trim().is_empty() {
                    scan_dir(
                        Path::new(raw_dir),
                        DiscoverySource::PluginPath,
                        &mut discovered,
                        &mut warnings,
                        &mut seen,
                    );
                }
            }
        }

        if self.include_system_path {
            if let Some(path_var) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_var) {
                    scan_dir(&dir, DiscoverySource::SystemPath, &mut discovered, &mut warnings, &mut seen);
                }
            }
        }

        // Auto-load the project scope when a `project_root` is set but
        // no explicit `with_scope(...)` override has been supplied. This
        // keeps the dozen-or-so direct PluginDiscovery callers in the
        // workspace honoring `.animus/plugin-scope.yaml` without each
        // having to wire the scope loader manually. Builders that want
        // unrestricted discovery can still pass
        // `PluginScope::unrestricted()` explicitly.
        let scope_owned;
        let effective_scope: Option<&PluginScope> = match &self.scope {
            Some(s) => Some(s),
            None => match &self.project_root {
                Some(root) => {
                    scope_owned = PluginScope::load_for_project(root).unwrap_or_else(|err| {
                        tracing::warn!(
                            error = %err,
                            "failed to auto-load plugin scope; falling back to unrestricted discovery"
                        );
                        PluginScope::unrestricted()
                    });
                    Some(&scope_owned)
                }
                None => None,
            },
        };
        if let Some(scope) = effective_scope {
            if !scope.admits_everything() {
                let before = discovered.len();
                discovered.retain(|plugin| scope.admits(plugin));
                let removed = before.saturating_sub(discovered.len());
                if removed > 0 {
                    tracing::debug!(
                        scope_mode = scope.mode.as_wire(),
                        removed,
                        kept = discovered.len(),
                        "plugin scope filter applied"
                    );
                }
            }
        }

        Ok((discovered, warnings))
    }

    fn discover_configured(
        &self,
        discovered: &mut Vec<DiscoveredPlugin>,
        warnings: &mut Vec<DiscoveryWarning>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let config_path = self.config_path.clone().unwrap_or_else(default_config_path);
        if !config_path.exists() {
            return Ok(());
        }

        let config = load_plugins_config(&config_path)
            .with_context(|| format!("failed to read plugin config at {}", config_path.display()))?;
        for (logical_name, entry) in config.plugins.iter().chain(config.providers.iter()) {
            // v0.5.8+: `name_override` records the install-time `--name <NAME>`
            // override so discovery, the lockfile entry, and the daemon
            // SubjectRouter all agree on the same logical plugin name. Falls
            // back to the manifest-name field, then the table key, for
            // pre-v0.5.8 entries that never recorded the override.
            let effective_name =
                entry.name_override.clone().or_else(|| entry.name.clone()).unwrap_or_else(|| logical_name.clone());
            let Some(path) = find_binary(&expand_home(&entry.binary)) else {
                let reason = format!("configured binary not found: {}", entry.binary);
                tracing::warn!(
                    plugin = %logical_name,
                    binary = %entry.binary,
                    source = "explicit_config",
                    "plugin manifest probe skipped: {reason}"
                );
                warnings.push(DiscoveryWarning {
                    name: effective_name.clone(),
                    path: PathBuf::from(&entry.binary),
                    source: DiscoverySource::ExplicitConfig,
                    reason,
                });
                continue;
            };
            let name = effective_name;
            if seen.contains(&name) {
                continue;
            }
            if entry.skip_manifest_check_at_install {
                tracing::warn!(
                    plugin = %name,
                    path = %path.display(),
                    "plugin {name} installed with --skip-manifest-check; manifest probe failures will be tolerated."
                );
            }
            match fetch_manifest(&path) {
                Ok(manifest) => {
                    seen.insert(name.clone());
                    discovered.push(DiscoveredPlugin { name, path, manifest, source: DiscoverySource::ExplicitConfig });
                }
                Err(error) => {
                    // Reserve the name even on probe failure so a lower-precedence
                    // directory scan can't silently shadow a broken explicit config
                    // entry. The warning still surfaces so operators can fix the
                    // broken plugin instead of being routed to an unintended copy.
                    seen.insert(name.clone());
                    let reason = format!("{error:#}");
                    tracing::warn!(
                        plugin = %name,
                        path = %path.display(),
                        source = "explicit_config",
                        "plugin manifest probe failed: {reason}"
                    );
                    warnings.push(DiscoveryWarning { name, path, source: DiscoverySource::ExplicitConfig, reason });
                }
            }
        }

        Ok(())
    }
}

pub fn discover_plugins(project_root: impl Into<PathBuf>) -> Result<Vec<DiscoveredPlugin>> {
    let root: PathBuf = project_root.into();
    let scope = PluginScope::load_for_project(&root).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            "failed to load plugin scope; falling back to unrestricted discovery"
        );
        PluginScope::unrestricted()
    });
    PluginDiscovery::new().with_project_root(root).with_scope(scope).discover()
}

/// Discover all installed plugins whose manifest `plugin_kind` equals
/// `kind` (case-sensitive — match the wire constants exactly:
/// `"workflow_runner"`, `"subject_backend"`, `"provider"`, `"transport"`,
/// etc.). Skips plugins whose `--manifest` probe failed (those surface as
/// [`DiscoveryWarning`]s on a full [`discover_plugins`] call).
///
/// Errors only when discovery itself fails (config read, registry parse).
/// An empty `Vec` means "no plugins of that kind installed".
pub fn discover_by_kind(project_root: impl Into<PathBuf>, kind: &str) -> Result<Vec<DiscoveredPlugin>> {
    let plugins = discover_plugins(project_root)?;
    Ok(plugins.into_iter().filter(|p| p.manifest.plugin_kind == kind).collect())
}

/// Probe a plugin binary's `--manifest` output.
///
/// This is the security-sensitive entry point for plugin discovery: it
/// executes an arbitrary binary on the user's machine. The probe is
/// sandboxed to defend against three classes of misbehavior:
///
/// 1. **Hangs.** Wrapped in a [`MANIFEST_PROBE_TIMEOUT`] (5s) — if the
///    plugin blocks on stdin or sleeps, the child is killed via
///    `kill_on_drop(true)` and the caller sees a clear timeout error.
/// 2. **Memory bombs.** Stdout is capped at
///    [`MANIFEST_PROBE_MAX_STDOUT`] (1 MiB) — manifests are small JSON,
///    anything bigger is rejected before the host allocates more memory.
/// 3. **Env leakage.** The child's environment is scrubbed via
///    `env_clear()` and only [`PLUGIN_BASE_ENV_ALLOWLIST`] is forwarded
///    — the manifest probe never needs API credentials, and any plugin
///    that asks for them during `--manifest` has a bug.
///
/// The function is sync (its callers are sync); internally it spawns a
/// dedicated single-threaded tokio runtime on a worker thread so it
/// remains safe to call from inside an outer tokio runtime as well as
/// from plain sync code.
pub fn fetch_manifest(path: &Path) -> Result<PluginManifest> {
    let owned_path = path.to_path_buf();
    let join_result = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .context("failed to build manifest-probe runtime")?;
        runtime.block_on(fetch_manifest_inner(&owned_path))
    })
    .join();

    match join_result {
        Ok(result) => result,
        Err(_) => anyhow::bail!("manifest probe worker thread panicked for {}", path.display()),
    }
}

async fn fetch_manifest_inner(path: &Path) -> Result<PluginManifest> {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("--manifest")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Scrub the daemon's environment before invoking the plugin. The same
    // allowlist used by full plugin spawns (host.rs) — manifest probes
    // never legitimately need plugin-declared env_required, since they
    // only print static metadata. A plugin that needs API credentials to
    // emit its manifest is a plugin bug, not a host concern.
    let allow: BTreeSet<&str> = PLUGIN_BASE_ENV_ALLOWLIST.iter().copied().collect();
    command.env_clear();
    for var in &allow {
        if let Some(value) = std::env::var_os(var) {
            command.env(var, value);
        }
    }

    let mut child = command.spawn().with_context(|| format!("failed to run {}", path.display()))?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("failed to capture plugin stdout"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("failed to capture plugin stderr"))?;

    let probe = async {
        let mut stdout_buf: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut stderr_buf: Vec<u8> = Vec::with_capacity(4 * 1024);
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut stdout_chunk = [0u8; 8 * 1024];
        let mut stderr_chunk = [0u8; 4 * 1024];

        // Interleave stdout/stderr reads so a plugin can't deadlock us by
        // filling one pipe while we wait on the other. Bail the moment
        // stdout exceeds the cap so the child can be killed before it
        // produces more data.
        while !stdout_done || !stderr_done {
            tokio::select! {
                read = stdout.read(&mut stdout_chunk), if !stdout_done => {
                    let n = read.with_context(|| format!("failed to read stdout from {}", path.display()))?;
                    if n == 0 {
                        stdout_done = true;
                    } else if stdout_buf.len() + n > MANIFEST_PROBE_MAX_STDOUT {
                        anyhow::bail!(
                            "plugin produced >1MiB on stdout for --manifest probe at {}; refusing to load",
                            path.display()
                        );
                    } else {
                        stdout_buf.extend_from_slice(&stdout_chunk[..n]);
                    }
                }
                read = stderr.read(&mut stderr_chunk), if !stderr_done => {
                    let n = read.with_context(|| format!("failed to read stderr from {}", path.display()))?;
                    if n == 0 {
                        stderr_done = true;
                    } else if stderr_buf.len() + n > MANIFEST_PROBE_MAX_STDOUT {
                        // Stderr cap mirrors stdout's — a plugin spewing
                        // GBs of stderr would wedge discovery too.
                        anyhow::bail!(
                            "plugin produced >1MiB on stderr for --manifest probe at {}; refusing to load",
                            path.display()
                        );
                    } else {
                        stderr_buf.extend_from_slice(&stderr_chunk[..n]);
                    }
                }
            }
        }

        let status = child.wait().await.with_context(|| format!("failed to wait on {}", path.display()))?;

        if !status.success() {
            let stderr_text = String::from_utf8_lossy(&stderr_buf);
            let trimmed = stderr_text.trim();
            if trimmed.is_empty() {
                anyhow::bail!("plugin manifest command failed for {} (exit={:?})", path.display(), status.code());
            }
            anyhow::bail!(
                "plugin manifest command failed for {} (exit={:?}): {}",
                path.display(),
                status.code(),
                trimmed
            );
        }

        serde_json::from_slice::<PluginManifest>(&stdout_buf)
            .with_context(|| format!("plugin {} returned malformed --manifest JSON", path.display()))
    };

    // Drive the probe under an overall deadline. When the timeout fires
    // (or when probe returns Err early), `child` is dropped and
    // `kill_on_drop` reaps the still-running process so we don't leak it.
    match tokio::time::timeout(MANIFEST_PROBE_TIMEOUT, probe).await {
        Ok(result) => result,
        Err(_) => {
            // Explicitly kill before drop so the reap is synchronous —
            // `kill_on_drop` schedules the reap async, and we want the
            // child gone before this function returns.
            let _ = child.start_kill();
            anyhow::bail!(
                "plugin manifest probe timed out after {}s for {}",
                MANIFEST_PROBE_TIMEOUT.as_secs(),
                path.display()
            )
        }
    }
}

fn scan_dir(
    dir: &Path,
    source: DiscoverySource,
    discovered: &mut Vec<DiscoveredPlugin>,
    warnings: &mut Vec<DiscoveryWarning>,
    seen: &mut HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_scanned_plugin_name(file_name) || seen.contains(file_name) {
            continue;
        }
        match fetch_manifest(&path) {
            Ok(manifest) => {
                seen.insert(file_name.to_string());
                discovered.push(DiscoveredPlugin { name: file_name.to_string(), path, manifest, source });
            }
            Err(error) => {
                // Reserve the name even on probe failure so a lower-precedence
                // source (e.g. global install dir) can't silently shadow a
                // broken higher-precedence override (e.g. project-local).
                // The warning still surfaces so operators can fix the
                // broken plugin instead of being routed to the wrong copy.
                seen.insert(file_name.to_string());
                let reason = format!("{error:#}");
                tracing::warn!(
                    plugin = %file_name,
                    path = %path.display(),
                    source = ?source,
                    "plugin manifest probe failed: {reason}"
                );
                warnings.push(DiscoveryWarning { name: file_name.to_string(), path: path.clone(), source, reason });
            }
        }
    }
}

fn is_scanned_plugin_name(name: &str) -> bool {
    name.starts_with("animus-plugin-") || name.starts_with("animus-provider-")
}

fn load_plugins_config(path: &Path) -> Result<PluginsConfig> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&content)?)
}

/// Canonical home for Animus state. Mirrors `protocol::Config::global_config_dir()`
/// but duplicated here to avoid a crate-level dep on `protocol`. Honors
/// `ANIMUS_CONFIG_DIR` for tests and overrides.
fn animus_home() -> PathBuf {
    if let Ok(value) = std::env::var("ANIMUS_CONFIG_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".animus")).unwrap_or_else(|| PathBuf::from(".animus"))
}

/// Returns the canonical plugin install directory.
///
/// Resolution order:
/// 1. `$ANIMUS_PLUGIN_DIR` (when set and non-empty)
/// 2. `<animus_home>/plugins`
pub fn plugin_install_dir() -> PathBuf {
    if let Ok(value) = std::env::var("ANIMUS_PLUGIN_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    animus_home().join("plugins")
}

/// Returns the canonical plugin registry yaml path.
///
/// The new location is `<animus_home>/plugins.yaml`. The legacy location
/// (`~/.config/animus/plugins.yaml`) is consulted automatically by
/// [`default_config_path`] when the new file does not yet exist, and is
/// migrated to the new path on the next write performed by the installer.
pub fn plugins_registry_path() -> PathBuf {
    animus_home().join("plugins.yaml")
}

/// Legacy registry location used before consolidation under `~/.animus/`.
/// Kept for one-shot migration on first read.
pub fn legacy_plugins_registry_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config/animus/plugins.yaml"))
        .unwrap_or_else(|| PathBuf::from(".config/animus/plugins.yaml"))
}

/// Read the registered `skip_manifest_check_at_install` flag for the named
/// plugin (or its provider counterpart) from the canonical registry.
///
/// Returns `false` when the registry is missing, the plugin is not listed, or
/// the flag is unset. Errors during registry read are swallowed and treated as
/// "flag absent" — the audit field is informational and must never block
/// discovery or `plugin info`.
pub fn registered_skip_manifest_check_at_install(plugin_name: &str) -> bool {
    let config_path = default_config_path();
    if !config_path.exists() {
        return false;
    }
    let Ok(config) = load_plugins_config(&config_path) else {
        return false;
    };
    let trimmed = plugin_name.trim();
    for (logical_name, entry) in config.plugins.iter().chain(config.providers.iter()) {
        let registered_name = entry.name.as_deref().unwrap_or(logical_name);
        let override_name = entry.name_override.as_deref();
        if registered_name == trimmed || logical_name == trimmed || override_name == Some(trimmed) {
            return entry.skip_manifest_check_at_install;
        }
    }
    false
}

fn default_config_path() -> PathBuf {
    let canonical = plugins_registry_path();
    if canonical.exists() {
        return canonical;
    }
    // When the caller has explicitly redirected `$ANIMUS_CONFIG_DIR`
    // (typically tests), respect that isolation and skip the legacy
    // `~/.config/animus/plugins.yaml` fallback — otherwise stale entries
    // from a developer's real home would leak into isolated runs.
    let config_dir_overridden = std::env::var("ANIMUS_CONFIG_DIR").map(|v| !v.trim().is_empty()).unwrap_or(false);
    if !config_dir_overridden {
        let legacy = legacy_plugins_registry_path();
        if legacy.exists() {
            return legacy;
        }
    }
    canonical
}

fn expand_home(value: &str) -> String {
    let Some(rest) = value.strip_prefix("~/") else {
        return value.to_string();
    };
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(rest).to_string_lossy().to_string())
        .unwrap_or_else(|| value.to_string())
}

fn find_binary(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute() || value.contains(std::path::MAIN_SEPARATOR) {
        return path.exists().then_some(path);
    }

    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var).map(|dir| dir.join(value)).find(|candidate| candidate.exists())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn default_discovery_does_not_scan_system_path() {
        let discovery = PluginDiscovery::new();
        assert!(!discovery.include_system_path, "PluginDiscovery::new() must not opt into $PATH scanning by default");
        assert!(
            !PluginDiscovery::default().include_system_path,
            "PluginDiscovery::default() must not opt into $PATH scanning"
        );
    }

    #[cfg(unix)]
    #[test]
    fn discover_by_kind_filters_to_matching_plugin_kind() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        let mk = |name: &str, kind: &str| -> PathBuf {
            let path = temp.path().join(name);
            let manifest = serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "plugin_kind": kind,
                "description": "test",
                "protocol_version": "1.0.0",
                "capabilities": []
            });
            fs::write(&path, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
            let mut perms = fs::metadata(&path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).expect("chmod");
            path
        };

        let wf_plugin = mk("a-workflow-runner-default", "workflow_runner");
        let subj_plugin = mk("b-subject-default", "subject_backend");

        let config_path = temp.path().join("plugins.yaml");
        fs::write(
            &config_path,
            format!(
                "plugins:\n  a-workflow-runner-default:\n    binary: {}\n  b-subject-default:\n    binary: {}\n",
                wf_plugin.to_string_lossy(),
                subj_plugin.to_string_lossy()
            ),
        )
        .expect("write config");

        let by_kind = PluginDiscovery::new()
            .with_config_path(config_path)
            .discover()
            .expect("discover")
            .into_iter()
            .filter(|p| p.manifest.plugin_kind == "workflow_runner")
            .collect::<Vec<_>>();

        assert_eq!(by_kind.len(), 1, "exactly one workflow_runner plugin");
        assert_eq!(by_kind[0].name, "a-workflow-runner-default");
    }

    #[test]
    fn configured_plugin_can_use_non_prefixed_binary() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));
        let plugin = temp.path().join("compatible-plugin");
        let manifest = serde_json::json!({
            "name": "compatible",
            "version": "0.1.0",
            "plugin_kind": "custom",
            "description": "test",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        fs::write(&plugin, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&plugin).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&plugin, permissions).expect("chmod");
        }

        let config_path = temp.path().join("plugins.yaml");
        fs::write(&config_path, format!("plugins:\n  compatible:\n    binary: {}\n", plugin.to_string_lossy()))
            .expect("write config");

        let discovered = PluginDiscovery::new().with_config_path(config_path).discover().expect("discover");

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "compatible");
    }

    /// v0.5.8: when `name_override` is recorded in `plugins.yaml`, discovery
    /// uses the override as the canonical [`DiscoveredPlugin::name`] instead
    /// of the manifest-declared name. Without this round-trip, the daemon's
    /// SubjectRouter alias map (keyed by `plugin.name`) cannot find lockfile
    /// entries that were keyed under the install-time `--name <NAME>` value.
    /// Closes codex P2 round-4 v0.5.7.
    #[cfg(unix)]
    #[test]
    fn name_override_overrides_manifest_name_in_discovery() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));
        let plugin = temp.path().join("renamed-bin");
        let manifest = serde_json::json!({
            "name": "animus-provider-default",
            "version": "0.1.0",
            "plugin_kind": "subject_backend",
            "description": "test",
            "protocol_version": "1.0.0",
            "capabilities": ["subject_kind:task"],
        });
        fs::write(&plugin, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        let mut permissions = fs::metadata(&plugin).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&plugin, permissions).expect("chmod");

        let config_path = temp.path().join("plugins.yaml");
        fs::write(
            &config_path,
            format!(
                "plugins:\n  custom-task:\n    binary: {}\n    name: animus-provider-default\n    name_override: custom-task\n",
                plugin.to_string_lossy()
            ),
        )
        .expect("write config");

        let discovered = PluginDiscovery::new().with_config_path(config_path).discover().expect("discover");
        assert_eq!(discovered.len(), 1);
        assert_eq!(
            discovered[0].name, "custom-task",
            "name_override must take precedence over the manifest-declared name in discovery",
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_manifest_probe_surfaces_warning_instead_of_silent_drop() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let temp = tempfile::tempdir().expect("tempdir");
        // Redirect $ANIMUS_CONFIG_DIR so the v0.4.19 unconditional global
        // install dir scan doesn't pick up the developer's real
        // `~/.animus/plugins/` and pollute the assertion counts.
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));
        let plugin = temp.path().join("animus-provider-explode");
        // Plugin script that fails when --manifest is invoked. Simulates the
        // oai/linear regression where a missing env var aborted the manifest
        // probe and the plugin silently disappeared from `animus plugin list`.
        fs::write(&plugin, "#!/bin/sh\necho 'OPENAI_API_KEY not set' >&2\nexit 1\n").expect("write plugin");
        let mut permissions = fs::metadata(&plugin).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&plugin, permissions).expect("chmod");

        let config_path = temp.path().join("plugins.yaml");
        fs::write(&config_path, format!("providers:\n  explode:\n    binary: {}\n", plugin.to_string_lossy()))
            .expect("write config");

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(config_path).discover_with_warnings().expect("discover");

        assert!(discovered.is_empty(), "plugin with failed manifest must not appear in discovered list");
        assert_eq!(warnings.len(), 1, "expected exactly one discovery warning, got {warnings:?}");
        let warning = &warnings[0];
        assert_eq!(warning.name, "explode");
        assert_eq!(warning.path, plugin);
        assert_eq!(warning.source, DiscoverySource::ExplicitConfig);
        assert!(
            warning.reason.contains("manifest"),
            "warning reason should mention the manifest failure, got: {}",
            warning.reason
        );
    }

    #[cfg(unix)]
    #[test]
    fn missing_configured_binary_surfaces_warning() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));
        let config_path = temp.path().join("plugins.yaml");
        fs::write(&config_path, "plugins:\n  ghost:\n    binary: /tmp/definitely-not-a-real-plugin-binary-xyz123\n")
            .expect("write config");

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(config_path).discover_with_warnings().expect("discover");

        assert!(discovered.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].name, "ghost");
        assert_eq!(warnings[0].source, DiscoverySource::ExplicitConfig);
        assert!(warnings[0].reason.contains("not found"));
    }

    #[cfg(unix)]
    #[test]
    fn scan_dir_failed_manifest_surfaces_warning() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));
        let plugins_dir = temp.path().join(".animus/plugins");
        fs::create_dir_all(&plugins_dir).expect("mkdir");
        let plugin = plugins_dir.join("animus-plugin-broken");
        fs::write(&plugin, "#!/bin/sh\nexit 2\n").expect("write plugin");
        let mut permissions = fs::metadata(&plugin).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&plugin, permissions).expect("chmod");

        // Point discovery at an empty config so only the project-local scan runs.
        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(temp.path())
            .with_config_path(empty_config)
            .discover_with_warnings()
            .expect("discover");

        assert!(discovered.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].name, "animus-plugin-broken");
        assert_eq!(warnings[0].source, DiscoverySource::ProjectLocal);
    }

    // ---- env-var-driven path resolution ---------------------------------
    //
    // The helpers below read `$ANIMUS_PLUGIN_DIR` / `$ANIMUS_CONFIG_DIR` /
    // `$HOME`. Cargo runs tests on multiple threads in the same process, so we
    // serialize the env-touching tests behind a mutex to avoid races with
    // other tests in this module that may also read these vars.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(prev) => std::env::set_var(self.key, prev),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn plugin_install_dir_honors_animus_plugin_dir_env_var() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let custom = temp.path().join("custom-plugins");
        let _env = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &custom);

        let resolved = plugin_install_dir();

        assert_eq!(resolved, custom, "$ANIMUS_PLUGIN_DIR must drive plugin_install_dir()");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_uses_animus_plugin_dir_env_var() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let install_dir = temp.path().join("env-install-dir");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");

        let manifest = serde_json::json!({
            "name": "animus-provider-envoy",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "test plugin",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let plugin_path = install_dir.join("animus-provider-envoy");
        fs::write(&plugin_path, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        let mut permissions = fs::metadata(&plugin_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&plugin_path, permissions).expect("chmod");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(discovered.len(), 1, "$ANIMUS_PLUGIN_DIR install dir must be scanned, got {discovered:?}");
        assert_eq!(discovered[0].name, "animus-provider-envoy");
        assert_eq!(discovered[0].path, plugin_path);
        assert_eq!(discovered[0].source, DiscoverySource::PluginPath);
    }

    #[test]
    fn plugin_registry_path_falls_back_to_legacy_when_canonical_missing() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("fake-home");
        fs::create_dir_all(&fake_home).expect("mkdir fake home");
        let legacy_dir = fake_home.join(".config/animus");
        fs::create_dir_all(&legacy_dir).expect("mkdir legacy");
        let legacy_file = legacy_dir.join("plugins.yaml");
        fs::write(&legacy_file, "plugins: {}\n").expect("write legacy");

        // Drive `animus_home()` purely via $HOME so the legacy-fallback
        // branch in `default_config_path()` is exercised. `ANIMUS_CONFIG_DIR`
        // *must not* be set during this test — when it's set, the
        // `config_dir_overridden` guard intentionally skips the legacy
        // fallback (this was the source of the v0.4.x "pre-existing flake":
        // some prior test left `ANIMUS_CONFIG_DIR` populated, so the
        // legacy fallback never ran). v0.4.10: explicitly unset both
        // overrides for the duration of this test.
        let _home = EnvVarGuard::set("HOME", &fake_home);
        let _config_clear = EnvVarGuard::set("ANIMUS_CONFIG_DIR", "");
        let _plugin_dir_clear = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");

        // animus_home() falls back to $HOME/.animus when ANIMUS_CONFIG_DIR is empty.
        let canonical = plugins_registry_path();
        assert_eq!(canonical, fake_home.join(".animus/plugins.yaml"));
        assert!(!canonical.exists(), "canonical registry path should not exist yet in this test");

        let resolved = default_config_path();
        assert_eq!(
            resolved, legacy_file,
            "default_config_path() must fall back to the legacy location when canonical is absent"
        );
    }

    // ---- fetch_manifest sandboxing (gap #10) -----------------------------

    #[cfg(unix)]
    #[test]
    fn fetch_manifest_kills_hanging_plugin_after_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let plugin = temp.path().join("animus-plugin-hang");
        // Sleep forever — discovery used to wait synchronously and freeze.
        fs::write(&plugin, "#!/bin/sh\nsleep 600\n").expect("write plugin");
        let mut perms = fs::metadata(&plugin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin, perms).expect("chmod");

        let started = std::time::Instant::now();
        let result = fetch_manifest(&plugin);
        let elapsed = started.elapsed();

        let err = result.expect_err("hanging plugin must not return a manifest");
        let reason = format!("{err:#}");
        assert!(reason.contains("timed out"), "error must mention timeout, got: {reason}");
        assert!(
            reason.contains(plugin.display().to_string().as_str()),
            "error must mention the plugin path so operators can debug, got: {reason}"
        );
        // Allow generous slack over the 5s budget — CI machines are slow.
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "fetch_manifest must return promptly after timeout, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fetch_manifest_kills_plugin_that_overruns_stdout_cap() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let plugin = temp.path().join("animus-plugin-spew");
        // Use `yes` to produce well over 1 MiB of stdout very quickly.
        // `head -c 4194304` is portable and bounds the worst case in case
        // our stdout cap fails — we don't want to fill the test runner's
        // disk.
        fs::write(
            &plugin,
            "#!/bin/sh\n# print ~4 MiB of garbage to stdout to overrun the 1 MiB cap\nyes 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' | head -c 4194304\n",
        )
        .expect("write plugin");
        let mut perms = fs::metadata(&plugin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin, perms).expect("chmod");

        let err = fetch_manifest(&plugin).expect_err("plugin that exceeds stdout cap must fail");
        let reason = format!("{err:#}");
        assert!(
            reason.contains(">1MiB") || reason.contains("1MiB"),
            "error must mention the stdout cap, got: {reason}"
        );
        assert!(
            reason.contains(plugin.display().to_string().as_str()),
            "error must mention the plugin path, got: {reason}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fetch_manifest_scrubs_secret_env_vars_from_plugin() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp = tempfile::tempdir().expect("tempdir");
        let plugin = temp.path().join("animus-plugin-snoop");
        // The plugin echoes a placeholder manifest if the secret is NOT
        // visible, otherwise it fails with the secret value. We use a
        // unique env name so it doesn't collide with the rest of the
        // test suite's env state.
        let secret_name = "ANIMUS_TEST_MANIFEST_PROBE_SECRET_XYZ";
        let script = format!(
            "#!/bin/sh\nif [ -n \"${{{}}}\" ]; then\n  echo 'plugin saw secret: '${{{}}} >&2\n  exit 17\nfi\nprintf '{{\"name\":\"snoop\",\"version\":\"0.1.0\",\"plugin_kind\":\"custom\",\"description\":\"t\",\"protocol_version\":\"1.0.0\",\"capabilities\":[]}}\\n'\n",
            secret_name, secret_name
        );
        fs::write(&plugin, script).expect("write plugin");
        let mut perms = fs::metadata(&plugin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin, perms).expect("chmod");

        let _secret = EnvVarGuard::set(secret_name, "sensitive-value-do-not-leak");

        let manifest = fetch_manifest(&plugin).expect("manifest probe must succeed when env is scrubbed");
        assert_eq!(manifest.name, "snoop", "manifest must round-trip when secret is scrubbed");
    }

    // ---- global plugin install dir precedence (v0.4.19) ----------------
    //
    // `~/.animus/plugins/` is the canonical install target for
    // `animus plugin install`. Discovery must scan it unconditionally so
    // operators do not have to symlink the directory into every project.

    #[cfg(unix)]
    fn write_executable_plugin(path: &Path, manifest_name: &str) {
        use std::os::unix::fs::PermissionsExt;

        let manifest = serde_json::json!({
            "name": manifest_name,
            "version": "0.1.0",
            "plugin_kind": "custom",
            "description": "test plugin",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        fs::write(path, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_scans_global_install_dir_without_plugin_dir_env_var() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        // Redirect `animus_home()` to a scratch dir so the test never touches
        // the developer's real `~/.animus/plugins/`. `animus_home()` honors
        // `ANIMUS_CONFIG_DIR`, so this also drives `plugin_install_dir()`.
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);

        let plugin_path = fake_install.join("animus-provider-globe");
        write_executable_plugin(&plugin_path, "animus-provider-globe");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(
            discovered.len(),
            1,
            "global install dir must be scanned even when $ANIMUS_PLUGIN_DIR is unset, got {discovered:?}"
        );
        assert_eq!(discovered[0].name, "animus-provider-globe");
        assert_eq!(discovered[0].path, plugin_path);
        assert_eq!(discovered[0].source, DiscoverySource::PluginPath);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_project_local_takes_precedence_over_global_install_dir() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);

        let project_root = temp.path().join("project");
        let project_plugins = project_root.join(".animus/plugins");
        fs::create_dir_all(&project_plugins).expect("mkdir project plugins");

        let plugin_name = "animus-plugin-duplicate";
        let project_path = project_plugins.join(plugin_name);
        let global_path = fake_install.join(plugin_name);
        write_executable_plugin(&project_path, plugin_name);
        write_executable_plugin(&global_path, plugin_name);

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(discovered.len(), 1, "duplicate name across sources must dedupe to one entry, got {discovered:?}");
        assert_eq!(discovered[0].path, project_path, "project-local install must outrank the global install dir");
        assert_eq!(discovered[0].source, DiscoverySource::ProjectLocal);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_global_install_dir_in_addition_to_project_local() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);

        let project_root = temp.path().join("project");
        let project_plugins = project_root.join(".animus/plugins");
        fs::create_dir_all(&project_plugins).expect("mkdir project plugins");

        let project_only = project_plugins.join("animus-plugin-projectonly");
        let global_only = fake_install.join("animus-plugin-globalonly");
        write_executable_plugin(&project_only, "animus-plugin-projectonly");
        write_executable_plugin(&global_only, "animus-plugin-globalonly");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(discovered.len(), 2, "both project-local and global plugins must be discovered, got {discovered:?}");
        let names: BTreeSet<&str> = discovered.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains("animus-plugin-projectonly"), "project-local plugin missing from {names:?}");
        assert!(names.contains("animus-plugin-globalonly"), "global plugin missing from {names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_failed_project_local_blocks_lower_precedence_global_duplicate() {
        // Even when a higher-precedence source fails its manifest probe, it
        // must still reserve the plugin name so a lower-precedence source
        // can't silently shadow the broken override. Codex P2 from the
        // v0.4.19 self-vet.
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let global_install = fake_home.join("plugins");
        fs::create_dir_all(&global_install).expect("mkdir global install");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);

        let project_root = temp.path().join("project");
        let project_plugins = project_root.join(".animus/plugins");
        fs::create_dir_all(&project_plugins).expect("mkdir project plugins");

        let plugin_name = "animus-plugin-duplicate";

        // Project-local copy: broken — manifest probe fails with non-zero
        // exit. This MUST block the global copy from being silently used.
        let broken_project = project_plugins.join(plugin_name);
        fs::write(&broken_project, "#!/bin/sh\nexit 2\n").expect("write broken project plugin");
        let mut perms = fs::metadata(&broken_project).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&broken_project, perms).expect("chmod");

        // Global copy: would be perfectly loadable if reached.
        let global_path = global_install.join(plugin_name);
        write_executable_plugin(&global_path, plugin_name);

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .discover_with_warnings()
            .expect("discover");

        assert!(discovered.is_empty(), "broken project-local plugin must block the global copy, got: {discovered:?}");
        assert_eq!(
            warnings.len(),
            1,
            "exactly one warning expected for the broken project-local copy, got: {warnings:?}"
        );
        assert_eq!(warnings[0].name, plugin_name);
        assert_eq!(warnings[0].source, DiscoverySource::ProjectLocal);
    }

    // ---- per-project scope filter (v0.5.9) -----------------------------

    #[cfg(unix)]
    #[test]
    fn scope_filter_allowlist_drops_out_of_scope_plugins() {
        use crate::scope::{PluginScope, PluginScopeMode};

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let keep_path = install_dir.join("animus-provider-default");
        let drop_path = install_dir.join("animus-provider-linear");
        write_executable_plugin(&keep_path, "animus-provider-default");
        write_executable_plugin(&drop_path, "animus-provider-linear");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let mut scope = PluginScope { mode: PluginScopeMode::Allowlist, ..PluginScope::default() };
        scope.allow.insert("animus-provider-default".to_string());

        let (discovered, _warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_scope(scope)
            .discover_with_warnings()
            .expect("discover");

        let names: BTreeSet<&str> = discovered.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains("animus-provider-default"), "in-scope plugin must remain, got {names:?}");
        assert!(!names.contains("animus-provider-linear"), "out-of-scope plugin must be filtered, got {names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn scope_filter_all_mode_returns_full_set() {
        use crate::scope::PluginScope;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let one = install_dir.join("animus-provider-default");
        let two = install_dir.join("animus-provider-linear");
        write_executable_plugin(&one, "animus-provider-default");
        write_executable_plugin(&two, "animus-provider-linear");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let scope = PluginScope::unrestricted();
        let (discovered, _warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_scope(scope)
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(discovered.len(), 2, "mode=all must return every discovered plugin");
    }

    #[cfg(unix)]
    #[test]
    fn scope_filter_preserves_warnings_for_out_of_scope_failed_plugin() {
        use crate::scope::{PluginScope, PluginScopeMode};
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let broken = install_dir.join("animus-provider-broken");
        fs::write(&broken, "#!/bin/sh\nexit 2\n").expect("write broken");
        let mut perms = fs::metadata(&broken).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&broken, perms).expect("chmod");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // Allowlist that does NOT include the broken plugin — but discovery
        // must still emit a warning so operators see the failed probe.
        let mut scope = PluginScope { mode: PluginScopeMode::Allowlist, ..PluginScope::default() };
        scope.allow.insert("animus-provider-default".to_string());

        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_scope(scope)
            .discover_with_warnings()
            .expect("discover");

        assert!(discovered.is_empty(), "broken plugin must not appear in discovered set");
        assert_eq!(warnings.len(), 1, "warning must survive scope filter, got {warnings:?}");
        assert_eq!(warnings[0].name, "animus-provider-broken");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_animus_plugin_dir_overrides_global_install_location() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        // Real install dir under the fake home should be IGNORED once
        // $ANIMUS_PLUGIN_DIR redirects discovery elsewhere.
        let ignored_install = fake_home.join("plugins");
        fs::create_dir_all(&ignored_install).expect("mkdir ignored install");
        let ignored_plugin = ignored_install.join("animus-plugin-ignored");
        write_executable_plugin(&ignored_plugin, "animus-plugin-ignored");

        let redirected = temp.path().join("env-install");
        fs::create_dir_all(&redirected).expect("mkdir redirected");
        let env_plugin = redirected.join("animus-plugin-envtarget");
        write_executable_plugin(&env_plugin, "animus-plugin-envtarget");

        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &redirected);

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        let names: BTreeSet<&str> = discovered.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains("animus-plugin-envtarget"), "$ANIMUS_PLUGIN_DIR target must be scanned, got {names:?}");
        assert!(
            !names.contains("animus-plugin-ignored"),
            "$ANIMUS_PLUGIN_DIR override must replace the default ~/.animus/plugins/ scan, got {names:?}"
        );
    }
}
