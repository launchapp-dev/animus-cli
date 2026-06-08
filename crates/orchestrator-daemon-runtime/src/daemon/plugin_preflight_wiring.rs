use std::path::Path;

use anyhow::Result;
use orchestrator_core::{
    summarize_discovered_plugins_with_lock, InstalledPluginSummary, PluginInstaller, PluginPreflightRunner,
    PluginPreflightSpec, PreflightResult,
};
use orchestrator_plugin_host::{discover_plugins, PluginLockfile};

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

    let installed = match discover_installed_plugins(project_root) {
        Ok(plugins) => plugins,
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
    let result = PluginPreflightRunner::run(&spec, installed, installer).await?;

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
    let root = Path::new(project_root);
    let plugins = discover_plugins(root)?;
    // v0.5.7: consult the project's plugins.lock so renamed entries
    // (installed_kind != native_kind) report against the kind operators
    // actually dispatch against. Missing/unreadable lockfile is non-fatal
    // (matches pre-v0.5.7 behavior).
    let lockfile = PluginLockfile::load_default(Some(root)).ok();
    Ok(summarize_discovered_plugins_with_lock(&plugins, lockfile.as_ref()))
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
