use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::PluginManifest;
use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::db_registry::PluginRegistrySource;
use crate::host::PLUGIN_BASE_ENV_ALLOWLIST;
use crate::lockfile::PluginLockfile;
use crate::manifest_cache::ManifestCache;
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
    /// The desired plugin set read from the Postgres `plugin_registry` served
    /// by the animus-postgres BaaS. Opt-in — only active once the daemon wires
    /// a [`PluginRegistrySource`] AFTER the bootstrap DB-backend plugin is up
    /// (see [`crate::db_registry`]).
    DbRegistry,
}

impl DiscoverySource {
    /// `true` when a candidate from this source can be committed into a
    /// cloned repository, and is therefore attacker-controlled on a server
    /// that clones untrusted repos.
    ///
    /// Only [`DiscoverySource::ProjectLocal`] is repo-shippable — it covers
    /// BOTH the project-local `<project>/.animus/plugins/` directory scan AND
    /// the project-local `<project>/.animus/plugins.yaml` registry (whose
    /// `binary:` entries the repo author controls). Every other source is
    /// operator-installed into the user/global registry
    /// (`ExplicitConfig` → `~/.animus/plugins.yaml`), the global install dir
    /// (`PluginPath`), or the operator's `$PATH` (`SystemPath`), and is
    /// trusted.
    ///
    /// UNTRUSTED candidates are pre-probe gated by the scope's filename slug
    /// ([`PluginScope::may_probe`]) and, one layer up, are only enumerated at
    /// all when the caller opts into project-local probing
    /// ([`PluginDiscovery::probe_project_local_plugins`]). TRUSTED candidates
    /// are always probed so the post-probe manifest-name fallback
    /// ([`crate::scope::PluginScope::admits`]) can still admit a plugin
    /// installed under a `--name <NAME>` override whose filename slug is not
    /// in the flavor admit set.
    pub fn is_untrusted(self) -> bool {
        matches!(self, DiscoverySource::ProjectLocal)
    }
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
    // BTreeMap so iteration order is deterministic: duplicate logical names
    // across `plugins:` / `providers:` always resolve to the same winner
    // (first in key order within `plugins:`, then `providers:`) instead of
    // varying with HashMap iteration order run to run.
    #[serde(default)]
    plugins: BTreeMap<String, PluginConfigEntry>,
    #[serde(default)]
    providers: BTreeMap<String, PluginConfigEntry>,
}

#[derive(Clone, Default)]
pub struct PluginDiscovery {
    project_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    include_system_path: bool,
    probe_project_local_plugins: bool,
    scope: Option<PluginScope>,
    db_registry: Option<Arc<dyn PluginRegistrySource>>,
}

// Manual Debug: `dyn PluginRegistrySource` is not `Debug`, so the derive can't
// apply. The source is rendered as an opaque presence marker.
impl std::fmt::Debug for PluginDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginDiscovery")
            .field("project_root", &self.project_root)
            .field("config_path", &self.config_path)
            .field("include_system_path", &self.include_system_path)
            .field("probe_project_local_plugins", &self.probe_project_local_plugins)
            .field("scope", &self.scope)
            .field("db_registry", &self.db_registry.as_ref().map(|_| "<PluginRegistrySource>"))
            .finish()
    }
}

/// A binary the discovery walker found and now needs a manifest for.
struct ProbeCandidate {
    name: String,
    path: PathBuf,
    source: DiscoverySource,
}

enum ProbeOutcome {
    Hit(PluginManifest),
    Probed(Result<PluginManifest>),
    /// The candidate was NOT executed because the active plugin scope
    /// could not admit it (its filename-derived slug is outside the
    /// admit set). Carries the human-facing reason for the surfaced
    /// [`DiscoveryWarning`].
    SkippedOutOfScope(String),
}

fn cap_parallelism() -> usize {
    let available = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    available.clamp(1, 8)
}

/// Resolve a sha256 hint for `path` so the manifest cache can be consulted
/// without spawning a `--manifest` probe.
///
/// We always hash the actual file on disk rather than trusting the
/// lockfile-recorded sha. Hashing a typical 5-20 MB plugin binary in
/// release mode costs a few milliseconds; trusting a possibly-stale
/// lockfile sha would let an out-of-band binary swap (e.g. `cp -p`, tar
/// restore) serve the wrong cached manifest under the old key. The
/// `lockfile` argument is retained so callers don't break and so future
/// versions can opportunistically skip the hash when the lockfile records
/// a path-bound integrity claim. Codex rounds 2 + 4 P2.
fn resolve_sha_for_binary(_lockfile: Option<&PluginLockfile>, _lock_name: &str, path: &Path) -> Option<String> {
    ManifestCache::hash_binary(path).ok()
}

/// Return `true` when `path` looks plausibly executable so the cache-hit
/// fast path doesn't mask a `chmod -x` regression that the previous
/// per-call `--manifest` probe would have caught. On Unix we require at
/// least one execute bit. TODO(codex-p3): this can still hand back a
/// cached manifest when only "other" or "group" exec bits remain set but
/// the current process is the owner without exec — a narrower
/// `access(path, X_OK)` check would also surface that case. The probe
/// will still fail at spawn time today and the operator can clear the
/// cache to recover.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

/// Pick the sha256 key to insert under after a live `--manifest` probe.
///
/// We hash before AND after the probe and only return a key when the two
/// digests agree. A binary swap concurrent with the probe (e.g. another
/// shell racing an `animus plugin install` or `animus plugin update`)
/// would otherwise let us insert the OLD manifest under the NEW binary's
/// sha key, poisoning future cache hits. When the pre/post hashes don't
/// match we skip caching this round — the next discovery run will see a
/// stable binary and repopulate the cache cleanly. Codex round 6 P2.
fn insert_key_for(path: &Path, pre_probe_sha: Option<&str>) -> Option<String> {
    let post_probe_sha = ManifestCache::hash_binary(path).ok()?;
    match pre_probe_sha {
        Some(pre) if pre == post_probe_sha => Some(post_probe_sha),
        Some(_) => None,
        None => Some(post_probe_sha),
    }
}

