mod runner;

#[cfg(test)]
mod tests;

pub use runner::{PluginInstaller, PluginPreflightRunner};

use animus_plugin_protocol::{PLUGIN_KIND_PROVIDER, PLUGIN_KIND_SUBJECT_BACKEND};
use serde::{Deserialize, Serialize};

use crate::plugin_registry::{
    default_provider_repo_spec, default_subject_repo_for_kind, format_repo_spec,
    DEFAULT_CONFIG_SOURCE_PLUGINS, DEFAULT_QUEUE_PLUGINS,
    DEFAULT_WORKFLOW_RUNNER_PLUGINS,
};

/// Plugin-kind wire value for `workflow_runner`. Kept local because the
/// in-tree `animus-plugin-protocol` crate is still on protocol v1.0 and
/// does not export it; the v0.5 protocol crate defines it as the wire
/// literal.
const PLUGIN_KIND_WORKFLOW_RUNNER: &str = "workflow_runner";
/// Plugin-kind wire value for `queue`. See [`PLUGIN_KIND_WORKFLOW_RUNNER`].
const PLUGIN_KIND_QUEUE: &str = "queue";
/// Plugin-kind wire value for `config_source` (v0.6). See [`PLUGIN_KIND_QUEUE`].
const PLUGIN_KIND_CONFIG_SOURCE: &str = "config_source";

/// Minimum `workflow_runner` plugin version that consumes phase skill
/// payloads. The reference runner
/// (`launchapp-dev/animus-workflow-runner-default`) started enforcing the
/// skill payload contract (prompt / tool_policy / mcp_servers / model /
/// capabilities / ...) at v0.4.2 — see
/// `docs/architecture/skill-system.md`. A runner below this floor installs
/// and runs fine but silently ignores phase skills, so preflight surfaces
/// a non-fatal WARNING rather than failing.
pub const WORKFLOW_RUNNER_SKILL_FLOOR: &str = "0.4.2";

/// Build the under-pin preflight warning for a discovered `workflow_runner`
/// plugin whose manifest `version` is below [`WORKFLOW_RUNNER_SKILL_FLOOR`].
/// Returns `None` when the version meets the floor or cannot be parsed
/// (an unparseable version is not actionable as an under-pin warning and
/// must never fail preflight). The accepted `version` may carry a leading
/// `v` (e.g. `v0.4.1`).
pub fn workflow_runner_underpin_warning(name: &str, version: &str) -> Option<String> {
    let installed = parse_version_triple(version)?;
    let floor = parse_version_triple(WORKFLOW_RUNNER_SKILL_FLOOR)?;
    if installed < floor {
        // Normalize an already-`v`-prefixed manifest version so the message
        // reads `v0.4.1`, never `vv0.4.1`.
        let display_version = version.trim().trim_start_matches('v');
        Some(format!(
            "installed workflow runner {name} v{display_version} silently ignores phase skills \
             (needs v{WORKFLOW_RUNNER_SKILL_FLOOR}+); upgrade with `animus plugin update`"
        ))
    } else {
        None
    }
}

/// Minimum `queue` plugin version that implements precise-wake
/// (`queue/next_deadline`) for the event-driven daemon scheduler. A queue
/// plugin below this floor leases and completes work fine but cannot signal
/// precise cron/deferred wake deadlines, so reactive dispatch falls back to the
/// slower heartbeat — preflight surfaces a non-fatal WARNING rather than failing.
pub const QUEUE_PRECISE_WAKE_FLOOR: &str = "0.3.2";

/// Build the under-pin preflight warning for a discovered `queue` plugin whose
/// manifest `version` is below [`QUEUE_PRECISE_WAKE_FLOOR`]. Returns `None` when
/// the version meets the floor or cannot be parsed (an unparseable version is
/// not actionable and must never fail preflight). The accepted `version` may
/// carry a leading `v`.
pub fn queue_underpin_warning(name: &str, version: &str) -> Option<String> {
    let installed = parse_version_triple(version)?;
    let floor = parse_version_triple(QUEUE_PRECISE_WAKE_FLOOR)?;
    if installed < floor {
        let display_version = version.trim().trim_start_matches('v');
        Some(format!(
            "installed queue plugin {name} v{display_version} lacks precise-wake \
             (queue/next_deadline); reactive dispatch falls back to the heartbeat \
             (needs v{QUEUE_PRECISE_WAKE_FLOOR}+); upgrade with `animus plugin update`"
        ))
    } else {
        None
    }
}

