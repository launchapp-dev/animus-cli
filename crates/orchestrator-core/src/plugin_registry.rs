//! Single source of truth for default plugin repo/tag pairs.
//!
//! Both `animus plugin install-defaults` (in `orchestrator-cli`) and the
//! daemon plugin preflight (`plugin_preflight::PluginPreflightSpec`) must
//! resolve the same `(owner/repo, tag)` for each role. Before this module
//! existed, preflight referenced `animus-provider-claude@v0.1.0` while
//! install-defaults shipped `v0.2.x`, and preflight mapped both `task` and
//! `requirement` subject kinds to `animus-subject-linear` even though the
//! correct backends are `animus-subject-default` and
//! `animus-subject-requirements`. Drift like that produced fix commands that
//! installed the wrong plugin.
//!
//! Bump the constants here when the curated launchapp-dev releases ship a
//! new tag and the workspace agrees to roll the floor.

/// Curated provider plugins installed by default. The order matters: the
/// first entry is the one preflight points users at for
/// `at_least_one_provider`.
pub const DEFAULT_PROVIDER_PLUGINS: &[(&str, &str)] = &[
    ("launchapp-dev/animus-provider-claude", "v0.2.8"),
    ("launchapp-dev/animus-provider-codex", "v0.2.3"),
    ("launchapp-dev/animus-provider-gemini", "v0.2.6"),
    ("launchapp-dev/animus-provider-opencode", "v0.2.6"),
    ("launchapp-dev/animus-provider-oai", "v0.2.2"),
];

/// v0.5 workflow_runner + queue plugins installed as part of the default
/// flavor. Required for `animus daemon start` once the plugin-only path
/// is the default (deletion gate in Wave 3 "Out of scope").
pub const DEFAULT_WORKFLOW_RUNNER_PLUGINS: &[(&str, &str)] =
    &[("launchapp-dev/animus-workflow-runner-default", "v0.4.8")];

/// v0.5 queue plugin. See [`DEFAULT_WORKFLOW_RUNNER_PLUGINS`].
pub const DEFAULT_QUEUE_PLUGINS: &[(&str, &str)] = &[("launchapp-dev/animus-queue-default", "v0.3.3")];

/// v0.6 config-source plugin (the default YAML source for the `config_source`
/// plugin role). Published as `launchapp-dev/animus-config-yaml` v0.1.0 in the
/// v0.6.1 release; the daemon requires a `config_source` to boot.
pub const DEFAULT_CONFIG_SOURCE_PLUGINS: &[(&str, &str)] = &[("launchapp-dev/animus-config-yaml", "v0.1.0")];

/// v0.5 durable-store plugin (DBOS Option A). The `animus-step-durable-dbos`
/// repository has not shipped a tagged release yet; until it does this list
/// stays empty so `animus plugin install-defaults --include-durable-stores`
/// no-ops cleanly instead of failing the install with a 404.
pub const DEFAULT_DURABLE_STORE_PLUGINS: &[(&str, &str)] = &[];

/// v0.5 memory-store plugin (Zep Cloud). Same shape as
/// [`DEFAULT_DURABLE_STORE_PLUGINS`] — the `animus-memory-zep` repo has not
/// shipped a tagged release yet; empty until it does.
pub const DEFAULT_MEMORY_STORE_PLUGINS: &[(&str, &str)] = &[];

/// v0.5.3 notifier plugin (HTTP + Slack webhook). Optional role; the
/// daemon starts cleanly without a notifier installed. Listed here so
/// `animus plugin install-defaults --include-notifiers` (and any future
/// auto-install pass) can resolve a curated tag.
pub const DEFAULT_NOTIFIER_PLUGINS: &[(&str, &str)] = &[("launchapp-dev/animus-notifier-http", "v0.1.0")];

/// Optional add-on opted in by `--include-oai-agent`.
pub const DEFAULT_OAI_AGENT_PLUGINS: &[(&str, &str)] = &[("launchapp-dev/animus-provider-oai-agent", "v0.1.5")];

/// Subject backends installed by `--include-subjects`. Indexed lookups
/// below (`default_subject_repo_for_kind`) assume each entry is listed
/// here so preflight and install-defaults stay in lockstep.
pub const DEFAULT_SUBJECT_PLUGINS: &[(&str, &str)] = &[
    ("launchapp-dev/animus-subject-default", "v0.1.4"),
    ("launchapp-dev/animus-subject-requirements", "v0.1.7"),
    ("launchapp-dev/animus-subject-linear", "v0.1.5"),
    ("launchapp-dev/animus-subject-sqlite", "v0.1.4"),
    ("launchapp-dev/animus-subject-markdown", "v0.1.4"),
];

/// Transport + web UI plugins installed by `--include-transports`.
pub const DEFAULT_TRANSPORT_PLUGINS: &[(&str, &str)] = &[
    ("launchapp-dev/animus-transport-http", "v0.2.1"),
    ("launchapp-dev/animus-transport-graphql", "v0.2.3"),
    ("launchapp-dev/animus-web-ui", "v0.1.0"),
];

/// Format a registry entry into the `owner/repo@tag` spec accepted by
/// `animus plugin install`.
pub fn format_repo_spec(entry: (&str, &str)) -> String {
    format!("{}@{}", entry.0, entry.1)
}

