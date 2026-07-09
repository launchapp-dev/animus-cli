use std::path::{Path, PathBuf};

use anyhow::Result;
use orchestrator_core::flavor::{active_flavor_id_in, load_flavor_in, locate_flavor_manifest_in};
use orchestrator_core::{
    summarize_discovered_plugins_with_lock, InstalledPluginSummary, PluginInstaller, PluginPreflightRunner,
    PluginPreflightSpec, PreflightResult,
};
use orchestrator_plugin_host::{PluginDiscovery, PluginLockfile, PluginScope, PluginScopeMode};
use std::collections::BTreeSet;

use crate::DaemonRunEvent;
use crate::DaemonRunHooks;

pub struct PreflightOutcome {
    pub result: PreflightResult,
    pub skipped: bool,
    pub auto_install: bool,
    /// Set when plugin discovery itself failed (e.g. registry permission
    /// denied, manifest parse error). Distinct from "no plugins installed":
    /// here, we could not even read the plugin set, so an operator must fix
    /// the underlying filesystem / manifest issue before install advice is
    /// meaningful.
    pub discovery_error: Option<String>,
}

impl PreflightOutcome {
    pub fn should_abort_startup(&self) -> bool {
        if self.discovery_error.is_some() {
            return true;
        }
        !self.skipped && !self.result.is_ok()
    }

    /// Renders the operator-facing abort message. When discovery failed,
    /// surfaces the actual error plus a diagnostic hint rather than the
    /// generic "install plugins" advice that masked the real fault.
    pub fn render_abort_message(&self) -> String {
        if let Some(err) = &self.discovery_error {
            return format!(
                "plugin preflight failed: could not read installed plugins: {err}\n\
                 Check `~/.animus/plugins/` permissions or run `animus plugin list` to inspect.\n",
            );
        }
        self.result.render_missing_message()
    }
}

pub async fn run_plugin_preflight<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    spec: PluginPreflightSpec,
    installer: Option<&dyn PluginInstaller>,
    runtime_skip: bool,
    hooks: &mut H,
) -> Result<PreflightOutcome> {
    let auto_install = spec.auto_install;
    if runtime_skip {
        let _ = hooks.handle_event(DaemonRunEvent::PluginPreflight {
            project_root: primary_root.to_string(),
            satisfied: Vec::new(),
            auto_installed: Vec::new(),
            missing: Vec::new(),
            skipped: true,
            auto_install,
        });
        return Ok(PreflightOutcome {
            result: PreflightResult::default(),
            skipped: true,
            auto_install,
            discovery_error: None,
        });
    }

    // Preflight runs against the SCOPED set so a project that opted out
    // of a flavor's required plugin sees a single actionable error
    // ("scope mode=X excludes plugin Y required for role Z"). The
    // discover_installed_plugins helper auto-applies project scope via
    // the discovery layer.
    let (installed, flavor_error) = match discover_installed_plugins_with_flavor_error(project_root) {
        Ok(discovered) => discovered,
        Err(e) => {
            let error_msg = format!("{e:#}");
            let _ = hooks.handle_event(DaemonRunEvent::PluginsDiscoveryFailed {
                project_root: primary_root.to_string(),
                error: error_msg.clone(),
            });
            let _ = hooks.handle_event(DaemonRunEvent::PluginPreflight {
                project_root: primary_root.to_string(),
                satisfied: Vec::new(),
                auto_installed: Vec::new(),
                missing: Vec::new(),
                skipped: false,
                auto_install,
            });
            return Ok(PreflightOutcome {
                result: PreflightResult::default(),
                skipped: false,
                auto_install,
                discovery_error: Some(error_msg),
            });
        }
    };
    record_plugins_installed_gauge(&installed);
    // A broken flavor manifest empties the flavor-only admit set, so the
    // missing roles below are a symptom — not an install gap. Withhold
    // the installer (auto-install would mutate the plugin set on a run
    // that aborts anyway, since rediscovery stays scoped to the empty
    // admit set), attach the error so the abort message leads with the
    // real cause, and skip the scope-exclude annotation whose `--extras`
    // advice would conflict with "fix the manifest".
    let installer = if flavor_error.is_some() { None } else { installer };
    let mut result = PluginPreflightRunner::run(&spec, installed, installer).await?;
    result.flavor_manifest_error = flavor_error;
    if !result.is_ok() && result.flavor_manifest_error.is_none() {
        annotate_missing_with_scope_excludes(project_root, &mut result);
    }
    // Non-fatal advisories (under-pinned workflow runner / queue, plugin
    // lockfile drift). Never affect the OK verdict or abort startup.
    let mut warnings = workflow_runner_warnings(project_root);
    warnings.extend(queue_warnings(project_root));
    warnings.extend(lock_drift_warnings(project_root));
    result.warnings = warnings;
    for warning in &result.warnings {
        tracing::warn!("plugin preflight: {warning}");
    }

    let _ = hooks.handle_event(DaemonRunEvent::PluginPreflight {
        project_root: primary_root.to_string(),
        satisfied: result.satisfied.clone(),
        auto_installed: result.auto_installed.iter().map(|a| format!("{}={}", a.role, a.repo)).collect(),
        missing: result.missing.iter().map(|m| m.role.clone()).collect(),
        skipped: false,
        auto_install,
    });

    Ok(PreflightOutcome { result, skipped: false, auto_install, discovery_error: None })
}

