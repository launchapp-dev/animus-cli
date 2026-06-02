mod runner;

#[cfg(test)]
mod tests;

pub use runner::{PluginInstaller, PluginPreflightRunner};

use animus_plugin_protocol::{PLUGIN_KIND_PROVIDER, PLUGIN_KIND_SUBJECT_BACKEND};
use serde::{Deserialize, Serialize};

use crate::plugin_registry::{
    default_provider_repo_spec, default_subject_repo_for_kind, format_repo_spec, DEFAULT_QUEUE_PLUGINS,
    DEFAULT_WORKFLOW_RUNNER_PLUGINS,
};

/// Plugin-kind wire value for `workflow_runner`. Kept local because the
/// in-tree `animus-plugin-protocol` crate is still on protocol v1.0 and
/// does not export it; the v0.5 protocol crate defines it as the wire
/// literal.
const PLUGIN_KIND_WORKFLOW_RUNNER: &str = "workflow_runner";
/// Plugin-kind wire value for `queue`. See [`PLUGIN_KIND_WORKFLOW_RUNNER`].
const PLUGIN_KIND_QUEUE: &str = "queue";

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
    TransportEnabled,
    WorkflowRunner,
    Queue,
}

impl RequiredRole {
    pub fn label(&self) -> String {
        match self {
            RequiredRole::AtLeastOneProvider => "at_least_one_provider".to_string(),
            RequiredRole::SubjectKind(kind) => format!("subject_kind:{kind}"),
            RequiredRole::TransportEnabled => "transport_enabled".to_string(),
            RequiredRole::WorkflowRunner => "workflow_runner".to_string(),
            RequiredRole::Queue => "queue".to_string(),
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
    pub fn daemon_default() -> Self {
        Self {
            required_roles: vec![
                RequiredRole::AtLeastOneProvider,
                RequiredRole::SubjectKind("task".to_string()),
                RequiredRole::SubjectKind("requirement".to_string()),
                RequiredRole::WorkflowRunner,
                RequiredRole::Queue,
            ],
            auto_install: false,
            auto_install_defaults: vec![
                ("at_least_one_provider".to_string(), default_provider_repo()),
                ("subject_kind:task".to_string(), default_task_backend_repo()),
                ("subject_kind:requirement".to_string(), default_requirement_backend_repo()),
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
}

impl PreflightResult {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty()
    }

    pub fn render_missing_message(&self) -> String {
        if self.missing.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str("plugin preflight failed: the daemon requires plugins that are not installed.\n");
        for missing in &self.missing {
            out.push_str(&format!("  - role `{}` unsatisfied; fix: `{}`\n", missing.role, missing.fix_command));
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

    pub fn covers_subject_kind(&self, kind: &str) -> bool {
        self.is_subject_backend() && self.subject_kinds.iter().any(|k| k == kind)
    }
}

pub fn summarize_discovered_plugins(
    plugins: &[orchestrator_plugin_host::DiscoveredPlugin],
) -> Vec<InstalledPluginSummary> {
    plugins
        .iter()
        .map(|plugin| {
            let subject_kinds = plugin
                .manifest
                .capabilities
                .iter()
                .filter_map(|cap| cap.strip_prefix("subject_kind:").map(|rest| rest.trim().to_string()))
                .filter(|k| !k.is_empty())
                .collect::<Vec<_>>();
            InstalledPluginSummary {
                name: plugin.name.clone(),
                plugin_kind: plugin.manifest.plugin_kind.clone(),
                subject_kinds,
            }
        })
        .collect()
}
