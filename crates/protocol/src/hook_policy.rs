//! Compiled harness hook policy: schema, loader, and evaluator.
//!
//! This is the kernel side of the harness-hook guardrail spine. The kernel
//! expresses guardrail intent and compiles it into a versioned policy file
//! (`hook-policy.v1.json`); the `animus-hook` sibling binary evaluates that
//! file synchronously against `{tool_name, tool_input}` for gate events
//! (`PreToolUse` / `PermissionRequest`) and prints the provider hook decision
//! JSON. Per-provider activation wiring (e.g. injecting Claude `--settings`
//! hooks config) lives out-of-tree at the plugin surface — this module is
//! provider-agnostic.
//!
//! Evaluation semantics are deliberately order-independent and fail-safe:
//! every matching rule is collected and the most restrictive decision wins
//! (`deny` > `ask` > `allow` > `defer`). A rule ordering mistake can
//! therefore never bypass a deny. When no rule matches, `default_decision`
//! applies (default `defer`, i.e. abstain — the harness falls through to its
//! normal permission flow).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Current compiled policy schema version.
pub const HOOK_POLICY_VERSION: u32 = 1;

/// File name of the compiled policy under the scoped config dir.
pub const HOOK_POLICY_FILE_NAME: &str = "hook-policy.v1.json";

/// A policy decision for a gated tool call.
///
/// Ordered by restrictiveness: `Deny` > `Ask` > `Allow` > `Defer`. `Defer`
/// means abstain — emit no opinion and let the harness's normal permission
/// flow decide. It does NOT suspend or queue the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PolicyDecision {
    #[default]
    Defer,
    Allow,
    Ask,
    Deny,
}

impl PolicyDecision {
    /// Stable lowercase wire name (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyDecision::Defer => "defer",
            PolicyDecision::Allow => "allow",
            PolicyDecision::Ask => "ask",
            PolicyDecision::Deny => "deny",
        }
    }
}

/// A regex matcher against one field of the tool input.
///
/// `field` is a dot path into the `tool_input` JSON object (e.g. `command`,
/// `file_path`, `options.cwd`). String values are matched directly; other
/// scalar values are matched against their JSON rendering. A missing field
/// never matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMatcher {
    pub field: String,
    pub regex: String,
}

/// One compiled guardrail rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookPolicyRule {
    /// Stable identifier surfaced in decision reasons and the event log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Gate events this rule applies to (`PreToolUse`, `PermissionRequest`).
    /// Empty means all gate events.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    /// Glob patterns matched against `tool_name` (`*` wildcard). Empty means
    /// every tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// All matchers must match for the rule to apply (AND semantics).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_matchers: Vec<InputMatcher>,
    pub decision: PolicyDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The compiled, versioned policy file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookPolicy {
    pub version: u32,
    /// Decision when no rule matches. Defaults to `defer` (abstain).
    #[serde(default)]
    pub default_decision: PolicyDecision,
    #[serde(default)]
    pub rules: Vec<HookPolicyRule>,
}

/// Outcome of evaluating a policy against one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyVerdict {
    pub decision: PolicyDecision,
    /// Human-readable reason for the decision (always present for non-defer
    /// verdicts so the harness can surface why a call was gated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `id` of the winning rule, when a rule produced the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
}

/// Policy load / validation failures.
#[derive(Debug, thiserror::Error)]
pub enum HookPolicyError {
    #[error("failed to read hook policy at {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse hook policy at {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported hook policy version {found} at {path} (supported: {HOOK_POLICY_VERSION})")]
    Version { path: String, found: u32 },
    #[error("invalid regex {pattern:?} in hook policy rule {rule}: {message}")]
    InvalidRegex { rule: String, pattern: String, message: String },
}