pub fn discover_installed_plugins(project_root: &str) -> Result<Vec<InstalledPluginSummary>> {
    Ok(discover_installed_plugins_with_flavor_error(project_root)?.0)
}

/// Like [`discover_installed_plugins`], but also reports whether a broken
/// flavor manifest is gating the discovered set. The second tuple slot is
/// `Some(message)` when the flavor manifest exists on disk, failed to
/// load, AND the resolved scope is in flavor-only mode — i.e. the empty
/// plugin set is a fail-closed consequence of the broken manifest, not a
/// missing install. Preflight callers attach it to
/// [`PreflightResult::flavor_manifest_error`] so the report names the
/// manifest instead of handing out install advice that cannot fix it.
pub fn discover_installed_plugins_with_flavor_error(
    project_root: &str,
) -> Result<(Vec<InstalledPluginSummary>, Option<String>)> {
    let root = Path::new(project_root);
    let scope = resolve_scope_for_project(root);
    let flavor_error = flavor_gating_error(&scope);
    let plugins = PluginDiscovery::new().with_project_root(root).with_scope(scope).discover()?;
    // v0.5.7: consult the project's plugins.lock so renamed entries
    // (installed_kind != native_kind) report against the kind operators
    // actually dispatch against. Missing/unreadable lockfile is non-fatal
    // (matches pre-v0.5.7 behavior).
    let lockfile = PluginLockfile::load_default(Some(root)).ok();
    Ok((summarize_discovered_plugins_with_lock(&plugins, lockfile.as_ref()), flavor_error))
}

/// Non-fatal preflight advisories derived from discovered plugin manifests.
///
/// Today the only advisory is an under-pinned `workflow_runner` plugin: a
/// runner whose manifest version is below the skill-payload floor installs
/// and runs fine but silently ignores phase skills. This NEVER fails
/// preflight — it is surfaced as a WARNING so operators can `animus plugin
/// update`. Discovery errors are swallowed (returns an empty list): a
/// warning probe must not be able to abort startup.
pub fn workflow_runner_warnings(project_root: &str) -> Vec<String> {
    let root = Path::new(project_root);
    let scope = resolve_scope_for_project(root);
    let Ok(plugins) = PluginDiscovery::new().with_project_root(root).with_scope(scope).discover() else {
        return Vec::new();
    };
    plugins
        .iter()
        .filter(|p| p.manifest.serves_kind("workflow_runner"))
        .filter_map(|p| orchestrator_core::workflow_runner_underpin_warning(&p.name, &p.manifest.version))
        .collect()
}

/// Discover installed `queue` plugins and surface a non-fatal preflight warning
/// for any whose manifest version is below the precise-wake floor. Mirrors
/// [`workflow_runner_warnings`]; the daemon concatenates both at startup.
pub fn queue_warnings(project_root: &str) -> Vec<String> {
    let root = Path::new(project_root);
    let scope = resolve_scope_for_project(root);
    let Ok(plugins) = PluginDiscovery::new().with_project_root(root).with_scope(scope).discover() else {
        return Vec::new();
    };
    plugins
        .iter()
        .filter(|p| p.manifest.serves_kind("queue"))
        .filter_map(|p| orchestrator_core::queue_underpin_warning(&p.name, &p.manifest.version))
        .collect()
}

