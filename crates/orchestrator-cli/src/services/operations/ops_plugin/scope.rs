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
use orchestrator_core::flavor::{load_flavor_in, locate_flavor_manifest_in, DEFAULT_FLAVOR_ID};
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
    let (flavor_plugins, flavor_present) = resolve_flavor_plugins(project_root);
    PluginScope::load_for_project_with_flavor(project_root, &flavor_plugins, flavor_present).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            "failed to load plugin scope; falling back to unrestricted discovery"
        );
        PluginScope::unrestricted()
    })
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
    // See the matching note in `plugin_preflight_wiring.rs`:
    // `load_flavor_in` returns the binary-bundled default manifest when
    // no on-disk file exists, which would otherwise make every project
    // appear to have an opt-in flavor. Use `locate_flavor_manifest_in`
    // to gate presence on the on-disk file actually existing.
    if locate_flavor_manifest_in(project_root, DEFAULT_FLAVOR_ID).is_none() {
        return (BTreeSet::new(), false);
    }
    match load_flavor_in(project_root, DEFAULT_FLAVOR_ID) {
        Ok(Some(manifest)) => {
            // Include recommended plugins — see the matching note in
            // `plugin_preflight_wiring.rs`. The default flavor lists
            // some required-role coverage (e.g. `animus-subject-requirements`)
            // under `recommended`, so an admit set that only honors
            // `required` would silently exclude a working install.
            let set: BTreeSet<String> =
                manifest.all_plugin_slugs(true).into_iter().map(|s| normalize_flavor_slug(&s)).collect();
            (set, true)
        }
        _ => (BTreeSet::new(), false),
    }
}

pub(crate) async fn handle_plugin_scope_show(args: PluginScopeShowArgs, project_root: &str) -> Result<()> {
    let root = Path::new(project_root);
    let (flavor_plugins, flavor_manifest_present) = resolve_flavor_plugins(root);
    let scope = PluginScope::load_for_project_with_flavor(root, &flavor_plugins, flavor_manifest_present)?;
    let effective: Vec<String> = scope.effective_admit_set().into_iter().collect();

    let output = PluginScopeShowOutput {
        schema: PLUGIN_SCOPE_SCHEMA_V1,
        project_root: root.display().to_string(),
        scope_file: scope.source_path.as_ref().map(|p| p.display().to_string()),
        mode: scope.mode.as_wire(),
        allow: scope.allow.iter().cloned().collect(),
        require: scope.require.iter().cloned().collect(),
        extras: scope.extras.iter().cloned().collect(),
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
