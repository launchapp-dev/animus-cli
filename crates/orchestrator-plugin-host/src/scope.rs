//! Per-project plugin scope filter.
//!
//! The kernel-and-flavors architecture (see
//! `docs/architecture/kernel-and-flavors.md`) lets operators install a
//! global pool of plugins under `~/.animus/plugins/` but limit any
//! individual project to a subset. This module loads the project-local
//! `<project_root>/.animus/plugin-scope.yaml` and turns it into a
//! predicate the discovery loop applies after manifest probes complete.
//!
//! Three modes are supported (see [`PluginScopeMode`]):
//!
//! * [`PluginScopeMode::All`] preserves the v0.5.8 behavior: every
//!   discovered plugin admits. This is the default when no scope file
//!   exists AND no flavor manifest is present.
//! * [`PluginScopeMode::FlavorOnly`] admits only plugins that the active
//!   flavor declares in its `required` plugin sections. This is the
//!   default when a flavor manifest is present and no explicit scope
//!   file has been written.
//! * [`PluginScopeMode::Allowlist`] admits only the plugin names listed
//!   in `allow` plus `extras`. Used when an operator wants to lock a
//!   project to a hand-picked set.
//!
//! The filter runs AFTER the full discovery sweep so manifest-probe
//! warnings still surface for installed-but-out-of-scope plugins —
//! operators still need diagnostic info on partial failures.
//!
//! `orchestrator-plugin-host` deliberately does not depend on the
//! `orchestrator-core` flavor loader. Callers that want
//! flavor-driven scoping (the daemon, the CLI's plugin commands) pass
//! the flavor's required plugin names in via
//! [`PluginScope::load_for_project_with_flavor`].

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::discovery::DiscoveredPlugin;

/// Schema constant emitted in every `plugin-scope.yaml` file written by
/// `animus plugin scope set`.
pub const PLUGIN_SCOPE_SCHEMA_V1: &str = "animus.plugin-scope.v1";

/// Repo-local file name (under `<project_root>/.animus/`) the scope
/// loader reads.
pub const PLUGIN_SCOPE_FILE: &str = "plugin-scope.yaml";

/// Canonical default flavor id. Mirrors
/// `orchestrator_core::flavor::DEFAULT_FLAVOR_ID`; kept local so the
/// plugin-host crate does not depend on `orchestrator-core`.
pub const DEFAULT_FLAVOR_ID: &str = "default";

/// How the scope filter behaves when no explicit decision has been
/// recorded. The Cargo round-trip preserves the same wire constants
/// (`all`, `flavor-only`, `allowlist`) — operators edit the yaml file
/// directly and the CLI's `--mode` flag uses the same set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PluginScopeMode {
    /// Preserves the v0.5.8 behavior — every discovered plugin admits.
    #[default]
    All,
    /// Admits only plugins the active flavor declares in its required
    /// sections. The caller supplies the resolved flavor plugin name
    /// list; an empty list collapses to "no plugins admitted" so the
    /// daemon's preflight surfaces an actionable error instead of
    /// silently routing to an unexpected plugin.
    FlavorOnly,
    /// Admits only plugin names listed in `allow` plus `extras`.
    Allowlist,
}

impl PluginScopeMode {
    /// CLI-facing wire string (`all`, `flavor-only`, `allowlist`). Kept
    /// hand-written so the parser in the CLI layer and the yaml
    /// round-trip share one source of truth.
    pub fn as_wire(&self) -> &'static str {
        match self {
            PluginScopeMode::All => "all",
            PluginScopeMode::FlavorOnly => "flavor-only",
            PluginScopeMode::Allowlist => "allowlist",
        }
    }

    /// Inverse of [`PluginScopeMode::as_wire`]. Returns `Err` for an
    /// unknown literal so `animus plugin scope set --mode foo` surfaces
    /// a clear error instead of silently defaulting.
    pub fn parse_wire(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "flavor-only" => Ok(Self::FlavorOnly),
            "allowlist" => Ok(Self::Allowlist),
            other => {
                anyhow::bail!("unknown plugin scope mode `{}` (expected one of: all, flavor-only, allowlist)", other)
            }
        }
    }
}

/// On-disk shape of `<project_root>/.animus/plugin-scope.yaml`. Kept
/// separate from [`PluginScope`] so the public API surface is name-set
/// based (cheap to query) while the yaml schema can grow additively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PluginScopeFile {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    /// Active flavor selection, persisted by `animus plugin
    /// install-defaults --flavor <name>` / `animus flavor install <name>`.
    /// `None` (the common case) means the canonical [`DEFAULT_FLAVOR_ID`].
    /// Drives which `flavors/<name>.toml` the scope resolver reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_flavor: Option<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    require: Vec<String>,
    #[serde(default)]
    extras: Vec<String>,
}