/// Non-fatal preflight advisories derived from comparing the plugin lockfile
/// against the installed/discovered plugin set. Two drift directions are
/// surfaced as WARNINGS (never failures): a lockfile entry whose installed
/// binary is missing or whose sha256 no longer matches the pin, and a
/// discovered plugin that is absent from the lockfile ("extra"). All errors
/// are swallowed (returns an empty list): a warning probe must never abort
/// startup. Operators resolve drift with `animus plugin lock verify`.
pub fn lock_drift_warnings(project_root: &str) -> Vec<String> {
    let root = Path::new(project_root);
    // Discover UNSCOPED: lockfile entries pin INSTALLED binaries regardless of
    // the project's flavor/`plugin-scope.yaml` filter, so a globally locked
    // plugin that the active scope happens to exclude is still installed and
    // must not be reported as "missing" drift (codex P2). The scoped set is the
    // runtime dispatch view, not the install-integrity view.
    let Ok(discovered) =
        PluginDiscovery::new().with_project_root(root).with_scope(PluginScope::unrestricted()).discover()
    else {
        return Vec::new();
    };

    // Sweep BOTH lockfile roots — the project `.animus/plugins.lock` and the
    // global `~/.animus/plugins.lock` — so a globally locked plugin that
    // discovery surfaces is not falsely flagged "extra" (matches the
    // `plugin lock verify` both-roots sweep; codex P3). A missing/unreadable
    // root is treated as empty (the probe must never abort startup).
    let project_lock = PluginLockfile::load_or_empty(&orchestrator_plugin_host::project_lockfile_path(root)).ok();
    let global_lock = PluginLockfile::load_or_empty(&orchestrator_plugin_host::global_lockfile_path()).ok();
    let mut entries: Vec<orchestrator_plugin_host::LockEntry> = Vec::new();
    let mut locked: BTreeSet<String> = BTreeSet::new();
    for lock in [project_lock.as_ref(), global_lock.as_ref()].into_iter().flatten() {
        for entry in &lock.plugins {
            if locked.insert(entry.name.clone()) {
                entries.push(entry.clone());
            }
        }
    }
    // NB: do NOT early-return when `entries` is empty — an installed plugin
    // with no lockfile entry at all is still "extra" drift, and the
    // extra-detection pass below must run to match `plugin lock verify`
    // (codex P3).
    let mut warnings = Vec::new();
    for entry in &entries {
        match discovered.iter().find(|p| p.name == entry.name) {
            None => warnings.push(format!(
                "plugin lock drift: {} installed binary missing; run `animus plugin lock verify`",
                entry.name
            )),
            Some(plugin) => {
                // Target-aware (schema 2.0): compare the on-disk binary against
                // the host-only `installed_binary_sha256` recorded for the
                // CURRENT target. Only a `Mismatch` is drift; `MissingTarget` /
                // `Missing` (1.0-migrated entry, or a lock generated on another
                // platform) carries no comparable claim here, so it is left to
                // `plugin lock verify` to report distinctly and not warned on.
                let drift = [project_lock.as_ref(), global_lock.as_ref()]
                    .into_iter()
                    .flatten()
                    .find(|lock| lock.find(&entry.name).is_some())
                    .map(|lock| lock.verify_installed(&entry.name, &plugin.path));
                if let Some(Ok(orchestrator_plugin_host::LockVerifyResult::Mismatch { .. })) = drift {
                    warnings.push(format!(
                        "plugin lock drift: {} sha256 mismatch; run `animus plugin lock verify`",
                        entry.name
                    ));
                }
            }
        }
    }
    for plugin in &discovered {
        if !locked.contains(&plugin.name) {
            warnings.push(format!(
                "plugin lock drift: {} installed but not in lockfile (extra); run `animus plugin lock verify`",
                plugin.name
            ));
        }
    }
    warnings
}

