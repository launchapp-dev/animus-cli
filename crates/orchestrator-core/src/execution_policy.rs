use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl Default for SandboxMode {
    fn default() -> Self {
        Self::WorkspaceWrite
    }
}

impl SandboxMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::DangerFullAccess => "danger_full_access",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPolicy {
    #[serde(default)]
    pub allow_prefixes: Vec<String>,
    #[serde(default)]
    pub allow_exact: Vec<String>,
    #[serde(default)]
    pub deny_prefixes: Vec<String>,
    #[serde(default)]
    pub deny_exact: Vec<String>,
}

impl ToolPolicy {
    pub fn has_allow_rules(&self) -> bool {
        !self.allow_prefixes.is_empty() || !self.allow_exact.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    #[serde(default)]
    pub sandbox_mode: SandboxMode,
    #[serde(default)]
    pub tool_policy: ToolPolicy,
    #[serde(default)]
    pub allow_elevated: bool,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            sandbox_mode: SandboxMode::WorkspaceWrite,
            tool_policy: ToolPolicy::default(),
            allow_elevated: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicyOverrides {
    #[serde(default)]
    pub sandbox_mode: Option<SandboxMode>,
    #[serde(default)]
    pub allow_prefixes: Option<Vec<String>>,
    #[serde(default)]
    pub allow_exact: Option<Vec<String>>,
    #[serde(default)]
    pub deny_prefixes: Option<Vec<String>>,
    #[serde(default)]
    pub deny_exact: Option<Vec<String>>,
    #[serde(default)]
    pub allow_elevated: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    Task,
    Phase,
    Agent,
    Global,
}

impl PolicySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Phase => "phase",
            Self::Agent => "agent",
            Self::Global => "global",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicySources {
    pub sandbox_mode: String,
    pub allow_prefixes: String,
    pub allow_exact: String,
    pub deny_prefixes: String,
    pub deny_exact: String,
    pub allow_elevated: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedExecutionPolicy {
    pub policy: ExecutionPolicy,
    pub sources: ExecutionPolicySources,
    pub policy_hash: String,
}

pub fn validate_execution_policy_overrides(
    field_path: &str,
    overrides: &ExecutionPolicyOverrides,
) -> Result<()> {
    validate_tool_values(
        &format!("{field_path}.allow_prefixes"),
        overrides.allow_prefixes.as_deref(),
    )?;
    validate_tool_values(
        &format!("{field_path}.allow_exact"),
        overrides.allow_exact.as_deref(),
    )?;
    validate_tool_values(
        &format!("{field_path}.deny_prefixes"),
        overrides.deny_prefixes.as_deref(),
    )?;
    validate_tool_values(
        &format!("{field_path}.deny_exact"),
        overrides.deny_exact.as_deref(),
    )?;
    Ok(())
}

fn validate_tool_values(field_path: &str, values: Option<&[String]>) -> Result<()> {
    let Some(values) = values else {
        return Ok(());
    };
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(anyhow!("{field_path} must not contain empty values"));
    }
    Ok(())
}

fn normalize_tool_entries(values: Option<&Vec<String>>) -> Vec<String> {
    let mut normalized = values
        .into_iter()
        .flat_map(|items| items.iter())
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn resolve_sandbox_mode_source(
    task_override: Option<&ExecutionPolicyOverrides>,
    phase_override: Option<&ExecutionPolicyOverrides>,
    agent_override: Option<&ExecutionPolicyOverrides>,
) -> (SandboxMode, PolicySource) {
    if let Some(mode) = task_override.and_then(|override_| override_.sandbox_mode) {
        return (mode, PolicySource::Task);
    }
    if let Some(mode) = phase_override.and_then(|override_| override_.sandbox_mode) {
        return (mode, PolicySource::Phase);
    }
    if let Some(mode) = agent_override.and_then(|override_| override_.sandbox_mode) {
        return (mode, PolicySource::Agent);
    }
    (SandboxMode::WorkspaceWrite, PolicySource::Global)
}

fn resolve_tool_source(
    task_override: Option<&ExecutionPolicyOverrides>,
    phase_override: Option<&ExecutionPolicyOverrides>,
    agent_override: Option<&ExecutionPolicyOverrides>,
    selector: fn(&ExecutionPolicyOverrides) -> Option<&Vec<String>>,
) -> (Vec<String>, PolicySource) {
    if let Some(values) = task_override.and_then(selector) {
        return (normalize_tool_entries(Some(values)), PolicySource::Task);
    }
    if let Some(values) = phase_override.and_then(selector) {
        return (normalize_tool_entries(Some(values)), PolicySource::Phase);
    }
    if let Some(values) = agent_override.and_then(selector) {
        return (normalize_tool_entries(Some(values)), PolicySource::Agent);
    }
    (Vec::new(), PolicySource::Global)
}

fn resolve_allow_elevated_source(
    task_override: Option<&ExecutionPolicyOverrides>,
    phase_override: Option<&ExecutionPolicyOverrides>,
    agent_override: Option<&ExecutionPolicyOverrides>,
) -> (bool, PolicySource) {
    if let Some(value) = task_override.and_then(|override_| override_.allow_elevated) {
        return (value, PolicySource::Task);
    }
    if let Some(value) = phase_override.and_then(|override_| override_.allow_elevated) {
        return (value, PolicySource::Phase);
    }
    if let Some(value) = agent_override.and_then(|override_| override_.allow_elevated) {
        return (value, PolicySource::Agent);
    }
    (false, PolicySource::Global)
}

pub fn resolve_execution_policy(
    task_override: Option<&ExecutionPolicyOverrides>,
    phase_override: Option<&ExecutionPolicyOverrides>,
    agent_override: Option<&ExecutionPolicyOverrides>,
) -> ResolvedExecutionPolicy {
    let (sandbox_mode, sandbox_mode_source) =
        resolve_sandbox_mode_source(task_override, phase_override, agent_override);
    let (allow_prefixes, allow_prefixes_source) = resolve_tool_source(
        task_override,
        phase_override,
        agent_override,
        |override_| override_.allow_prefixes.as_ref(),
    );
    let (allow_exact, allow_exact_source) = resolve_tool_source(
        task_override,
        phase_override,
        agent_override,
        |override_| override_.allow_exact.as_ref(),
    );
    let (deny_prefixes, deny_prefixes_source) = resolve_tool_source(
        task_override,
        phase_override,
        agent_override,
        |override_| override_.deny_prefixes.as_ref(),
    );
    let (deny_exact, deny_exact_source) = resolve_tool_source(
        task_override,
        phase_override,
        agent_override,
        |override_| override_.deny_exact.as_ref(),
    );
    let (allow_elevated, allow_elevated_source) =
        resolve_allow_elevated_source(task_override, phase_override, agent_override);

    let policy = ExecutionPolicy {
        sandbox_mode,
        tool_policy: ToolPolicy {
            allow_prefixes,
            allow_exact,
            deny_prefixes,
            deny_exact,
        },
        allow_elevated,
    };
    let sources = ExecutionPolicySources {
        sandbox_mode: sandbox_mode_source.as_str().to_string(),
        allow_prefixes: allow_prefixes_source.as_str().to_string(),
        allow_exact: allow_exact_source.as_str().to_string(),
        deny_prefixes: deny_prefixes_source.as_str().to_string(),
        deny_exact: deny_exact_source.as_str().to_string(),
        allow_elevated: allow_elevated_source.as_str().to_string(),
    };
    let policy_hash = execution_policy_hash(&policy);

    ResolvedExecutionPolicy {
        policy,
        sources,
        policy_hash,
    }
}

pub fn execution_policy_hash(policy: &ExecutionPolicy) -> String {
    let bytes = serde_json::to_vec(policy).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_policy_uses_task_phase_agent_global_precedence() {
        let agent = ExecutionPolicyOverrides {
            sandbox_mode: Some(SandboxMode::WorkspaceWrite),
            allow_prefixes: Some(vec!["ao.".to_string()]),
            allow_exact: Some(vec!["phase_transition".to_string()]),
            deny_prefixes: None,
            deny_exact: None,
            allow_elevated: Some(false),
        };
        let phase = ExecutionPolicyOverrides {
            sandbox_mode: Some(SandboxMode::ReadOnly),
            allow_prefixes: Some(vec!["mcp__ao__".to_string()]),
            allow_exact: None,
            deny_prefixes: Some(vec!["bash".to_string()]),
            deny_exact: None,
            allow_elevated: None,
        };
        let task = ExecutionPolicyOverrides {
            sandbox_mode: None,
            allow_prefixes: None,
            allow_exact: Some(vec!["ao.task.list".to_string()]),
            deny_prefixes: None,
            deny_exact: Some(vec!["ao.git.push".to_string()]),
            allow_elevated: Some(true),
        };

        let resolved = resolve_execution_policy(Some(&task), Some(&phase), Some(&agent));

        assert_eq!(resolved.policy.sandbox_mode, SandboxMode::ReadOnly);
        assert_eq!(
            resolved.policy.tool_policy.allow_prefixes,
            vec!["mcp__ao__".to_string()]
        );
        assert_eq!(
            resolved.policy.tool_policy.allow_exact,
            vec!["ao.task.list".to_string()]
        );
        assert_eq!(
            resolved.policy.tool_policy.deny_prefixes,
            vec!["bash".to_string()]
        );
        assert_eq!(
            resolved.policy.tool_policy.deny_exact,
            vec!["ao.git.push".to_string()]
        );
        assert!(resolved.policy.allow_elevated);
        assert_eq!(resolved.sources.sandbox_mode, "phase");
        assert_eq!(resolved.sources.allow_prefixes, "phase");
        assert_eq!(resolved.sources.allow_exact, "task");
        assert_eq!(resolved.sources.deny_prefixes, "phase");
        assert_eq!(resolved.sources.deny_exact, "task");
        assert_eq!(resolved.sources.allow_elevated, "task");
    }

    #[test]
    fn resolve_policy_defaults_to_global_safe_values() {
        let resolved = resolve_execution_policy(None, None, None);
        assert_eq!(resolved.policy.sandbox_mode, SandboxMode::WorkspaceWrite);
        assert!(!resolved.policy.tool_policy.has_allow_rules());
        assert!(!resolved.policy.allow_elevated);
        assert_eq!(resolved.sources.sandbox_mode, "global");
        assert_eq!(resolved.sources.allow_prefixes, "global");
    }

    #[test]
    fn policy_hash_is_stable_for_equivalent_policy() {
        let left = resolve_execution_policy(
            Some(&ExecutionPolicyOverrides {
                sandbox_mode: Some(SandboxMode::WorkspaceWrite),
                allow_prefixes: Some(vec!["AO.".to_string(), "ao.".to_string()]),
                allow_exact: None,
                deny_prefixes: None,
                deny_exact: None,
                allow_elevated: Some(false),
            }),
            None,
            None,
        );
        let right = resolve_execution_policy(
            Some(&ExecutionPolicyOverrides {
                sandbox_mode: Some(SandboxMode::WorkspaceWrite),
                allow_prefixes: Some(vec!["ao.".to_string()]),
                allow_exact: None,
                deny_prefixes: None,
                deny_exact: None,
                allow_elevated: Some(false),
            }),
            None,
            None,
        );
        assert_eq!(left.policy_hash, right.policy_hash);
    }

    #[test]
    fn validate_overrides_rejects_empty_values() {
        let overrides = ExecutionPolicyOverrides {
            sandbox_mode: None,
            allow_prefixes: Some(vec!["".to_string()]),
            allow_exact: None,
            deny_prefixes: None,
            deny_exact: None,
            allow_elevated: None,
        };
        let err = validate_execution_policy_overrides("policy", &overrides)
            .expect_err("empty tool entries should fail");
        assert!(err.to_string().contains("policy.allow_prefixes"));
    }
}