/// Resolved per-project plugin scope. Discovery applies this as a
/// predicate against each [`DiscoveredPlugin`].
#[derive(Debug, Clone)]
pub struct PluginScope {
    pub mode: PluginScopeMode,
    /// Plugin names explicitly admitted (from the file's `allow:` block).
    pub allow: BTreeSet<String>,
    /// Role-style declarations (e.g. `subject_kind:task`,
    /// `at_least_one_provider`). Surfaced verbatim so the daemon's
    /// preflight layer can cross-reference them; the discovery filter
    /// itself does not consume them.
    pub require: BTreeSet<String>,
    /// Plugin names layered on top of `allow:` (and, in
    /// [`PluginScopeMode::FlavorOnly`], on top of the flavor-declared set).
    pub extras: BTreeSet<String>,
    /// Names resolved from the active flavor manifest. Empty when the
    /// caller did not supply a flavor or when no flavor manifest exists.
    /// Stored so [`PluginScope::admits`] is a pure function of the
    /// loaded scope without re-reading disk.
    pub flavor_plugins: BTreeSet<String>,
    /// Persisted active flavor selection from the scope file's
    /// `active_flavor:` key. `None` means the canonical
    /// [`DEFAULT_FLAVOR_ID`] is in effect. Surfaced so `animus flavor
    /// current` and `animus plugin scope show` can report which flavor
    /// the resolver is scoping against.
    pub active_flavor: Option<String>,
    /// Path the scope was loaded from, if any. `None` means the scope
    /// was synthesized from defaults (no `plugin-scope.yaml` on disk).
    pub source_path: Option<PathBuf>,
    /// Diagnostic recorded when a flavor manifest was located on disk but
    /// failed to parse: `(manifest_path, error)`. The scope stays
    /// fail-closed (`flavor-only` with an empty admit set), and discovery
    /// surfaces this as a [`crate::DiscoveryWarning`] so the operator sees
    /// why every plugin was filtered out instead of a bare "plugin not
    /// installed" symptom.
    pub flavor_manifest_error: Option<(PathBuf, String)>,
}

impl Default for PluginScope {
    fn default() -> Self {
        Self {
            mode: PluginScopeMode::All,
            allow: BTreeSet::new(),
            require: BTreeSet::new(),
            extras: BTreeSet::new(),
            flavor_plugins: BTreeSet::new(),
            active_flavor: None,
            source_path: None,
            flavor_manifest_error: None,
        }
    }
}

impl PluginScope {
    /// Convenience: the unrestricted scope used when callers explicitly
    /// want to skip filtering (e.g. `animus plugin list --no-scope` in a
    /// future revision).
    pub fn unrestricted() -> Self {
        Self::default()
    }

    /// Resolve the scope for a given project root, auto-detecting the
    /// active flavor's required plugin list by parsing
    /// `<project_root>/flavors/default.toml` when present.
    ///
    /// This is the default helper called by [`crate::discover_plugins`]
    /// so runtime call sites get flavor-aware scoping without each
    /// having to wire in the orchestrator-core flavor loader. The TOML
    /// parse is intentionally minimal — see
    /// [`load_flavor_required_slugs_from_disk`] — and any parse failure
    /// degrades to "no flavor plugins resolved" rather than erroring,
    /// matching the documented "default-deny on broken flavor" policy.
    /// The failure is loud, not silent: it is logged at `warn` and
    /// recorded on [`PluginScope::flavor_manifest_error`] so discovery
    /// can surface it as a [`crate::DiscoveryWarning`].
    pub fn load_for_project(project_root: &Path) -> Result<Self> {
        let active_flavor = read_active_flavor(project_root);
        // A persisted active flavor whose `flavors/<name>.toml` is gone is
        // STALE: resolve plugins against the `default` flavor instead of
        // fail-closing to an empty admit set (matches the daemon and CLI
        // resolvers). `active_flavor` is still surfaced verbatim so
        // `scope show` reports what the operator recorded.
        let stale_fallback = matches!(active_flavor.as_deref(),
            Some(name) if name != DEFAULT_FLAVOR_ID && locate_flavor_manifest(project_root, name).is_none());
        let flavor_name: String = if stale_fallback {
            let name = active_flavor.as_deref().unwrap_or(DEFAULT_FLAVOR_ID);
            tracing::warn!(
                flavor = %name,
                "persisted active flavor `{name}` has no manifest on disk (flavors/{name}.toml); \
                 falling back to the `default` flavor for plugin scope resolution"
            );
            DEFAULT_FLAVOR_ID.to_string()
        } else {
            active_flavor.as_deref().unwrap_or(DEFAULT_FLAVOR_ID).to_string()
        };
        let flavor_name = flavor_name.as_str();
        let (mut flavor_plugins, flavor_manifest_error) =
            match load_flavor_required_slugs_from_disk(project_root, flavor_name) {
                Ok(plugins) => (plugins, None),
                Err(err) => {
                    let manifest_path = locate_flavor_manifest(project_root, flavor_name)
                        .unwrap_or_else(|| project_root.join("flavors").join(format!("{flavor_name}.toml")));
                    tracing::warn!(
                        manifest = %manifest_path.display(),
                        error = %format!("{err:#}"),
                        "flavor manifest failed to load; flavor-only scope will admit NO plugins until it is fixed"
                    );
                    (BTreeSet::new(), Some((manifest_path, format!("{err:#}"))))
                }
            };
        // On a STALE fallback to `default` with no on-disk
        // `flavors/default.toml`, parse the binary-bundled default manifest
        // so direct `PluginDiscovery` callers get the same non-empty admit
        // set the daemon/CLI resolvers do — otherwise an explicit `mode:
        // flavor-only` scope file would filter out every plugin. Parity
        // with `orchestrator_core::flavor`'s bundled fallback, kept
        // dependency-free by embedding the same TOML.
        let mut bundled_default_used = false;
        if stale_fallback && flavor_plugins.is_empty() && locate_flavor_manifest(project_root, flavor_name).is_none() {
            flavor_plugins = bundled_default_flavor_slugs();
            bundled_default_used = !flavor_plugins.is_empty();
        }
        let flavor_present = bundled_default_used
            || !flavor_plugins.is_empty()
            || locate_flavor_manifest(project_root, flavor_name).is_some();
        let mut scope = Self::load_for_project_with_flavor(project_root, &flavor_plugins, flavor_present)?;
        scope.flavor_manifest_error = flavor_manifest_error;
        if scope.active_flavor.is_none() {
            scope.active_flavor = active_flavor;
        }
        Ok(scope)
    }