/// Drive a batch of probe candidates against the cache + (when warranted)
/// a parallel `--manifest` probe pool. Returns results in the same order
/// as `candidates`.
fn resolve_manifests(
    candidates: &[ProbeCandidate],
    cache: &ManifestCache,
    lockfile: Option<&PluginLockfile>,
    scope: Option<&PluginScope>,
) -> Vec<ProbeOutcome> {
    let mut outcomes: Vec<Option<ProbeOutcome>> = (0..candidates.len()).map(|_| None).collect();

    // SECURITY: gate the `--manifest` probe (which EXECUTES the candidate
    // binary) on TRUST + scope BEFORE any hashing or spawning.
    //
    // The pre-probe skip fires ONLY for UNTRUSTED (repo-shippable)
    // candidates — `DiscoverySource::ProjectLocal`, covering both the
    // project-local `.animus/plugins/` dir scan and the project-local
    // `.animus/plugins.yaml` registry — whose filename-derived slug the
    // active scope cannot admit ([`PluginScope::may_probe`]). This is the
    // load-bearing defense against a cloned hostile repo shipping an
    // attacker binary (or a `plugins.yaml` `binary:` entry pointing at one)
    // and getting it executed during discovery. Because the attacker
    // controls the file name, only the path/filename gate is authoritative
    // for these sources — a logical-name escape is NOT offered, or a hostile
    // repo could pair an admitted logical key (`animus-provider-claude`)
    // with `binary: .animus/plugins/evil` and get the evil binary run.
    //
    // TRUSTED candidates (global registry, global install dir, `$PATH`) are
    // never pre-probe skipped: they are operator-installed, not
    // repo-shippable, so we always probe them and let the post-probe
    // [`crate::scope::PluginScope::admits`] predicate filter — which
    // additionally consults the manifest-declared name so a plugin installed
    // under a `--name <NAME>` override (whose filename slug is outside the
    // flavor admit set) is still discovered.
    //
    // Unrestricted (mode=all) scopes admit everything, so local-dev behavior
    // is unchanged.
    let mut gated: Vec<bool> = vec![false; candidates.len()];
    if let Some(scope) = scope {
        if !scope.admits_everything() {
            for (idx, cand) in candidates.iter().enumerate() {
                if !cand.source.is_untrusted() {
                    continue;
                }
                if !scope.may_probe(&cand.path) {
                    gated[idx] = true;
                    outcomes[idx] = Some(ProbeOutcome::SkippedOutOfScope(format!(
                        "skipped --manifest probe: `{}` is not admitted by the active plugin scope ({}); \
                         binary was NOT executed",
                        cand.name,
                        scope.mode.as_wire(),
                    )));
                }
            }
        }
    }

    let cache_enabled = cache.is_enabled();
    // When the cache is disabled (kill switch), skip every hash — we
    // cannot use the digest for lookup OR insert, so paying the I/O is
    // pure waste. Codex round 8 P2.
    let mut shas: Vec<Option<String>> = vec![None; candidates.len()];
    if cache_enabled {
        for (idx, cand) in candidates.iter().enumerate() {
            if gated[idx] {
                continue;
            }
            shas[idx] = resolve_sha_for_binary(lockfile, &cand.name, &cand.path);
        }
    }

    let mut probe_indices: Vec<usize> = Vec::new();
    for (idx, cand) in candidates.iter().enumerate() {
        if gated[idx] {
            continue;
        }
        if !cache_enabled || !is_executable(&cand.path) {
            probe_indices.push(idx);
            continue;
        }
        if let Some(sha) = shas[idx].as_deref() {
            if let Some(manifest) = cache.lookup_for_path(sha, &cand.path) {
                outcomes[idx] = Some(ProbeOutcome::Hit(manifest));
                continue;
            }
        }
        probe_indices.push(idx);
    }

    if probe_indices.is_empty() {
        return outcomes.into_iter().map(|o| o.expect("every candidate must have an outcome")).collect();
    }

    let parallelism = cap_parallelism().min(probe_indices.len()).max(1);
    if parallelism <= 1 || probe_indices.len() == 1 {
        for idx in probe_indices {
            // Reuse the lookup-side sha we already paid for as the
            // pre-probe hash so the TOCTOU detection costs one hash, not
            // two. When the cache was disabled or sha resolution failed
            // earlier, fall back to hashing here. Codex round 8 P2.
            let pre_sha = shas[idx].clone().or_else(|| {
                if cache_enabled {
                    ManifestCache::hash_binary(&candidates[idx].path).ok()
                } else {
                    None
                }
            });
            let result = fetch_manifest(&candidates[idx].path);
            if cache_enabled {
                if let Ok(ref manifest) = result {
                    if let Some(sha) = insert_key_for(&candidates[idx].path, pre_sha.as_deref()) {
                        let _ = cache.insert(&sha, manifest);
                    }
                }
            }
            outcomes[idx] = Some(ProbeOutcome::Probed(result));
        }
    } else {
        let (tx, rx) = mpsc::channel::<(usize, Option<String>, Result<PluginManifest>)>();
        let queue: std::sync::Arc<std::sync::Mutex<std::vec::IntoIter<usize>>> =
            std::sync::Arc::new(std::sync::Mutex::new(probe_indices.clone().into_iter()));
        let paths: std::sync::Arc<Vec<PathBuf>> =
            std::sync::Arc::new(candidates.iter().map(|c| c.path.clone()).collect());
        let pre_shas: std::sync::Arc<Vec<Option<String>>> = std::sync::Arc::new(shas.clone());
        let mut handles = Vec::with_capacity(parallelism);
        for _ in 0..parallelism {
            let queue = std::sync::Arc::clone(&queue);
            let paths = std::sync::Arc::clone(&paths);
            let pre_shas = std::sync::Arc::clone(&pre_shas);
            let tx = tx.clone();
            handles.push(std::thread::spawn(move || loop {
                let next_idx = {
                    let mut guard = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.next()
                };
                let Some(idx) = next_idx else {
                    break;
                };
                let pre_sha = pre_shas[idx].clone().or_else(|| {
                    if cache_enabled {
                        ManifestCache::hash_binary(&paths[idx]).ok()
                    } else {
                        None
                    }
                });
                let result = fetch_manifest(&paths[idx]);
                if tx.send((idx, pre_sha, result)).is_err() {
                    break;
                }
            }));
        }
        drop(tx);
        for (idx, pre_sha, result) in rx {
            if cache_enabled {
                if let Ok(ref manifest) = result {
                    if let Some(sha) = insert_key_for(&candidates[idx].path, pre_sha.as_deref()) {
                        let _ = cache.insert(&sha, manifest);
                    }
                }
            }
            outcomes[idx] = Some(ProbeOutcome::Probed(result));
        }
        for handle in handles {
            let _ = handle.join();
        }
    }

    outcomes.into_iter().map(|o| o.expect("every candidate must have an outcome")).collect()
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

    /// Opt in to scanning `$PATH` for `animus-*` plugin binaries.
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

    /// Opt in to the project-local (repo-shippable) discovery tier:
    /// the `<project_root>/.animus/plugins/` directory scan AND the
    /// `<project_root>/.animus/plugins.yaml` project registry.
    ///
    /// Defaults to `false`. This is the hostile-repo defense: BOTH sources
    /// EXECUTE a binary the repo author controls (the dir scan runs every
    /// matching `animus-plugin-*` / `animus-provider-*` file with
    /// `--manifest`; the registry runs whatever its `binary:` entries point
    /// at), so a cloud daemon that clones a repo shipping
    /// `.animus/plugins/animus-provider-evil` — or a `.animus/plugins.yaml`
    /// with `binary: ./.animus/plugins/evil` — would otherwise run that
    /// attacker binary during discovery. Cloud daemons / servers therefore
    /// leave this OFF (see [`discover_plugins`]); explicit local-dev flows
    /// that intentionally support project-scoped installs opt in (see
    /// [`discover_plugins_including_project_local`]).
    ///
    /// When opted in, probes from these sources remain subject to the
    /// pre-probe [`PluginScope::may_probe`] gate (defense in depth for a
    /// local dev with a restricted flavor/allowlist scope). Every other tier
    /// (global registry, global install dir, `$ANIMUS_PLUGIN_PATH`, `$PATH`)
    /// is unaffected by this flag.
    pub fn probe_project_local_plugins(mut self, probe_project_local_plugins: bool) -> Self {
        self.probe_project_local_plugins = probe_project_local_plugins;
        self
    }

    /// Apply a [`PluginScope`] filter to discovery.
    ///
    /// The scope is resolved BEFORE any `--manifest` probe and gates which
    /// candidates are executed at all: a candidate whose filename-derived
    /// slug the scope cannot admit is never spawned (see
    /// [`PluginScope::may_probe`]) and surfaces as a "skipped, out of
    /// scope" [`DiscoveryWarning`] instead. Candidates that clear the
    /// pre-probe gate are still filtered by the post-probe
    /// [`PluginScope::admits`] predicate (which additionally consults the
    /// manifest-declared name for `--name` overrides).
    ///
    /// When this builder method is not called, [`PluginScopeMode::All`]
    /// semantics apply (the v0.5.8 behavior) and every candidate is
    /// probeable.
    pub fn with_scope(mut self, scope: PluginScope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Wire the DB-backed plugin registry ([`PluginRegistrySource`]) as an
    /// additional discovery tier. The tier resolves the desired plugin set
    /// recorded in the Postgres `plugin_registry` against the binaries present
    /// on the volume.
    ///
    /// Bootstrap paradox: the daemon must call this ONLY after the bootstrap
    /// DB-backend plugin is up (the plugin that serves the registry can't be
    /// gated on the registry). When this builder is not called, discovery runs
    /// the file/dir tiers alone and the DB tier is a no-op. See
    /// [`crate::db_registry`].
    pub fn with_db_registry(mut self, source: Arc<dyn PluginRegistrySource>) -> Self {
        self.db_registry = Some(source);
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
    /// 1. Project-local tier: the install dir scan
    ///    (`<project_root>/.animus/plugins/`) followed by the project
    ///    registry (`<project_root>/.animus/plugins.yaml`) — the
    ///    highest-priority tier so a project-scoped install
    ///    (`animus plugin install --project`) shadows BOTH a hand-pinned
    ///    global registry entry and a global install of the same name. Both
    ///    of these sources are repo-shippable (UNTRUSTED) and are only walked
    ///    when the caller opts into
    ///    [`PluginDiscovery::probe_project_local_plugins`]; the server-safe
    ///    default ([`discover_plugins`]) skips the entire tier so a cloned
    ///    hostile repo's binaries are never executed during discovery.
    /// 2. Explicit registry config (`~/.animus/plugins.yaml`, or the path
    ///    supplied to [`PluginDiscovery::with_config_path`]). Every global
    ///    `animus plugin install` records its entry here, so this tier is
    ///    where registry-recorded global installs resolve.
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

        let cache = ManifestCache::from_default();
        let lockfile = PluginLockfile::load_default(self.project_root.as_deref()).ok();

        // SECURITY: resolve the effective scope FIRST — before any
        // `scan_dir` / `--manifest` probe. The probe EXECUTES the candidate
        // binary, so it MUST be gated by the scope's admit set up front,
        // not filtered afterwards. Previously scope resolution ran last and
        // every candidate (including project-local binaries shipped inside a
        // cloned hostile repo) was executed before the filter applied.
        //
        // Auto-load the project scope when a `project_root` is set but no
        // explicit `with_scope(...)` override has been supplied, so the
        // dozen-or-so direct PluginDiscovery callers honor
        // `.animus/plugin-scope.yaml` without wiring the loader manually.
        // Builders wanting unrestricted discovery pass
        // `PluginScope::unrestricted()` explicitly.
        let effective_scope: Option<PluginScope> = match &self.scope {
            Some(s) => Some(s.clone()),
            None => self.project_root.as_ref().map(|root| {
                PluginScope::load_for_project(root).unwrap_or_else(|err| {
                    tracing::warn!(
                        error = %err,
                        "failed to auto-load plugin scope; falling back to unrestricted discovery"
                    );
                    PluginScope::unrestricted()
                })
            }),
        };
        let scope_ref = effective_scope.as_ref();

        // Project-local installs scan FIRST: the explicit registry yaml is
        // written by every global `animus plugin install`, so letting it win
        // would silently invert the documented "project shadows global" rule
        // for any name that was ever installed globally.
        if let Some(project_root) = &self.project_root {
            // The raw project-local directory scan EXECUTES any
            // `animus-plugin-*` / `animus-provider-*` binary it finds. A
            // cloned repo can ship such a binary, so this scan is opt-in
            // (default OFF) and is only enabled for explicit local-dev
            // flows — cloud daemons never probe binaries shipped inside a
            // cloned repo. See [`PluginDiscovery::probe_project_local_plugins`].
            // Both project-local sources are repo-shippable (UNTRUSTED) and
            // EXECUTE binaries the repo author controls:
            //   * the `.animus/plugins/` directory scan runs any
            //     `animus-plugin-*` / `animus-provider-*` binary it finds;
            //   * the `.animus/plugins.yaml` registry runs whatever its
            //     `binary:` entries point at.
            // A cloned hostile repo can ship EITHER, so BOTH are gated behind
            // the same opt-in — cloud daemons / servers leave it OFF and never
            // execute a repo-shipped binary during discovery (see
            // [`PluginDiscovery::probe_project_local_plugins`] and the
            // server-safe [`discover_plugins`]). Explicit local-dev flows opt
            // in via [`discover_plugins_including_project_local`].
            if self.probe_project_local_plugins {
                scan_dir(
                    &project_root.join(".animus/plugins"),
                    DiscoverySource::ProjectLocal,
                    &mut discovered,
                    &mut warnings,
                    &mut seen,
                    &cache,
                    lockfile.as_ref(),
                    scope_ref,
                );
                // The dir scan only matches `animus-plugin-*` /
                // `animus-provider-*` file names; the project registry tier
                // resolves project-scoped installs of every other official
                // plugin name (`animus-subject-*`, `animus-queue-*`, ...).
                self.discover_project_registry(
                    project_root,
                    &mut discovered,
                    &mut warnings,
                    &mut seen,
                    &cache,
                    lockfile.as_ref(),
                    scope_ref,
                );
            }
        }

        self.discover_configured(&mut discovered, &mut warnings, &mut seen, &cache, lockfile.as_ref(), scope_ref)?;

        // DB-registry tier: the desired plugin set recorded in the Postgres
        // `plugin_registry`, resolved against binaries on the volume. Opt-in
        // and gated on the bootstrap paradox — only active once the daemon has
        // wired a source (i.e. after the bootstrap DB-backend plugin is up).
        // Runs after the explicit/project registry tiers (so a hand-pinned
        // config entry still wins) and before the unconditional global-dir
        // scan.
        if let Some(source) = self.db_registry.clone() {
            self.discover_db_registry(
                source.as_ref(),
                &mut discovered,
                &mut warnings,
                &mut seen,
                &cache,
                lockfile.as_ref(),
                scope_ref,
            );
        }

        // Scan the global plugin install dir unconditionally. This is the
        // canonical destination for `animus plugin install` and the
        // historical "user dropped a binary here by hand" location. When
        // `$ANIMUS_PLUGIN_DIR` is set, [`plugin_install_dir`] returns that
        // override; otherwise it resolves to `~/.animus/plugins/`
        // (honoring `$ANIMUS_CONFIG_DIR` for hermetic tests).
        scan_dir(
            &plugin_install_dir(),
            DiscoverySource::PluginPath,
            &mut discovered,
            &mut warnings,
            &mut seen,
            &cache,
            lockfile.as_ref(),
            scope_ref,
        );

        if let Ok(plugin_path) = std::env::var("ANIMUS_PLUGIN_PATH") {
            for raw_dir in plugin_path.split(':') {
                if !raw_dir.trim().is_empty() {
                    scan_dir(
                        Path::new(raw_dir),
                        DiscoverySource::PluginPath,
                        &mut discovered,
                        &mut warnings,
                        &mut seen,
                        &cache,
                        lockfile.as_ref(),
                        scope_ref,
                    );
                }
            }
        }

        if self.include_system_path {
            if let Some(path_var) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_var) {
                    scan_dir(
                        &dir,
                        DiscoverySource::SystemPath,
                        &mut discovered,
                        &mut warnings,
                        &mut seen,
                        &cache,
                        lockfile.as_ref(),
                        scope_ref,
                    );
                }
            }
        }

        if let Some(scope) = scope_ref {
            // A flavor manifest that exists but failed to parse leaves the
            // scope fail-closed (flavor-only with an EMPTY admit set).
            // Surface that as a DiscoveryWarning so `animus plugin list`
            // shows the real cause instead of every plugin silently
            // disappearing behind a "plugin not installed" symptom. When an
            // explicit `.animus/plugin-scope.yaml` overrides the mode, the
            // broken manifest does not gate discovery — say so instead of
            // claiming plugins were filtered out.
            if let Some((manifest_path, reason)) = scope.flavor_manifest_error.as_ref() {
                let consequence = if matches!(scope.mode, crate::scope::PluginScopeMode::FlavorOnly) {
                    "flavor-only scope admits NO plugins until it is fixed"
                } else {
                    "discovery is unaffected because the project's plugin-scope file overrides the mode"
                };
                warnings.push(DiscoveryWarning {
                    name: manifest_path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("flavor-manifest")
                        .to_string(),
                    path: manifest_path.clone(),
                    source: DiscoverySource::ProjectLocal,
                    reason: format!("flavor manifest failed to load; {consequence}: {reason}"),
                });
            }
            if !scope.admits_everything() {
                let before = discovered.len();
                discovered.retain(|plugin| scope.admits(plugin));
                let removed = before.saturating_sub(discovered.len());
                if removed > 0 && scope.effective_admit_set().is_empty() {
                    tracing::info!(
                        scope_mode = scope.mode.as_wire(),
                        removed,
                        "plugin scope filter removed every discovered plugin: the effective admit set is empty \
                         (broken or empty flavor manifest?)"
                    );
                } else if removed > 0 {
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
        cache: &ManifestCache,
        lockfile: Option<&PluginLockfile>,
        scope: Option<&PluginScope>,
    ) -> Result<()> {
        let config_path = self.config_path.clone().unwrap_or_else(default_config_path);
        if !config_path.exists() {
            return Ok(());
        }

        let config = load_plugins_config(&config_path)
            .with_context(|| format!("failed to read plugin config at {}", config_path.display()))?;
        self.discover_from_config(
            config,
            DiscoverySource::ExplicitConfig,
            discovered,
            warnings,
            seen,
            cache,
            lockfile,
            scope,
        );
        Ok(())
    }

    /// Project-registry tier: `<project_root>/.animus/plugins.yaml`,
    /// written by `animus plugin install --project`. Needed because the
    /// project dir scan picks up the `animus-*` executable namespace; this
    /// registry is still needed for custom executable names and install-time
    /// logical-name overrides, exactly like the global registry at
    /// `~/.animus/plugins.yaml`.
    /// A corrupt project registry degrades to a warning (the daemon must
    /// not lose every plugin because one project file is broken).
    fn discover_project_registry(
        &self,
        project_root: &Path,
        discovered: &mut Vec<DiscoveredPlugin>,
        warnings: &mut Vec<DiscoveryWarning>,
        seen: &mut HashSet<String>,
        cache: &ManifestCache,
        lockfile: Option<&PluginLockfile>,
        scope: Option<&PluginScope>,
    ) {
        let config_path = project_plugins_registry_path(project_root);
        if !config_path.exists() {
            return;
        }
        let config = match load_plugins_config(&config_path) {
            Ok(config) => config,
            Err(err) => {
                warnings.push(DiscoveryWarning {
                    name: config_path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("plugins.yaml")
                        .to_string(),
                    path: config_path.clone(),
                    source: DiscoverySource::ProjectLocal,
                    reason: format!("failed to read project plugin registry: {err:#}"),
                });
                return;
            }
        };
        self.discover_from_config(
            config,
            DiscoverySource::ProjectLocal,
            discovered,
            warnings,
            seen,
            cache,
            lockfile,
            scope,
        );
    }

    /// DB-registry tier: resolve the desired plugin set from a
    /// [`PluginRegistrySource`] against binaries present in the plugin install
    /// dir. Enabled rows whose binary is missing surface a
    /// [`DiscoveryWarning`] (and reserve the name) so operators see the gap
    /// instead of silently losing a plugin the DB said should be present.
    /// A read error from the source degrades to a single warning — the daemon
    /// must not lose every plugin because the registry read failed.
    fn discover_db_registry(
        &self,
        source: &dyn PluginRegistrySource,
        discovered: &mut Vec<DiscoveredPlugin>,
        warnings: &mut Vec<DiscoveryWarning>,
        seen: &mut HashSet<String>,
        cache: &ManifestCache,
        lockfile: Option<&PluginLockfile>,
        scope: Option<&PluginScope>,
    ) {
        let install_dir = plugin_install_dir();
        let entries = match source.desired_plugins() {
            Ok(entries) => entries,
            Err(err) => {
                let reason = format!("failed to read DB plugin registry: {err:#}");
                tracing::warn!("plugin DB-registry discovery skipped: {reason}");
                warnings.push(DiscoveryWarning {
                    name: "plugin_registry".to_string(),
                    path: install_dir,
                    source: DiscoverySource::DbRegistry,
                    reason,
                });
                return;
            }
        };

        let mut candidates: Vec<ProbeCandidate> = Vec::new();
        for entry in entries {
            // Disabled rows are skipped entirely (not name-reserved) so a
            // lower-precedence dir scan can still pick the binary up if it
            // matches the scanned prefixes and the operator wants it.
            if !entry.enabled {
                continue;
            }
            let name = entry.name.trim().to_string();
            if name.is_empty() || seen.contains(&name) {
                continue;
            }
            // The installer places the correct per-target (or noarch) binary at
            // `<install_dir>/<name>`, so resolution keys off the name; the
            // row's `target` is advisory and only enriches the missing-binary
            // warning.
            let path = install_dir.join(&name);
            if !path.exists() {
                seen.insert(name.clone());
                let target_note = entry.target.as_deref().map(|t| format!(" (target {t})")).unwrap_or_default();
                let reason = format!(
                    "DB registry lists plugin '{name}'{target_note} but no binary is present at {}",
                    path.display()
                );
                tracing::warn!("plugin DB-registry discovery: {reason}");
                warnings.push(DiscoveryWarning { name, path, source: DiscoverySource::DbRegistry, reason });
                continue;
            }
            seen.insert(name.clone());
            candidates.push(ProbeCandidate { name, path, source: DiscoverySource::DbRegistry });
        }

        let outcomes = resolve_manifests(&candidates, cache, lockfile, scope);
        for (cand, outcome) in candidates.into_iter().zip(outcomes) {
            let ProbeCandidate { name, path, source } = cand;
            match outcome {
                ProbeOutcome::Hit(manifest) | ProbeOutcome::Probed(Ok(manifest)) => {
                    seen.insert(name.clone());
                    discovered.push(DiscoveredPlugin { name, path, manifest, source });
                }
                ProbeOutcome::Probed(Err(error)) => {
                    seen.insert(name.clone());
                    let reason = format!("{error:#}");
                    tracing::warn!(
                        plugin = %name,
                        path = %path.display(),
                        "DB-registry plugin manifest probe failed: {reason}"
                    );
                    warnings.push(DiscoveryWarning { name, path, source, reason });
                }
                ProbeOutcome::SkippedOutOfScope(reason) => {
                    // Reserve the name so a lower-precedence source can't
                    // silently shadow this (equally out-of-scope) entry, and
                    // surface a warning so operators understand the plugin was
                    // located but deliberately NOT executed.
                    seen.insert(name.clone());
                    tracing::debug!(
                        plugin = %name,
                        path = %path.display(),
                        source = ?source,
                        "{reason}"
                    );
                    warnings.push(DiscoveryWarning { name, path, source, reason });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn discover_from_config(
        &self,
        config: PluginsConfig,
        source: DiscoverySource,
        discovered: &mut Vec<DiscoveredPlugin>,
        warnings: &mut Vec<DiscoveryWarning>,
        seen: &mut HashSet<String>,
        cache: &ManifestCache,
        lockfile: Option<&PluginLockfile>,
        scope: Option<&PluginScope>,
    ) {
        let mut candidates: Vec<ProbeCandidate> = Vec::new();
        for (logical_name, entry) in config.plugins.iter().chain(config.providers.iter()) {
            // v0.5.8+: `name_override` records the install-time `--name <NAME>`
            // override so discovery, the lockfile entry, and the daemon
            // SubjectRouter all agree on the same logical plugin name. Falls
            // back to the manifest-name field, then the table key, for
            // pre-v0.5.8 entries that never recorded the override.
            let effective_name =
                entry.name_override.clone().or_else(|| entry.name.clone()).unwrap_or_else(|| logical_name.clone());
            if seen.contains(&effective_name) {
                continue;
            }
            let Some(path) = resolve_configured_binary(&entry.binary) else {
                // Reserve the name even when the configured binary is gone so
                // a lower-precedence directory scan can't silently shadow a
                // stale explicit config entry. The warning still surfaces so
                // operators fix the entry instead of being routed to an
                // unintended copy.
                seen.insert(effective_name.clone());
                let reason = format!("configured binary not found: {}", entry.binary);
                tracing::warn!(
                    plugin = %logical_name,
                    binary = %entry.binary,
                    source = ?source,
                    "plugin manifest probe skipped: {reason}"
                );
                warnings.push(DiscoveryWarning {
                    name: effective_name.clone(),
                    path: PathBuf::from(&entry.binary),
                    source,
                    reason,
                });
                continue;
            };
            let name = effective_name;
            if entry.skip_manifest_check_at_install {
                tracing::warn!(
                    plugin = %name,
                    path = %path.display(),
                    "plugin {name} installed with --skip-manifest-check; manifest probe failures will be tolerated."
                );
            }
            // Reserve the name immediately so a duplicate row in the same
            // config (e.g. `plugins:` and `providers:` keying the same
            // effective name) doesn't enqueue a second probe candidate for
            // the same logical plugin. Codex round 1 P2.
            seen.insert(name.clone());
            candidates.push(ProbeCandidate { name, path, source });
        }

        let outcomes = resolve_manifests(&candidates, cache, lockfile, scope);
        for (cand, outcome) in candidates.into_iter().zip(outcomes) {
            let ProbeCandidate { name, path, source } = cand;
            match outcome {
                ProbeOutcome::Hit(manifest) | ProbeOutcome::Probed(Ok(manifest)) => {
                    seen.insert(name.clone());
                    discovered.push(DiscoveredPlugin { name, path, manifest, source });
                }
                ProbeOutcome::Probed(Err(error)) => {
                    // Reserve the name even on probe failure so a lower-precedence
                    // directory scan can't silently shadow a broken explicit config
                    // entry. The warning still surfaces so operators can fix the
                    // broken plugin instead of being routed to an unintended copy.
                    seen.insert(name.clone());
                    let reason = format!("{error:#}");
                    tracing::warn!(
                        plugin = %name,
                        path = %path.display(),
                        source = ?source,
                        "plugin manifest probe failed: {reason}"
                    );
                    warnings.push(DiscoveryWarning { name, path, source, reason });
                }
                ProbeOutcome::SkippedOutOfScope(reason) => {
                    // Reserve the name so a lower-precedence source can't
                    // silently shadow this (equally out-of-scope) entry, and
                    // surface a warning so operators understand the plugin was
                    // located but deliberately NOT executed.
                    seen.insert(name.clone());
                    tracing::debug!(
                        plugin = %name,
                        path = %path.display(),
                        source = ?source,
                        "{reason}"
                    );
                    warnings.push(DiscoveryWarning { name, path, source, reason });
                }
            }
        }
    }
}

/// Server-safe plugin discovery: the default entry used by the daemon and
/// every runtime resolution path.
///
/// The entire repo-shippable project-local tier is left OFF — BOTH the
/// `<project_root>/.animus/plugins/` directory scan AND the
/// `<project_root>/.animus/plugins.yaml` project registry — so a cloud
/// daemon that clones a hostile repo never executes a binary the repo
/// author controls (whether shipped as a file under `.animus/plugins/` or
/// pointed at by a `plugins.yaml` `binary:` entry) during discovery.
/// Legitimately installed plugins in `~/.animus/plugins/` (the global
/// install dir) and the `~/.animus/plugins.yaml` global registry are all
/// still discovered, so this does not stop a server from finding its real
/// (operator-installed) plugins. Local-dev flows that intentionally support
/// project-scoped installs should call
/// [`discover_plugins_including_project_local`] instead.
pub fn discover_plugins(project_root: impl Into<PathBuf>) -> Result<Vec<DiscoveredPlugin>> {
    discover_plugins_inner(project_root, false)
}

/// Like [`discover_plugins`], but also scans the project-local
/// `<project_root>/.animus/plugins/` directory (executing any
/// `animus-plugin-*` / `animus-provider-*` binary it finds via
/// `--manifest`). Use ONLY from explicit local-dev surfaces where the
/// operator trusts the working tree — never from a daemon/server that may
/// run against a cloned, untrusted repo.
pub fn discover_plugins_including_project_local(project_root: impl Into<PathBuf>) -> Result<Vec<DiscoveredPlugin>> {
    discover_plugins_inner(project_root, true)
}

fn discover_plugins_inner(
    project_root: impl Into<PathBuf>,
    probe_project_local: bool,
) -> Result<Vec<DiscoveredPlugin>> {
    let root: PathBuf = project_root.into();
    let scope = PluginScope::load_for_project(&root).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            "failed to load plugin scope; falling back to unrestricted discovery"
        );
        PluginScope::unrestricted()
    });
    PluginDiscovery::new()
        .with_project_root(root)
        .with_scope(scope)
        .probe_project_local_plugins(probe_project_local)
        .discover()
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
    // v0.7 multi-kind: match a plugin's primary `plugin_kind` OR any of its
    // additional `plugin_kinds`, so one plugin can be discovered for several
    // roles.
    Ok(plugins.into_iter().filter(|p| p.manifest.serves_kind(kind)).collect())
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
    command.env("TOKIO_WORKER_THREADS", animus_runtime_utils::cgroup_threads::tokio_worker_threads().to_string());

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

#[allow(clippy::too_many_arguments)]
fn scan_dir(
    dir: &Path,
    source: DiscoverySource,
    discovered: &mut Vec<DiscoveredPlugin>,
    warnings: &mut Vec<DiscoveryWarning>,
    seen: &mut HashSet<String>,
    cache: &ManifestCache,
    lockfile: Option<&PluginLockfile>,
    scope: Option<&PluginScope>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut candidates: Vec<ProbeCandidate> = Vec::new();
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
        // Reserve the name immediately so a duplicate file_name within the
        // same scan dir cannot enqueue a second probe candidate. Codex
        // round 1 P2.
        seen.insert(file_name.to_string());
        candidates.push(ProbeCandidate { name: file_name.to_string(), path, source });
    }

    let outcomes = resolve_manifests(&candidates, cache, lockfile, scope);
    for (cand, outcome) in candidates.into_iter().zip(outcomes) {
        let ProbeCandidate { name, path, source } = cand;
        match outcome {
            ProbeOutcome::Hit(manifest) | ProbeOutcome::Probed(Ok(manifest)) => {
                seen.insert(name.clone());
                discovered.push(DiscoveredPlugin { name, path, manifest, source });
            }
            ProbeOutcome::Probed(Err(error)) => {
                // Reserve the name even on probe failure so a lower-precedence
                // source (e.g. global install dir) can't silently shadow a
                // broken higher-precedence override (e.g. project-local).
                // The warning still surfaces so operators can fix the
                // broken plugin instead of being routed to the wrong copy.
                seen.insert(name.clone());
                let reason = format!("{error:#}");
                tracing::warn!(
                    plugin = %name,
                    path = %path.display(),
                    source = ?source,
                    "plugin manifest probe failed: {reason}"
                );
                warnings.push(DiscoveryWarning { name, path, source, reason });
            }
            ProbeOutcome::SkippedOutOfScope(reason) => {
                // The binary was located but NOT executed because the
                // active scope could not admit its filename-derived slug.
                // Reserve the name and surface a warning so the plugin is
                // not silently absent AND is never probed by a
                // lower-precedence source.
                seen.insert(name.clone());
                tracing::debug!(
                    plugin = %name,
                    path = %path.display(),
                    source = ?source,
                    "{reason}"
                );
                warnings.push(DiscoveryWarning { name, path, source, reason });
            }
        }
    }
}

/// Whether a binary file name is picked up by the directory-scan discovery
/// tiers (project-local dir, global install dir, `$ANIMUS_PLUGIN_PATH`,
/// `$PATH`). All Animus plugin kinds share the `animus-*` executable
/// namespace, including consolidated plugins such as `animus-postgres`.
/// Names outside this namespace are only discoverable via a
/// registry entry (`~/.animus/plugins.yaml` for global installs,
/// `<project>/.animus/plugins.yaml` for project-scoped ones).
pub fn is_scanned_plugin_name(name: &str) -> bool {
    name.starts_with("animus-")
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

/// Returns the project-local plugin install directory
/// (`<project_root>/.animus/plugins/`). Discovery scans this directory at a
/// HIGHER precedence than the global install dir, so a project-local install
/// shadows a global install of the same name.
pub fn project_plugin_install_dir(project_root: &Path) -> PathBuf {
    project_root.join(".animus").join("plugins")
}

/// Returns the project-local plugin registry yaml path
/// (`<project_root>/.animus/plugins.yaml`). Mirrors the global
/// [`plugins_registry_path`] shape; written by `animus plugin install
/// --project`.
pub fn project_plugins_registry_path(project_root: &Path) -> PathBuf {
    project_root.join(".animus").join("plugins.yaml")
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
    registered_skip_manifest_check_at_install_scoped(None, plugin_name)
}

/// Like [`registered_skip_manifest_check_at_install`], but consults the
/// project-local registry (`<project_root>/.animus/plugins.yaml`) FIRST so
/// the audit flag of an `animus plugin install --project
/// --skip-manifest-check` install is reported correctly. Falls back to the
/// global registry when the project registry is absent or has no matching
/// entry.
pub fn registered_skip_manifest_check_at_install_scoped(project_root: Option<&Path>, plugin_name: &str) -> bool {
    if let Some(root) = project_root {
        let project_path = root.join(".animus").join("plugins.yaml");
        if project_path.exists() {
            if let Ok(config) = load_plugins_config(&project_path) {
                if let Some(flag) = skip_manifest_flag_in_config(&config, plugin_name) {
                    return flag;
                }
            }
        }
    }
    let config_path = default_config_path();
    if !config_path.exists() {
        return false;
    }
    let Ok(config) = load_plugins_config(&config_path) else {
        return false;
    };
    skip_manifest_flag_in_config(&config, plugin_name).unwrap_or(false)
}

fn skip_manifest_flag_in_config(config: &PluginsConfig, plugin_name: &str) -> Option<bool> {
    let trimmed = plugin_name.trim();
    for (logical_name, entry) in config.plugins.iter().chain(config.providers.iter()) {
        let registered_name = entry.name.as_deref().unwrap_or(logical_name);
        let override_name = entry.name_override.as_deref();
        if registered_name == trimmed || logical_name == trimmed || override_name == Some(trimmed) {
            return Some(entry.skip_manifest_check_at_install);
        }
    }
    None
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

/// Resolve a configured `binary:` value to an existing on-disk path using the
/// same semantics discovery applies: `~/` home expansion, then absolute /
/// relative path resolution, then a `$PATH` lookup for bare command names.
/// Returns `None` when nothing resolves — the canonical "this configured
/// binary is gone" signal that drives both discovery warnings and prune.
pub fn resolve_configured_binary(value: &str) -> Option<PathBuf> {
    find_binary(&expand_home(value))
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

    #[test]
    fn directory_scan_accepts_all_animus_plugin_names() {
        assert!(is_scanned_plugin_name("animus-plugin-task"));
        assert!(is_scanned_plugin_name("animus-provider-claude"));
        assert!(is_scanned_plugin_name("animus-subject-default"));
        assert!(is_scanned_plugin_name("animus-postgres"));
        assert!(!is_scanned_plugin_name("postgres"));
        assert!(!is_scanned_plugin_name("unrelated-executable"));
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

    /// v0.7 multi-kind regression: a consolidated plugin whose PRIMARY
    /// `plugin_kind` is `subject_backend` but which ALSO declares
    /// `log_storage_backend` as a NON-primary `plugin_kinds` entry MUST be
    /// resolved by [`discover_by_kind`] for that secondary role. This is the
    /// property TASK-275 exists to guarantee: role resolution keys off
    /// `PluginManifest::serves_kind`, not the single primary field.
    ///
    /// Drives the real `discover_by_kind` entry point (not a hand-rolled
    /// `serves_kind` filter) so a regression that reverted it to
    /// `plugin_kind ==` matching would fail here.
    #[cfg(unix)]
    #[test]
    fn discover_by_kind_resolves_non_primary_declared_role() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        // Install the plugin into a scanned $ANIMUS_PLUGIN_DIR so the real
        // `discover_by_kind` -> `discover_plugins` pipeline picks it up. Clear
        // $ANIMUS_PLUGIN_PATH so a developer/CI environment pointing at extra
        // plugin dirs can't leak unrelated plugins into the `len() == 1` and
        // `provider`-is-empty assertions below (keeps the test hermetic).
        let install_dir = temp.path().join("install-dir");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        // Consolidated plugin: primary kind subject_backend, additional kinds
        // include log_storage_backend + queue (served as NON-primary roles).
        let name = "animus-plugin-consolidated-backend";
        let path = install_dir.join(name);
        let manifest = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "plugin_kind": "subject_backend",
            "plugin_kinds": ["log_storage_backend", "queue"],
            "description": "test",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        fs::write(&path, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write plugin");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");

        let project_root = temp.path().join("project");
        fs::create_dir_all(&project_root).expect("mkdir project root");

        // Resolved via the REAL helper for the NON-primary declared role.
        let as_log_storage = discover_by_kind(&project_root, "log_storage_backend").expect("discover log_storage");
        assert_eq!(
            as_log_storage.len(),
            1,
            "discover_by_kind must resolve the consolidated plugin for its non-primary log_storage_backend role"
        );
        assert_eq!(as_log_storage[0].name, name);

        // Still resolved for its PRIMARY role too.
        assert_eq!(
            discover_by_kind(&project_root, "subject_backend").expect("discover subject_backend").len(),
            1,
            "discover_by_kind must still resolve the plugin for its primary subject_backend role"
        );
        // And for its other secondary role.
        assert_eq!(
            discover_by_kind(&project_root, "queue").expect("discover queue").len(),
            1,
            "discover_by_kind must resolve the plugin for its second non-primary queue role"
        );
        // NOT resolved for a role it does not declare.
        assert!(
            discover_by_kind(&project_root, "provider").expect("discover provider").is_empty(),
            "discover_by_kind must not resolve the plugin for an undeclared role"
        );
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
            .probe_project_local_plugins(true)
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
    // `$HOME` and `$ANIMUS_DISABLE_MANIFEST_CACHE`. Cargo runs tests on
    // multiple threads in the same process, so we serialize the
    // env-touching tests behind a single CRATE-WIDE mutex to avoid races
    // with other modules (notably `manifest_cache::tests`) that also
    // mutate the same env vars. Codex round 4 P2.
    use crate::TEST_ENV_GUARD as ENV_GUARD;

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

    #[cfg(unix)]
    #[test]
    fn fetch_manifest_sets_tokio_worker_threads_after_clearing_env() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let plugin = temp.path().join("animus-plugin-worker-env");
        let expected = animus_runtime_utils::cgroup_threads::tokio_worker_threads();
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$TOKIO_WORKER_THREADS\" != \"{expected}\" ]; then\n\
               echo \"unexpected TOKIO_WORKER_THREADS=$TOKIO_WORKER_THREADS\" >&2\n\
               exit 17\n\
             fi\n\
             printf '{{\"name\":\"worker-env\",\"version\":\"0.1.0\",\"plugin_kind\":\"custom\",\"description\":\"t\",\"protocol_version\":\"1.0.0\",\"capabilities\":[]}}\\n'\n"
        );
        fs::write(&plugin, script).expect("write plugin");
        let mut perms = fs::metadata(&plugin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin, perms).expect("chmod");

        let manifest = fetch_manifest(&plugin).expect("manifest probe must receive the tokio worker bound");
        assert_eq!(manifest.name, "worker-env");
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
            .probe_project_local_plugins(true)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(discovered.len(), 1, "duplicate name across sources must dedupe to one entry, got {discovered:?}");
        assert_eq!(discovered[0].path, project_path, "project-local install must outrank the global install dir");
        assert_eq!(discovered[0].source, DiscoverySource::ProjectLocal);
    }

    /// Every global `animus plugin install` records its binary in the
    /// explicit registry yaml, so the "project shadows global" rule is only
    /// real if the project-local tier also outranks the EXPLICIT CONFIG
    /// tier. Regression guard for the v0.5.x project-scoped install work.
    #[cfg(unix)]
    #[test]
    fn discovery_project_local_takes_precedence_over_explicit_config() {
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

        let plugin_name = "animus-plugin-registered";
        let project_path = project_plugins.join(plugin_name);
        let global_path = fake_install.join(plugin_name);
        write_executable_plugin(&project_path, plugin_name);
        write_executable_plugin(&global_path, plugin_name);

        // Registry entry as written by a global `animus plugin install`.
        let config = temp.path().join("plugins.yaml");
        fs::write(&config, format!("plugins:\n  {plugin_name}:\n    binary: {}\n", global_path.display()))
            .expect("write config");

        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&config)
            .probe_project_local_plugins(true)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(discovered.len(), 1, "duplicate name across tiers must dedupe to one entry, got {discovered:?}");
        assert_eq!(
            discovered[0].path, project_path,
            "project-local install must outrank the registry-recorded global install"
        );
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
            .probe_project_local_plugins(true)
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
            .probe_project_local_plugins(true)
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

    /// SECURITY: an out-of-scope binary must be skipped BEFORE the
    /// `--manifest` probe — it is never executed. The plugin here is a
    /// `exit 2` script that, if probed, would fail with a manifest error;
    /// under a scope that excludes it we must instead see a "skipped, out
    /// of scope" warning proving the binary was never run.
    #[cfg(unix)]
    /// A TRUSTED, operator-installed plugin in the global install dir that
    /// falls outside a restricted scope is PROBED (the operator installed it,
    /// so executing it for a `--manifest` probe is safe) and then filtered
    /// out post-probe by [`PluginScope::admits`]. It must NOT be pre-probe
    /// skipped — otherwise a `--name` renamed install whose filename slug is
    /// outside the flavor admit set would vanish before its manifest name can
    /// be matched (the P2 regression). The spy binary proves it ran.
    #[cfg(unix)]
    #[test]
    fn out_of_scope_trusted_plugin_is_probed_then_filtered_post_probe() {
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

        // Spy: records every execution. The binary prints a VALID manifest so
        // its absence from the discovered set is attributable to the
        // post-probe scope filter, not a broken probe.
        let ran_marker = temp.path().join("trusted-ran");
        fs::write(&ran_marker, "0").expect("seed marker");
        let installed = install_dir.join("animus-provider-installed");
        let manifest = serde_json::json!({
            "name": "animus-provider-installed",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "operator-installed",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = ran_marker.display(),
            manifest = manifest,
        );
        fs::write(&installed, script).expect("write installed plugin");
        let mut perms = fs::metadata(&installed).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&installed, perms).expect("chmod");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // Allowlist that does NOT include the installed plugin.
        let mut scope = PluginScope { mode: PluginScopeMode::Allowlist, ..PluginScope::default() };
        scope.allow.insert("animus-provider-default".to_string());

        let (discovered, _warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_scope(scope)
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(
            fs::read_to_string(&ran_marker).unwrap().trim(),
            "1",
            "a TRUSTED installed plugin must be probed even when out of scope"
        );
        assert!(
            discovered.iter().all(|p| p.name != "animus-provider-installed"),
            "out-of-scope plugin must be filtered from the discovered set post-probe, got {discovered:?}"
        );
    }

    /// SECURITY (hostile-repo sim): a project-local
    /// `.animus/plugins/animus-provider-evil` shipped inside a cloned repo
    /// must NOT be executed during discovery when a restricted (flavor-only)
    /// scope is active — EVEN when project-local probing is opted in. The
    /// binary is a spy: it increments a counter file every time it runs, so
    /// we can prove `fetch_manifest` never spawned it.
    #[cfg(unix)]
    #[test]
    fn hostile_repo_project_local_binary_not_executed_under_flavor_only_scope() {
        use crate::scope::{PluginScope, PluginScopeMode};
        use std::collections::BTreeSet;
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        let project_root = temp.path().join("cloned-hostile-repo");
        let project_plugins = project_root.join(".animus/plugins");
        fs::create_dir_all(&project_plugins).expect("mkdir project plugins");

        // Spy: the attacker binary records every execution. If discovery
        // probes it, this counter advances past 0. It prints a perfectly
        // valid manifest so any failure to discover is attributable to the
        // scope gate, NOT a broken binary.
        let ran_marker = temp.path().join("attacker-ran");
        fs::write(&ran_marker, "0").expect("seed marker");
        let evil = project_plugins.join("animus-provider-evil");
        let manifest = serde_json::json!({
            "name": "animus-provider-evil",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "pwned",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = ran_marker.display(),
            manifest = manifest,
        );
        fs::write(&evil, script).expect("write attacker plugin");
        let mut perms = fs::metadata(&evil).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&evil, perms).expect("chmod");

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // Flavor-only scope whose admit set does NOT contain the attacker.
        let mut flavor: BTreeSet<String> = BTreeSet::new();
        flavor.insert("animus-provider-claude".to_string());
        let scope = PluginScope { mode: PluginScopeMode::FlavorOnly, flavor_plugins: flavor, ..PluginScope::default() };

        // Note: project-local probing is EXPLICITLY opted in here — the
        // scope gate alone must still prevent execution.
        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .with_scope(scope)
            .probe_project_local_plugins(true)
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(fs::read_to_string(&ran_marker).unwrap().trim(), "0", "attacker binary MUST NOT be executed");
        assert!(discovered.is_empty(), "attacker plugin must not be discovered, got {discovered:?}");
        let evil_warning = warnings
            .iter()
            .find(|w| w.name == "animus-provider-evil")
            .unwrap_or_else(|| panic!("expected a scope-skip warning for the attacker plugin, got {warnings:?}"));
        assert!(
            evil_warning.reason.contains("NOT executed"),
            "warning must state the binary was not executed, got: {}",
            evil_warning.reason
        );
    }

    /// A scoped configured registry entry whose `binary:` points at an
    /// arbitrarily-named wrapper must still be probed when its LOGICAL name
    /// is admitted — the pre-probe gate keys on the logical name for config
    /// entries, not just the binary basename. Codex round 1 P2.
    #[cfg(unix)]
    #[test]
    fn scoped_configured_entry_with_wrapper_binary_is_probed_by_logical_name() {
        use crate::scope::{PluginScope, PluginScopeMode};
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        // Binary basename (`claude-wrapper`) does NOT match the admitted
        // slug; the operator-declared logical name (`animus-provider-claude`)
        // does. The manifest declares the canonical name.
        let wrapper = temp.path().join("claude-wrapper");
        let manifest = serde_json::json!({
            "name": "animus-provider-claude",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "wrapped",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        fs::write(&wrapper, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).expect("write wrapper");
        let mut perms = fs::metadata(&wrapper).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&wrapper, perms).expect("chmod");

        let config_path = temp.path().join("plugins.yaml");
        fs::write(
            &config_path,
            format!("plugins:\n  animus-provider-claude:\n    binary: {}\n", wrapper.to_string_lossy()),
        )
        .expect("write config");

        let mut scope = PluginScope { mode: PluginScopeMode::Allowlist, ..PluginScope::default() };
        scope.allow.insert("animus-provider-claude".to_string());

        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&config_path)
            .with_scope(scope)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "admitted logical name must not be gated out, got {warnings:?}");
        assert_eq!(discovered.len(), 1, "scoped configured wrapper entry must be discovered, got {discovered:?}");
        assert_eq!(discovered[0].name, "animus-provider-claude");
    }

    /// SECURITY: a hostile PROJECT registry (`<project>/.animus/plugins.yaml`,
    /// shipped in a cloned repo) must NOT be able to get an out-of-scope
    /// binary executed — even when project-local probing is EXPLICITLY opted
    /// in AND the registry declares an admitted logical key pointing at a
    /// repo-shipped wrapper. Because the project registry is an UNTRUSTED
    /// source, the repo-controlled binary must fail the path/filename
    /// [`PluginScope::may_probe`] gate and never run (no logical-name escape
    /// for untrusted sources).
    #[cfg(unix)]
    #[test]
    fn hostile_project_registry_cannot_probe_via_logical_name_escape() {
        use crate::scope::{PluginScope, PluginScopeMode};
        use std::collections::BTreeSet;
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        let project_root = temp.path().join("cloned-hostile-repo");
        let animus_dir = project_root.join(".animus");
        let evil_dir = animus_dir.join("plugins");
        fs::create_dir_all(&evil_dir).expect("mkdir");

        // Spy: the wrapper records every execution.
        let ran_marker = temp.path().join("evil-ran");
        fs::write(&ran_marker, "0").expect("seed marker");
        let evil = evil_dir.join("evil-wrapper");
        let manifest = serde_json::json!({
            "name": "animus-provider-claude",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "trojan",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = ran_marker.display(),
            manifest = manifest,
        );
        fs::write(&evil, script).expect("write evil wrapper");
        let mut perms = fs::metadata(&evil).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&evil, perms).expect("chmod");

        // Hostile PROJECT registry: admitted logical key, evil binary path.
        fs::write(
            animus_dir.join("plugins.yaml"),
            format!("plugins:\n  animus-provider-claude:\n    binary: {}\n", evil.to_string_lossy()),
        )
        .expect("write project registry");

        // Empty global registry so only the project registry is in play.
        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // Flavor-only scope that ADMITS `animus-provider-claude`.
        let mut flavor: BTreeSet<String> = BTreeSet::new();
        flavor.insert("animus-provider-claude".to_string());
        let scope = PluginScope { mode: PluginScopeMode::FlavorOnly, flavor_plugins: flavor, ..PluginScope::default() };

        // project-local probing is EXPLICITLY opted in — the untrusted-source
        // filename gate must still refuse the repo-shipped wrapper.
        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .with_scope(scope)
            .probe_project_local_plugins(true)
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(fs::read_to_string(&ran_marker).unwrap().trim(), "0", "evil wrapper MUST NOT be executed");
        assert!(discovered.is_empty(), "hostile project-registry entry must not be discovered, got {discovered:?}");
        let evil_warn = warnings
            .iter()
            .find(|w| w.name == "animus-provider-claude")
            .unwrap_or_else(|| panic!("expected a scope-skip warning, got {warnings:?}"));
        assert!(evil_warn.reason.contains("NOT executed"), "reason: {}", evil_warn.reason);
    }

    /// SECURITY (P1 regression guard): a hostile PROJECT registry
    /// (`<project>/.animus/plugins.yaml`, shipped in a cloned repo) with a
    /// `binary:` pointing at a repo-shipped attacker binary must NOT be
    /// executed on the SERVER-SAFE discovery path — even under the default
    /// unrestricted (`all`) scope, where the pre-probe scope gate admits
    /// everything. The project registry is an UNTRUSTED source, so it is only
    /// walked when project-local probing is opted in; the server-safe default
    /// leaves it OFF, closing the "effective scope is `all`, so the registry
    /// still probes" hole. The spy binary proves it never ran.
    #[cfg(unix)]
    #[test]
    fn hostile_project_registry_binary_not_executed_on_server_safe_path_under_all_scope() {
        use crate::scope::PluginScope;
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        let project_root = temp.path().join("cloned-hostile-repo");
        let animus_dir = project_root.join(".animus");
        let evil_dir = animus_dir.join("plugins");
        fs::create_dir_all(&evil_dir).expect("mkdir");

        // Spy: the attacker binary records every execution.
        let ran_marker = temp.path().join("evil-ran");
        fs::write(&ran_marker, "0").expect("seed marker");
        let evil = evil_dir.join("evil");
        let manifest = serde_json::json!({
            "name": "animus-provider-evil",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "pwned",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = ran_marker.display(),
            manifest = manifest,
        );
        fs::write(&evil, script).expect("write attacker binary");
        let mut perms = fs::metadata(&evil).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&evil, perms).expect("chmod");

        // Hostile project registry committed into the cloned repo, pointing at
        // the repo-shipped attacker binary.
        fs::write(
            animus_dir.join("plugins.yaml"),
            format!("plugins:\n  animus-provider-evil:\n    binary: {}\n", evil.to_string_lossy()),
        )
        .expect("write project registry");

        // Server-safe posture: no project-local probing, unrestricted (`all`)
        // scope (the effective scope when the repo ships no restrictive
        // flavor). This is precisely the P1 hole.
        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_scope(PluginScope::unrestricted())
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(
            fs::read_to_string(&ran_marker).unwrap().trim(),
            "0",
            "server-safe discovery MUST NOT execute a repo-shipped project-registry binary"
        );
        assert!(discovered.is_empty(), "hostile project-registry entry must not be discovered, got {discovered:?}");
        assert!(
            warnings.iter().all(|w| w.name != "animus-provider-evil"),
            "an un-walked untrusted registry emits no warning for its entries, got {warnings:?}"
        );

        // The public server-safe helper agrees (it also defaults to `all`
        // scope for a repo with no flavor manifest).
        let via_helper = discover_plugins(&project_root).expect("discover_plugins");
        assert!(via_helper.iter().all(|p| p.name != "animus-provider-evil"), "got {via_helper:?}");
        assert_eq!(
            fs::read_to_string(&ran_marker).unwrap().trim(),
            "0",
            "still not executed via the server-safe discover_plugins helper"
        );
    }

    /// P2 regression guard: a plugin installed under `--name <NAME>` (recorded
    /// as a `name_override` in the TRUSTED global registry) whose MANIFEST
    /// name is a flavor-required slug must STILL be probed and admitted under
    /// a flavor-only scope. Its filename/override slug (`custom-task`) is NOT
    /// in the flavor admit set, so a pre-probe filename gate would skip it
    /// before the post-probe manifest-name fallback could match — trusted
    /// installed candidates must therefore never be pre-probe gated.
    #[cfg(unix)]
    #[test]
    fn renamed_trusted_install_still_probed_and_admitted_under_flavor_only_scope() {
        use crate::scope::{PluginScope, PluginScopeMode};
        use std::collections::BTreeSet;
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        // Operator-installed binary whose on-disk name matches the `--name`
        // override (`custom-task`) but whose MANIFEST declares the canonical
        // flavor-required slug `animus-subject-default`.
        let ran_marker = temp.path().join("renamed-ran");
        fs::write(&ran_marker, "0").expect("seed marker");
        let binary = temp.path().join("custom-task");
        let manifest = serde_json::json!({
            "name": "animus-subject-default",
            "version": "0.1.0",
            "plugin_kind": "subject_backend",
            "description": "installed as custom-task",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = ran_marker.display(),
            manifest = manifest,
        );
        fs::write(&binary, script).expect("write renamed plugin");
        let mut perms = fs::metadata(&binary).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary, perms).expect("chmod");

        // TRUSTED global registry entry recording the `--name` override.
        let config_path = temp.path().join("plugins.yaml");
        fs::write(
            &config_path,
            format!(
                "plugins:\n  animus-subject-default:\n    binary: {}\n    name_override: custom-task\n",
                binary.to_string_lossy()
            ),
        )
        .expect("write config");

        // Flavor-only scope requiring the canonical manifest slug.
        let mut flavor: BTreeSet<String> = BTreeSet::new();
        flavor.insert("animus-subject-default".to_string());
        let scope = PluginScope { mode: PluginScopeMode::FlavorOnly, flavor_plugins: flavor, ..PluginScope::default() };

        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&config_path)
            .with_scope(scope)
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(
            fs::read_to_string(&ran_marker).unwrap().trim(),
            "1",
            "renamed trusted install must be probed (not pre-probe skipped on its filename slug)"
        );
        assert_eq!(
            discovered.len(),
            1,
            "renamed required plugin must be discovered, got {discovered:?} / {warnings:?}"
        );
        assert_eq!(discovered[0].name, "custom-task", "discovery keeps the --name override as the logical name");
        assert_eq!(discovered[0].manifest.name, "animus-subject-default");
    }

    /// SECURITY (belt): the default (daemon/server) discovery path does NOT
    /// scan the project-local `.animus/plugins/` directory at all, so a
    /// cloned repo's binaries are never probed regardless of scope. The spy
    /// binary proves it is never executed.
    #[cfg(unix)]
    #[test]
    fn default_discovery_does_not_probe_project_local_dir() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        let project_root = temp.path().join("cloned-repo");
        let project_plugins = project_root.join(".animus/plugins");
        fs::create_dir_all(&project_plugins).expect("mkdir project plugins");

        let ran_marker = temp.path().join("ran");
        fs::write(&ran_marker, "0").expect("seed marker");
        let plugin = project_plugins.join("animus-provider-shipped");
        let manifest = serde_json::json!({
            "name": "animus-provider-shipped",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "shipped in repo",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = ran_marker.display(),
            manifest = manifest,
        );
        fs::write(&plugin, script).expect("write plugin");
        let mut perms = fs::metadata(&plugin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin, perms).expect("chmod");

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // No .probe_project_local_plugins(true) — this is the daemon/server
        // default. `discover_plugins` uses the same posture.
        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .discover_with_warnings()
            .expect("discover");

        assert_eq!(fs::read_to_string(&ran_marker).unwrap().trim(), "0", "project-local binary MUST NOT be executed");
        assert!(discovered.is_empty(), "project-local dir must not be scanned by default, got {discovered:?}");
        assert!(warnings.is_empty(), "an un-scanned dir emits no warnings, got {warnings:?}");

        // And the public server-safe helper agrees.
        let via_helper = discover_plugins(&project_root).expect("discover_plugins");
        assert!(via_helper.is_empty(), "discover_plugins must not scan project-local dir, got {via_helper:?}");
        assert_eq!(fs::read_to_string(&ran_marker).unwrap().trim(), "0", "still not executed via discover_plugins");

        // The explicit local-dev opt-in DOES scan it (and executes it once).
        let via_local = discover_plugins_including_project_local(&project_root).expect("local discover");
        assert_eq!(via_local.len(), 1, "local-dev helper must scan project-local dir, got {via_local:?}");
        assert_eq!(via_local[0].name, "animus-provider-shipped");
    }

    /// A flavor manifest that exists but fails to parse must not silently
    /// filter every plugin: discovery stays fail-closed (flavor-only with an
    /// empty admit set) but surfaces a [`DiscoveryWarning`] naming the broken
    /// manifest so `animus plugin list` shows the real cause.
    #[cfg(unix)]
    #[test]
    fn broken_flavor_manifest_surfaces_discovery_warning() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");
        let _clear_flavors_dir = EnvVarGuard::set("ANIMUS_FLAVORS_DIR", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let installed = install_dir.join("animus-provider-default");
        write_executable_plugin(&installed, "animus-provider-default");

        let project_root = temp.path().join("project");
        let flavors = project_root.join("flavors");
        fs::create_dir_all(&flavors).expect("mkdir flavors");
        let manifest_path = flavors.join("default.toml");
        fs::write(&manifest_path, "this is [not valid TOML\n").expect("write broken flavor");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // No explicit scope: the auto-load path resolves the broken flavor.
        let (discovered, warnings) = PluginDiscovery::new()
            .with_project_root(&project_root)
            .with_config_path(&empty_config)
            .discover_with_warnings()
            .expect("discover");

        assert!(discovered.is_empty(), "broken flavor must stay fail-closed, got {discovered:?}");
        let flavor_warning = warnings
            .iter()
            .find(|w| w.path == manifest_path)
            .unwrap_or_else(|| panic!("expected a warning for the broken flavor manifest, got {warnings:?}"));
        assert!(
            flavor_warning.reason.contains("flavor manifest failed to load"),
            "unexpected reason: {}",
            flavor_warning.reason
        );
    }

    /// A configured plugin whose binary is missing must still reserve its
    /// name so a lower-precedence directory scan cannot silently shadow the
    /// stale explicit config entry.
    #[cfg(unix)]
    #[test]
    fn missing_configured_binary_blocks_lower_precedence_shadow() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        // Lower-precedence copy in the global install dir that would shadow
        // the broken config entry if the name were not reserved.
        let shadow = install_dir.join("animus-plugin-stale");
        write_executable_plugin(&shadow, "animus-plugin-stale");

        let config_path = temp.path().join("plugins.yaml");
        fs::write(
            &config_path,
            "plugins:\n  animus-plugin-stale:\n    binary: /tmp/definitely-not-a-real-plugin-binary-xyz123\n",
        )
        .expect("write config");

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(&config_path).discover_with_warnings().expect("discover");

        assert!(
            discovered.is_empty(),
            "global install dir copy must not shadow the broken explicit config entry, got {discovered:?}"
        );
        assert_eq!(warnings.len(), 1, "expected exactly one warning, got {warnings:?}");
        assert_eq!(warnings[0].name, "animus-plugin-stale");
        assert!(warnings[0].reason.contains("not found"));
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

    // ---- manifest cache integration ------------------------------------

    #[cfg(unix)]
    #[test]
    fn discovery_second_run_skips_manifest_probe_when_cache_is_warm() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");
        let _clear_disable = EnvVarGuard::set("ANIMUS_DISABLE_MANIFEST_CACHE", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _cache_dir = EnvVarGuard::set("ANIMUS_CACHE_DIR", fake_home.join("cache"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &fake_install);

        let spawn_marker = fake_home.join("spawn-count");
        fs::write(&spawn_marker, "0").unwrap();
        let plugin_path = fake_install.join("animus-plugin-cached");
        let manifest = serde_json::json!({
            "name": "animus-plugin-cached",
            "version": "0.1.0",
            "plugin_kind": "custom",
            "description": "cache-test",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        // Plugin script increments the counter every time it's spawned so
        // we can prove the second discover() never spawned it again.
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = spawn_marker.display(),
            manifest = manifest,
        );
        fs::write(&plugin_path, script).expect("write plugin");
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").unwrap();

        let first =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("first discover");
        assert_eq!(first.0.len(), 1, "expected one discovered plugin on first pass");
        assert!(first.1.is_empty(), "no warnings expected, got {:?}", first.1);
        let after_first = fs::read_to_string(&spawn_marker).unwrap();
        assert_eq!(after_first.trim(), "1", "first discover must spawn the plugin once");

        let second =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("second discover");
        assert_eq!(second.0.len(), 1, "expected one discovered plugin on second pass");
        let after_second = fs::read_to_string(&spawn_marker).unwrap();
        assert_eq!(
            after_second.trim(),
            "1",
            "warm cache must NOT spawn the plugin again, counter went {after_first} -> {after_second}"
        );
        assert_eq!(second.0[0].manifest.name, "animus-plugin-cached");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_re_probes_after_binary_mtime_advances_past_cache_entry() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");
        let _clear_disable = EnvVarGuard::set("ANIMUS_DISABLE_MANIFEST_CACHE", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _cache_dir = EnvVarGuard::set("ANIMUS_CACHE_DIR", fake_home.join("cache"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &fake_install);

        let plugin_path = fake_install.join("animus-plugin-rotating");
        let mk_script = |kind: &str| {
            let manifest = serde_json::json!({
                "name": "animus-plugin-rotating",
                "version": "0.1.0",
                "plugin_kind": kind,
                "description": "rotating",
                "protocol_version": "1.0.0",
                "capabilities": []
            });
            format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)
        };
        fs::write(&plugin_path, mk_script("custom-v1")).unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").unwrap();

        let first =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("first discover");
        assert_eq!(first.0[0].manifest.plugin_kind, "custom-v1");

        // Advance mtime past the cache entry and swap script contents — but
        // because the swap also changes the sha, a fresh cache key would be
        // computed. To isolate the mtime safety net we instead keep the
        // script byte-identical to the first version *for the cache key*
        // and verify the safety net forces a re-probe when mtime advances.
        // Then a second swap to v2 proves the re-probe actually consults
        // the binary again.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&plugin_path, mk_script("custom-v2")).unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let second =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("second discover");
        assert_eq!(
            second.0[0].manifest.plugin_kind, "custom-v2",
            "discovery must observe the updated manifest after the binary was rewritten"
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_kill_switch_env_var_disables_cache_round_trip() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _cache_dir = EnvVarGuard::set("ANIMUS_CACHE_DIR", fake_home.join("cache"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &fake_install);
        let _kill_switch = EnvVarGuard::set("ANIMUS_DISABLE_MANIFEST_CACHE", "1");

        let spawn_marker = fake_home.join("spawn-count");
        fs::write(&spawn_marker, "0").unwrap();
        let plugin_path = fake_install.join("animus-plugin-killswitch");
        let manifest = serde_json::json!({
            "name": "animus-plugin-killswitch",
            "version": "0.1.0",
            "plugin_kind": "custom",
            "description": "k",
            "protocol_version": "1.0.0",
            "capabilities": []
        });
        let script = format!(
            "#!/bin/sh\nold=$(cat {marker})\necho $((old + 1)) > {marker}\nprintf '{manifest}\\n'\n",
            marker = spawn_marker.display(),
            manifest = manifest,
        );
        fs::write(&plugin_path, script).unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").unwrap();

        for _ in 0..2 {
            let _ = PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("discover");
        }
        let spawns = fs::read_to_string(&spawn_marker).unwrap();
        assert_eq!(spawns.trim(), "2", "kill switch must force every discover to spawn the plugin");
    }

    #[cfg(unix)]
    #[test]
    fn cache_hit_is_rejected_when_binary_loses_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");
        let _clear_disable = EnvVarGuard::set("ANIMUS_DISABLE_MANIFEST_CACHE", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).unwrap();
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _cache_dir = EnvVarGuard::set("ANIMUS_CACHE_DIR", fake_home.join("cache"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &fake_install);

        let plugin_path = fake_install.join("animus-plugin-chmod");
        write_executable_plugin(&plugin_path, "animus-plugin-chmod");

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").unwrap();

        let first =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("first discover");
        assert_eq!(first.0.len(), 1, "first discover must warm the cache");

        // `chmod -x` preserves the binary's bytes AND mtime. The naive
        // cache-hit path would still report the plugin as discovered.
        // Codex round 5 P2 guards against this by rejecting cache hits
        // on non-executable binaries.
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let second =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("second discover");
        assert!(
            second.0.is_empty(),
            "non-executable binary must NOT be served from cache as discovered, got {:?}",
            second.0
        );
        assert!(!second.1.is_empty(), "chmod -x must surface a discovery warning, got no warnings");
    }

    #[cfg(unix)]
    #[test]
    fn parallel_probes_return_results_in_input_order_for_many_plugins() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");
        let _clear_disable = EnvVarGuard::set("ANIMUS_DISABLE_MANIFEST_CACHE", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let fake_install = fake_home.join("plugins");
        fs::create_dir_all(&fake_install).unwrap();
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _cache_dir = EnvVarGuard::set("ANIMUS_CACHE_DIR", fake_home.join("cache"));
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &fake_install);

        let mut expected_names: Vec<String> = Vec::new();
        for idx in 0..12u32 {
            let name = format!("animus-plugin-parallel-{idx:02}");
            expected_names.push(name.clone());
            let path = fake_install.join(&name);
            let manifest = serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "plugin_kind": "custom",
                "description": format!("p{idx}"),
                "protocol_version": "1.0.0",
                "capabilities": []
            });
            fs::write(&path, format!("#!/bin/sh\nprintf '{}\\n'\n", manifest)).unwrap();
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        }

        let empty_config = temp.path().join("plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").unwrap();

        let (discovered, warnings) =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("discover");
        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        assert_eq!(discovered.len(), expected_names.len());
        let mut got_names: Vec<String> = discovered.iter().map(|p| p.name.clone()).collect();
        got_names.sort();
        let mut expected_sorted = expected_names.clone();
        expected_sorted.sort();
        assert_eq!(got_names, expected_sorted, "every input plugin must be discovered exactly once");
        for plugin in &discovered {
            assert_eq!(
                plugin.manifest.name, plugin.name,
                "manifest must match its candidate, no cross-talk between parallel probes"
            );
        }
    }

    // ---- DB-registry discovery tier (TASK-194) -------------------------

    use crate::db_registry::{DbRegistryEntry, StaticRegistrySource};
    use std::sync::Arc;

    /// A subject-backend plugin name (`animus-subject-*`) is NOT matched by the
    /// directory-scan tiers, so without the DB tier it is invisible; with a
    /// wired registry source it is discovered and tagged `DbRegistry`.
    #[cfg(unix)]
    #[test]
    fn db_registry_tier_discovers_enabled_plugin_from_volume() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let plugin_path = install_dir.join("animus-subject-default");
        write_executable_plugin(&plugin_path, "animus-subject-default");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        // Without a wired source the plugin is invisible (bootstrap paradox:
        // the DB tier is off until the daemon opts in).
        let (without_db, _) =
            PluginDiscovery::new().with_config_path(&empty_config).discover_with_warnings().expect("discover");
        assert!(
            without_db.iter().all(|p| p.name != "animus-subject-default"),
            "subject plugin must not be discovered without the DB tier, got {without_db:?}"
        );

        let source = Arc::new(StaticRegistrySource::new(vec![DbRegistryEntry::enabled("animus-subject-default")]));
        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_db_registry(source)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        let row = discovered
            .iter()
            .find(|p| p.name == "animus-subject-default")
            .expect("DB-registry tier must discover the enabled plugin");
        assert_eq!(row.source, DiscoverySource::DbRegistry);
        assert_eq!(row.path, plugin_path);
    }

    /// Disabled rows are skipped; enabled rows whose binary is absent from the
    /// volume surface a warning instead of being silently dropped.
    #[cfg(unix)]
    #[test]
    fn db_registry_tier_skips_disabled_and_warns_on_missing_binary() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let disabled = DbRegistryEntry { enabled: false, ..DbRegistryEntry::enabled("animus-subject-disabled") };
        let missing =
            DbRegistryEntry { target: Some("noarch".to_string()), ..DbRegistryEntry::enabled("animus-postgres") };
        let source = Arc::new(StaticRegistrySource::new(vec![disabled, missing]));

        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_db_registry(source)
            .discover_with_warnings()
            .expect("discover");

        assert!(discovered.is_empty(), "no binaries on the volume, nothing to discover, got {discovered:?}");
        assert_eq!(warnings.len(), 1, "only the enabled-but-missing row warns, got {warnings:?}");
        assert_eq!(warnings[0].name, "animus-postgres");
        assert_eq!(warnings[0].source, DiscoverySource::DbRegistry);
        assert!(warnings[0].reason.contains("noarch"), "warning should carry the target, got {}", warnings[0].reason);
        assert!(warnings[0].reason.contains("no binary is present"));
    }

    /// A read failure from the registry source degrades to a single warning —
    /// the other tiers (here, the global-dir scan) still resolve.
    #[cfg(unix)]
    #[test]
    fn db_registry_read_error_degrades_to_warning_without_sinking_discovery() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let fake_home = temp.path().join("animus-home");
        let install_dir = fake_home.join("plugins");
        fs::create_dir_all(&install_dir).expect("mkdir install dir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", &fake_home);
        let _plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", &install_dir);

        // A prefixed plugin that the unconditional global-dir scan still finds.
        let scanned = install_dir.join("animus-provider-scanned");
        write_executable_plugin(&scanned, "animus-provider-scanned");

        let empty_config = temp.path().join("empty-plugins.yaml");
        fs::write(&empty_config, "plugins: {}\n").expect("write empty config");

        let source = Arc::new(StaticRegistrySource::failing("connection refused"));
        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&empty_config)
            .with_db_registry(source)
            .discover_with_warnings()
            .expect("discover");

        assert!(
            discovered.iter().any(|p| p.name == "animus-provider-scanned"),
            "file/dir tiers must still resolve when the DB read fails, got {discovered:?}"
        );
        let db_warning = warnings
            .iter()
            .find(|w| w.source == DiscoverySource::DbRegistry)
            .expect("DB read error must surface a warning");
        assert!(db_warning.reason.contains("connection refused"), "unexpected reason: {}", db_warning.reason);
    }

    /// A hand-pinned explicit config entry outranks the DB tier: the DB row's
    /// name is already reserved by the higher-precedence tier, so it does not
    /// re-add or shadow it.
    #[cfg(unix)]
    #[test]
    fn explicit_config_takes_precedence_over_db_registry() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear_plugin_dir = EnvVarGuard::set("ANIMUS_PLUGIN_DIR", "");
        let _clear_plugin_path = EnvVarGuard::set("ANIMUS_PLUGIN_PATH", "");

        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path().join("animus-home"));

        let plugin = temp.path().join("configured-subject");
        write_executable_plugin(&plugin, "animus-subject-default");

        let config_path = temp.path().join("plugins.yaml");
        fs::write(
            &config_path,
            format!("plugins:\n  animus-subject-default:\n    binary: {}\n", plugin.to_string_lossy()),
        )
        .expect("write config");

        let source = Arc::new(StaticRegistrySource::new(vec![DbRegistryEntry::enabled("animus-subject-default")]));
        let (discovered, warnings) = PluginDiscovery::new()
            .with_config_path(&config_path)
            .with_db_registry(source)
            .discover_with_warnings()
            .expect("discover");

        assert!(warnings.is_empty(), "expected zero warnings, got {warnings:?}");
        let rows: Vec<&DiscoveredPlugin> = discovered.iter().filter(|p| p.name == "animus-subject-default").collect();
        assert_eq!(rows.len(), 1, "name must dedupe to a single entry across tiers, got {rows:?}");
        assert_eq!(rows[0].source, DiscoverySource::ExplicitConfig, "explicit config must outrank the DB tier");
        assert_eq!(rows[0].path, plugin);
    }
}
