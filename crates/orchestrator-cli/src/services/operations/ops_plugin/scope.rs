//! `animus plugin scope` — per-project plugin scope CLI.
//!
//! Reads, writes, and resets the project-local
//! `<project>/.animus/plugin-scope.yaml` file consumed by
//! `orchestrator_plugin_host::PluginScope`. The same loader the daemon
//! discovery layer uses powers `show` so operators can confirm the
//! effective admit-set before opting in.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use orchestrator_core::flavor::{active_flavor_id_in, load_flavor_in, locate_flavor_manifest_in, DEFAULT_FLAVOR_ID};
use orchestrator_plugin_host::{PluginScope, PluginScopeMode, PLUGIN_SCOPE_FILE, PLUGIN_SCOPE_SCHEMA_V1};
use serde::Serialize;

use crate::cli_types::{PluginScopeResetArgs, PluginScopeSetArgs, PluginScopeShowArgs};
use crate::shared::print_value;

#[derive(Debug, Serialize)]
struct PluginScopeShowOutput {
    schema: &'static str,
    project_root: String,
    scope_file: Option<String>,
    mode: &'static str,
    allow: Vec<String>,
    require: Vec<String>,
    extras: Vec<String>,
    /// Persisted active flavor selection, or `default` when none is
    /// recorded. This is the flavor the resolver scopes against.
    active_flavor: String,
    /// `persisted` when read from `.animus/plugin-scope.yaml`, else
    /// `default`.
    active_flavor_source: &'static str,
    flavor_plugins: Vec<String>,
    effective_admit: Vec<String>,
    flavor_manifest_present: bool,
}

#[derive(Debug, Serialize)]
struct PluginScopeSetOutput {
    schema: &'static str,
    project_root: String,
    scope_file: String,
    mode: &'static str,
    allow: Vec<String>,
    require: Vec<String>,
    extras: Vec<String>,
    replaced: bool,
}

#[derive(Debug, Serialize)]
struct PluginScopeResetOutput {
    schema: &'static str,
    project_root: String,
    scope_file: String,
    deleted: bool,
}

fn scope_file_path(project_root: &Path) -> PathBuf {
    project_root.join(".animus").join(PLUGIN_SCOPE_FILE)
}

/// Helper for the CLI discover helpers in `ops_plugin.rs`. Returns the
/// scope to apply to `animus plugin list` (and friends) for the given
/// project root, falling back to [`PluginScope::unrestricted`] on
/// load failure so a broken scope file never silently empties the list.
pub(crate) fn load_project_scope(project_root: &Path) -> PluginScope {
    let (flavor_plugins, flavor_present, flavor_error) = resolve_flavor_plugins_with_error(project_root);
    let mut scope = PluginScope::load_for_project_with_flavor(project_root, &flavor_plugins, flavor_present)
        .unwrap_or_else(|err| {
            tracing::warn!(
                error = %err,
                "failed to load plugin scope; falling back to unrestricted discovery"
            );
            PluginScope::unrestricted()
        });
    // Carry the flavor parse failure onto the scope so
    // `discover_with_warnings` surfaces it as a DiscoveryWarning in
    // `animus plugin list` — without it the fail-closed empty admit set
    // looks like every plugin silently vanished.
    if scope.flavor_manifest_error.is_none() {
        scope.flavor_manifest_error = flavor_error;
    }
    scope
}

/// Strip the OWNER/ prefix from a flavor manifest's slug so it lines up
/// with the binary file name discovery records. `flavors/default.toml`
/// uses `launchapp-dev/animus-subject-default`; the installed binary is
/// just `animus-subject-default`. Without this step
/// `mode: flavor-only` would never admit anything.
fn normalize_flavor_slug(slug: &str) -> String {
    if let Some((_owner, rest)) = slug.split_once('/') {
        rest.to_string()
    } else {
        slug.to_string()
    }
}

fn resolve_flavor_plugins(project_root: &Path) -> (BTreeSet<String>, bool) {
    let (plugins, present, _) = resolve_flavor_plugins_with_error(project_root);
    (plugins, present)
}