    /// Resolve the scope for a given project root.
    ///
    /// `flavor_plugins` is the set of plugin names the active flavor
    /// declares as `required` (any section). `flavor_manifest_present`
    /// is `true` when the caller has confirmed a flavor manifest exists
    /// on disk (used to decide the default mode when no scope file is
    /// written).
    ///
    /// Discovery falls back to [`Self::admits_everything`] when the
    /// project root cannot be resolved.
    pub fn load_for_project_with_flavor(
        project_root: &Path,
        flavor_plugins: &BTreeSet<String>,
        flavor_manifest_present: bool,
    ) -> Result<Self> {
        let scope_path = project_root.join(".animus").join(PLUGIN_SCOPE_FILE);
        if scope_path.exists() {
            return Self::load_from_file(&scope_path, flavor_plugins);
        }

        // No scope file: the active flavor can only come from a future
        // scope file, so honor the canonical default here. (When a scope
        // file IS present, `load_from_file` reads `active_flavor:` from it.)
        let active_flavor = read_active_flavor(project_root);
        let flavor_name = active_flavor.as_deref().unwrap_or(DEFAULT_FLAVOR_ID);

        // Two distinct presence signals:
        //   * `flavor_manifest_present` is the caller's authoritative
        //     assertion ("I parsed the flavor and I'm certain it
        //     exists"). When the caller asserts presence we honor the
        //     `flavor-only` default even with an empty plugin set —
        //     better to fail closed (admit nothing, surface a clear
        //     preflight error) than to silently flip to `mode: all` and
        //     admit every globally installed plugin behind the
        //     operator's back.
        //   * Auto-detection via `locate_default_flavor_manifest` is
        //     only consulted when the caller did NOT supply a flavor
        //     resolver AND the plugin set is non-empty. In that case
        //     the lightweight TOML parser already resolved the
        //     plugins, so we can safely activate `flavor-only`.
        // FlavorOnly when EITHER the caller asserts a flavor is present
        // (fail-closed even with an empty plugin set), OR auto-detection
        // finds a manifest and the lightweight TOML parser already
        // resolved a non-empty plugin set. Otherwise `mode: all`.
        let mode = if flavor_manifest_present
            || (!flavor_plugins.is_empty() && locate_flavor_manifest(project_root, flavor_name).is_some())
        {
            PluginScopeMode::FlavorOnly
        } else {
            PluginScopeMode::All
        };

        Ok(Self {
            mode,
            allow: BTreeSet::new(),
            require: BTreeSet::new(),
            extras: BTreeSet::new(),
            flavor_plugins: flavor_plugins.clone(),
            active_flavor,
            source_path: None,
            flavor_manifest_error: None,
        })
    }

    /// Parse a `.animus/plugin-scope.yaml` file from disk. Public so the
    /// CLI's `animus plugin scope show` command can round-trip the same
    /// loader the daemon uses.
    pub fn load_from_file(path: &Path, flavor_plugins: &BTreeSet<String>) -> Result<Self> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read plugin scope file at {}", path.display()))?;
        let parsed: PluginScopeFile = serde_yaml::from_str(&body)
            .with_context(|| format!("failed to parse plugin scope file at {}", path.display()))?;

        if let Some(schema) = parsed.schema.as_deref() {
            if schema != PLUGIN_SCOPE_SCHEMA_V1 {
                anyhow::bail!(
                    "unknown plugin scope schema `{}` at {} (expected `{}`)",
                    schema,
                    path.display(),
                    PLUGIN_SCOPE_SCHEMA_V1
                );
            }
        }

        let mode = match parsed.mode.as_deref() {
            Some(raw) => PluginScopeMode::parse_wire(raw)
                .with_context(|| format!("invalid `mode` value in {}", path.display()))?,
            None => PluginScopeMode::All,
        };

