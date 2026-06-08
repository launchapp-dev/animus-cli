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

/// Conventional path the loader probes for a flavor manifest. Matches the
/// rule in `crates/orchestrator-core/src/flavor.rs`. Only used to decide
/// the default mode when the project has neither a scope file nor a
/// flavor name supplied by the caller.
const FLAVOR_DEFAULT_MANIFEST_REL: &str = "flavors/default.toml";

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
    /// Path the scope was loaded from, if any. `None` means the scope
    /// was synthesized from defaults (no `plugin-scope.yaml` on disk).
    pub source_path: Option<PathBuf>,
}

impl Default for PluginScope {
    fn default() -> Self {
        Self {
            mode: PluginScopeMode::All,
            allow: BTreeSet::new(),
            require: BTreeSet::new(),
            extras: BTreeSet::new(),
            flavor_plugins: BTreeSet::new(),
            source_path: None,
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
    pub fn load_for_project(project_root: &Path) -> Result<Self> {
        let flavor_plugins = load_flavor_required_slugs_from_disk(project_root).unwrap_or_default();
        let flavor_present = !flavor_plugins.is_empty() || locate_default_flavor_manifest(project_root).is_some();
        Self::load_for_project_with_flavor(project_root, &flavor_plugins, flavor_present)
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
            || (!flavor_plugins.is_empty() && locate_default_flavor_manifest(project_root).is_some())
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
            source_path: None,
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
            source_path: Some(path.to_path_buf()),
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
            return self.flavor_plugins.iter().any(|s| s == &plugin.manifest.name);
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
fn load_flavor_required_slugs_from_disk(project_root: &Path) -> Result<BTreeSet<String>> {
    let path = match locate_default_flavor_manifest(project_root) {
        Some(p) => p,
        None => return Ok(BTreeSet::new()),
    };
    let body =
        fs::read_to_string(&path).with_context(|| format!("failed to read flavor manifest at {}", path.display()))?;
    let parsed: FlavorManifestStub =
        toml::from_str(&body).with_context(|| format!("failed to parse flavor manifest at {}", path.display()))?;

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
        // Include both `required` and `recommended` — the default
        // v0.5 flavor lists some role-covering backends under
        // `recommended` (e.g. `animus-subject-requirements` for the
        // `subject_kind:requirement` role) and a strict `required`-only
        // admit set would silently filter out an installed plugin that
        // preflight legitimately needs.
        for slug in section.required.iter().chain(section.recommended.iter()) {
            out.insert(normalize_flavor_slug_str(slug));
        }
    }
    Ok(out)
}

/// Mirror of `orchestrator_core::flavor::locate_flavor_manifest_in` so
/// the plugin-host crate can resolve the same flavor location order
/// without depending on `orchestrator-core`. Keep these in sync — the
/// flavor schema source-of-truth is `crates/orchestrator-core/src/flavor.rs`.
///
/// Order:
/// 1. `$ANIMUS_FLAVORS_DIR/default.toml` when the env var is set.
/// 2. `<project_root>/flavors/default.toml`.
/// 3. Walk up ancestors looking for a sibling `flavors/default.toml`.
fn locate_default_flavor_manifest(project_root: &Path) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ANIMUS_FLAVORS_DIR") {
        let path = Path::new(&dir).join("default.toml");
        if path.is_file() {
            return Some(path);
        }
    }

    let direct = project_root.join(FLAVOR_DEFAULT_MANIFEST_REL);
    if direct.is_file() {
        return Some(direct);
    }

    let mut walker: &Path = project_root;
    while let Some(parent) = walker.parent() {
        let candidate = parent.join(FLAVOR_DEFAULT_MANIFEST_REL);
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
    fn mode_wire_round_trip() {
        for mode in [PluginScopeMode::All, PluginScopeMode::FlavorOnly, PluginScopeMode::Allowlist] {
            let parsed = PluginScopeMode::parse_wire(mode.as_wire()).expect("parse");
            assert_eq!(parsed, mode);
        }
        assert!(PluginScopeMode::parse_wire("nope").is_err());
    }
}