fn resolve_flavor_plugins_with_error(project_root: &Path) -> (BTreeSet<String>, bool, Option<(PathBuf, String)>) {
    // Resolve the persisted active flavor (default `default`) so plugin
    // listing/scoping admits the active flavor's plugins, not always
    // `flavors/default.toml`. A persisted name whose manifest is gone is
    // STALE: fall back to the `default` flavor (matching
    // `plugin_preflight_wiring.rs`) rather than fail-closed to empty. See
    // the matching note there: `load_flavor_in` returns the binary-bundled
    // default manifest when no on-disk file exists, so we gate presence on
    // `locate_flavor_manifest_in` actually finding the file.
    let active = active_flavor_id_in(project_root);
    let stale_fallback = active != DEFAULT_FLAVOR_ID && locate_flavor_manifest_in(project_root, &active).is_none();
    let flavor_name = if stale_fallback {
        tracing::warn!(
            flavor = %active,
            "persisted active flavor `{active}` has no manifest on disk (flavors/{active}.toml); \
             falling back to the `default` flavor for plugin scope resolution"
        );
        DEFAULT_FLAVOR_ID.to_string()
    } else {
        active
    };
    // EXCEPTION (mirrors `plugin_preflight_wiring.rs`): on a stale
    // fallback to `default`, resolve the bundled-default flavor's plugins
    // even without an on-disk manifest so an explicit `mode: flavor-only`
    // file does not collapse to an empty admit set.
    if locate_flavor_manifest_in(project_root, &flavor_name).is_none() && !stale_fallback {
        return (BTreeSet::new(), false, None);
    }
    let manifest_path = locate_flavor_manifest_in(project_root, &flavor_name)
        .unwrap_or_else(|| project_root.join("flavors").join("default.toml"));
    match load_flavor_in(project_root, &flavor_name) {
        Ok(Some(manifest)) => {
            // Include recommended plugins — see the matching note in
            // `plugin_preflight_wiring.rs`. The default flavor lists
            // some required-role coverage (e.g. `animus-subject-requirements`)
            // under `recommended`, so an admit set that only honors
            // `required` would silently exclude a working install.
            let set: BTreeSet<String> =
                manifest.all_plugin_slugs(true).into_iter().map(|s| normalize_flavor_slug(&s)).collect();
            (set, true, None)
        }
        Ok(None) => (BTreeSet::new(), false, None),
        Err(err) => {
            // The manifest EXISTS but failed to read/parse/validate.
            // Report it present so the scope stays fail-closed
            // (`flavor-only` with an empty admit set, matching the
            // daemon's posture) and capture the error so `animus plugin
            // list` shows the real cause as a discovery warning.
            let reason = format!("{err:#}");
            tracing::warn!(
                manifest = %manifest_path.display(),
                error = %reason,
                "flavor manifest failed to load; flavor-only scope will admit NO plugins until it is fixed"
            );
            (BTreeSet::new(), true, Some((manifest_path, reason)))
        }
    }
}

