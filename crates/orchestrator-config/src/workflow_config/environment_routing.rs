//! Environment routing resolver.
//!
//! Given a unit of work (its subject kind + harness) plus the phase/workflow
//! `environment:` overrides and the config-level [`EnvironmentRouting`] table,
//! decide WHICH environment plugin id (if any) should materialize the workspace
//! and run the harness. This is a pure function over compiled-config types — no
//! IO, no plugin RPC. The runtime `EnvironmentClient` (prepare/exec/teardown)
//! is a follow-on; this resolver only picks the id.
//!
//! Precedence (highest first):
//! 1. `phase_env` — an explicit phase-level `environment:` override.
//! 2. The first matching `routing.rules` entry (first-match-wins). A rule
//!    matches when its `match.kind` is unset-or-equals `subject_kind` AND its
//!    `match.harness` is unset-or-equals `harness`.
//! 3. `workflow_env` — a workflow-level `environment:` override.
//! 4. `routing.default` — the config-level fallback environment.
//! 5. `None` — no explicit environment; the runner falls back to its built-in
//!    local behavior.

use animus_config_protocol::workflow_types::EnvironmentRouting;

/// Resolve the environment plugin id for a unit of work.
///
/// See the module docs for the full precedence table. Returns the environment
/// plugin id to route to, or `None` when nothing selects one (the runner then
/// uses its built-in local behavior).
pub fn resolve_environment(
    subject_kind: Option<&str>,
    harness: Option<&str>,
    phase_env: Option<&str>,
    workflow_env: Option<&str>,
    routing: Option<&EnvironmentRouting>,
) -> Option<String> {
    // 1. Phase-level override always wins.
    if let Some(env) = phase_env {
        return Some(env.to_string());
    }

    // 2. First matching routing rule (first-match-wins).
    if let Some(routing) = routing {
        for rule in &routing.rules {
            let kind_ok = match &rule.match_on.kind {
                Some(k) => subject_kind == Some(k.as_str()),
                None => true,
            };
            let harness_ok = match &rule.match_on.harness {
                Some(h) => harness == Some(h.as_str()),
                None => true,
            };
            if kind_ok && harness_ok {
                return Some(rule.environment.clone());
            }
        }
    }

    // 3. Workflow-level override.
    if let Some(env) = workflow_env {
        return Some(env.to_string());
    }

    // 4. Config-level default.
    if let Some(routing) = routing {
        if let Some(default) = &routing.default {
            return Some(default.clone());
        }
    }

    // 5. Nothing selected.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_config_protocol::workflow_types::{EnvironmentMatch, EnvironmentRule};

    fn rule(kind: Option<&str>, harness: Option<&str>, env: &str) -> EnvironmentRule {
        EnvironmentRule {
            match_on: EnvironmentMatch { kind: kind.map(str::to_string), harness: harness.map(str::to_string) },
            environment: env.to_string(),
            spec: None,
        }
    }

    #[test]
    fn phase_override_beats_everything() {
        let routing = EnvironmentRouting {
            default: Some("default-env".to_string()),
            rules: vec![rule(Some("task"), None, "rule-env")],
        };
        let got =
            resolve_environment(Some("task"), Some("claude"), Some("phase-env"), Some("workflow-env"), Some(&routing));
        assert_eq!(got.as_deref(), Some("phase-env"));
    }

    #[test]
    fn kind_only_rule_matches() {
        let routing = EnvironmentRouting { default: None, rules: vec![rule(Some("task"), None, "task-env")] };
        assert_eq!(
            resolve_environment(Some("task"), Some("claude"), None, None, Some(&routing)).as_deref(),
            Some("task-env")
        );
        // Different kind -> no match -> None (no default).
        assert_eq!(resolve_environment(Some("requirement"), Some("claude"), None, None, Some(&routing)), None);
    }

    #[test]
    fn harness_only_rule_matches() {
        let routing = EnvironmentRouting { default: None, rules: vec![rule(None, Some("codex"), "codex-env")] };
        // Any kind, harness=codex -> match.
        assert_eq!(
            resolve_environment(Some("task"), Some("codex"), None, None, Some(&routing)).as_deref(),
            Some("codex-env")
        );
        // Wrong harness -> no match.
        assert_eq!(resolve_environment(Some("task"), Some("claude"), None, None, Some(&routing)), None);
    }

    #[test]
    fn kind_and_harness_rule_matches_only_both() {
        let routing =
            EnvironmentRouting { default: None, rules: vec![rule(Some("task"), Some("claude"), "task-claude-env")] };
        assert_eq!(
            resolve_environment(Some("task"), Some("claude"), None, None, Some(&routing)).as_deref(),
            Some("task-claude-env")
        );
        // Right kind, wrong harness.
        assert_eq!(resolve_environment(Some("task"), Some("codex"), None, None, Some(&routing)), None);
        // Wrong kind, right harness.
        assert_eq!(resolve_environment(Some("requirement"), Some("claude"), None, None, Some(&routing)), None);
    }

    #[test]
    fn first_matching_rule_wins() {
        let routing = EnvironmentRouting {
            default: Some("default-env".to_string()),
            rules: vec![rule(Some("task"), Some("claude"), "specific-env"), rule(Some("task"), None, "broad-env")],
        };
        // Both rules match task+claude; first one wins.
        assert_eq!(
            resolve_environment(Some("task"), Some("claude"), None, None, Some(&routing)).as_deref(),
            Some("specific-env")
        );
        // Only the broad rule matches task+codex.
        assert_eq!(
            resolve_environment(Some("task"), Some("codex"), None, None, Some(&routing)).as_deref(),
            Some("broad-env")
        );
    }

    #[test]
    fn workflow_env_beats_default_but_not_rule() {
        let routing = EnvironmentRouting {
            default: Some("default-env".to_string()),
            rules: vec![rule(Some("task"), None, "rule-env")],
        };
        // No rule matches (requirement) -> workflow_env wins over default.
        assert_eq!(
            resolve_environment(Some("requirement"), None, None, Some("workflow-env"), Some(&routing)).as_deref(),
            Some("workflow-env")
        );
        // Rule matches (task) -> rule beats workflow_env.
        assert_eq!(
            resolve_environment(Some("task"), None, None, Some("workflow-env"), Some(&routing)).as_deref(),
            Some("rule-env")
        );
    }

    #[test]
    fn default_fallback_when_no_rule_and_no_overrides() {
        let routing = EnvironmentRouting {
            default: Some("default-env".to_string()),
            rules: vec![rule(Some("task"), None, "task-env")],
        };
        assert_eq!(
            resolve_environment(Some("requirement"), Some("claude"), None, None, Some(&routing)).as_deref(),
            Some("default-env")
        );
    }

    #[test]
    fn no_routing_no_overrides_returns_none() {
        assert_eq!(resolve_environment(Some("task"), Some("claude"), None, None, None), None);
    }

    #[test]
    fn empty_match_rule_is_catch_all() {
        let routing = EnvironmentRouting { default: None, rules: vec![rule(None, None, "catch-all-env")] };
        assert_eq!(resolve_environment(None, None, None, None, Some(&routing)).as_deref(), Some("catch-all-env"));
        assert_eq!(
            resolve_environment(Some("anything"), Some("whatever"), None, None, Some(&routing)).as_deref(),
            Some("catch-all-env")
        );
    }

    #[test]
    fn workflow_env_only_no_routing() {
        assert_eq!(
            resolve_environment(Some("task"), None, None, Some("workflow-env"), None).as_deref(),
            Some("workflow-env")
        );
    }
}
