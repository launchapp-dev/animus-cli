//! `animus.toml` lifecycle: `install`, `add`, `remove`, plus the `init`
//! scaffolding (`animus.toml`, `.env.example`, and the project `.gitignore`).
//!
//! The manifest declares intent; resolution reuses the existing plugin/pack
//! install machinery (`ops_plugin::run_plugin_install`,
//! `ops_pack::install_pack_from_source_root`) so the lockfile and registry are
//! produced by exactly the same code path as the imperative commands.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_config::load_pack_manifest;
use orchestrator_config::pack_registry::{load_pack_inventory, PackRegistrySource};
use orchestrator_config::pack_selection::{
    load_pack_selection_state, save_pack_selection_state, PackSelectionEntry, PackSelectionSource,
};
use orchestrator_config::project_manifest::{
    load_project_manifest, project_manifest_path, save_project_manifest, Dependency, ProjectManifest,
};
use orchestrator_plugin_host::{plugin_install_dir, LockEntry, PluginLockfile};
use serde::Deserialize;
use serde_json::json;

use super::ops_init::{recommended_packs, RecommendedPackPin};
use super::ops_pack::install_pack_from_source_root;
use super::ops_plugin::{
    run_locked_install_default, run_plugin_install, run_plugin_uninstall, PluginInstallOutput, PluginInstallRequest,
    PluginUninstallRequest,
};
use crate::cli_types::{AddArgs, InstallArgs, RemoveArgs};
use crate::{invalid_input_error, not_found_error, print_value};

const DEFAULT_INSTALL_MANIFEST_JSON: &str = include_str!("../../../config/default-install.json");

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