/// Persist the active flavor selection into
/// `<project_root>/.animus/plugin-scope.yaml` so the daemon and CLI scope
/// resolvers admit the chosen flavor's plugins. Called by `animus plugin
/// install-defaults --flavor <name>` / `animus flavor install <name>` on
/// success.
///
/// Merges into any existing scope file rather than clobbering the mode /
/// allow / extras the operator already set. Selecting the canonical
/// `default` flavor CLEARS a previously persisted selection (so switching
/// back to default does not leave a stale `active_flavor:` key). When the
/// selection is `default` and no scope file exists, this is a no-op — the
/// resolver already defaults to `default`.
pub(crate) fn persist_active_flavor(project_root: &Path, flavor: &str) -> Result<()> {
    let path = scope_file_path(project_root);
    let is_default = flavor == DEFAULT_FLAVOR_ID;

    if !path.exists() {
        if is_default {
            // Nothing to persist and nothing to clear.
            return Ok(());
        }
        // Fresh scope file carrying the selection. Pick the SAME mode the
        // resolver would synthesize from defaults so the persisted file
        // does not change scoping behavior versus having no file:
        //   * `flavors/<name>.toml` present → `flavor-only` (scope the
        //     project to the chosen flavor's plugin set — the whole point
        //     of installing a non-default flavor).
        //   * absent → `all` (nothing to scope against yet).
        // Writing a blanket `mode: all` would short-circuit the
        // `load_for_project_with_flavor` flavor-only default (the file,
        // once present, wins over the synthesized mode), silently widening
        // discovery to every globally installed plugin.
        let manifest_present = locate_flavor_manifest_in(project_root, flavor).is_some();
        let mut scope = PluginScope::default();
        scope.mode = if manifest_present { PluginScopeMode::FlavorOnly } else { PluginScopeMode::All };
        scope.active_flavor = Some(flavor.to_string());
        scope.write_to_file(&path)?;
        return Ok(());
    }

    // Merge into the existing file. Pass an empty flavor-plugin set: we
    // only round-trip mode/allow/require/extras and rewrite the
    // selection; `flavor_plugins` is recomputed by the resolver on read.
    let mut scope = PluginScope::load_from_file(&path, &BTreeSet::new())
        .with_context(|| format!("failed to load existing scope file at {}", path.display()))?;
    scope.active_flavor = if is_default { None } else { Some(flavor.to_string()) };

    // When CLEARING back to the default flavor, a `mode: flavor-only` left
    // over from a prior non-default install would keep scoping the project
    // against the (possibly absent) default manifest — and if there is no
    // on-disk `flavors/default.toml`, the admit set collapses to empty and
    // every plugin is filtered out even though default plugins just
    // installed. Re-synthesize the mode the resolver would pick for the
    // default flavor (`flavor-only` only when `flavors/default.toml` is on
    // disk; otherwise `all`) so clearing the selection restores a working
    // scope. Allowlist mode is the operator's explicit choice and is left
    // untouched.
    if is_default && scope.mode == PluginScopeMode::FlavorOnly {
        scope.mode = if locate_flavor_manifest_in(project_root, DEFAULT_FLAVOR_ID).is_some() {
            PluginScopeMode::FlavorOnly
        } else {
            PluginScopeMode::All
        };
    }

    scope.write_to_file(&path)?;
    Ok(())
}

pub(crate) async fn handle_plugin_scope_show(args: PluginScopeShowArgs, project_root: &str) -> Result<()> {
    let root = Path::new(project_root);
    let (flavor_plugins, flavor_manifest_present) = resolve_flavor_plugins(root);
    let scope = PluginScope::load_for_project_with_flavor(root, &flavor_plugins, flavor_manifest_present)?;
    let effective: Vec<String> = scope.effective_admit_set().into_iter().collect();

    let persisted_flavor = orchestrator_plugin_host::read_active_flavor(root);
    let (active_flavor, active_flavor_source) = match &persisted_flavor {
        Some(name) => (name.clone(), "persisted"),
        None => (DEFAULT_FLAVOR_ID.to_string(), "default"),
    };

    let output = PluginScopeShowOutput {
        schema: PLUGIN_SCOPE_SCHEMA_V1,
        project_root: root.display().to_string(),
        scope_file: scope.source_path.as_ref().map(|p| p.display().to_string()),
        mode: scope.mode.as_wire(),
        allow: scope.allow.iter().cloned().collect(),
        require: scope.require.iter().cloned().collect(),
        extras: scope.extras.iter().cloned().collect(),
        active_flavor,
        active_flavor_source,
        flavor_plugins: scope.flavor_plugins.iter().cloned().collect(),
        effective_admit: effective.clone(),
        flavor_manifest_present,
    };

    if args.json {
        return print_value(&output, true);
    }

    println!("plugin scope @ {}", output.project_root);
    println!("  mode               : {}", output.mode);
    if let Some(path) = &output.scope_file {
        println!("  scope file         : {path}");
    } else {
        println!("  scope file         : (none; using defaults)");
    }
    println!("  active flavor      : {} (source: {})", output.active_flavor, output.active_flavor_source);
    println!("  flavor manifest    : {}", if output.flavor_manifest_present { "present" } else { "absent" });
    if !output.flavor_plugins.is_empty() {
        println!("  flavor plugins     :");
        for name in &output.flavor_plugins {
            println!("    - {name}");
        }
    }
    if !output.allow.is_empty() {
        println!("  allow              :");
        for name in &output.allow {
            println!("    - {name}");
        }
    }
    if !output.extras.is_empty() {
        println!("  extras             :");
        for name in &output.extras {
            println!("    - {name}");
        }
    }
    if !output.require.is_empty() {
        println!("  require            :");
        for role in &output.require {
            println!("    - {role}");
        }
    }
    println!("  effective admit set:");
    if effective.is_empty() {
        match scope.mode {
            PluginScopeMode::All => println!("    (unrestricted — every discovered plugin admits)"),
            _ => println!("    (empty — no discovered plugin will admit)"),
        }
    } else {
        for name in &effective {
            println!("    - {name}");
        }
    }
    Ok(())
}