        Ok(Self {
            mode,
            allow: parsed.allow.into_iter().collect(),
            require: parsed.require.into_iter().collect(),
            extras: parsed.extras.into_iter().collect(),
            flavor_plugins: flavor_plugins.clone(),
            active_flavor: parsed.active_flavor.filter(|s| !s.is_empty()),
            source_path: Some(path.to_path_buf()),
            flavor_manifest_error: None,
        })
    }

    /// Serialize the scope back to disk as canonical yaml. The CLI's
    /// `animus plugin scope set` calls this after merging user-supplied
    /// flags. Always writes the v1 schema constant so future loaders can
    /// validate.
    pub fn write_to_file(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create scope file parent dir {}", parent.display()))?;
        }
        let file = PluginScopeFile {
            schema: Some(PLUGIN_SCOPE_SCHEMA_V1.to_string()),
            mode: Some(self.mode.as_wire().to_string()),
            // Persist only a non-default selection; `None`/`default`
            // serializes to nothing so the common case keeps a clean file.
            active_flavor: self.active_flavor.clone().filter(|s| !s.is_empty() && s != DEFAULT_FLAVOR_ID),
            allow: self.allow.iter().cloned().collect(),
            require: self.require.iter().cloned().collect(),
            extras: self.extras.iter().cloned().collect(),
        };
        let body = serde_yaml::to_string(&file).context("failed to serialize plugin scope file")?;
        fs::write(path, body).with_context(|| format!("failed to write plugin scope file at {}", path.display()))?;
        Ok(())
    }

    /// `true` when this scope does not restrict any discovered plugin.
    /// Cheap fast-path the discovery loop checks before walking the
    /// admit-set predicate.
    pub fn admits_everything(&self) -> bool {
        matches!(self.mode, PluginScopeMode::All)
    }

    /// Returns the effective admit-set for the current mode, resolved
    /// against the flavor-declared plugin list. Used by `animus plugin
    /// scope show` to render the same set the discovery filter applies.
    pub fn effective_admit_set(&self) -> BTreeSet<String> {
        match self.mode {
            PluginScopeMode::All => BTreeSet::new(),
            PluginScopeMode::FlavorOnly => {
                let mut out: BTreeSet<String> = self.flavor_plugins.clone();
                out.extend(self.extras.iter().cloned());
                out
            }
            PluginScopeMode::Allowlist => {
                let mut out: BTreeSet<String> = self.allow.clone();
                out.extend(self.extras.iter().cloned());
                out
            }
        }
    }

    /// `true` when the supplied plugin admits under the current scope.
    /// The discovery loop calls this for every probed plugin AFTER the
    /// manifest probe completes; failures still surface as warnings.
    ///
    /// Honors the v0.5.7 `--name <NAME>` install override: a plugin
    /// recorded under a name_override (e.g. an `animus-subject-default`
    /// installed as `custom-task`) admits when EITHER the override name
    /// OR the manifest-declared name appears in the effective admit set.
    /// Without this dual check, flavor-only scoping would mark required
    /// flavor plugins as missing whenever they were installed under a
    /// non-default name.
    pub fn admits(&self, plugin: &DiscoveredPlugin) -> bool {
        if self.admits_everything() {
            return true;
        }
        if self.admits_by_name(&plugin.name) {
            return true;
        }
        // Fall back to the manifest-declared name so `name_override`
        // installs still match the flavor's plugin slug set. ONLY
        // applies in `flavor-only` mode where the admit set comes from
        // OWNER/REPO slugs (which match the manifest name). For
        // `allowlist` mode the operator is explicit about which logical
        // names admit, so a renamed install with a non-matching
        // override must NOT be silently admitted via its manifest name.
        if matches!(self.mode, PluginScopeMode::FlavorOnly) && plugin.name != plugin.manifest.name {
            return self.admits_by_name(&plugin.manifest.name);
        }
        false
    }

    /// Name-only variant of [`Self::admits`]. Exposed so callers that
    /// only have the binary file name (e.g. when deciding whether to
    /// probe at all) can apply the same predicate without constructing
    /// a synthetic [`DiscoveredPlugin`].
    pub fn admits_by_name(&self, name: &str) -> bool {
        if self.admits_everything() {
            return true;
        }
        let admit = self.effective_admit_set();
        admit.iter().any(|candidate| candidate == name)
    }

    /// Security gate applied BEFORE the `--manifest` probe executes a
    /// candidate binary. Returns `true` when this scope could admit the
    /// plugin, so it is safe to spend a probe (which EXECUTES the binary)
    /// on it.
    ///
    /// The slug is derived from the binary's file name WITHOUT executing
    /// it — plugin binaries are named `animus-<kind>-<...>`, and both the
    /// directory-scan candidate name and (for the common canonical-binary
    /// case) the flavor admit-set slug agree with that file name. This
    /// mirrors [`Self::admits_by_name`] but sources the identity from the
    /// path instead of a probed manifest, so a cloned hostile repo that
    /// ships `.animus/plugins/animus-provider-evil` is NOT executed during
    /// discovery under a restricted (flavor-only / allowlist) scope.
    ///
    /// An unrestricted ([`PluginScopeMode::All`]) scope always returns
    /// `true` — the local-dev default is unchanged and this gate only
    /// bites server / flavor-scoped contexts.
    ///
    /// Note: because the manifest is not read here, a plugin installed
    /// under `--name <NAME>` whose ON-DISK binary is ALSO renamed away
    /// from its canonical `animus-*` slug cannot be recognized pre-probe
    /// and will be skipped under a restricted scope. The common
    /// `--name` case keeps the canonical binary file name, so its slug
    /// still matches the flavor admit set. The post-probe
    /// [`Self::admits`] retain still applies for the manifest-name
    /// fallback on candidates that clear this gate.
    pub fn may_probe(&self, path: &Path) -> bool {
        if self.admits_everything() {
            return true;
        }
        match path.file_name().and_then(|value| value.to_str()) {
            Some(slug) => self.admits_by_name(slug),
            None => false,
        }
    }
}