/// Look up the curated pin for a plugin slug across every default table.
/// Returns `None` for slugs the curated registry does not yet pin (e.g.
/// the v0.5 `default` flavor lists `animus-provider-ollama` and the
/// v0.5 trigger plugins ahead of their first curated release). Callers
/// should treat `None` as "skip with a warning, don't fail".
pub fn resolve_tag_for_slug(slug: &str) -> Option<&'static str> {
    for table in [
        DEFAULT_PROVIDER_PLUGINS,
        DEFAULT_OAI_AGENT_PLUGINS,
        DEFAULT_SUBJECT_PLUGINS,
        DEFAULT_TRANSPORT_PLUGINS,
        DEFAULT_WORKFLOW_RUNNER_PLUGINS,
        DEFAULT_QUEUE_PLUGINS,
        DEFAULT_CONFIG_SOURCE_PLUGINS,
        DEFAULT_DURABLE_STORE_PLUGINS,
        DEFAULT_MEMORY_STORE_PLUGINS,
        DEFAULT_NOTIFIER_PLUGINS,
    ] {
        if let Some((_, tag)) = table.iter().find(|(s, _)| *s == slug) {
            return Some(*tag);
        }
    }
    None
}

/// Resolve a curated `(owner/repo, tag)` pin from a bare plugin slug (the repo
/// basename, e.g. `animus-provider-claude`). Used by `animus install` /
/// `animus add` so a manifest's short-form dependency (`animus-provider-claude
/// = ">=0.2.7"`) resolves to the full `launchapp-dev/animus-provider-claude`
/// repo and its curated tag. Returns `None` for slugs no curated table pins —
/// the caller then requires an explicit `{ git = "OWNER/REPO", tag = "..." }`.
pub fn resolve_curated_plugin_by_basename(name: &str) -> Option<(&'static str, &'static str)> {
    for table in [
        DEFAULT_PROVIDER_PLUGINS,
        DEFAULT_OAI_AGENT_PLUGINS,
        DEFAULT_SUBJECT_PLUGINS,
        DEFAULT_TRANSPORT_PLUGINS,
        DEFAULT_WORKFLOW_RUNNER_PLUGINS,
        DEFAULT_QUEUE_PLUGINS,
        DEFAULT_CONFIG_SOURCE_PLUGINS,
        DEFAULT_DURABLE_STORE_PLUGINS,
        DEFAULT_MEMORY_STORE_PLUGINS,
        DEFAULT_NOTIFIER_PLUGINS,
    ] {
        if let Some((slug, tag)) = table.iter().find(|(slug, _)| slug.rsplit('/').next() == Some(name)) {
            return Some((*slug, *tag));
        }
    }
    None
}

/// Repo+tag spec for the default provider that preflight should install
/// when `at_least_one_provider` is unsatisfied.
pub fn default_provider_repo_spec() -> String {
    let first = DEFAULT_PROVIDER_PLUGINS.first().copied().expect("DEFAULT_PROVIDER_PLUGINS must be non-empty");
    format_repo_spec(first)
}

/// Resolve the correct curated subject backend for a given `subject_kind`.
/// Returns `None` when no curated backend claims the kind, in which case
/// the daemon falls back to whichever third-party plugin the user installs.
pub fn default_subject_repo_for_kind(kind: &str) -> Option<String> {
    let basename = match kind {
        // Task workflows use the in-tree `animus-subject-default` backend,
        // which is the curated drop-in replacement for the removed in-tree
        // `InTreeTaskSubjectBackend`.
        "task" => "animus-subject-default",
        // Requirement workflows use the dedicated requirements backend,
        // NOT animus-subject-linear (that is a Linear-issue mirror that
        // happens to claim `subject_kind:task`).
        "requirement" => "animus-subject-requirements",
        _ => return None,
    };
    DEFAULT_SUBJECT_PLUGINS
        .iter()
        .find(|(slug, _)| slug.rsplit('/').next() == Some(basename))
        .copied()
        .map(format_repo_spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_default_is_claude_at_curated_tag() {
        let spec = default_provider_repo_spec();
        assert!(
            spec.starts_with("launchapp-dev/animus-provider-claude@"),
            "default provider must be claude, got: {spec}"
        );
        assert!(spec.contains("@v0."), "default provider spec must pin a tag, got: {spec}");
    }

    #[test]
    fn subject_kind_task_resolves_to_subject_default() {
        let spec = default_subject_repo_for_kind("task").expect("task kind must resolve");
        assert!(
            spec.starts_with("launchapp-dev/animus-subject-default@"),
            "task subject must be animus-subject-default (NOT animus-subject-linear), got: {spec}"
        );
    }

    #[test]
    fn subject_kind_requirement_resolves_to_subject_requirements() {
        let spec = default_subject_repo_for_kind("requirement").expect("requirement kind must resolve");
        assert!(
            spec.starts_with("launchapp-dev/animus-subject-requirements@"),
            "requirement subject must be animus-subject-requirements (NOT animus-subject-linear), got: {spec}"
        );
    }

    #[test]
    fn unknown_subject_kind_returns_none() {
        assert!(default_subject_repo_for_kind("nonsense").is_none());
    }

    #[test]
    fn all_subject_plugins_have_non_empty_tags() {
        for (slug, tag) in DEFAULT_SUBJECT_PLUGINS {
            assert!(!tag.is_empty(), "tag for {slug} must be non-empty");
            assert!(tag.starts_with('v'), "tag for {slug} must look like v0.x.y, got {tag}");
        }
    }

    #[test]
    fn all_provider_plugins_have_non_empty_tags() {
        for (slug, tag) in DEFAULT_PROVIDER_PLUGINS {
            assert!(!tag.is_empty(), "tag for {slug} must be non-empty");
            assert!(tag.starts_with('v'), "tag for {slug} must look like v0.x.y, got {tag}");
        }
    }
}
