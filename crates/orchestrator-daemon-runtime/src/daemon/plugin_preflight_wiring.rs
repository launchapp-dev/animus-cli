use std::path::{Path, PathBuf};

use anyhow::Result;
use orchestrator_core::flavor::{load_flavor_in, locate_flavor_manifest_in, DEFAULT_FLAVOR_ID};
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
    // `load_flavor_in` falls back to the binary-bundled default flavor
    // when no on-disk manifest is present. For the scope-default
    // decision we must distinguish "project actually opted into a
    // flavor" from "binary-bundled fallback" — otherwise every project
    // silently defaults to `flavor-only` mode the moment Animus is
    // installed. `locate_flavor_manifest_in` returns None when the
    // operator has not written `flavors/<id>.toml`, so we drive the
    // presence flag from that.
    let Some(manifest_path) = locate_flavor_manifest_in(project_root, DEFAULT_FLAVOR_ID) else {
        return (BTreeSet::new(), false, None);
    };
    match load_flavor_in(project_root, DEFAULT_FLAVOR_ID) {
        Ok(Some(manifest)) => {
            // Include `recommended` plugins as well: the v0.5 default
            // flavor lists e.g. `animus-subject-requirements` under
            // `subjects.recommended`, but daemon preflight still
            // requires the `subject_kind:requirement` role to be
            // covered. A strict `required`-only admit set would filter
            // the installed recommended backend out of scoped discovery
            // and leave preflight reporting the role as missing despite
            // a working install. Recommended plugins are part of the
            // flavor's curated bundle (see `animus plugin install-defaults
            // --include-subjects`), so admitting them is the right
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