/// Minimal flavor-manifest reader used by [`PluginScope::load_for_project`]
/// to auto-detect the required plugin set without depending on
/// `orchestrator-core`. Parses the on-disk `flavors/default.toml` (NOT
/// the binary-bundled fallback) and pulls the `required` arrays from
/// every section the v0.5 flavor schema declares — see
/// `crates/orchestrator-core/src/flavor.rs` for the schema source of
/// truth.
///
/// Returns `Ok(None)` when no `flavors/default.toml` is found on disk;
/// returns `Ok(Some(...))` with the normalized (OWNER stripped) plugin
/// binary names on success. Parse errors are propagated so callers can
/// log them — the public [`PluginScope::load_for_project`] swallows the
/// error and falls back to "no flavor plugins resolved" to keep
/// discovery alive when an operator hand-edits an invalid manifest.
/// Read the persisted `active_flavor:` selection from
/// `<project_root>/.animus/plugin-scope.yaml`, if present. Returns `None`
/// when the file is absent, unparseable, or has no `active_flavor` key —
/// callers fall back to [`DEFAULT_FLAVOR_ID`]. A blank value is treated as
/// unset. Kept dependency-free so the plugin-host crate resolves the
/// active flavor without importing `orchestrator-core`.
pub fn read_active_flavor(project_root: &Path) -> Option<String> {
    let path = project_root.join(".animus").join(PLUGIN_SCOPE_FILE);
    let body = fs::read_to_string(&path).ok()?;
    let parsed: PluginScopeFile = serde_yaml::from_str(&body).ok()?;
    parsed.active_flavor.filter(|s| !s.trim().is_empty())
}

/// Binary-bundled copy of the canonical `flavors/default.toml`, mirroring
/// `orchestrator_core::flavor`'s bundled fallback so the plugin-host scope
/// loader can resolve the default flavor's plugin set even when no on-disk
/// manifest exists. Embedding the same file keeps the plugin-host crate
/// dependency-free of `orchestrator-core`.
const BUNDLED_DEFAULT_FLAVOR: &str = include_str!("../../../flavors/default.toml");

/// Parse the bundled default flavor manifest into its normalized plugin
/// slug set (required + recommended, OWNER stripped). Returns an empty set
/// if the embedded TOML ever fails to parse — callers treat that as "no
/// bundled plugins resolved" rather than erroring.
fn bundled_default_flavor_slugs() -> BTreeSet<String> {
    let parsed: FlavorManifestStub = match toml::from_str(BUNDLED_DEFAULT_FLAVOR) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(error = %err, "bundled default flavor manifest failed to parse");
            return BTreeSet::new();
        }
    };
    flavor_stub_slugs(&parsed)
}

/// Collect the normalized (OWNER stripped) plugin slug set from a parsed
/// flavor stub: both `required` and `recommended` across every role
/// section. Shared by the disk reader and the bundled-default fallback.
fn flavor_stub_slugs(parsed: &FlavorManifestStub) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for section in [
        &parsed.workflow_runner,
        &parsed.queue,
        &parsed.providers,
        &parsed.subjects,
        &parsed.transports,
        &parsed.ui,
        &parsed.triggers,
        &parsed.durable_store,
        &parsed.memory_store,
    ] {
        for slug in section.required.iter().chain(section.recommended.iter()) {
            out.insert(normalize_flavor_slug_str(slug));
        }
    }
    out
}

fn load_flavor_required_slugs_from_disk(project_root: &Path, flavor_name: &str) -> Result<BTreeSet<String>> {
    let path = match locate_flavor_manifest(project_root, flavor_name) {
        Some(p) => p,
        None => return Ok(BTreeSet::new()),
    };
    let body =
        fs::read_to_string(&path).with_context(|| format!("failed to read flavor manifest at {}", path.display()))?;
    let parsed: FlavorManifestStub =
        toml::from_str(&body).with_context(|| format!("failed to parse flavor manifest at {}", path.display()))?;

    // Include both `required` and `recommended` — the default v0.5 flavor
    // lists some role-covering backends under `recommended` (e.g.
    // `animus-subject-requirements` for the `subject_kind:requirement`
    // role) and a strict `required`-only admit set would silently filter
    // out an installed plugin that preflight legitimately needs.
    Ok(flavor_stub_slugs(&parsed))
}

/// Mirror of `orchestrator_core::flavor::locate_flavor_manifest_in` so
/// the plugin-host crate can resolve the same flavor location order
/// without depending on `orchestrator-core`. Keep these in sync — the
/// flavor schema source-of-truth is `crates/orchestrator-core/src/flavor.rs`.
///
/// Order:
/// 1. `$ANIMUS_FLAVORS_DIR/<name>.toml` when the env var is set.
/// 2. `<project_root>/flavors/<name>.toml`.
/// 3. Walk up ancestors looking for a sibling `flavors/<name>.toml`.
fn locate_flavor_manifest(project_root: &Path, flavor_name: &str) -> Option<PathBuf> {
    let rel = PathBuf::from("flavors").join(format!("{flavor_name}.toml"));
    if let Ok(dir) = std::env::var("ANIMUS_FLAVORS_DIR") {
        let path = Path::new(&dir).join(format!("{flavor_name}.toml"));
        if path.is_file() {
            return Some(path);
        }
    }

    let direct = project_root.join(&rel);
    if direct.is_file() {
        return Some(direct);
    }

    let mut walker: &Path = project_root;
    while let Some(parent) = walker.parent() {
        let candidate = parent.join(&rel);
        if candidate.is_file() {
            return Some(candidate);
        }
        walker = parent;
    }

    None
}