/// Render the scope's recorded flavor-manifest failure when (and only
/// when) it actually gates discovery. An explicit
/// `.animus/plugin-scope.yaml` that overrides the mode to `all` or
/// `allowlist` makes the broken manifest irrelevant to the plugin
/// universe, matching the DiscoveryWarning consequence wording in
/// `orchestrator_plugin_host::discovery`.
fn flavor_gating_error(scope: &PluginScope) -> Option<String> {
    if !matches!(scope.mode, PluginScopeMode::FlavorOnly) {
        return None;
    }
    scope
        .flavor_manifest_error
        .as_ref()
        .map(|(path, reason)| format!("flavor manifest at {} failed to load: {reason}", path.display()))
}

/// Load the active flavor's plugin-name set (normalized to binary file
/// names — see [`normalize_flavor_slug`]) and use it to build a
/// [`PluginScope`] anchored at `project_root`. A broken flavor manifest
/// stays fail-closed (flavor-only with an empty admit set) and is
/// recorded on [`PluginScope::flavor_manifest_error`]; only a broken
/// scope FILE falls back to [`PluginScope::unrestricted`] so discovery
/// itself stays alive.
pub fn resolve_scope_for_project(project_root: &Path) -> PluginScope {
    let (flavor_plugins, flavor_present, flavor_error) = resolve_flavor_plugins(project_root);
    let mut scope = match PluginScope::load_for_project_with_flavor(project_root, &flavor_plugins, flavor_present) {
        Ok(scope) => scope,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to load plugin scope for preflight; falling back to unrestricted discovery"
            );
            PluginScope::unrestricted()
        }
    };
    if scope.flavor_manifest_error.is_none() {
        scope.flavor_manifest_error = flavor_error;
    }
    scope
}

fn normalize_flavor_slug(slug: &str) -> String {
    if let Some((_owner, rest)) = slug.split_once('/') {
        rest.to_string()
    } else {
        slug.to_string()
    }
}