pub(crate) async fn handle_plugin_scope_set(args: PluginScopeSetArgs, project_root: &str) -> Result<()> {
    let root = Path::new(project_root);
    let path = scope_file_path(root);
    let (flavor_plugins, _flavor_manifest_present) = resolve_flavor_plugins(root);

    let mut scope = if args.replace || !path.exists() {
        PluginScope { mode: PluginScopeMode::All, flavor_plugins: flavor_plugins.clone(), ..PluginScope::default() }
    } else {
        PluginScope::load_from_file(&path, &flavor_plugins).with_context(|| {
            format!("failed to load existing scope file at {} (use --replace to overwrite)", path.display())
        })?
    };

    let file_existed_before_set = path.exists();
    if let Some(mode_str) = args.mode.as_deref() {
        scope.mode = PluginScopeMode::parse_wire(mode_str)?;
    } else {
        // Default to `allowlist` so user-supplied `--allow` / `--extras`
        // are not silently ignored when:
        //  - the scope file doesn't exist yet (first-time set), OR
        //  - `--replace` was supplied (we are reinitializing from
        //    defaults rather than carrying over an existing mode).
        // The existing-mode path (no flag + no replace + file present)
        // still respects whatever mode was already on disk.
        let allow_or_extras_supplied = !args.allow.is_empty() || !args.extras.is_empty();
        if allow_or_extras_supplied && (!file_existed_before_set || args.replace) {
            scope.mode = PluginScopeMode::Allowlist;
        }
    }

    if args.replace {
        scope.allow.clear();
        scope.extras.clear();
        scope.require.clear();
    }
    for name in args.allow {
        scope.allow.insert(name);
    }
    for name in args.extras {
        scope.extras.insert(name);
    }
    for role in args.require {
        scope.require.insert(role);
    }

    scope.write_to_file(&path)?;

    let output = PluginScopeSetOutput {
        schema: PLUGIN_SCOPE_SCHEMA_V1,
        project_root: root.display().to_string(),
        scope_file: path.display().to_string(),
        mode: scope.mode.as_wire(),
        allow: scope.allow.iter().cloned().collect(),
        require: scope.require.iter().cloned().collect(),
        extras: scope.extras.iter().cloned().collect(),
        replaced: args.replace,
    };
    if args.json {
        return print_value(&output, true);
    }
    println!("wrote {}", output.scope_file);
    println!("  mode               : {}", output.mode);
    if !output.allow.is_empty() {
        println!("  allow              : {}", output.allow.join(", "));
    }
    if !output.extras.is_empty() {
        println!("  extras             : {}", output.extras.join(", "));
    }
    if !output.require.is_empty() {
        println!("  require            : {}", output.require.join(", "));
    }
    Ok(())
}