fn normalize_flavor_slug_str(slug: &str) -> String {
    if let Some((_owner, rest)) = slug.split_once('/') {
        rest.to_string()
    } else {
        slug.to_string()
    }
}

#[derive(Debug, Default, Deserialize)]
struct FlavorManifestStub {
    #[serde(default)]
    workflow_runner: FlavorSectionStub,
    #[serde(default)]
    queue: FlavorSectionStub,
    #[serde(default)]
    providers: FlavorSectionStub,
    #[serde(default)]
    subjects: FlavorSectionStub,
    #[serde(default)]
    transports: FlavorSectionStub,
    #[serde(default)]
    ui: FlavorSectionStub,
    #[serde(default)]
    triggers: FlavorSectionStub,
    #[serde(default)]
    durable_store: FlavorSectionStub,
    #[serde(default)]
    memory_store: FlavorSectionStub,
}

#[derive(Debug, Default, Deserialize)]
struct FlavorSectionStub {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    recommended: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_plugin_protocol::PluginManifest;
    use std::path::PathBuf;

    fn manifest(name: &str, kind: &str) -> PluginManifest {
        let raw = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "plugin_kind": kind,
            "description": "test",
            "protocol_version": "1.0.0",
            "capabilities": [],
        });
        serde_json::from_value(raw).expect("manifest")
    }

    fn plugin(name: &str) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.to_string(),
            path: PathBuf::from(format!("/tmp/{name}")),
            manifest: manifest(name, "subject_backend"),
            source: crate::discovery::DiscoverySource::PluginPath,
        }
    }

    #[test]
    fn mode_default_is_all() {
        let scope = PluginScope::default();
        assert!(scope.admits_everything());
        assert!(scope.admits(&plugin("animus-subject-default")));
        assert!(scope.admits(&plugin("animus-subject-linear")));
    }

    #[test]
    fn allowlist_mode_admits_only_named_plus_extras() {
        let mut scope = PluginScope { mode: PluginScopeMode::Allowlist, ..PluginScope::default() };
        scope.allow.insert("animus-subject-default".to_string());
        scope.extras.insert("animus-provider-claude".to_string());
        assert!(!scope.admits_everything());
        assert!(scope.admits(&plugin("animus-subject-default")));
        assert!(scope.admits(&plugin("animus-provider-claude")));
        assert!(!scope.admits(&plugin("animus-subject-linear")));
    }

    #[test]
    fn flavor_only_mode_admits_flavor_plugins_plus_extras() {
        let mut flavor: BTreeSet<String> = BTreeSet::new();
        flavor.insert("animus-subject-default".to_string());
        flavor.insert("animus-provider-claude".to_string());
        let mut scope =
            PluginScope { mode: PluginScopeMode::FlavorOnly, flavor_plugins: flavor, ..PluginScope::default() };
        scope.extras.insert("animus-subject-linear".to_string());

        assert!(scope.admits(&plugin("animus-subject-default")));
        assert!(scope.admits(&plugin("animus-provider-claude")));
        assert!(scope.admits(&plugin("animus-subject-linear")));
        assert!(!scope.admits(&plugin("animus-subject-sqlite")));
    }

    #[test]
    fn load_for_project_defaults_to_all_when_no_flavor_or_scope_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scope = PluginScope::load_for_project(temp.path()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::All);
        assert!(scope.source_path.is_none());
    }

    #[test]
    fn load_for_project_defaults_to_flavor_only_when_flavor_manifest_present() {
        let temp = tempfile::tempdir().expect("tempdir");
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(flavors.join("default.toml"), "schema = \"animus.flavor.v1\"\n").expect("write flavor stub");

        let flavor_plugins: BTreeSet<String> =
            ["animus-subject-default".to_string(), "animus-provider-claude".to_string()].into_iter().collect();

        let scope = PluginScope::load_for_project_with_flavor(temp.path(), &flavor_plugins, true).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly);
        assert!(scope.admits(&plugin("animus-subject-default")));
        assert!(!scope.admits(&plugin("animus-subject-linear")));
    }

    #[test]
    fn load_from_file_round_trips_allowlist_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin-scope.yaml");
        let body = r#"schema: animus.plugin-scope.v1
mode: allowlist
allow:
  - animus-subject-default
  - animus-provider-claude
require:
  - subject_kind:task
extras:
  - animus-subject-linear