/// Parse a `major.minor.patch` version (optionally `v`-prefixed, with any
/// pre-release/build suffix on the patch component ignored) into a
/// comparable tuple. Returns `None` for anything that does not start with
/// three dot-separated integers — an unparseable version is not a valid
/// under-pin signal.
fn parse_version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let trimmed = version.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch_raw = parts.next()?;
    // Drop a `-prerelease` / `+build` suffix on the patch component.
    let patch_digits: String = patch_raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    let patch = patch_digits.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// Default provider repo spec preflight should auto-install when
/// `at_least_one_provider` is unsatisfied. Resolved at call time from the
/// shared `plugin_registry` constants so version bumps land in one place.
pub fn default_provider_repo() -> String {
    default_provider_repo_spec()
}

/// Default backend repo spec for the `task` subject kind. Points at
/// `animus-subject-default`, NOT `animus-subject-linear` (Linear is a
/// third-party mirror that happens to claim `subject_kind:task`).
pub fn default_task_backend_repo() -> String {
    default_subject_repo_for_kind("task")
        .expect("task subject kind must have a curated default backend (animus-subject-default)")
}

/// Default backend repo spec for the `requirement` subject kind. Points at
/// `animus-subject-requirements`, the dedicated requirements backend.
pub fn default_requirement_backend_repo() -> String {
    default_subject_repo_for_kind("requirement")
        .expect("requirement subject kind must have a curated default backend (animus-subject-requirements)")
}

/// Default repo spec preflight should auto-install when the
/// `workflow_runner` role is unsatisfied.
pub fn default_workflow_runner_repo() -> String {
    let first =
        DEFAULT_WORKFLOW_RUNNER_PLUGINS.first().copied().expect("DEFAULT_WORKFLOW_RUNNER_PLUGINS must be non-empty");
    format_repo_spec(first)
}

/// Default repo spec preflight should auto-install when the `queue` role
/// is unsatisfied.
pub fn default_queue_repo() -> String {
    let first = DEFAULT_QUEUE_PLUGINS.first().copied().expect("DEFAULT_QUEUE_PLUGINS must be non-empty");
    format_repo_spec(first)
}

/// Default repo spec preflight should auto-install when the `config_source`
/// role is unsatisfied (v0.6).
pub fn default_config_source_repo() -> String {
    let first = DEFAULT_CONFIG_SOURCE_PLUGINS
        .first()
        .copied()
        .expect("DEFAULT_CONFIG_SOURCE_PLUGINS must be non-empty");
    format_repo_spec(first)
}

/// Compatibility shim: legacy string-typed export still pinned at the
/// historical Claude provider tag for any out-of-tree code that imported
/// `DEFAULT_PROVIDER_REPO` directly. New code should call
/// `default_provider_repo()` so version bumps to the curated registry
/// flow through automatically.
pub const DEFAULT_PROVIDER_REPO: &str = "launchapp-dev/animus-provider-claude@v0.2.1";
/// Compatibility shim — see `DEFAULT_PROVIDER_REPO`. Points at the
/// curated `animus-subject-default` backend (NOT the Linear mirror).
pub const DEFAULT_TASK_BACKEND_REPO: &str = "launchapp-dev/animus-subject-default@v0.1.1";
/// Compatibility shim — see `DEFAULT_PROVIDER_REPO`. Points at the
/// curated `animus-subject-requirements` backend.
pub const DEFAULT_REQUIREMENT_BACKEND_REPO: &str = "launchapp-dev/animus-subject-requirements@v0.1.6";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum RequiredRole {
    AtLeastOneProvider,
    SubjectKind(String),
    /// Satisfied by ANY installed subject_backend plugin, regardless of the
    /// kind(s) it serves. The daemon needs a place to read/write subjects, but
    /// it must not dictate WHICH kinds a deployment uses — a song app legitimately
    /// has no `task`/`requirement` backend. Prefer this over `SubjectKind(..)` in
    /// the daemon default so any valid subject backend satisfies preflight.
    AtLeastOneSubjectBackend,
    TransportEnabled,
    WorkflowRunner,
    Queue,
    /// Satisfied by ANY installed `config_source` plugin — the source of the
    /// workflow/agent config (YAML, Postgres, API). v0.6: kept OUT of the
    /// daemon default required-roles until the load path resolves it (else
    /// existing daemons fail preflight on upgrade).
    ConfigSource,
}