pub(crate) async fn handle_manifest_install(args: InstallArgs, project_root: &str, json: bool) -> Result<()> {
    let root = Path::new(project_root);
    let manifest = load_project_manifest(root)?.ok_or_else(|| {
        invalid_input_error(format!(
            "no {} found in {} — run `animus init` to scaffold one",
            orchestrator_config::project_manifest::PROJECT_MANIFEST_FILE_NAME,
            project_root
        ))
    })?;

    let lock_path = PluginLockfile::default_path(Some(root));
    let lock_entries: Vec<LockEntry> =
        PluginLockfile::load_or_empty(&lock_path).map(|lock| lock.plugins).unwrap_or_default();
    let by_name: BTreeMap<String, &LockEntry> =
        lock_entries.iter().map(|entry| (entry.name.to_ascii_lowercase(), entry)).collect();

    if args.locked {
        // `--locked` reproduces the committed lockfile. Refuse if the manifest
        // declares a plugin the lockfile does not pin OR pins differently than
        // intent (stale lock) — npm-ci semantics.
        let mut drift: Vec<String> = Vec::new();
        let mut manifest_names: BTreeSet<String> = BTreeSet::new();
        for (name, dep) in &manifest.plugins {
            let installed = installed_plugin_name(name, dep).to_ascii_lowercase();
            manifest_names.insert(installed.clone());
            match by_name.get(&installed) {
                None => drift.push(format!("{name} (not pinned)")),
                Some(entry) if !lock_entry_matches_dep(entry, dep, root) => drift.push(format!(
                    "{name} (lock pins {}@{}, manifest declares a different pin)",
                    entry.source_repo.as_deref().unwrap_or("?"),
                    entry.version
                )),
                Some(_) => {}
            }
        }
        // A lock entry no longer declared in the manifest is also drift —
        // otherwise `run_locked_install_default` would silently reinstall a
        // plugin that was `animus remove`d from intent (npm-ci exactness).
        for entry in &lock_entries {
            if !manifest_names.contains(&entry.name.to_ascii_lowercase()) {
                drift.push(format!("{} (pinned in lockfile but absent from animus.toml)", entry.name));
            }
        }
        if !drift.is_empty() {
            return Err(invalid_input_error(format!(
                "--locked: manifest drifted from {}: {}. Run `animus install` (without --locked) to refresh the lockfile.",
                lock_path.display(),
                drift.join(", ")
            )));
        }
        // Install the declared packs (packs are NOT recorded in plugins.lock,
        // so a fresh CI/container checkout still needs them installed +
        // activated). A pack failure fails the whole locked run.
        for (id, dep) in &manifest.packs {
            install_pack_dep(id, dep, root, false)
                .with_context(|| format!("--locked: failed to install declared pack '{id}'"))?;
        }
        // A pack-only manifest has no plugin pins to reproduce — the plugin
        // locked installer would error on an empty/missing plugins.lock, so
        // skip it.
        if manifest.plugins.is_empty() {
            return print_value(
                json!({ "schema": "animus.install.v1", "locked": true, "plugins": [], "packs": manifest.packs.len() }),
                json,
            );
        }
        return run_locked_install_default(project_root, args.force, json).await;
    }

    let mut plugin_rows = Vec::new();
    for (name, dep) in &manifest.plugins {
        let installed = installed_plugin_name(name, dep);
        let key = installed.to_ascii_lowercase();
        // Skip ONLY when the binary is present AND the lock entry matches the
        // manifest pin. On a fresh `git clone` the lock is committed but
        // `.animus/plugins/` is gitignored, so a matching lock with no binary
        // must still install (regression guard: a lock-only match previously
        // skipped and left the project with zero plugins).
        let on_disk = plugin_install_dir().join(&installed).exists();
        if !args.force && on_disk {
            if let Some(entry) = by_name.get(&key) {
                if lock_entry_matches_dep(entry, dep, root) {
                    plugin_rows
                        .push(json!({ "name": name, "status": "skipped", "reason": "already installed and pinned" }));
                    continue;
                }
            }
        }
        // Binary on disk but absent/mismatched in the lock -> force a reinstall
        // so the install pipeline records (or refreshes) the project lock entry.
        match install_plugin_dep(name, dep, project_root, args.force || on_disk).await {
            Ok(output) => {
                plugin_rows.push(json!({ "name": name, "status": "installed", "path": output.installed_path }))
            }
            Err(err) => plugin_rows.push(json!({ "name": name, "status": "failed", "error": err.to_string() })),
        }
    }

    let mut pack_rows = Vec::new();
    for (id, dep) in &manifest.packs {
        match install_pack_dep(id, dep, root, args.force) {
            Ok(output) => pack_rows.push(
                json!({ "id": id, "status": if output.activated && output.already_present { "already installed" } else { "installed" }, "version": output.version }),
            ),
            Err(err) => pack_rows.push(json!({ "id": id, "status": "failed", "error": err.to_string() })),
        }
    }

    let failed = plugin_rows.iter().chain(pack_rows.iter()).filter(|row| row["status"] == "failed").count();
    print_value(
        json!({
            "schema": "animus.install.v1",
            "manifest": project_manifest_path(root).display().to_string(),
            "plugins": plugin_rows,
            "packs": pack_rows,
            "failed": failed,
        }),
        json,
    )?;
    // Fail the command (non-zero exit) when any dependency failed — critical
    // for the documented CI / container `animus install [--locked]` use case.
    if failed > 0 {
        return Err(invalid_input_error(format!("{failed} dependency install(s) failed; see the summary above")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// add / remove
// ---------------------------------------------------------------------------

pub(crate) async fn handle_manifest_add(args: AddArgs, project_root: &str, json: bool) -> Result<()> {
    let root = Path::new(project_root);
    let (name, dep) = parse_add_spec(&args)?;
    let mut manifest = load_project_manifest(root)?.unwrap_or_else(default_kernel_manifest);

    // Install FIRST, then write the manifest. This keeps a failed install from
    // leaving an orphan manifest entry, and lets packs key by their CANONICAL
    // id (from the pack manifest) rather than a repo basename — `add --pack
    // OWNER/REPO@tag` gives a basename, but inventory/selection use the pack id
    // (e.g. `animus.task`).
    if args.pack {
        let outcome = install_pack_dep(&name, &dep, root, args.force)
            .map_err(|err| invalid_input_error(format!("failed to install pack '{name}': {err}")))?;
        manifest.upsert_pack(&outcome.pack_id, dep.clone());
        save_project_manifest(root, &manifest)?;
        let status = if outcome.already_present { "already installed" } else { "installed" };
        print_value(
            json!({ "schema": "animus.add.v1", "added": { "id": outcome.pack_id, "status": status, "version": outcome.version } }),
            json,
        )
    } else {
        let output = install_plugin_dep(&name, &dep, project_root, args.force)
            .await
            .map_err(|err| invalid_input_error(format!("failed to install '{name}': {err}")))?;
        manifest.upsert_plugin(&name, dep.clone());
        save_project_manifest(root, &manifest)?;
        print_value(
            json!({ "schema": "animus.add.v1", "added": { "name": name, "status": "installed", "path": output.installed_path } }),
            json,
        )
    }
}

pub(crate) async fn handle_manifest_remove(args: RemoveArgs, project_root: &str, json: bool) -> Result<()> {
    let root = Path::new(project_root);
    let mut manifest = load_project_manifest(root)?.ok_or_else(|| {
        invalid_input_error(format!(
            "no {} found in {}",
            orchestrator_config::project_manifest::PROJECT_MANIFEST_FILE_NAME,
            project_root
        ))
    })?;

    let removed = if args.pack { manifest.remove_pack(&args.name) } else { manifest.remove_plugin(&args.name) };
    if !removed {
        return Err(not_found_error(format!(
            "'{}' is not declared in {}",
            args.name,
            orchestrator_config::project_manifest::PROJECT_MANIFEST_FILE_NAME
        )));
    }
    save_project_manifest(root, &manifest)?;

    let uninstall = if args.pack {
        // Packs are machine-global; removal deactivates the project's selection
        // (the version dir under ~/.animus/packs is shared and left in place).
        deactivate_pack(root, &args.name).map(|_| json!({ "id": args.name, "status": "deactivated" }))
    } else {
        run_plugin_uninstall(PluginUninstallRequest {
            name: args.name.clone(),
            plugin_dir: None,
            project_root: Some(project_root.to_string()),
            project: false,
        })
        .map(|_| json!({ "name": args.name, "status": "uninstalled" }))
    };

    match uninstall {
        Ok(row) => print_value(json!({ "schema": "animus.remove.v1", "removed": row }), json),
        // The manifest edit already landed; report the uninstall failure but
        // do not fail the whole command (the dependency is gone from intent).
        Err(err) => print_value(
            json!({ "schema": "animus.remove.v1", "removed": { "name": args.name, "status": "manifest_only", "error": err.to_string() } }),
            json,
        ),
    }
}

// ---------------------------------------------------------------------------
// init scaffolding (animus.toml + .env.example + .gitignore)
// ---------------------------------------------------------------------------

/// Project-root `.gitignore` lines Animus manages. Derived/secret/scratch are
/// ignored; the committed artifacts (`animus.toml`, `plugins.lock`,
/// `.env.example`, `.animus-version`, workflow YAML) are NOT. `.animus/*.lock`
/// catches the fs2 sidecar (`plugins.lock.lock`); the trailing negation
/// re-includes the committable `plugins.lock`.
const GITIGNORE_LINES: &[&str] = &[
    ".env",
    ".animus/plugins/",
    ".animus/plugins.yaml",
    ".animus/*.lock",
    ".animus/runs/",
    ".animus/artifacts/",
    "!.animus/plugins.lock",
];

const GITIGNORE_HEADER: &str =
    "# Animus — ignore derived/secret/scratch (animus.toml, animus.lock, .env.example, workflows stay committed)";

const ENV_EXAMPLE_CONTENTS: &str = "\
# Animus project secrets (example, committed).
# Copy this file to `.env`, fill in the values, and run `animus install` to load
# them into the device-encrypted secret store. `.env` is gitignored; this file
# declares which keys the project expects.
#
# ANTHROPIC_API_KEY=
# OPENAI_API_KEY=
";

/// Scaffold `animus.toml`, `.env.example`, and the project `.gitignore`.
/// Idempotent: existing `animus.toml` / `.env.example` are left untouched, and
/// the `.gitignore` is merged (only missing managed lines are appended; an
/// existing user `.gitignore` is never clobbered). Returns the list of files
/// created or modified.
pub(crate) fn ensure_project_scaffold(project_root: &Path) -> Result<Vec<String>> {
    let mut written = Vec::new();

    let manifest_path = project_manifest_path(project_root);
    if !manifest_path.exists() {
        save_project_manifest(project_root, &default_install_manifest())?;
        written.push(orchestrator_config::project_manifest::PROJECT_MANIFEST_FILE_NAME.to_string());
    }

    let env_example = project_root.join(".env.example");
    if !env_example.exists() {
        std::fs::write(&env_example, ENV_EXAMPLE_CONTENTS)
            .with_context(|| format!("failed to write {}", env_example.display()))?;
        written.push(".env.example".to_string());
    }

    if ensure_project_root_gitignore(project_root)? {
        written.push(".gitignore".to_string());
    }

    Ok(written)
}

/// Merge the managed lines into the project-root `.gitignore`. Appends only the
/// lines not already present (verbatim match), under a single header the first
/// time any line is added. Never rewrites or reorders existing content. Returns
/// `true` when the file was created or modified.
pub(crate) fn ensure_project_root_gitignore(project_root: &Path) -> Result<bool> {
    let path = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let present: BTreeSet<&str> = existing.lines().map(str::trim).collect();

    let missing: Vec<&str> = GITIGNORE_LINES.iter().copied().filter(|line| !present.contains(*line)).collect();
    if missing.is_empty() {
        return Ok(false);
    }

    let mut out = existing.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !present.contains(GITIGNORE_HEADER) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(GITIGNORE_HEADER);
        out.push('\n');
    }
    for line in missing {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(&path, out).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// resolution helpers
// ---------------------------------------------------------------------------

fn base_install_request(project_root: &str, force: bool) -> PluginInstallRequest {
    PluginInstallRequest {
        force,
        allow_org: vec!["launchapp-dev".to_string()],
        yes: true,
        allow_shadow_builtin: true,
        project_root: Some(project_root.to_string()),
        ..Default::default()
    }
}

async fn install_plugin_dep(
    name: &str,
    dep: &Dependency,
    project_root: &str,
    force: bool,
) -> Result<PluginInstallOutput> {
    let req = match dep {
        Dependency::Version(_) => {
            // A bare version resolves against the curated tables by repo
            // BASENAME (the manifest key), since the install pipeline needs a
            // full OWNER/REPO source. Non-curated names must use the git form.
            let (slug, tag) = orchestrator_core::resolve_curated_plugin_by_basename(name).ok_or_else(|| {
                invalid_input_error(format!(
                    "plugin '{name}': no curated source for a bare version requirement — declare it as \
                     {{ git = \"OWNER/REPO\", tag = \"...\" }}"
                ))
            })?;
            PluginInstallRequest {
                source: Some(slug.to_string()),
                tag: Some(tag.to_string()),
                ..base_install_request(project_root, force)
            }
        }
        Dependency::Git { repo, tag, .. } => PluginInstallRequest {
            source: Some(repo.clone()),
            tag: Some(tag.clone()),
            ..base_install_request(project_root, force)
        },
        Dependency::Path { path } => PluginInstallRequest {
            path: Some(resolve_source_path(project_root, path)),
            // Pin the installed/locked name to the manifest key so drift checks
            // and `animus remove` find it even when the path basename differs.
            name: Some(name.to_string()),
            ..base_install_request(project_root, force)
        },
    };
    run_plugin_install(req).await
}

/// True when an existing lock entry already satisfies the manifest dependency.
/// Git pins must match repo + tag; path deps must match the recorded source
/// path (`path:<abs>`); version deps match on name alone (the lock records no
/// version requirement to compare against).
fn lock_entry_matches_dep(entry: &LockEntry, dep: &Dependency, project_root: &Path) -> bool {
    match dep {
        Dependency::Git { repo, tag, .. } => {
            entry.source_repo.as_deref().is_some_and(|locked| locked.eq_ignore_ascii_case(repo))
                && normalize_version(&entry.version) == normalize_version(tag)
        }
        Dependency::Path { path } => {
            // Path installs record `source_repo = "path:<resolved>"`. A changed
            // path must NOT match the stale lock entry (else `animus install`
            // skips it and leaves the old plugin in place).
            let want = format!("path:{}", resolve_source_path(&project_root.to_string_lossy(), path));
            entry.source_repo.as_deref().is_some_and(|locked| locked == want)
        }
        Dependency::Version(_) => true,
    }
}

/// The outcome of resolving + installing a pack dependency.
struct PackOutcome {
    /// The canonical pack id (from the pack manifest, e.g. `animus.task`) — used
    /// as the `animus.toml` key so `add --pack OWNER/REPO@tag` stores the id, not
    /// the repo basename.
    pack_id: String,
    version: String,
    /// True when the pinned version was already machine-installed and we only
    /// (re)activated the project's selection — `animus install` is idempotent.
    already_present: bool,
    activated: bool,
}

fn install_pack_dep(id: &str, dep: &Dependency, project_root: &Path, force: bool) -> Result<PackOutcome> {
    // Resolve the source coordinates + the pinned version without downloading.
    let (repo_tag, path_source): (Option<(String, String)>, Option<PathBuf>) = match dep {
        Dependency::Path { path } => {
            (None, Some(PathBuf::from(resolve_source_path(&project_root.to_string_lossy(), path))))
        }
        Dependency::Git { repo, tag, .. } => (Some((repo.clone(), tag.clone())), None),
        Dependency::Version(_) => {
            let pin = recommended_packs().into_iter().find(|pin| pin.id.eq_ignore_ascii_case(id)).ok_or_else(|| {
                anyhow!(
                    "pack '{id}': no known source for a bare version requirement — declare it as \
                     {{ git = \"OWNER/REPO\", tag = \"...\" }}"
                )
            })?;
            (Some((pin.repo, pin.tag)), None)
        }
    };

    let pinned_version: Option<String> = match (&repo_tag, &path_source) {
        (Some((_, tag)), _) => Some(normalize_version(tag)),
        (_, Some(path)) => load_pack_manifest(path).ok().map(|loaded| loaded.manifest.version),
        _ => None,
    };

    // Idempotency: a pack version already machine-installed (init, or another
    // project) is reused — re-activate the project selection instead of erroring
    // on the "already exists" path. (Packs live under ~/.animus/packs/, global.)
    if !force {
        if let Some(version) = &pinned_version {
            let installed = load_pack_inventory(project_root)?.entries.into_iter().find(|entry| {
                entry.pack_id.eq_ignore_ascii_case(id)
                    && &entry.version == version
                    && entry.source == PackRegistrySource::Installed
            });
            if installed.is_some() {
                activate_pack_selection(project_root, id, version)?;
                return Ok(PackOutcome {
                    pack_id: id.to_string(),
                    version: version.clone(),
                    already_present: true,
                    activated: true,
                });
            }
        }
    }

    let scratch = tempfile::tempdir().context("failed to create scratch dir for pack download")?;
    let source_root: PathBuf = match (repo_tag, path_source) {
        (_, Some(path)) => path,
        (Some((repo, tag)), None) => {
            let pin = RecommendedPackPin { id: id.to_string(), repo, tag };
            super::ops_init::resolve_recommended_pack_source(&pin, scratch.path())?
        }
        (None, None) => unreachable!("pack dependency resolved to neither a git nor a path source"),
    };
    let output = install_pack_from_source_root(project_root, &source_root, true, force)?;
    Ok(PackOutcome {
        pack_id: output.pack_id,
        version: output.version,
        already_present: false,
        activated: output.activated,
    })
}

/// Strip a single leading `v` so a release tag (`v0.3.3`) and a lockfile /
/// manifest version (`0.3.3`) compare equal.
fn normalize_version(value: &str) -> String {
    value.strip_prefix('v').unwrap_or(value).to_string()
}

fn activate_pack_selection(project_root: &Path, pack_id: &str, version: &str) -> Result<()> {
    let mut state = load_pack_selection_state(project_root)?;
    state.upsert(PackSelectionEntry {
        pack_id: pack_id.to_string(),
        version: Some(format!("={version}")),
        source: Some(PackSelectionSource::Installed),
        enabled: true,
    })?;
    save_pack_selection_state(project_root, &state)?;
    Ok(())
}

/// The plugin name a dependency installs under (for the lockfile drift check).
/// Curated/version deps install under their slug; git deps under the repo
/// basename.
fn installed_plugin_name(name: &str, dep: &Dependency) -> String {
    match dep {
        Dependency::Git { repo, .. } => repo.rsplit('/').next().unwrap_or(name).to_string(),
        _ => name.to_string(),
    }
}

fn resolve_source_path(project_root: &str, path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        path.to_string()
    } else {
        Path::new(project_root).join(candidate).display().to_string()
    }
}

fn parse_add_spec(args: &AddArgs) -> Result<(String, Dependency)> {
    if let Some(path) = &args.path {
        return Ok((args.spec.clone(), Dependency::Path { path: path.clone() }));
    }
    let (base, version) = match args.spec.split_once('@') {
        Some((base, version)) => (base.to_string(), Some(version.to_string())),
        None => (args.spec.clone(), None),
    };
    if base.contains('/') {
        let tag = version.ok_or_else(|| {
            invalid_input_error(format!("git spec '{}' requires a release tag: OWNER/REPO@TAG", args.spec))
        })?;
        let name = base.rsplit('/').next().unwrap_or(&base).to_string();
        Ok((name, Dependency::Git { repo: base, tag, version: None }))
    } else {
        Ok((base, Dependency::Version(version.unwrap_or_else(|| "*".to_string()))))
    }
}

fn deactivate_pack(project_root: &Path, pack_id: &str) -> Result<()> {
    let mut state = load_pack_selection_state(project_root)?;
    let before = state.selections.len();
    state.selections.retain(|entry| !entry.pack_id.eq_ignore_ascii_case(pack_id));
    if state.selections.len() != before {
        save_pack_selection_state(project_root, &state)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// default manifest (the starter flavor, sourced from default-install.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DefaultInstall {
    #[serde(default)]
    packs: Vec<DefaultPack>,
    #[serde(default)]
    plugins: BTreeMap<String, Vec<DefaultPlugin>>,
}

#[derive(Debug, Deserialize)]
struct DefaultPack {
    id: String,
    repo: String,
    tag: String,
}

#[derive(Debug, Deserialize)]
struct DefaultPlugin {
    repo: String,
    tag: String,
}

fn default_kernel_manifest() -> ProjectManifest {
    ProjectManifest { kernel: Some(format!(">={}", env!("CARGO_PKG_VERSION"))), ..Default::default() }
}

/// Build the starter `animus.toml` from the recommended default set. Plugins +
/// packs are emitted as explicit `{ git, tag }` pins so the scaffold is
/// fully reproducible without relying on the curated resolution tables.
pub(crate) fn default_install_manifest() -> ProjectManifest {
    let mut manifest = default_kernel_manifest();
    let defaults: DefaultInstall = serde_json::from_str(DEFAULT_INSTALL_MANIFEST_JSON)
        .unwrap_or(DefaultInstall { packs: Vec::new(), plugins: BTreeMap::new() });

    for group in defaults.plugins.values() {
        for plugin in group {
            let name = plugin.repo.rsplit('/').next().unwrap_or(&plugin.repo).to_string();
            manifest.upsert_plugin(
                &name,
                Dependency::Git { repo: plugin.repo.clone(), tag: plugin.tag.clone(), version: None },
            );
        }
    }
    for pack in defaults.packs {
        manifest.upsert_pack(&pack.id, Dependency::Git { repo: pack.repo, tag: pack.tag, version: None });
    }
    manifest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_manifest_covers_curated_set() {
        let manifest = default_install_manifest();
        assert!(manifest.kernel.is_some());
        assert!(manifest.plugins.contains_key("animus-provider-claude"), "providers present");
        assert!(manifest.plugins.contains_key("animus-config-yaml"), "config source present");
        assert!(manifest.packs.contains_key("animus.core-skills"), "core-skills pack present");
        // Every default dep is an explicit git pin (reproducible scaffold).
        for dep in manifest.plugins.values().chain(manifest.packs.values()) {
            assert!(matches!(dep, Dependency::Git { .. }), "scaffold deps are git-pinned");
        }
    }

    #[test]
    fn parse_add_spec_handles_all_forms() {
        let version = parse_add_spec(&AddArgs {
            spec: "animus-provider-claude@>=0.2.7".to_string(),
            pack: false,
            path: None,
            force: false,
        })
        .unwrap();
        assert_eq!(version.0, "animus-provider-claude");
        assert_eq!(version.1, Dependency::Version(">=0.2.7".to_string()));

        let bare = parse_add_spec(&AddArgs { spec: "foo".to_string(), pack: false, path: None, force: false }).unwrap();
        assert_eq!(bare.1, Dependency::Version("*".to_string()));

        let git = parse_add_spec(&AddArgs {
            spec: "launchapp-dev/animus-queue-default@v0.3.3".to_string(),
            pack: false,
            path: None,
            force: false,
        })
        .unwrap();
        assert_eq!(git.0, "animus-queue-default");
        assert_eq!(
            git.1,
            Dependency::Git {
                repo: "launchapp-dev/animus-queue-default".to_string(),
                tag: "v0.3.3".to_string(),
                version: None,
            }
        );

        let pathed = parse_add_spec(&AddArgs {
            spec: "vendored".to_string(),
            pack: false,
            path: Some("plugins/vendored".to_string()),
            force: false,
        })
        .unwrap();
        assert_eq!(pathed.1, Dependency::Path { path: "plugins/vendored".to_string() });
    }

    #[test]
    fn normalize_version_strips_single_leading_v() {
        assert_eq!(normalize_version("v0.3.3"), "0.3.3");
        assert_eq!(normalize_version("0.3.3"), "0.3.3");
        assert_eq!(normalize_version("v1.0.0-rc.1"), "1.0.0-rc.1");
    }

    #[test]
    fn bare_version_plugin_resolves_via_curated_basename() {
        // The manifest key is the bare basename; resolution must find the full
        // launchapp-dev/<repo> slug + curated tag (regression guard for the
        // codex P1 — bare source previously failed parse_repo_spec).
        let (slug, _tag) = orchestrator_core::resolve_curated_plugin_by_basename("animus-provider-claude")
            .expect("curated provider must resolve");
        assert_eq!(slug, "launchapp-dev/animus-provider-claude");
        assert!(orchestrator_core::resolve_curated_plugin_by_basename("not-a-real-plugin").is_none());
    }

    #[test]
    fn git_spec_without_tag_is_rejected() {
        let err = parse_add_spec(&AddArgs { spec: "owner/repo".to_string(), pack: false, path: None, force: false })
            .expect_err("git spec needs a tag");
        assert!(err.to_string().contains("requires a release tag"), "got: {err}");
    }

    #[test]
    fn gitignore_is_idempotent_and_merge_safe() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        // Pre-existing user .gitignore must not be clobbered.
        std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();

        assert!(ensure_project_root_gitignore(root).unwrap(), "first run modifies");
        let after = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(after.contains("node_modules/"), "user content preserved");
        assert!(after.contains(".env"), "managed lines appended");
        assert!(after.contains("!.animus/plugins.lock"), "lock re-included");
        // animus.toml / animus.lock must NOT be ignored.
        assert!(!after.lines().any(|l| l.trim() == "animus.toml"), "manifest stays committed");
        assert!(
            !after.lines().any(|l| l.trim() == "animus.lock" || l.trim() == "plugins.lock"),
            "lock stays committed"
        );

        assert!(!ensure_project_root_gitignore(root).unwrap(), "second run is a no-op");
        let after2 = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(after, after2, "idempotent");
    }

    #[test]
    fn scaffold_writes_manifest_env_and_gitignore_then_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let first = ensure_project_scaffold(root).unwrap();
        assert!(first.contains(&"animus.toml".to_string()));
        assert!(first.contains(&".env.example".to_string()));
        assert!(first.contains(&".gitignore".to_string()));
        assert!(root.join("animus.toml").exists());
        // Re-running scaffolds nothing new.
        let second = ensure_project_scaffold(root).unwrap();
        assert!(second.is_empty(), "scaffold is idempotent, got {second:?}");
    }
}