"#;
        std::fs::write(&path, body).expect("write");
        let scope = PluginScope::load_from_file(&path, &BTreeSet::new()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::Allowlist);
        assert!(scope.allow.contains("animus-subject-default"));
        assert!(scope.require.contains("subject_kind:task"));
        assert!(scope.extras.contains("animus-subject-linear"));
        assert!(scope.admits(&plugin("animus-subject-default")));
        assert!(scope.admits(&plugin("animus-subject-linear")));
        assert!(!scope.admits(&plugin("animus-subject-sqlite")));
    }

    #[test]
    fn load_from_file_rejects_unknown_schema() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin-scope.yaml");
        std::fs::write(&path, "schema: animus.plugin-scope.v999\nmode: all\n").expect("write");
        let err = PluginScope::load_from_file(&path, &BTreeSet::new()).expect_err("schema should be rejected");
        assert!(format!("{err:#}").contains("unknown plugin scope schema"));
    }

    #[test]
    fn load_from_file_rejects_unknown_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin-scope.yaml");
        std::fs::write(&path, "schema: animus.plugin-scope.v1\nmode: superset\n").expect("write");
        let err = PluginScope::load_from_file(&path, &BTreeSet::new()).expect_err("mode should be rejected");
        assert!(format!("{err:#}").contains("unknown plugin scope mode"));
    }

    #[test]
    fn write_to_file_round_trips_through_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin-scope.yaml");
        let mut scope = PluginScope { mode: PluginScopeMode::Allowlist, ..PluginScope::default() };
        scope.allow.insert("animus-subject-default".to_string());
        scope.extras.insert("animus-provider-claude".to_string());
        scope.require.insert("subject_kind:task".to_string());
        scope.write_to_file(&path).expect("write");

        let loaded = PluginScope::load_from_file(&path, &BTreeSet::new()).expect("reload");
        assert_eq!(loaded.mode, PluginScopeMode::Allowlist);
        assert_eq!(loaded.allow, scope.allow);
        assert_eq!(loaded.extras, scope.extras);
        assert_eq!(loaded.require, scope.require);
    }

    #[test]
    fn admits_honors_name_override_via_manifest_name_fallback() {
        let mut flavor: BTreeSet<String> = BTreeSet::new();
        flavor.insert("animus-subject-default".to_string());
        let scope = PluginScope { mode: PluginScopeMode::FlavorOnly, flavor_plugins: flavor, ..PluginScope::default() };

        // Plugin was installed with `--name custom-task`, so discovery
        // records `name = "custom-task"` even though the manifest still
        // declares the canonical `animus-subject-default`. The admit
        // predicate must match on either.
        let renamed = DiscoveredPlugin {
            name: "custom-task".to_string(),
            path: PathBuf::from("/tmp/custom-task"),
            manifest: manifest("animus-subject-default", "subject_backend"),
            source: crate::discovery::DiscoverySource::PluginPath,
        };
        assert!(scope.admits(&renamed), "renamed plugin matching flavor slug via manifest name must admit");
    }

    #[test]
    fn admits_honors_name_override_via_extras_fallback() {
        let mut scope = PluginScope { mode: PluginScopeMode::FlavorOnly, ..PluginScope::default() };
        scope.extras.insert("animus-subject-default".to_string());

        // The manifest-name fallback must consult the same union as
        // `effective_admit_set()` (flavor plugins PLUS extras), so a
        // renamed install whose manifest name only appears in `extras:`
        // still admits.
        let renamed = DiscoveredPlugin {
            name: "custom-task".to_string(),
            path: PathBuf::from("/tmp/custom-task"),
            manifest: manifest("animus-subject-default", "subject_backend"),
            source: crate::discovery::DiscoverySource::PluginPath,
        };
        assert!(scope.admits(&renamed), "renamed plugin matching an extras slug via manifest name must admit");
    }

    #[test]
    fn load_for_project_records_error_and_fails_closed_on_broken_flavor_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(flavors.join("default.toml"), "this is [not valid TOML\n").expect("write broken flavor");

        let scope = PluginScope::load_for_project(temp.path()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly, "broken flavor must stay fail-closed");
        assert!(scope.effective_admit_set().is_empty());
        assert!(!scope.admits(&plugin("animus-subject-default")));
        let (path, reason) = scope.flavor_manifest_error.as_ref().expect("parse failure must be recorded");
        assert_eq!(path, &flavors.join("default.toml"));
        assert!(reason.contains("failed to parse flavor manifest"), "unexpected reason: {reason}");
    }

    #[test]
    fn load_for_project_auto_resolves_flavor_plugin_slugs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(
            flavors.join("default.toml"),
            r#"schema = "animus.flavor.v1"
id = "default"
version = "0.5.0"
title = "Test"
description = "Test"

[workflow_runner]
required = ["launchapp-dev/animus-workflow-runner-default"]

[providers]
required = ["launchapp-dev/animus-provider-claude"]
"#,
        )
        .expect("write flavor");

        let scope = PluginScope::load_for_project(temp.path()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly);
        assert!(scope.flavor_plugins.contains("animus-provider-claude"));
        assert!(scope.flavor_plugins.contains("animus-workflow-runner-default"));
        assert!(scope.admits(&plugin("animus-provider-claude")));
        assert!(!scope.admits(&plugin("animus-subject-linear")));
    }

    #[test]
    fn read_active_flavor_returns_none_when_no_scope_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(read_active_flavor(temp.path()).is_none());
    }

    #[test]
    fn read_active_flavor_reads_persisted_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        std::fs::write(
            animus.join(PLUGIN_SCOPE_FILE),
            "schema: animus.plugin-scope.v1\nmode: all\nactive_flavor: enterprise\n",
        )
        .expect("write scope");
        assert_eq!(read_active_flavor(temp.path()).as_deref(), Some("enterprise"));
    }

    #[test]
    fn write_then_read_round_trips_active_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(".animus").join(PLUGIN_SCOPE_FILE);
        let mut scope = PluginScope::default();
        scope.active_flavor = Some("enterprise".to_string());
        scope.write_to_file(&path).expect("write");

        let loaded = PluginScope::load_from_file(&path, &BTreeSet::new()).expect("reload");
        assert_eq!(loaded.active_flavor.as_deref(), Some("enterprise"));
        // And the dependency-free reader agrees.
        assert_eq!(read_active_flavor(temp.path()).as_deref(), Some("enterprise"));
    }

    #[test]
    fn write_omits_default_active_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(".animus").join(PLUGIN_SCOPE_FILE);
        let mut scope = PluginScope::default();
        scope.active_flavor = Some(DEFAULT_FLAVOR_ID.to_string());
        scope.write_to_file(&path).expect("write");
        let body = std::fs::read_to_string(&path).expect("read");
        assert!(!body.contains("active_flavor"), "default selection must not be persisted: {body}");
    }

    #[test]
    fn load_for_project_resolves_against_persisted_active_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        // Two flavor manifests on disk: default (empty) and enterprise.
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(
            flavors.join("default.toml"),
            "schema = \"animus.flavor.v1\"\nid = \"default\"\nversion = \"0.5.0\"\ntitle = \"d\"\ndescription = \"d\"\n",
        )
        .expect("write default");
        std::fs::write(
            flavors.join("enterprise.toml"),
            r#"schema = "animus.flavor.v1"
id = "enterprise"
version = "0.5.0"
title = "ent"
description = "ent"

[providers]
required = ["acme/animus-provider-enterprise"]
"#,
        )
        .expect("write enterprise");
        // Persist the enterprise selection.
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        std::fs::write(
            animus.join(PLUGIN_SCOPE_FILE),
            "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: enterprise\n",
        )
        .expect("write scope");

        let scope = PluginScope::load_for_project(temp.path()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly);
        assert!(
            scope.admits(&plugin("animus-provider-enterprise")),
            "enterprise flavor's plugin must admit when it is the active flavor"
        );
        assert!(scope.active_flavor.as_deref() == Some("enterprise"));
    }

    #[test]
    fn load_for_project_stale_active_flavor_uses_bundled_default_not_empty() {
        // Persisted non-default flavor with NO on-disk manifest, an
        // explicit `mode: flavor-only` scope file, and NO flavors/ dir
        // (so even `default` resolves only via the binary-bundled copy).
        // Without the bundled-default fallback the admit set would be
        // empty and every plugin would be filtered out.
        let temp = tempfile::tempdir().expect("tempdir");
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        std::fs::write(
            animus.join(PLUGIN_SCOPE_FILE),
            "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: ghost\n",
        )
        .expect("write scope");

        let scope = PluginScope::load_for_project(temp.path()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly);
        assert!(
            !scope.effective_admit_set().is_empty(),
            "stale active flavor must resolve the bundled default flavor's plugins, not an empty admit set"
        );
        // active_flavor is still surfaced verbatim for diagnostics.
        assert_eq!(scope.active_flavor.as_deref(), Some("ghost"));
    }

    #[test]
    fn bundled_default_flavor_slugs_is_non_empty() {
        assert!(!bundled_default_flavor_slugs().is_empty(), "the embedded default flavor must resolve some plugins");
    }

    #[test]
    fn may_probe_gates_execution_on_filename_slug() {
        // Unrestricted: everything may be probed (local-dev default).
        let all = PluginScope::unrestricted();
        assert!(all.may_probe(&PathBuf::from("/some/repo/.animus/plugins/animus-provider-evil")));

        // Flavor-only: only admitted slugs may be probed. A hostile
        // binary shipped in a cloned repo is NOT executed.
        let mut flavor: BTreeSet<String> = BTreeSet::new();
        flavor.insert("animus-subject-default".to_string());
        flavor.insert("animus-provider-claude".to_string());
        let scope = PluginScope { mode: PluginScopeMode::FlavorOnly, flavor_plugins: flavor, ..PluginScope::default() };

        assert!(
            scope.may_probe(&PathBuf::from("/repo/.animus/plugins/animus-subject-default")),
            "in-flavor slug must be probeable"
        );
        assert!(
            !scope.may_probe(&PathBuf::from("/repo/.animus/plugins/animus-provider-evil")),
            "out-of-flavor (hostile) slug must NOT be probed"
        );
        // No file name → conservatively refuse under a restricted scope.
        assert!(!scope.may_probe(&PathBuf::from("/")));
    }

    #[test]
    fn may_probe_matches_admits_for_real_plugin_names() {
        // The pre-probe filename gate must agree with the post-probe
        // `admits` predicate for canonically-named plugin binaries, so
        // legitimate plugins are never spuriously skipped.
        let mut allow: BTreeSet<String> = BTreeSet::new();
        allow.insert("animus-subject-default".to_string());
        allow.insert("animus-provider-claude".to_string());
        let scope = PluginScope { mode: PluginScopeMode::Allowlist, allow, ..PluginScope::default() };

        for name in
            ["animus-subject-default", "animus-provider-claude", "animus-subject-linear", "animus-queue-default"]
        {
            let path = PathBuf::from(format!("/repo/.animus/plugins/{name}"));
            assert_eq!(
                scope.may_probe(&path),
                scope.admits(&plugin(name)),
                "may_probe and admits must agree for canonically-named binary `{name}`",
            );
        }
    }

    #[test]
    fn mode_wire_round_trip() {
        for mode in [PluginScopeMode::All, PluginScopeMode::FlavorOnly, PluginScopeMode::Allowlist] {
            let parsed = PluginScopeMode::parse_wire(mode.as_wire()).expect("parse");
            assert_eq!(parsed, mode);
        }
        assert!(PluginScopeMode::parse_wire("nope").is_err());
    }
}