impl RequiredRole {
    pub fn label(&self) -> String {
        match self {
            RequiredRole::AtLeastOneProvider => "at_least_one_provider".to_string(),
            RequiredRole::SubjectKind(kind) => format!("subject_kind:{kind}"),
            RequiredRole::AtLeastOneSubjectBackend => "at_least_one_subject_backend".to_string(),
            RequiredRole::TransportEnabled => "transport_enabled".to_string(),
            RequiredRole::WorkflowRunner => "workflow_runner".to_string(),
            RequiredRole::Queue => "queue".to_string(),
            RequiredRole::ConfigSource => "config_source".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreflightSpec {
    pub required_roles: Vec<RequiredRole>,
    pub auto_install: bool,
    pub auto_install_defaults: Vec<(String, String)>,
}

impl PluginPreflightSpec {
    /// Default daemon preflight: provider + task/requirement subjects +
    /// workflow_runner + queue. The `notifier` role is intentionally NOT
    /// required — notifications are advisory; the daemon starts cleanly
    /// without an installed notifier plugin, and missing notifiers must
    /// not block daemon startup. See `DEFAULT_NOTIFIER_PLUGINS` in
    /// `plugin_registry` for the curated tag if an operator opts in via
    /// `animus plugin install`.
    pub fn daemon_default() -> Self {
        Self {
            required_roles: vec![
                RequiredRole::AtLeastOneProvider,
                // Require A valid subject backend, not specific kinds. The daemon
                // must not force `task`/`requirement` on every deployment.
                RequiredRole::AtLeastOneSubjectBackend,
                RequiredRole::WorkflowRunner,
                RequiredRole::Queue,
            ],
            auto_install: false,
            auto_install_defaults: vec![
                ("at_least_one_provider".to_string(), default_provider_repo()),
                // If NO subject backend is installed, the default is the task
                // backend — a sensible starter, not a requirement.
                ("at_least_one_subject_backend".to_string(), default_task_backend_repo()),
                ("workflow_runner".to_string(), default_workflow_runner_repo()),
                ("queue".to_string(), default_queue_repo()),
            ],
        }
    }

    pub fn with_auto_install(mut self) -> Self {
        self.auto_install = true;
        self
    }

    pub fn install_target_for(&self, role_label: &str) -> Option<&str> {
        self.auto_install_defaults.iter().find(|(label, _)| label == role_label).map(|(_, repo)| repo.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightResult {
    pub satisfied: Vec<String>,
    pub missing: Vec<MissingPlugin>,
    pub auto_installed: Vec<AutoInstalledPlugin>,
    /// Set when the project's flavor manifest exists on disk but failed to
    /// load while the plugin scope is in `flavor-only` mode. The scope then
    /// fails closed (empty admit set), so every required role reports as
    /// missing for a reason `animus plugin install` cannot fix. Callers
    /// resolve and attach this after [`PluginPreflightRunner::run`]; the
    /// runner itself never sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor_manifest_error: Option<String>,
    /// Non-fatal advisories that never gate daemon startup. Today the only
    /// source is an under-pinned `workflow_runner` plugin whose manifest
    /// version is below [`WORKFLOW_RUNNER_SKILL_FLOOR`] — such a runner
    /// silently ignores phase skill payloads. Callers attach these after
    /// [`PluginPreflightRunner::run`]; the runner itself never sets them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl PreflightResult {
    pub fn is_ok(&self) -> bool {
        // Warnings are advisory and never affect the OK verdict.
        self.missing.is_empty() && self.flavor_manifest_error.is_none()
    }

    pub fn render_missing_message(&self) -> String {
        if self.is_ok() {
            return String::new();
        }
        let mut out = String::new();
        if let Some(reason) = &self.flavor_manifest_error {
            // List missing roles WITHOUT their install fix commands:
            // rediscovery keeps filtering those plugins out until the
            // manifest is fixed, so install advice cannot remediate.
            out.push_str(&format!("plugin preflight failed: {reason}\n"));
            out.push_str(
                "The flavor-only plugin scope admits NO plugins until the manifest is fixed, so every \
                 required role below reports as missing. Fix (or delete) the manifest instead of \
                 installing plugins.\n",
            );
            for missing in &self.missing {
                out.push_str(&format!("  - role `{}` unsatisfied\n", missing.role));
            }
            return out;
        }
        out.push_str("plugin preflight failed: the daemon requires plugins that are not installed.\n");
        for missing in &self.missing {
            out.push_str(&format!("  - role `{}` unsatisfied; fix: `{}`\n", missing.role, missing.fix_command));
        }
        if self.missing.len() > 1 {
            // Composed fix: the default flavor's REQUIRED set covers every
            // daemon-preflight role, so one manifest-driven install
            // resolves all of the above instead of N per-role commands.
            out.push_str(
                "Fix all missing roles with one command: `animus plugin install-defaults --flavor default --yes`\n",
            );
        }
        out.push_str(
            "Re-run with `--auto-install` to install defaults, or run `animus plugin install <repo>@<tag>` manually.\n",
        );
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPlugin {
    pub role: String,
    pub fix_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoInstalledPlugin {
    pub role: String,
    pub repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstalledPluginSummary {
    pub name: String,
    pub plugin_kind: String,
    pub subject_kinds: Vec<String>,
}

impl InstalledPluginSummary {
    pub fn is_provider(&self) -> bool {
        self.plugin_kind == PLUGIN_KIND_PROVIDER
    }

    pub fn is_subject_backend(&self) -> bool {
        self.plugin_kind == PLUGIN_KIND_SUBJECT_BACKEND
    }

    pub fn is_workflow_runner(&self) -> bool {
        self.plugin_kind == PLUGIN_KIND_WORKFLOW_RUNNER
    }

    pub fn is_queue(&self) -> bool {
        self.plugin_kind == PLUGIN_KIND_QUEUE
    }

    pub fn is_config_source(&self) -> bool {
        self.plugin_kind == PLUGIN_KIND_CONFIG_SOURCE
    }

    pub fn covers_subject_kind(&self, kind: &str) -> bool {
        self.is_subject_backend() && self.subject_kinds.iter().any(|k| k == kind)
    }
}

pub fn summarize_discovered_plugins(
    plugins: &[orchestrator_plugin_host::DiscoveredPlugin],
) -> Vec<InstalledPluginSummary> {
    summarize_discovered_plugins_with_lock(plugins, None)
}

/// Like [`summarize_discovered_plugins`] but, when supplied a v0.5.7+
/// [`orchestrator_plugin_host::PluginLockfile`], translates the
/// manifest-declared `subject_kind:*` capabilities into the user-facing
/// `installed_kind` recorded at install time. Preflight then reports
/// satisfaction against the kind operators actually dispatch against
/// (`archive`) rather than the plugin's native kind (`task`). When no
/// lockfile is supplied or the lockfile carries no rename, the summary
/// is identical to the pre-v0.5.7 capability-string view.
pub fn summarize_discovered_plugins_with_lock(
    plugins: &[orchestrator_plugin_host::DiscoveredPlugin],
    lockfile: Option<&orchestrator_plugin_host::PluginLockfile>,
) -> Vec<InstalledPluginSummary> {
    plugins
        .iter()
        .map(|plugin| {
            let lock_entry = lockfile.and_then(|lock| lock.find(&plugin.name));
            let native_to_installed: Option<(String, String)> =
                lock_entry.and_then(|entry| match (entry.effective_installed_kind(), entry.effective_native_kind()) {
                    (Some(installed), Some(native)) if installed != native => {
                        Some((native.to_string(), installed.to_string()))
                    }
                    _ => None,
                });
            let subject_kinds = plugin
                .manifest
                .capabilities
                .iter()
                .filter_map(|cap| cap.strip_prefix("subject_kind:").map(|rest| rest.trim().to_string()))
                .filter(|k| !k.is_empty())
                .map(|k| match &native_to_installed {
                    Some((native, installed)) if k == *native => installed.clone(),
                    _ => k,
                })
                .collect::<Vec<_>>();
            InstalledPluginSummary {
                name: plugin.name.clone(),
                plugin_kind: plugin.manifest.plugin_kind.clone(),
                subject_kinds,
            }
        })
        .collect()
}