impl HookPolicy {
    /// Load and validate a compiled policy file. Validation compiles every
    /// matcher regex up front so evaluation cannot fail later.
    pub fn load(path: &Path) -> Result<Self, HookPolicyError> {
        let display = path.display().to_string();
        let raw =
            std::fs::read_to_string(path).map_err(|source| HookPolicyError::Read { path: display.clone(), source })?;
        let policy: HookPolicy =
            serde_json::from_str(&raw).map_err(|source| HookPolicyError::Parse { path: display.clone(), source })?;
        policy.validate(&display)?;
        Ok(policy)
    }

    /// Validate the schema version and matcher regexes.
    pub fn validate(&self, path: &str) -> Result<(), HookPolicyError> {
        if self.version != HOOK_POLICY_VERSION {
            return Err(HookPolicyError::Version { path: path.to_string(), found: self.version });
        }
        for (index, rule) in self.rules.iter().enumerate() {
            for matcher in &rule.input_matchers {
                if let Err(err) = regex::Regex::new(&matcher.regex) {
                    return Err(HookPolicyError::InvalidRegex {
                        rule: rule.id.clone().unwrap_or_else(|| format!("#{index}")),
                        pattern: matcher.regex.clone(),
                        message: err.to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Evaluate the policy for one gate event + tool call.
    ///
    /// All matching rules are collected and the most restrictive decision
    /// wins (`deny` > `ask` > `allow`); the first rule (in file order) at the
    /// winning restrictiveness supplies the reason. Rules whose decision is
    /// `defer` never outrank the default. When nothing matches, the policy's
    /// `default_decision` is returned.
    pub fn evaluate(&self, event: &str, tool_name: &str, tool_input: &serde_json::Value) -> PolicyVerdict {
        let mut winner: Option<&HookPolicyRule> = None;
        for rule in &self.rules {
            if !rule_matches(rule, event, tool_name, tool_input) {
                continue;
            }
            if winner.map(|current| rule.decision > current.decision).unwrap_or(true) {
                winner = Some(rule);
            }
        }
        match winner {
            Some(rule) if rule.decision > PolicyDecision::Defer => PolicyVerdict {
                decision: rule.decision,
                reason: Some(rule.reason.clone().unwrap_or_else(|| {
                    format!(
                        "animus hook policy rule {} matched tool {tool_name}",
                        rule.id.as_deref().unwrap_or("<unnamed>")
                    )
                })),
                rule_id: rule.id.clone(),
            },
            _ => PolicyVerdict {
                decision: self.default_decision,
                reason: (self.default_decision > PolicyDecision::Defer)
                    .then(|| format!("animus hook policy default decision ({})", self.default_decision.as_str())),
                rule_id: None,
            },
        }
    }
}

fn rule_matches(rule: &HookPolicyRule, event: &str, tool_name: &str, tool_input: &serde_json::Value) -> bool {
    if !rule.events.is_empty() && !rule.events.iter().any(|e| e == event) {
        return false;
    }
    if !rule.tools.is_empty() && !rule.tools.iter().any(|pattern| glob_matches(pattern, tool_name)) {
        return false;
    }
    rule.input_matchers.iter().all(|matcher| input_matcher_matches(matcher, tool_input))
}

fn input_matcher_matches(matcher: &InputMatcher, tool_input: &serde_json::Value) -> bool {
    let Some(value) = lookup_field(tool_input, &matcher.field) else {
        return false;
    };
    let haystack = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // Validation compiled this regex already; an invalid pattern here would
    // mean the file changed between load and evaluate, so fail safe (no match
    // is the conservative outcome only for allow rules — but load() makes
    // this unreachable in practice).
    regex::Regex::new(&matcher.regex).map(|re| re.is_match(&haystack)).unwrap_or(false)
}

fn lookup_field<'a>(value: &'a serde_json::Value, dot_path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in dot_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

/// Simple `*`-wildcard glob match (no other metacharacters).
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((b'*', rest)) => (0..=value.len()).any(|skip| inner(rest, &value[skip..])),
            Some((ch, rest)) => value.split_first().map(|(v, vrest)| v == ch && inner(rest, vrest)).unwrap_or(false),
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(rules: Vec<HookPolicyRule>) -> HookPolicy {
        HookPolicy { version: HOOK_POLICY_VERSION, default_decision: PolicyDecision::Defer, rules }
    }

    fn rule(decision: PolicyDecision) -> HookPolicyRule {
        HookPolicyRule { id: None, events: vec![], tools: vec![], input_matchers: vec![], decision, reason: None }
    }

    #[test]
    fn glob_matching() {
        assert!(glob_matches("Bash", "Bash"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("mcp__*", "mcp__github__create_pr"));
        assert!(glob_matches("*__push", "mcp__git__push"));
        assert!(!glob_matches("Bash", "bash"));
        assert!(!glob_matches("mcp__*", "Bash"));
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "x"));
    }

    #[test]
    fn no_rules_returns_default_defer() {
        let p = policy(vec![]);
        let verdict = p.evaluate("PreToolUse", "Bash", &json!({"command": "ls"}));
        assert_eq!(verdict.decision, PolicyDecision::Defer);
        assert!(verdict.reason.is_none());
        assert!(verdict.rule_id.is_none());
    }

    #[test]
    fn deny_rule_matches_bash_command_regex() {
        let mut r = rule(PolicyDecision::Deny);
        r.id = Some("no-force-push".to_string());
        r.tools = vec!["Bash".to_string()];
        r.input_matchers =
            vec![InputMatcher { field: "command".to_string(), regex: r"git\s+push\b.*(--force|-f)\b".to_string() }];
        r.reason = Some("Force pushes are blocked.".to_string());
        let p = policy(vec![r]);

        let verdict = p.evaluate("PreToolUse", "Bash", &json!({"command": "git push --force origin main"}));
        assert_eq!(verdict.decision, PolicyDecision::Deny);
        assert_eq!(verdict.reason.as_deref(), Some("Force pushes are blocked."));
        assert_eq!(verdict.rule_id.as_deref(), Some("no-force-push"));

        let verdict = p.evaluate("PreToolUse", "Bash", &json!({"command": "git push origin main"}));
        assert_eq!(verdict.decision, PolicyDecision::Defer);
    }

    #[test]
    fn deny_outranks_allow_regardless_of_order() {
        let mut allow = rule(PolicyDecision::Allow);
        allow.tools = vec!["Bash".to_string()];
        let mut deny = rule(PolicyDecision::Deny);
        deny.tools = vec!["Bash".to_string()];
        deny.id = Some("deny-all-bash".to_string());

        for rules in [vec![allow.clone(), deny.clone()], vec![deny.clone(), allow.clone()]] {
            let verdict = policy(rules).evaluate("PreToolUse", "Bash", &json!({"command": "ls"}));
            assert_eq!(verdict.decision, PolicyDecision::Deny);
            assert_eq!(verdict.rule_id.as_deref(), Some("deny-all-bash"));
        }
    }

    #[test]
    fn ask_outranks_allow_but_not_deny() {
        assert!(PolicyDecision::Deny > PolicyDecision::Ask);
        assert!(PolicyDecision::Ask > PolicyDecision::Allow);
        assert!(PolicyDecision::Allow > PolicyDecision::Defer);
    }

    #[test]
    fn event_filter_applies() {
        let mut r = rule(PolicyDecision::Deny);
        r.events = vec!["PermissionRequest".to_string()];
        let p = policy(vec![r]);
        assert_eq!(p.evaluate("PreToolUse", "Bash", &json!({})).decision, PolicyDecision::Defer);
        assert_eq!(p.evaluate("PermissionRequest", "Bash", &json!({})).decision, PolicyDecision::Deny);
    }

    #[test]
    fn missing_field_never_matches() {
        let mut r = rule(PolicyDecision::Deny);
        r.input_matchers = vec![InputMatcher { field: "command".to_string(), regex: ".*".to_string() }];
        let p = policy(vec![r]);
        assert_eq!(p.evaluate("PreToolUse", "Edit", &json!({"file_path": "/x"})).decision, PolicyDecision::Defer);
    }

    #[test]
    fn dot_path_and_non_string_values() {
        let mut r = rule(PolicyDecision::Deny);
        r.input_matchers = vec![InputMatcher { field: "options.force".to_string(), regex: "^true$".to_string() }];
        let p = policy(vec![r]);
        assert_eq!(p.evaluate("PreToolUse", "X", &json!({"options": {"force": true}})).decision, PolicyDecision::Deny);
        assert_eq!(
            p.evaluate("PreToolUse", "X", &json!({"options": {"force": false}})).decision,
            PolicyDecision::Defer
        );
    }

    #[test]
    fn all_input_matchers_must_match() {
        let mut r = rule(PolicyDecision::Deny);
        r.input_matchers = vec![
            InputMatcher { field: "command".to_string(), regex: "rm".to_string() },
            InputMatcher { field: "command".to_string(), regex: "-rf".to_string() },
        ];
        let p = policy(vec![r]);
        assert_eq!(p.evaluate("PreToolUse", "Bash", &json!({"command": "rm -rf /"})).decision, PolicyDecision::Deny);
        assert_eq!(p.evaluate("PreToolUse", "Bash", &json!({"command": "rm x"})).decision, PolicyDecision::Defer);
    }

    #[test]
    fn default_decision_non_defer_carries_reason() {
        let p = HookPolicy { version: HOOK_POLICY_VERSION, default_decision: PolicyDecision::Ask, rules: vec![] };
        let verdict = p.evaluate("PreToolUse", "Bash", &json!({}));
        assert_eq!(verdict.decision, PolicyDecision::Ask);
        assert!(verdict.reason.is_some());
    }

    #[test]
    fn matched_rule_without_reason_gets_synthesized_reason() {
        let mut r = rule(PolicyDecision::Deny);
        r.id = Some("r1".to_string());
        let p = policy(vec![r]);
        let verdict = p.evaluate("PreToolUse", "Bash", &json!({}));
        assert!(verdict.reason.as_deref().unwrap_or_default().contains("r1"));
    }

    #[test]
    fn defer_rule_never_overrides_default() {
        let p = HookPolicy {
            version: HOOK_POLICY_VERSION,
            default_decision: PolicyDecision::Allow,
            rules: vec![rule(PolicyDecision::Defer)],
        };
        assert_eq!(p.evaluate("PreToolUse", "Bash", &json!({})).decision, PolicyDecision::Allow);
    }

    #[test]
    fn load_rejects_wrong_version_and_bad_regex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook-policy.v1.json");

        std::fs::write(&path, serde_json::to_string(&json!({"version": 2, "rules": []})).unwrap()).unwrap();
        assert!(matches!(HookPolicy::load(&path), Err(HookPolicyError::Version { found: 2, .. })));

        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "version": 1,
                "rules": [{"decision": "deny", "input_matchers": [{"field": "command", "regex": "("}]}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(HookPolicy::load(&path), Err(HookPolicyError::InvalidRegex { .. })));

        assert!(matches!(HookPolicy::load(&dir.path().join("missing.json")), Err(HookPolicyError::Read { .. })));
    }

    #[test]
    fn load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook-policy.v1.json");
        let p = policy(vec![HookPolicyRule {
            id: Some("r".to_string()),
            events: vec!["PreToolUse".to_string()],
            tools: vec!["Bash".to_string()],
            input_matchers: vec![InputMatcher { field: "command".to_string(), regex: "x".to_string() }],
            decision: PolicyDecision::Deny,
            reason: Some("nope".to_string()),
        }]);
        std::fs::write(&path, serde_json::to_string_pretty(&p).unwrap()).unwrap();
        let loaded = HookPolicy::load(&path).unwrap();
        assert_eq!(loaded.rules.len(), 1);
        assert_eq!(loaded.rules[0].decision, PolicyDecision::Deny);
    }
}