pub(crate) async fn handle_plugin_scope_reset(args: PluginScopeResetArgs, project_root: &str) -> Result<()> {
    let root = Path::new(project_root);
    let path = scope_file_path(root);
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to delete plugin scope file at {}", path.display()))?;
    }
    let output = PluginScopeResetOutput {
        schema: PLUGIN_SCOPE_SCHEMA_V1,
        project_root: root.display().to_string(),
        scope_file: path.display().to_string(),
        deleted: existed,
    };
    if args.json {
        return print_value(&output, true);
    }
    if existed {
        println!("deleted {}", output.scope_file);
    } else {
        println!("no scope file to delete at {}", output.scope_file);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_types::{PluginScopeResetArgs, PluginScopeSetArgs, PluginScopeShowArgs};

    fn project(temp: &tempfile::TempDir) -> String {
        std::fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
        temp.path().display().to_string()
    }

    #[tokio::test]
    async fn set_show_reset_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = project(&temp);

        handle_plugin_scope_set(
            PluginScopeSetArgs {
                mode: Some("allowlist".to_string()),
                allow: vec!["animus-subject-default".to_string()],
                extras: vec!["animus-provider-claude".to_string()],
                require: vec!["subject_kind:task".to_string()],
                replace: false,
                json: true,
            },
            &root,
        )
        .await
        .expect("set");

        let scope_path = Path::new(&root).join(".animus").join(PLUGIN_SCOPE_FILE);
        assert!(scope_path.exists(), "scope file must be written");

        // Show round-trips the file we just wrote.
        handle_plugin_scope_show(PluginScopeShowArgs { json: true }, &root).await.expect("show");

        handle_plugin_scope_reset(PluginScopeResetArgs { json: true }, &root).await.expect("reset");
        assert!(!scope_path.exists(), "reset must delete the scope file");
    }

    #[test]
    fn load_project_scope_fails_closed_and_records_error_on_broken_flavor_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        let manifest_path = flavors.join("default.toml");
        std::fs::write(&manifest_path, "this is [not valid TOML\n").expect("write broken flavor");

        let scope = load_project_scope(temp.path());
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly, "broken flavor must stay fail-closed");
        assert!(scope.effective_admit_set().is_empty());
        let (path, reason) = scope.flavor_manifest_error.as_ref().expect("parse failure must be recorded");
        assert_eq!(path, &manifest_path);
        assert!(reason.contains("default.toml"), "reason should reference the manifest: {reason}");
    }

    #[tokio::test]
    async fn first_time_set_with_allow_defaults_to_allowlist_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = project(&temp);
        // First-time set with --allow but no --mode and no --replace.
        // The historical bug: scope file was created with `mode: all`,
        // silently ignoring the operator's allow list.
        handle_plugin_scope_set(
            PluginScopeSetArgs {
                mode: None,
                allow: vec!["animus-subject-default".to_string()],
                extras: vec![],
                require: vec![],
                replace: false,
                json: true,
            },
            &root,
        )
        .await
        .expect("set");

        let scope_path = Path::new(&root).join(".animus").join(PLUGIN_SCOPE_FILE);
        let scope = PluginScope::load_from_file(&scope_path, &BTreeSet::new()).expect("load");
        assert_eq!(
            scope.mode,
            PluginScopeMode::Allowlist,
            "first-time set with --allow must default to allowlist, not silently fall back to all"
        );
        assert!(scope.allow.contains("animus-subject-default"));
    }

    #[test]
    fn persist_active_flavor_writes_selection_into_fresh_scope_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        persist_active_flavor(root, "enterprise").expect("persist");

        let path = root.join(".animus").join(PLUGIN_SCOPE_FILE);
        assert!(path.exists(), "persisting a non-default flavor must create the scope file");
        assert_eq!(orchestrator_plugin_host::read_active_flavor(root).as_deref(), Some("enterprise"));
        // No flavors/ dir → no manifest → mode stays `all` (nothing to
        // scope against), matching the resolver's synthesized default.
        let scope = PluginScope::load_from_file(&path, &BTreeSet::new()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::All);
    }

    #[test]
    fn persist_active_flavor_with_manifest_present_writes_flavor_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let flavors = root.join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(
            flavors.join("enterprise.toml"),
            "schema = \"animus.flavor.v1\"\nid = \"enterprise\"\nversion = \"0.5.0\"\ntitle = \"e\"\ndescription = \"e\"\n",
        )
        .expect("write flavor");

        persist_active_flavor(root, "enterprise").expect("persist");
        let path = root.join(".animus").join(PLUGIN_SCOPE_FILE);
        let scope = PluginScope::load_from_file(&path, &BTreeSet::new()).expect("load");
        // Manifest present → fresh file must default to flavor-only so the
        // project is actually scoped to the chosen flavor's plugin set.
        assert_eq!(scope.mode, PluginScopeMode::FlavorOnly);
        assert_eq!(scope.active_flavor.as_deref(), Some("enterprise"));
    }

    #[test]
    fn persist_default_flavor_is_noop_when_no_scope_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        persist_active_flavor(root, DEFAULT_FLAVOR_ID).expect("persist");
        let path = root.join(".animus").join(PLUGIN_SCOPE_FILE);
        assert!(!path.exists(), "persisting the default flavor must not create a scope file");
        assert!(orchestrator_plugin_host::read_active_flavor(root).is_none());
    }

    #[test]
    fn persist_active_flavor_merges_and_preserves_mode_and_allow() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let animus = root.join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir");
        std::fs::write(
            animus.join(PLUGIN_SCOPE_FILE),
            "schema: animus.plugin-scope.v1\nmode: allowlist\nallow:\n  - animus-subject-default\n",
        )
        .expect("write");

        persist_active_flavor(root, "enterprise").expect("persist");
        let scope = PluginScope::load_from_file(&animus.join(PLUGIN_SCOPE_FILE), &BTreeSet::new()).expect("load");
        assert_eq!(scope.mode, PluginScopeMode::Allowlist, "existing mode must survive the merge");
        assert!(scope.allow.contains("animus-subject-default"), "existing allow must survive the merge");
        assert_eq!(scope.active_flavor.as_deref(), Some("enterprise"));
    }

    #[test]
    fn persist_default_flavor_clears_stale_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        persist_active_flavor(root, "enterprise").expect("persist enterprise");
        assert_eq!(orchestrator_plugin_host::read_active_flavor(root).as_deref(), Some("enterprise"));
        // Switching back to default must clear the stale selection.
        persist_active_flavor(root, DEFAULT_FLAVOR_ID).expect("persist default");
        assert!(orchestrator_plugin_host::read_active_flavor(root).is_none());
    }

    #[test]
    fn resolve_flavor_plugins_unknown_persisted_name_falls_back_to_default_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let animus = root.join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir");
        // Persist a flavor that has NO manifest on disk; no flavors/ dir at
        // all, so even `default` resolves only via the binary-bundled
        // manifest.
        std::fs::write(
            animus.join(PLUGIN_SCOPE_FILE),
            "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: ghost\n",
        )
        .expect("write");

        // A stale name must fall back to the DEFAULT flavor's (bundled)
        // plugin set, never fail-closed to an empty admit set — otherwise
        // an explicit `mode: flavor-only` file filters out every plugin.
        let (plugins, present, error) = resolve_flavor_plugins_with_error(root);
        assert!(present, "stale active flavor must resolve the bundled default flavor (present)");
        assert!(!plugins.is_empty(), "stale fallback must populate the default flavor's plugin set, not empty");
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn set_replace_clears_existing_lists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = project(&temp);
        handle_plugin_scope_set(
            PluginScopeSetArgs {
                mode: Some("allowlist".to_string()),
                allow: vec!["animus-subject-default".to_string()],
                extras: vec![],
                require: vec![],
                replace: false,
                json: true,
            },
            &root,
        )
        .await
        .expect("initial set");

        handle_plugin_scope_set(
            PluginScopeSetArgs {
                mode: Some("allowlist".to_string()),
                allow: vec!["animus-subject-linear".to_string()],
                extras: vec![],
                require: vec![],
                replace: true,
                json: true,
            },
            &root,
        )
        .await
        .expect("replace set");

        let scope_path = Path::new(&root).join(".animus").join(PLUGIN_SCOPE_FILE);
        let scope = PluginScope::load_from_file(&scope_path, &BTreeSet::new()).expect("load");
        assert!(scope.allow.contains("animus-subject-linear"));
        assert!(!scope.allow.contains("animus-subject-default"), "replace must clear prior allow entries");
    }
}