fn resolve_flavor_plugins(project_root: &Path) -> (BTreeSet<String>, bool, Option<(PathBuf, String)>) {
    // Resolve the persisted active flavor (default `default`) so a
    // project that opted into a non-default flavor via `animus plugin
    // install-defaults --flavor <name>` scopes against THAT flavor's
    // plugin set rather than always `flavors/default.toml`. A persisted
    // name whose `flavors/<name>.toml` is gone (renamed/deleted) is
    // STALE: per the spec we fall back to the `default` flavor rather than
    // fail-closed to an empty admit set (which an explicit `mode:
    // flavor-only` file would otherwise enforce).
    let active = active_flavor_id_in(project_root);
    let default_id = orchestrator_core::flavor::DEFAULT_FLAVOR_ID;
    let stale_fallback = active != default_id && locate_flavor_manifest_in(project_root, &active).is_none();
    let flavor_name = if stale_fallback {
        tracing::warn!(
            flavor = %active,
            "persisted active flavor `{active}` has no manifest on disk (flavors/{active}.toml); \
             falling back to the `default` flavor for plugin scope resolution"
        );
        default_id.to_string()
    } else {
        active
    };
    // `load_flavor_in` falls back to the binary-bundled default flavor
    // when no on-disk manifest is present. For the scope-default
    // decision we must distinguish "project actually opted into a
    // flavor" from "binary-bundled fallback" — otherwise every project
    // silently defaults to `flavor-only` mode the moment Animus is
    // installed. `locate_flavor_manifest_in` returns None when the
    // operator has not written `flavors/<id>.toml`, so we drive the
    // presence flag from that.
    //
    // EXCEPTION: when we fell back from a stale non-default selection to
    // `default`, an explicit `mode: flavor-only` scope file is still in
    // force, so returning an empty admit set here would filter out every
    // plugin. Instead, resolve the bundled-default flavor's plugins
    // (`load_flavor_in` honors the binary-bundled manifest) so the admit
    // set is non-empty — the documented "fall back to default, never
    // fail-closed to empty" behavior.
    if locate_flavor_manifest_in(project_root, &flavor_name).is_none() && !stale_fallback {
        return (BTreeSet::new(), false, None);
    }
    let manifest_path = locate_flavor_manifest_in(project_root, &flavor_name)
        .unwrap_or_else(|| project_root.join("flavors").join("default.toml"));
    match load_flavor_in(project_root, &flavor_name) {
        Ok(Some(manifest)) => {
            // Include `recommended` plugins as well: operators who opted
            // into the recommended set (`animus plugin install-defaults
            // --include-recommended`) must not have those installed
            // plugins filtered out of scoped discovery. A strict
            // `required`-only admit set would leave e.g. an installed
            // `animus-subject-linear` invisible despite a working
            // install. Recommended plugins are part of the flavor's
            // curated bundle, so admitting them is the right
            // default-deny tradeoff.
            let set: BTreeSet<String> =
                manifest.all_plugin_slugs(true).into_iter().map(|s| normalize_flavor_slug(&s)).collect();
            (set, true, None)
        }
        Ok(None) => (BTreeSet::new(), false, None),
        Err(err) => {
            // The manifest EXISTS but failed to read/parse/validate.
            // Report it present so the scope stays fail-closed
            // (`flavor-only` with an empty admit set, matching discovery
            // and `animus plugin list`) and capture the error so the
            // preflight report names the broken manifest instead of
            // handing out install advice. Failing open here (mode `all`)
            // would let preflight pass against the unscoped plugin
            // universe while discovery filters everything out.
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

/// Walk the missing-plugin list and append a "scope excludes <name>" hint
/// when the plugin IS installed on disk but the project scope is
/// filtering it out. Helps the operator distinguish "not installed" from
/// "installed but out of scope" without sending them on a wild goose
/// chase through `animus plugin install`.
fn annotate_missing_with_scope_excludes(project_root: &str, result: &mut PreflightResult) {
    let root = Path::new(project_root);
    let scope = resolve_scope_for_project(root);
    if matches!(scope.mode, PluginScopeMode::All) {
        return;
    }
    // Discover with NO scope so we can detect installed-but-excluded.
    // The discover builder auto-loads scope when `project_root` is set
    // and no explicit scope is supplied — we must pass
    // `PluginScope::unrestricted()` here to bypass that auto-load,
    // otherwise the scope filter strips the very plugin we are trying
    // to find and the operator gets generic "install ..." advice
    // instead of the scope-aware fix message.
    let unscoped =
        match PluginDiscovery::new().with_project_root(root).with_scope(PluginScope::unrestricted()).discover() {
            Ok(plugins) => plugins,
            Err(_) => return,
        };
    let lockfile = PluginLockfile::load_default(Some(root)).ok();
    let unscoped_summary = summarize_discovered_plugins_with_lock(&unscoped, lockfile.as_ref());
    for missing in &mut result.missing {
        if let Some(candidate) = find_unscoped_satisfier(&missing.role, &unscoped_summary) {
            missing.fix_command = match scope.mode {
                // In flavor-only mode the operator opted into the
                // flavor's curated set; the right fix is to LAYER the
                // missing plugin via `--extras` so the flavor admit set
                // stays in effect. `--mode allowlist --allow <x>` would
                // strip every flavor plugin and break the other roles.
                PluginScopeMode::FlavorOnly => format!(
                    "scope mode=`flavor-only` excludes plugin `{candidate}` required for role `{role}`. \
                     Run `animus plugin scope set --extras {candidate}` or `animus plugin scope reset`.",
                    candidate = candidate,
                    role = missing.role,
                ),
                _ => format!(
                    "scope mode=`{mode}` excludes plugin `{candidate}` required for role `{role}`. \
                     Run `animus plugin scope set --mode allowlist --allow {candidate}` or `animus plugin scope reset`.",
                    mode = scope.mode.as_wire(),
                    candidate = candidate,
                    role = missing.role,
                ),
            };
        }
    }
}

fn find_unscoped_satisfier(role_label: &str, plugins: &[InstalledPluginSummary]) -> Option<String> {
    if role_label == "at_least_one_provider" {
        return plugins.iter().find(|p| p.is_provider()).map(|p| p.name.clone());
    }
    if role_label == "workflow_runner" {
        return plugins.iter().find(|p| p.is_workflow_runner()).map(|p| p.name.clone());
    }
    if role_label == "queue" {
        return plugins.iter().find(|p| p.is_queue()).map(|p| p.name.clone());
    }
    if let Some(kind) = role_label.strip_prefix("subject_kind:") {
        return plugins.iter().find(|p| p.covers_subject_kind(kind)).map(|p| p.name.clone());
    }
    None
}

fn record_plugins_installed_gauge(installed: &[InstalledPluginSummary]) {
    use std::collections::HashMap;
    let mut by_kind: HashMap<&str, u64> = HashMap::new();
    for p in installed {
        *by_kind.entry(p.plugin_kind.as_str()).or_insert(0) += 1;
    }
    crate::metrics::set_gauge("plugins_installed_total", installed.len() as f64);
    for (kind, count) in by_kind {
        crate::metrics::set_gauge(&crate::metrics::labeled("plugins_installed", &[("kind", kind)]), count as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_FLAVOR: &str = r#"schema = "animus.flavor.v1"
id = "default"
version = "0.5.0"
title = "Test"
description = "Test"

[providers]
required = ["launchapp-dev/animus-provider-claude"]
"#;

    fn write_flavor(root: &Path, body: &str) -> PathBuf {
        let flavors = root.join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        let path = flavors.join("default.toml");
        std::fs::write(&path, body).expect("write flavor");
        path
    }

    #[test]
    fn lock_drift_warnings_never_warns_about_a_locked_and_present_plugin() {
        // The probe must never abort startup, and a project whose lockfile
        // entries are all satisfied (or which has no project lockfile of its
        // own) must not produce a SPURIOUS warning naming a plugin this
        // project tracks. We assert the function returns cleanly and never
        // emits a "missing"/"mismatch" warning for a plugin we never locked.
        // (Mutating $HOME to fully isolate the global lockfile is avoided
        // here: it would race the parallel `stable_test_home()` pin used by
        // other tests in this crate.)
        let temp = tempfile::tempdir().expect("tempdir");
        let warnings = lock_drift_warnings(temp.path().to_string_lossy().as_ref());
        assert!(
            !warnings.iter().any(|w| w.contains("plugin-that-this-project-never-locked")),
            "probe must not invent warnings: {warnings:?}"
        );
    }

    #[test]
    fn lock_drift_warnings_flags_missing_binary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        let lock_path = animus.join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&lock_path);
        let mut targets = std::collections::BTreeMap::new();
        if let Some(triple) = orchestrator_plugin_host::current_target_triple() {
            targets.insert(
                triple.to_string(),
                orchestrator_plugin_host::TargetIntegrity {
                    archive_sha256: "a".repeat(64),
                    signature_bundle_sha256: None,
                    installed_binary_sha256: Some("a".repeat(64)),
                },
            );
        }
        lock.upsert(orchestrator_plugin_host::LockEntry {
            name: "animus-provider-ghost".to_string(),
            version: "v0.1.0".to_string(),
            targets,
            installed_at: chrono::Utc::now().to_rfc3339(),
            installed_kind: None,
            native_kind: None,
            source_repo: Some("launchapp-dev/animus-provider-ghost".to_string()),
            resolved_commit: None,
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        });
        lock.save().expect("save lock");

        // The binary was never installed, so discovery will not find it: the
        // lockfile entry is drift.
        let warnings = lock_drift_warnings(temp.path().to_string_lossy().as_ref());
        assert!(
            warnings.iter().any(|w| w.contains("animus-provider-ghost") && w.contains("missing")),
            "a locked entry with no installed binary must warn: {warnings:?}"
        );
    }

    #[test]
    fn resolve_scope_fails_closed_and_records_error_on_broken_flavor_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest_path = write_flavor(temp.path(), "this is [not valid TOML\n");

        let scope = resolve_scope_for_project(temp.path());
        assert!(matches!(scope.mode, PluginScopeMode::FlavorOnly), "broken flavor must stay fail-closed");
        assert!(scope.effective_admit_set().is_empty());
        let (path, reason) = scope.flavor_manifest_error.as_ref().expect("parse failure must be recorded");
        assert_eq!(path, &manifest_path);
        assert!(reason.contains("failed to parse flavor manifest"), "unexpected reason: {reason}");

        let gating = flavor_gating_error(&scope).expect("flavor-only scope must report a gating error");
        assert!(gating.contains(&manifest_path.display().to_string()), "gating error must name the manifest: {gating}");
    }

    #[test]
    fn resolve_scope_with_intact_manifest_resolves_plugins_without_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_flavor(temp.path(), VALID_FLAVOR);

        let scope = resolve_scope_for_project(temp.path());
        assert!(matches!(scope.mode, PluginScopeMode::FlavorOnly));
        assert!(scope.flavor_plugins.contains("animus-provider-claude"));
        assert!(scope.flavor_manifest_error.is_none());
        assert!(flavor_gating_error(&scope).is_none());
    }

    #[test]
    fn resolve_scope_without_manifest_stays_mode_all() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scope = resolve_scope_for_project(temp.path());
        assert!(matches!(scope.mode, PluginScopeMode::All));
        assert!(scope.flavor_manifest_error.is_none());
        assert!(flavor_gating_error(&scope).is_none());
    }

    #[test]
    fn resolve_scope_honors_persisted_active_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        // Default flavor present but EMPTY; a non-default flavor declares
        // the plugin we expect the active selection to admit.
        std::fs::write(flavors.join("default.toml"), VALID_FLAVOR).expect("write default");
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
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        std::fs::write(
            animus.join("plugin-scope.yaml"),
            "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: enterprise\n",
        )
        .expect("write scope");

        let scope = resolve_scope_for_project(temp.path());
        assert!(matches!(scope.mode, PluginScopeMode::FlavorOnly));
        assert!(
            scope.flavor_plugins.contains("animus-provider-enterprise"),
            "resolver must scope against the persisted active flavor, not flavors/default.toml"
        );
        assert!(
            !scope.flavor_plugins.contains("animus-provider-claude"),
            "default flavor's plugins must NOT leak when a non-default flavor is active"
        );
    }

    #[test]
    fn resolve_scope_stale_active_flavor_falls_back_to_default_flavor_not_empty() {
        // Explicit `mode: flavor-only` file + a STALE active flavor whose
        // manifest was removed. Without the default fallback the admit set
        // would be empty (every plugin filtered out). With it, the default
        // flavor's plugins admit instead.
        let temp = tempfile::tempdir().expect("tempdir");
        let flavors = temp.path().join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(flavors.join("default.toml"), VALID_FLAVOR).expect("write default");
        // NB: no flavors/enterprise.toml on disk — the selection is stale.
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        std::fs::write(
            animus.join("plugin-scope.yaml"),
            "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: enterprise\n",
        )
        .expect("write scope");

        let scope = resolve_scope_for_project(temp.path());
        assert!(matches!(scope.mode, PluginScopeMode::FlavorOnly));
        assert!(
            scope.flavor_plugins.contains("animus-provider-claude"),
            "stale active flavor must fall back to the default flavor's plugin set, not an empty admit set"
        );
        assert!(!scope.effective_admit_set().is_empty(), "must not fail-closed to empty on a stale active flavor");
    }

    #[test]
    fn resolve_scope_unknown_active_flavor_falls_back_to_mode_all() {
        let temp = tempfile::tempdir().expect("tempdir");
        let animus = temp.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        // Persist a flavor with no manifest on disk and no flavors/ dir.
        std::fs::write(
            animus.join("plugin-scope.yaml"),
            "schema: animus.plugin-scope.v1\nmode: all\nactive_flavor: ghost\n",
        )
        .expect("write scope");

        let scope = resolve_scope_for_project(temp.path());
        // Never fail-closed to empty on an unknown persisted name.
        assert!(matches!(scope.mode, PluginScopeMode::All));
        assert!(scope.flavor_manifest_error.is_none());
    }

    // ===== F2: cross-crate scope-ladder drift guard =====
    //
    // The stale-active-flavor → default → bundled-default fail-closed
    // fallback is implemented TWICE — once in this crate
    // (`resolve_scope_for_project`) and once in `orchestrator-plugin-host`
    // (`PluginScope::load_for_project`) — because plugin-host must not
    // depend on orchestrator-core's flavor loader. A refactor to share one
    // ladder is therefore impossible; instead these tests run BOTH ladders
    // against the same fixtures and assert identical admit sets + modes, so
    // the duplication can never silently drift.

    /// Read the canonical `flavors/default.toml` from the workspace root so
    /// fixtures use the SAME manifest both crates embed via `include_str!`.
    /// Reading the real file (rather than a literal copy) is itself the
    /// byte-identity guard: if either crate's embedded copy diverged from
    /// this file, its bundled-default admit set would no longer match the
    /// admit set this on-disk manifest produces, and the assertions below
    /// would fail.
    fn canonical_default_flavor() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../flavors/default.toml");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    fn assert_ladders_agree(project_root: &Path, label: &str) {
        let daemon_scope = resolve_scope_for_project(project_root);
        let host_scope = PluginScope::load_for_project(project_root).expect("plugin-host ladder must not error");
        assert_eq!(daemon_scope.mode, host_scope.mode, "[{label}] daemon-runtime and plugin-host scope MODES drifted");
        assert_eq!(
            daemon_scope.effective_admit_set(),
            host_scope.effective_admit_set(),
            "[{label}] daemon-runtime and plugin-host ADMIT SETS drifted"
        );
    }

    fn write_default_flavor(root: &Path) {
        let flavors = root.join("flavors");
        std::fs::create_dir_all(&flavors).expect("mkdir flavors");
        std::fs::write(flavors.join("default.toml"), canonical_default_flavor()).expect("write default flavor");
    }

    fn write_scope_file(root: &Path, body: &str) {
        let animus = root.join(".animus");
        std::fs::create_dir_all(&animus).expect("mkdir .animus");
        std::fs::write(animus.join("plugin-scope.yaml"), body).expect("write scope file");
    }

    #[test]
    fn scope_ladders_agree_no_scope_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_default_flavor(temp.path());
        assert_ladders_agree(temp.path(), "no-scope-file");
    }

    #[test]
    fn scope_ladders_agree_persisted_valid_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_default_flavor(temp.path());
        // `default` is persisted AND present on disk — the common valid case.
        write_scope_file(temp.path(), "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: default\n");
        assert_ladders_agree(temp.path(), "persisted-valid-flavor");
    }

    #[test]
    fn scope_ladders_agree_persisted_stale_flavor_falls_back_to_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_default_flavor(temp.path());
        // Persist a non-default flavor whose `flavors/enterprise.toml` is
        // absent: BOTH ladders must fall back to the default flavor's admit
        // set rather than fail-closing to empty.
        write_scope_file(temp.path(), "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: enterprise\n");
        assert_ladders_agree(temp.path(), "persisted-stale-flavor");
    }

    #[test]
    fn scope_ladders_agree_stale_flavor_bundled_default_fallback() {
        // No `flavors/default.toml` on disk at all, but an explicit
        // `mode: flavor-only` + stale active flavor. Both ladders must reach
        // for their binary-bundled default manifest and resolve the SAME
        // admit set — this is the direct byte-identity guard on the two
        // embedded `flavors/default.toml` copies.
        let temp = tempfile::tempdir().expect("tempdir");
        write_scope_file(temp.path(), "schema: animus.plugin-scope.v1\nmode: flavor-only\nactive_flavor: enterprise\n");
        let daemon_scope = resolve_scope_for_project(temp.path());
        let host_scope = PluginScope::load_for_project(temp.path()).expect("plugin-host ladder must not error");
        assert!(
            !daemon_scope.effective_admit_set().is_empty(),
            "bundled-default fallback must produce a non-empty admit set"
        );
        assert_eq!(
            daemon_scope.effective_admit_set(),
            host_scope.effective_admit_set(),
            "bundled-default admit sets drifted — an embedded flavors/default.toml copy diverged",
        );
    }

    #[test]
    fn explicit_scope_file_mode_override_does_not_gate_on_broken_flavor() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_flavor(temp.path(), "this is [not valid TOML\n");
        let animus_dir = temp.path().join(".animus");
        std::fs::create_dir_all(&animus_dir).expect("mkdir .animus");
        std::fs::write(animus_dir.join("plugin-scope.yaml"), "schema: animus.plugin-scope.v1\nmode: all\n")
            .expect("write scope file");

        let scope = resolve_scope_for_project(temp.path());
        assert!(matches!(scope.mode, PluginScopeMode::All));
        assert!(scope.flavor_manifest_error.is_some(), "the error stays recorded for diagnostics");
        assert!(
            flavor_gating_error(&scope).is_none(),
            "an explicit mode override means the broken manifest does not gate the plugin universe"
        );
    }
}
