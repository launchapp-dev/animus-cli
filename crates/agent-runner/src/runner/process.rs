use anyhow::{bail, Context, Result};
use chrono::Utc;
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration, MissedTickBehavior};
use tracing::{debug, info, warn};

use super::lifecycle::spawn_wait_task;
use super::process_builder::{build_cli_invocation, resolve_idle_timeout_secs};
use super::stream_bridge::spawn_stream_forwarders;
use crate::cleanup::{track_process, untrack_process};

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

#[derive(Debug, Clone)]
struct McpStdioConfig {
    command: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct McpToolEnforcement {
    enabled: bool,
    endpoint: Option<String>,
    stdio: Option<McpStdioConfig>,
    agent_id: String,
    allowed_prefixes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::DangerFullAccess => "danger_full_access",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ToolPolicyEnforcement {
    #[serde(default)]
    allow_prefixes: Vec<String>,
    #[serde(default)]
    allow_exact: Vec<String>,
    #[serde(default)]
    deny_prefixes: Vec<String>,
    #[serde(default)]
    deny_exact: Vec<String>,
}

impl ToolPolicyEnforcement {
    fn has_allow_rules(&self) -> bool {
        !self.allow_prefixes.is_empty() || !self.allow_exact.is_empty()
    }
}

#[derive(Debug, Clone)]
struct ElevationApproval {
    request_id: String,
    approved: bool,
    approved_by: Option<String>,
    comment: Option<String>,
}

#[derive(Debug, Clone)]
struct ExecutionPolicyEnforcement {
    sandbox_mode: SandboxMode,
    tool_policy: ToolPolicyEnforcement,
    allow_elevated: bool,
    policy_hash: String,
    approval: Option<ElevationApproval>,
}

impl Default for ExecutionPolicyEnforcement {
    fn default() -> Self {
        let mut policy = Self {
            sandbox_mode: SandboxMode::WorkspaceWrite,
            tool_policy: ToolPolicyEnforcement::default(),
            allow_elevated: false,
            policy_hash: String::new(),
            approval: None,
        };
        policy.policy_hash = execution_policy_hash(&policy);
        policy
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ElevationAuditLog {
    #[serde(default)]
    requests: Vec<ElevationRequestRecord>,
    #[serde(default)]
    outcomes: Vec<ElevationOutcomeRecord>,
    #[serde(default)]
    consumed_approvals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ElevationRequestRecord {
    request_id: String,
    run_id: String,
    policy_hash: String,
    action: String,
    reason: String,
    requested_at: String,
    workflow_id: Option<String>,
    task_id: Option<String>,
    phase_id: Option<String>,
    agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ElevationOutcomeRecord {
    request_id: String,
    run_id: String,
    approved: bool,
    success: bool,
    message: String,
    recorded_at: String,
    approved_by: Option<String>,
    comment: Option<String>,
}

#[derive(Debug, Default)]
struct TempPathCleanup {
    paths: Vec<PathBuf>,
}

impl TempPathCleanup {
    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
}

impl Drop for TempPathCleanup {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn default_allowed_tool_prefixes(agent_id: &str) -> Vec<String> {
    let normalized = agent_id.trim().to_ascii_lowercase();
    let mut prefixes = vec![
        "ao.".to_string(),
        "mcp__ao__".to_string(),
        "mcp.ao.".to_string(),
        format!("{normalized}."),
        format!("mcp__{normalized}__"),
        format!("mcp.{normalized}."),
    ];

    let snake = normalized.replace('-', "_");
    prefixes.push(format!("{snake}."));
    prefixes.push(format!("mcp__{snake}__"));
    prefixes.push(format!("mcp.{snake}."));

    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn resolve_mcp_tool_enforcement(
    runtime_contract: Option<&serde_json::Value>,
) -> McpToolEnforcement {
    let Some(contract) = runtime_contract else {
        return McpToolEnforcement {
            enabled: false,
            endpoint: None,
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: Vec::new(),
        };
    };

    let supports_mcp = contract
        .pointer("/cli/capabilities/supports_mcp")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let endpoint = contract
        .pointer("/mcp/endpoint")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let stdio_command = contract
        .pointer("/mcp/stdio/command")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let stdio_args = contract
        .pointer("/mcp/stdio/args")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stdio = stdio_command.map(|command| McpStdioConfig {
        command,
        args: stdio_args,
    });
    let has_endpoint = endpoint.is_some();
    let has_stdio = stdio.is_some();
    let agent_id = contract
        .pointer("/mcp/agent_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ao")
        .to_string();
    let explicit_enforce = contract
        .pointer("/mcp/enforce_only")
        .and_then(serde_json::Value::as_bool);
    let enabled = explicit_enforce.unwrap_or((has_endpoint || has_stdio) && supports_mcp);

    let mut allowed_prefixes = contract
        .pointer("/mcp/allowed_tool_prefixes")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if enabled && allowed_prefixes.is_empty() {
        allowed_prefixes = default_allowed_tool_prefixes(&agent_id);
    }

    McpToolEnforcement {
        enabled,
        endpoint,
        stdio,
        agent_id,
        allowed_prefixes,
    }
}

fn normalize_tool_entries(values: &[String]) -> Vec<String> {
    let mut normalized = values
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn parse_tool_entries(value: Option<&serde_json::Value>) -> Vec<String> {
    let values = value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    normalize_tool_entries(&values)
}

fn parse_sandbox_mode(value: Option<&str>) -> SandboxMode {
    match value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("read_only") => SandboxMode::ReadOnly,
        Some("danger_full_access") => SandboxMode::DangerFullAccess,
        _ => SandboxMode::WorkspaceWrite,
    }
}

fn parse_elevation_approval(value: Option<&serde_json::Value>) -> Option<ElevationApproval> {
    let object = value?.as_object()?;
    let request_id = object
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let approved = object
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let approved_by = object
        .get("approved_by")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let comment = object
        .get("comment")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    Some(ElevationApproval {
        request_id,
        approved,
        approved_by,
        comment,
    })
}

fn execution_policy_hash(policy: &ExecutionPolicyEnforcement) -> String {
    let value = serde_json::json!({
        "sandbox_mode": policy.sandbox_mode.as_str(),
        "allow_elevated": policy.allow_elevated,
        "tool_policy": policy.tool_policy,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn resolve_execution_policy_enforcement(
    runtime_contract: Option<&serde_json::Value>,
) -> ExecutionPolicyEnforcement {
    let mut policy = ExecutionPolicyEnforcement::default();
    let Some(contract) = runtime_contract else {
        return policy;
    };

    let execution = contract
        .pointer("/policy/execution")
        .or_else(|| contract.pointer("/execution_policy"));
    let Some(execution) = execution else {
        policy.policy_hash = execution_policy_hash(&policy);
        return policy;
    };

    policy.sandbox_mode =
        parse_sandbox_mode(execution.get("sandbox_mode").and_then(serde_json::Value::as_str));
    policy.allow_elevated = execution
        .get("allow_elevated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let tool_policy = execution
        .get("tool_policy")
        .filter(|value| value.is_object())
        .unwrap_or(execution);
    policy.tool_policy.allow_prefixes =
        parse_tool_entries(tool_policy.get("allow_prefixes"));
    policy.tool_policy.allow_exact = parse_tool_entries(tool_policy.get("allow_exact"));
    policy.tool_policy.deny_prefixes = parse_tool_entries(tool_policy.get("deny_prefixes"));
    policy.tool_policy.deny_exact = parse_tool_entries(tool_policy.get("deny_exact"));

    policy.approval = parse_elevation_approval(
        execution
            .get("elevation_approval")
            .or_else(|| execution.pointer("/elevation/approval")),
    );

    policy.policy_hash = execution
        .get("policy_hash")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| execution_policy_hash(&policy));

    policy
}

fn elevation_audit_path(execution_context: Option<&serde_json::Value>) -> Option<PathBuf> {
    execution_context
        .and_then(|context| context.get("project_root"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Path::new)
        .map(|project_root| {
            project_root
                .join(".ao")
                .join("state")
                .join("elevation-audit.v1.json")
        })
}

fn load_elevation_audit(path: &Path) -> ElevationAuditLog {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ElevationAuditLog::default();
    };
    serde_json::from_str::<ElevationAuditLog>(&raw).unwrap_or_default()
}

fn write_elevation_audit(path: &Path, log: &ElevationAuditLog) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create elevation audit dir {}", parent.display()))?;
    }
    let payload = serde_json::to_vec_pretty(log).context("Failed to encode elevation audit log")?;
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp_path = path.with_file_name(format!(
        "{}.{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("elevation-audit"),
        std::process::id(),
        now_nanos
    ));
    std::fs::write(&tmp_path, payload).with_context(|| {
        format!(
            "Failed to write temporary elevation audit file {}",
            tmp_path.display()
        )
    })?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to atomically update elevation audit file {}",
            path.display()
        )
    })?;
    Ok(())
}

fn mutate_elevation_audit<T>(
    audit_path: Option<&Path>,
    mutate: impl FnOnce(&mut ElevationAuditLog) -> T,
) -> Result<T> {
    let Some(path) = audit_path else {
        let mut log = ElevationAuditLog::default();
        return Ok(mutate(&mut log));
    };

    let mut log = load_elevation_audit(path);
    let result = mutate(&mut log);
    write_elevation_audit(path, &log)?;
    Ok(result)
}

fn elevation_request_id(run_id: &RunId, policy_hash: &str, action: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.0.as_bytes());
    hasher.update(policy_hash.as_bytes());
    hasher.update(action.as_bytes());
    let digest = hasher.finalize();
    format!(
        "elv-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7]
    )
}

fn context_identity(
    execution_context: Option<&serde_json::Value>,
    key: &str,
) -> Option<String> {
    execution_context
        .and_then(|context| context.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn append_elevation_request(
    audit_path: Option<&Path>,
    record: ElevationRequestRecord,
) -> Result<()> {
    mutate_elevation_audit(audit_path, |log| {
        if !log
            .requests
            .iter()
            .any(|existing| existing.request_id == record.request_id)
        {
            log.requests.push(record);
        }
    })?;
    Ok(())
}

fn append_elevation_outcome(
    audit_path: Option<&Path>,
    record: ElevationOutcomeRecord,
) -> Result<()> {
    mutate_elevation_audit(audit_path, |log| {
        log.outcomes.push(record);
    })?;
    Ok(())
}

fn consume_approval_once(audit_path: Option<&Path>, approval_id: &str) -> Result<bool> {
    mutate_elevation_audit(audit_path, |log| {
        if log
            .consumed_approvals
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(approval_id))
        {
            false
        } else {
            log.consumed_approvals.push(approval_id.to_string());
            true
        }
    })
}

fn require_elevation_for_action(
    run_id: &RunId,
    execution_context: Option<&serde_json::Value>,
    policy: &ExecutionPolicyEnforcement,
    action: &str,
    reason: &str,
) -> Result<()> {
    let request_id = elevation_request_id(run_id, &policy.policy_hash, action);
    let audit_path = elevation_audit_path(execution_context);
    append_elevation_request(
        audit_path.as_deref(),
        ElevationRequestRecord {
            request_id: request_id.clone(),
            run_id: run_id.0.clone(),
            policy_hash: policy.policy_hash.clone(),
            action: action.to_string(),
            reason: reason.to_string(),
            requested_at: Utc::now().to_rfc3339(),
            workflow_id: context_identity(execution_context, "workflow_id"),
            task_id: context_identity(execution_context, "task_id"),
            phase_id: context_identity(execution_context, "phase_id"),
            agent_id: context_identity(execution_context, "agent_id"),
        },
    )?;

    if !policy.allow_elevated {
        let message = "policy does not allow elevated execution".to_string();
        append_elevation_outcome(
            audit_path.as_deref(),
            ElevationOutcomeRecord {
                request_id,
                run_id: run_id.0.clone(),
                approved: false,
                success: false,
                message: message.clone(),
                recorded_at: Utc::now().to_rfc3339(),
                approved_by: None,
                comment: None,
            },
        )?;
        let payload = serde_json::json!({
            "code": "POLICY_VIOLATION",
            "run_id": run_id.0.as_str(),
            "policy_hash": policy.policy_hash.as_str(),
            "action": action,
            "reason": reason,
            "message": message,
        });
        bail!("POLICY_VIOLATION: {payload}");
    }

    let approval = policy.approval.as_ref().filter(|approval| approval.approved);
    let Some(approval) = approval else {
        append_elevation_outcome(
            audit_path.as_deref(),
            ElevationOutcomeRecord {
                request_id: request_id.clone(),
                run_id: run_id.0.clone(),
                approved: false,
                success: false,
                message: "elevation approval is required".to_string(),
                recorded_at: Utc::now().to_rfc3339(),
                approved_by: None,
                comment: None,
            },
        )?;
        let payload = serde_json::json!({
            "code": "ELEVATION_REQUIRED",
            "run_id": run_id.0.as_str(),
            "policy_hash": policy.policy_hash.as_str(),
            "elevation_request_id": request_id,
            "action": action,
            "reason": reason,
            "guidance": "approve this exact request_id and retry with runtime_contract.policy.execution.elevation_approval",
        });
        bail!("ELEVATION_REQUIRED: {payload}");
    };

    if !approval.request_id.eq_ignore_ascii_case(&request_id) {
        append_elevation_outcome(
            audit_path.as_deref(),
            ElevationOutcomeRecord {
                request_id: request_id.clone(),
                run_id: run_id.0.clone(),
                approved: false,
                success: false,
                message: "approval does not match requested action".to_string(),
                recorded_at: Utc::now().to_rfc3339(),
                approved_by: approval.approved_by.clone(),
                comment: approval.comment.clone(),
            },
        )?;
        let payload = serde_json::json!({
            "code": "POLICY_VIOLATION",
            "run_id": run_id.0.as_str(),
            "policy_hash": policy.policy_hash.as_str(),
            "expected_request_id": request_id,
            "provided_request_id": approval.request_id.as_str(),
            "action": action,
            "reason": reason,
            "message": "approval mismatch",
        });
        bail!("POLICY_VIOLATION: {payload}");
    }

    let consumed = consume_approval_once(audit_path.as_deref(), &approval.request_id)?;
    if !consumed {
        append_elevation_outcome(
            audit_path.as_deref(),
            ElevationOutcomeRecord {
                request_id,
                run_id: run_id.0.clone(),
                approved: true,
                success: false,
                message: "approval already consumed".to_string(),
                recorded_at: Utc::now().to_rfc3339(),
                approved_by: approval.approved_by.clone(),
                comment: approval.comment.clone(),
            },
        )?;
        let payload = serde_json::json!({
            "code": "POLICY_VIOLATION",
            "run_id": run_id.0.as_str(),
            "policy_hash": policy.policy_hash.as_str(),
            "action": action,
            "reason": reason,
            "message": "approval already consumed",
            "elevation_request_id": approval.request_id.as_str(),
        });
        bail!("POLICY_VIOLATION: {payload}");
    }

    append_elevation_outcome(
        audit_path.as_deref(),
        ElevationOutcomeRecord {
            request_id,
            run_id: run_id.0.clone(),
            approved: true,
            success: true,
            message: "elevation approved".to_string(),
            recorded_at: Utc::now().to_rfc3339(),
            approved_by: approval.approved_by.clone(),
            comment: approval.comment.clone(),
        },
    )?;

    Ok(())
}

fn enforce_sandbox_mode_before_launch(
    run_id: &RunId,
    tool: &str,
    cwd: &str,
    execution_context: Option<&serde_json::Value>,
    policy: &ExecutionPolicyEnforcement,
) -> Result<()> {
    match policy.sandbox_mode {
        SandboxMode::WorkspaceWrite => Ok(()),
        SandboxMode::ReadOnly => require_elevation_for_action(
            run_id,
            execution_context,
            policy,
            "sandbox_read_only_override",
            &format!(
                "sandbox mode '{}' blocks side-effecting CLI launch for tool '{}' in '{}'",
                policy.sandbox_mode.as_str(),
                tool,
                cwd
            ),
        ),
        SandboxMode::DangerFullAccess => require_elevation_for_action(
            run_id,
            execution_context,
            policy,
            "sandbox_danger_full_access_launch",
            &format!(
                "sandbox mode '{}' requires explicit elevation approval for tool '{}' in '{}'",
                policy.sandbox_mode.as_str(),
                tool,
                cwd
            ),
        ),
    }
}

fn canonical_cli_name(command: &str) -> String {
    let trimmed = command.trim();
    Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn ensure_flag(args: &mut Vec<String>, flag: &str, insert_at: usize) {
    if args.iter().any(|value| value == flag) {
        return;
    }
    args.insert(insert_at.min(args.len()), flag.to_string());
}

fn ensure_flag_value(args: &mut Vec<String>, flag: &str, value: &str, insert_at: usize) {
    if let Some(index) = args.iter().position(|entry| entry == flag) {
        if index + 1 < args.len() {
            args[index + 1] = value.to_string();
        } else {
            args.push(value.to_string());
        }
        return;
    }

    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, flag.to_string());
    args.insert((insert_at + 1).min(args.len()), value.to_string());
}

fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn ensure_codex_config_override(args: &mut Vec<String>, key: &str, value_expr: &str) {
    let key_prefix = format!("{key}=");
    let target = format!("{key}={value_expr}");

    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str();
        if flag == "-c" || flag == "--config" {
            if args[index + 1].starts_with(&key_prefix) {
                args[index + 1] = target;
                return;
            }
            index += 2;
            continue;
        }
        index += 1;
    }

    // Keep prompt payload as the final argv token when present.
    let insert_at = args.len().saturating_sub(1);
    args.insert(insert_at, "-c".to_string());
    args.insert(insert_at + 1, target);
}

fn is_safe_codex_server_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn parse_codex_mcp_server_names(payload: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|name| is_safe_codex_server_name(name))
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn discover_codex_mcp_server_names() -> Vec<String> {
    let output = match std::process::Command::new("codex")
        .args(["mcp", "list", "--json"])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            debug!(error = %error, "Failed to list configured Codex MCP servers");
            return Vec::new();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(
            status = %output.status,
            stderr = %truncate_for_log(&stderr, 200),
            "Codex MCP list command returned non-success status"
        );
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_codex_mcp_server_names(&stdout)
}

#[derive(Debug, Clone, Copy)]
enum McpServerTransport<'a> {
    Http(&'a str),
    Stdio {
        command: &'a str,
        args: &'a [String],
    },
}

fn resolve_mcp_server_transport<'a>(
    enforcement: &'a McpToolEnforcement,
) -> Result<McpServerTransport<'a>> {
    if let Some(stdio) = enforcement.stdio.as_ref() {
        return Ok(McpServerTransport::Stdio {
            command: stdio.command.trim(),
            args: &stdio.args,
        });
    }
    if let Some(endpoint) = enforcement.endpoint.as_deref() {
        return Ok(McpServerTransport::Http(endpoint));
    }

    bail!(
        "MCP-only policy is enabled, but neither mcp.endpoint nor mcp.stdio.command is configured"
    );
}

fn sanitize_token_for_filename(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn write_temp_json_file(
    run_id: &RunId,
    prefix: &str,
    value: &serde_json::Value,
) -> Result<PathBuf> {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!(
        "ao-{prefix}-{}-{}-{now_nanos}.json",
        sanitize_token_for_filename(&run_id.0),
        std::process::id()
    ));
    let payload =
        serde_json::to_vec(value).context("Failed to serialize strict MCP config JSON")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("Failed to write strict MCP config file {}", path.display()))?;
    Ok(path)
}

fn apply_claude_native_mcp_lockdown(
    args: &mut Vec<String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
) {
    let server = match transport {
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "http",
            "url": endpoint
        }),
        McpServerTransport::Stdio { command, args } => serde_json::json!({
            "command": command,
            "args": args
        }),
    };
    let config = serde_json::json!({
        "mcpServers": {
            agent_id: server
        }
    })
    .to_string();
    ensure_flag(args, "--strict-mcp-config", 0);
    ensure_flag_value(args, "--mcp-config", &config, 0);
    ensure_flag_value(args, "--permission-mode", "bypassPermissions", 0);
}

fn apply_codex_native_mcp_lockdown(
    args: &mut Vec<String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    configured_servers: &[String],
) {
    for server_name in configured_servers {
        if server_name.eq_ignore_ascii_case(agent_id) {
            continue;
        }
        ensure_codex_config_override(args, &format!("mcp_servers.{server_name}.enabled"), "false");
    }

    let base = format!("mcp_servers.{agent_id}");
    match transport {
        McpServerTransport::Http(endpoint) => {
            ensure_codex_config_override(args, &format!("{base}.url"), &toml_string(endpoint));
        }
        McpServerTransport::Stdio { command, args: stdio_args } => {
            ensure_codex_config_override(
                args,
                &format!("{base}.command"),
                &toml_string(command),
            );
            let toml_args = format!(
                "[{}]",
                stdio_args
                    .iter()
                    .map(|arg| toml_string(arg))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            ensure_codex_config_override(args, &format!("{base}.args"), &toml_args);
        }
    }
    ensure_codex_config_override(args, &format!("{base}.enabled"), "true");
}

fn apply_gemini_native_mcp_lockdown(
    args: &mut Vec<String>,
    env: &mut HashMap<String, String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    run_id: &RunId,
    temp_cleanup: &mut TempPathCleanup,
) -> Result<()> {
    ensure_flag_value(args, "--allowed-mcp-server-names", agent_id, 0);
    let server = match transport {
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "http",
            "url": endpoint
        }),
        McpServerTransport::Stdio { command, args } => serde_json::json!({
            "type": "stdio",
            "command": command,
            "args": args,
            // Gemini MCP currently fails on draft 2020-12 tool schemas.
            // Force AO MCP tool schemas to draft-07 for this adapter only.
            "env": {
                "AO_MCP_SCHEMA_DRAFT": "draft07"
            }
        }),
    };
    let settings = serde_json::json!({
        "tools": {
            "core": []
        },
        "mcp": {
            "allowed": [agent_id],
            "excluded": []
        },
        "mcpServers": {
            agent_id: server
        }
    });
    let settings_path = write_temp_json_file(run_id, "gemini-mcp", &settings)?;
    env.insert(
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(),
        settings_path.to_string_lossy().to_string(),
    );
    temp_cleanup.track(settings_path);
    Ok(())
}

fn apply_opencode_native_mcp_lockdown(
    env: &mut HashMap<String, String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
) {
    let server = match transport {
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "remote",
            "url": endpoint,
            "enabled": true
        }),
        McpServerTransport::Stdio { command, args } => {
            let mut command_with_args = Vec::with_capacity(args.len() + 1);
            command_with_args.push(command.to_string());
            command_with_args.extend(args.iter().cloned());
            serde_json::json!({
                "type": "local",
                "command": command_with_args,
                "enabled": true
            })
        }
    };
    let config = serde_json::json!({
        "mcp": {
            agent_id: server
        }
    });
    env.insert("OPENCODE_CONFIG_CONTENT".to_string(), config.to_string());
}

fn apply_native_mcp_policy(
    invocation: &mut super::process_builder::CliInvocation,
    enforcement: &McpToolEnforcement,
    env: &mut HashMap<String, String>,
    run_id: &RunId,
    temp_cleanup: &mut TempPathCleanup,
) -> Result<()> {
    if !enforcement.enabled {
        return Ok(());
    }

    let transport = resolve_mcp_server_transport(enforcement)?;
    let agent_id = enforcement.agent_id.trim();
    let cli = canonical_cli_name(&invocation.command);

    match cli.as_str() {
        "claude" => apply_claude_native_mcp_lockdown(&mut invocation.args, transport, agent_id),
        "codex" => {
            let configured_servers = discover_codex_mcp_server_names();
            apply_codex_native_mcp_lockdown(
                &mut invocation.args,
                transport,
                agent_id,
                &configured_servers,
            );
        }
        "gemini" => apply_gemini_native_mcp_lockdown(
            &mut invocation.args,
            env,
            transport,
            agent_id,
            run_id,
            temp_cleanup,
        )?,
        "opencode" => apply_opencode_native_mcp_lockdown(env, transport, agent_id),
        _ => {
            bail!(
                "MCP-only policy enabled, but no native enforcement adapter exists for CLI command '{}'",
                invocation.command
            );
        }
    }

    Ok(())
}

fn is_tool_denied_by_policy(tool_name: &str, policy: &ToolPolicyEnforcement) -> bool {
    policy
        .deny_exact
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(tool_name))
        || policy
            .deny_prefixes
            .iter()
            .any(|prefix| tool_name.starts_with(prefix))
}

fn is_tool_allowed_by_policy_allowlist(tool_name: &str, policy: &ToolPolicyEnforcement) -> bool {
    if !policy.has_allow_rules() {
        return true;
    }
    policy
        .allow_exact
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(tool_name))
        || policy
            .allow_prefixes
            .iter()
            .any(|prefix| tool_name.starts_with(prefix))
}

fn is_tool_call_allowed_by_mcp_policy(
    tool_name: &str,
    parameters: &serde_json::Value,
    enforcement: &McpToolEnforcement,
) -> bool {
    if !enforcement.enabled {
        return true;
    }
    if matches!(tool_name, "phase_transition" | "phase-transition") {
        return true;
    }

    let is_mcp_discovery_helper = matches!(
        tool_name,
        "list_mcp_resources" | "list_mcp_resource_templates" | "read_mcp_resource"
    );

    // Codex stream-json MCP events include a plain tool name and a "server" field
    // inside arguments. Allow calls explicitly targeting the locked agent_id.
    let target_server = parameters
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    if let Some(server) = target_server {
        if server == enforcement.agent_id.to_ascii_lowercase() {
            return true;
        }
        // Codex emits MCP discovery helper calls on its own host server.
        if server == "codex" && is_mcp_discovery_helper {
            return true;
        }
        return false;
    }

    // These are MCP discovery helpers emitted by Codex itself; they do not map to a
    // concrete downstream AO tool name.
    if is_mcp_discovery_helper {
        return true;
    }

    enforcement
        .allowed_prefixes
        .iter()
        .any(|prefix| tool_name.starts_with(prefix))
}

fn is_tool_call_allowed_with_policy(
    tool_name: &str,
    parameters: &serde_json::Value,
    mcp_enforcement: &McpToolEnforcement,
    execution_policy: &ExecutionPolicyEnforcement,
) -> bool {
    let normalized = tool_name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if is_tool_denied_by_policy(&normalized, &execution_policy.tool_policy) {
        return false;
    }
    if !is_tool_call_allowed_by_mcp_policy(&normalized, parameters, mcp_enforcement) {
        return false;
    }
    is_tool_allowed_by_policy_allowlist(&normalized, &execution_policy.tool_policy)
}

#[cfg(test)]
fn is_tool_call_allowed(
    tool_name: &str,
    parameters: &serde_json::Value,
    enforcement: &McpToolEnforcement,
) -> bool {
    is_tool_call_allowed_with_policy(
        tool_name,
        parameters,
        enforcement,
        &ExecutionPolicyEnforcement::default(),
    )
}

// Keeping this explicit signature preserves current call sites across the
// runner orchestration path during the staged refactor. (2026-02-11)
#[allow(clippy::too_many_arguments)]
pub async fn spawn_cli_process(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&serde_json::Value>,
    execution_context: Option<&serde_json::Value>,
    cwd: &str,
    env: HashMap<String, String>,
    timeout_secs: Option<u64>,
    run_id: &RunId,
    event_tx: mpsc::Sender<AgentRunEvent>,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<i32> {
    let mut invocation = build_cli_invocation(tool, model, prompt, runtime_contract).await?;
    let mut env = env;
    let hard_timeout_secs = timeout_secs.filter(|value| *value > 0);
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, hard_timeout_secs, runtime_contract);
    let mcp_tool_enforcement = resolve_mcp_tool_enforcement(runtime_contract);
    let execution_policy = resolve_execution_policy_enforcement(runtime_contract);
    enforce_sandbox_mode_before_launch(run_id, tool, cwd, execution_context, &execution_policy)?;
    env.insert(
        "AO_SANDBOX_MODE".to_string(),
        execution_policy.sandbox_mode.as_str().to_string(),
    );
    env.insert(
        "AO_EXECUTION_POLICY_HASH".to_string(),
        execution_policy.policy_hash.clone(),
    );
    let mut temp_cleanup = TempPathCleanup::default();
    apply_native_mcp_policy(
        &mut invocation,
        &mcp_tool_enforcement,
        &mut env,
        run_id,
        &mut temp_cleanup,
    )?;
    let prompt_len = prompt.chars().count();
    let prompt_preview = truncate_for_log(prompt, 160);

    info!(
        run_id = %run_id.0.as_str(),
        tool,
        model,
        cwd,
        command = %invocation.command,
        args = ?invocation.args,
        prompt_chars = prompt_len,
        prompt_via_stdin = invocation.prompt_via_stdin,
        has_runtime_contract = runtime_contract.is_some(),
        hard_timeout_secs = ?hard_timeout_secs,
        idle_timeout_secs = ?idle_timeout_secs,
        env_vars = env.len(),
        mcp_only = mcp_tool_enforcement.enabled,
        mcp_endpoint = ?mcp_tool_enforcement.endpoint,
        mcp_stdio_command = ?mcp_tool_enforcement
            .stdio
            .as_ref()
            .map(|config| config.command.as_str()),
        mcp_stdio_args = ?mcp_tool_enforcement
            .stdio
            .as_ref()
            .map(|config| config.args.as_slice()),
        mcp_agent_id = %mcp_tool_enforcement.agent_id,
        mcp_allowed_prefixes = ?mcp_tool_enforcement.allowed_prefixes,
        sandbox_mode = %execution_policy.sandbox_mode.as_str(),
        allow_elevated = execution_policy.allow_elevated,
        policy_hash = %execution_policy.policy_hash,
        policy_allow_prefixes = ?execution_policy.tool_policy.allow_prefixes,
        policy_allow_exact = ?execution_policy.tool_policy.allow_exact,
        policy_deny_prefixes = ?execution_policy.tool_policy.deny_prefixes,
        policy_deny_exact = ?execution_policy.tool_policy.deny_exact,
        "Spawning CLI process"
    );
    debug!(
        run_id = %run_id.0.as_str(),
        prompt_preview = %prompt_preview,
        "CLI prompt preview (truncated)"
    );

    let mut command = Command::new(&invocation.command);
    command
        .args(&invocation.args)
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to spawn CLI process '{}'", invocation.command))?;

    // Always close stdin; only some CLIs should receive the prompt via stdin.
    if let Some(mut stdin) = child.stdin.take() {
        if invocation.prompt_via_stdin && !prompt.is_empty() {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
                warn!(
                    run_id = %run_id.0.as_str(),
                    command = %invocation.command,
                    error = %e,
                    "Failed to write prompt to stdin"
                );
            } else {
                debug!(
                    run_id = %run_id.0.as_str(),
                    command = %invocation.command,
                    bytes = prompt.len(),
                    "Wrote prompt payload to stdin"
                );
            }
        }
        drop(stdin);
    }

    let pid = child.id().context("Failed to get PID")?;
    info!(
        run_id = %run_id.0.as_str(),
        pid,
        command = %invocation.command,
        "CLI process spawned"
    );
    if let Err(e) = track_process(&run_id.0, pid) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to record process in orphan tracker"
        );
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::*;
        use windows::Win32::System::Threading::OpenProcess;

        unsafe {
            if let Ok(job) = CreateJobObjectW(None, None) {
                if let Ok(process_handle) = OpenProcess(
                    windows::Win32::System::Threading::PROCESS_SET_QUOTA
                        | windows::Win32::System::Threading::PROCESS_TERMINATE,
                    false,
                    pid,
                ) {
                    if AssignProcessToJobObject(job, process_handle).is_ok() {
                        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                        if SetInformationJobObject(
                            job,
                            JobObjectExtendedLimitInformation,
                            &info as *const _ as *const _,
                            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                        )
                        .is_ok()
                        {
                            crate::cleanup::track_job(pid, job);
                        } else {
                            let _ = CloseHandle(job);
                        }
                    } else {
                        let _ = CloseHandle(job);
                    }
                    let _ = CloseHandle(process_handle);
                } else {
                    let _ = CloseHandle(job);
                }
            }
        }
    }

    let stdout = child.stdout.take().context("Failed to capture stdout")?;
    let stderr = child.stderr.take().context("Failed to capture stderr")?;

    let (output_tx, mut output_rx) = mpsc::channel::<AgentRunEvent>(100);
    let (wait_tx, mut wait_rx) = tokio::sync::oneshot::channel();

    spawn_stream_forwarders(stdout, stderr, run_id.clone(), output_tx.clone());

    drop(output_tx);

    spawn_wait_task(child, run_id.clone(), wait_tx);

    let run_id_for_select = run_id.clone();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let run_started_at = Instant::now();
    let mut last_activity_at = run_started_at;
    let mut output_chunks_total: u64 = 0;
    let mut output_chunks_stdout: u64 = 0;
    let mut output_chunks_stderr: u64 = 0;
    let mut skipped_initial_heartbeat_tick = false;
    let mcp_tool_enforcement_for_select = mcp_tool_enforcement.clone();
    let execution_policy_for_select = execution_policy.clone();

    let run_loop = async move {
        loop {
            tokio::select! {
                Some(evt) = output_rx.recv() => {
                    if let AgentRunEvent::ToolCall { tool_info, .. } = &evt {
                        if !is_tool_call_allowed_with_policy(
                            &tool_info.tool_name,
                            &tool_info.parameters,
                            &mcp_tool_enforcement_for_select,
                            &execution_policy_for_select,
                        ) {
                            let server_context = tool_info
                                .parameters
                                .get("server")
                                .and_then(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(ToString::to_string);
                            let error = serde_json::json!({
                                "code": "POLICY_VIOLATION",
                                "reason": "tool_call_blocked",
                                "tool_name": tool_info.tool_name,
                                "tool_server": server_context,
                                "policy_hash": execution_policy_for_select.policy_hash,
                                "mcp_allowed_prefixes": mcp_tool_enforcement_for_select.allowed_prefixes,
                                "policy_allow_prefixes": execution_policy_for_select.tool_policy.allow_prefixes,
                                "policy_allow_exact": execution_policy_for_select.tool_policy.allow_exact,
                                "policy_deny_prefixes": execution_policy_for_select.tool_policy.deny_prefixes,
                                "policy_deny_exact": execution_policy_for_select.tool_policy.deny_exact,
                            })
                            .to_string();
                            warn!(
                                run_id = %run_id_for_select.0.as_str(),
                                pid,
                                tool_name = %tool_info.tool_name,
                                tool_server = ?server_context,
                                allowed_prefixes = ?mcp_tool_enforcement_for_select.allowed_prefixes,
                                policy_hash = %execution_policy_for_select.policy_hash,
                                "Run emitted disallowed tool call under policy enforcement"
                            );
                            let _ = event_tx.send(evt.clone()).await;
                            let _ = event_tx.send(AgentRunEvent::Error {
                                run_id: run_id_for_select.clone(),
                                error: format!("POLICY_VIOLATION: {error}"),
                            }).await;
                            let killed = crate::cleanup::kill_process(pid as i32);
                            if !killed {
                                warn!(
                                    run_id = %run_id_for_select.0.as_str(),
                                    pid,
                                    "Failed to terminate process after policy violation"
                                );
                            }
                            if let Err(track_error) = untrack_process(&run_id_for_select.0) {
                                warn!(
                                    run_id = %run_id_for_select.0.as_str(),
                                    pid,
                                    error = %track_error,
                                    "Failed to remove process from orphan tracker after policy violation"
                                );
                            }
                            #[cfg(windows)]
                            crate::cleanup::untrack_job(pid);
                            bail!("POLICY_VIOLATION: {error}");
                        }
                    }
                    if let AgentRunEvent::OutputChunk { stream_type, text, .. } = &evt {
                        output_chunks_total += 1;
                        match stream_type {
                            OutputStreamType::Stdout => output_chunks_stdout += 1,
                            OutputStreamType::Stderr => output_chunks_stderr += 1,
                            OutputStreamType::System => {}
                        }
                        if output_chunks_total == 1 {
                            info!(
                                run_id = %run_id_for_select.0.as_str(),
                                pid,
                                stream = ?stream_type,
                                preview = %truncate_for_log(text, 200),
                                "Received first CLI output chunk"
                            );
                        }
                    }
                    last_activity_at = Instant::now();
                    let _ = event_tx.send(evt).await;
                }
                _ = heartbeat.tick() => {
                    if !skipped_initial_heartbeat_tick {
                        skipped_initial_heartbeat_tick = true;
                        continue;
                    }

                    let elapsed_secs = run_started_at.elapsed().as_secs();
                    let idle_secs = last_activity_at.elapsed().as_secs();
                    info!(
                        run_id = %run_id_for_select.0.as_str(),
                        pid,
                        elapsed_secs,
                        idle_secs,
                        output_chunks_total,
                        output_chunks_stdout,
                        output_chunks_stderr,
                        idle_timeout_secs = ?idle_timeout_secs,
                        "CLI run heartbeat"
                    );

                    if let Some(idle_limit_secs) = idle_timeout_secs {
                        if idle_secs >= idle_limit_secs {
                            warn!(
                                run_id = %run_id_for_select.0.as_str(),
                                pid,
                                idle_secs,
                                idle_limit_secs,
                                output_chunks_total,
                                "CLI run exceeded idle timeout; terminating process group"
                            );
                            let killed = crate::cleanup::kill_process(pid as i32);
                            if !killed {
                                warn!(run_id = %run_id_for_select.0.as_str(), pid, "Failed to terminate idle-timed-out process");
                            }
                            if let Err(e) = untrack_process(&run_id_for_select.0) {
                                warn!(
                                    run_id = %run_id_for_select.0.as_str(),
                                    pid,
                                    error = %e,
                                    "Failed to remove process from orphan tracker after idle timeout"
                                );
                            }
                            #[cfg(windows)]
                            crate::cleanup::untrack_job(pid);
                            bail!("Process idle timeout after {}s without activity", idle_limit_secs);
                        }
                    }
                }
                _ = &mut cancel_rx => {
                    warn!(
                        run_id = %run_id_for_select.0.as_str(),
                        pid,
                        "Process cancelled by caller; terminating process group"
                    );
                    let killed = crate::cleanup::kill_process(pid as i32);
                    if !killed {
                        warn!(run_id = %run_id_for_select.0.as_str(), pid, "Failed to terminate cancelled process");
                    }
                    if let Err(e) = untrack_process(&run_id_for_select.0) {
                        warn!(run_id = %run_id_for_select.0.as_str(), pid, error = %e, "Failed to remove process from orphan tracker");
                    }
                    #[cfg(windows)]
                    crate::cleanup::untrack_job(pid);
                    bail!("Process cancelled by user");
                }
                result = &mut wait_rx => {
                    while let Some(evt) = output_rx.recv().await {
                        let _ = event_tx.send(evt).await;
                    }
                    return match result {
                        Ok(wait_result) => wait_result.map_err(anyhow::Error::from),
                        Err(_) => Err(anyhow::anyhow!("Wait task failed")),
                    };
                }
            }
        }
    };

    let status: std::process::ExitStatus = match hard_timeout_secs {
        Some(timeout_secs) => {
            let timeout_duration = Duration::from_secs(timeout_secs);
            match timeout(timeout_duration, run_loop).await {
                Ok(Ok(status)) => status,
                Ok(Err(e)) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        error = %e,
                        "CLI process execution returned an error"
                    );
                    if let Err(untrack_err) = untrack_process(&run_id.0) {
                        warn!(
                            run_id = %run_id.0.as_str(),
                            pid,
                            error = %untrack_err,
                            "Failed to remove process from orphan tracker after execution error"
                        );
                    }
                    return Err(e);
                }
                Err(_) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        timeout_secs,
                        "CLI process timed out; terminating process group"
                    );
                    let killed = crate::cleanup::kill_process(pid as i32);
                    if !killed {
                        warn!(run_id = %run_id.0.as_str(), pid, "Failed to terminate timed-out process");
                    }
                    if let Err(e) = untrack_process(&run_id.0) {
                        warn!(
                            run_id = %run_id.0.as_str(),
                            pid,
                            error = %e,
                            "Failed to remove process from orphan tracker after timeout"
                        );
                    }
                    #[cfg(windows)]
                    crate::cleanup::untrack_job(pid);
                    bail!("Process timed out");
                }
            }
        }
        None => match run_loop.await {
            Ok(status) => status,
            Err(e) => {
                warn!(
                    run_id = %run_id.0.as_str(),
                    pid,
                    error = %e,
                    "CLI process execution returned an error"
                );
                if let Err(untrack_err) = untrack_process(&run_id.0) {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        error = %untrack_err,
                        "Failed to remove process from orphan tracker after execution error"
                    );
                }
                return Err(e);
            }
        },
    };

    if let Err(e) = untrack_process(&run_id.0) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to remove process from orphan tracker after completion"
        );
    }

    #[cfg(windows)]
    crate::cleanup::untrack_job(pid);

    let exit_code = status.code().unwrap_or(-1);
    info!(run_id = %run_id.0.as_str(), pid, exit_code, "CLI process completed");
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mcp_tool_enforcement_defaults_to_ao_prefix_when_endpoint_is_set() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "endpoint": "http://127.0.0.1:3101/mcp/ao" }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert!(enforcement.enabled);
        assert_eq!(
            enforcement.endpoint.as_deref(),
            Some("http://127.0.0.1:3101/mcp/ao")
        );
        assert_eq!(enforcement.agent_id, "ao");
        assert!(enforcement
            .allowed_prefixes
            .iter()
            .any(|prefix| prefix == "ao."));
        assert!(enforcement
            .allowed_prefixes
            .iter()
            .any(|prefix| prefix == "mcp__ao__"));
    }

    #[test]
    fn mcp_tool_enforcement_rejects_non_matching_tool_calls() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "endpoint": "http://127.0.0.1:3101/mcp/ao",
                "enforce_only": true,
                "allowed_tool_prefixes": ["ao."]
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert!(is_tool_call_allowed("ao.task.list", &json!({}), &enforcement));
        assert!(is_tool_call_allowed("phase_transition", &json!({}), &enforcement));
        assert!(!is_tool_call_allowed("Bash", &json!({}), &enforcement));
        assert!(!is_tool_call_allowed(
            "stories-search",
            &json!({ "server": "shortcut" }),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "requirements-get",
            &json!({ "server": "ao" }),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "list_mcp_resources",
            &json!({}),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "list_mcp_resources",
            &json!({ "server": "codex" }),
            &enforcement
        ));
    }

    #[test]
    fn execution_policy_parser_reads_runtime_contract_policy_block() {
        let contract = json!({
            "policy": {
                "execution": {
                    "sandbox_mode": "danger_full_access",
                    "allow_elevated": true,
                    "policy_hash": "abc123",
                    "tool_policy": {
                        "allow_prefixes": ["ao."],
                        "allow_exact": ["phase_transition"],
                        "deny_prefixes": ["bash"],
                        "deny_exact": ["ao.git.push"]
                    },
                    "elevation_approval": {
                        "request_id": "elv-123",
                        "approved": true,
                        "approved_by": "operator"
                    }
                }
            }
        });
        let policy = resolve_execution_policy_enforcement(Some(&contract));
        assert_eq!(policy.sandbox_mode, SandboxMode::DangerFullAccess);
        assert!(policy.allow_elevated);
        assert_eq!(policy.policy_hash, "abc123");
        assert_eq!(policy.tool_policy.allow_prefixes, vec!["ao.".to_string()]);
        assert_eq!(policy.tool_policy.deny_prefixes, vec!["bash".to_string()]);
        assert_eq!(
            policy
                .approval
                .as_ref()
                .map(|approval| approval.request_id.as_str()),
            Some("elv-123")
        );
    }

    #[test]
    fn execution_policy_deny_rules_override_allow_rules() {
        let mcp = McpToolEnforcement {
            enabled: false,
            endpoint: None,
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: Vec::new(),
        };
        let mut policy = ExecutionPolicyEnforcement {
            sandbox_mode: SandboxMode::WorkspaceWrite,
            tool_policy: ToolPolicyEnforcement {
                allow_prefixes: vec!["ao.".to_string()],
                allow_exact: Vec::new(),
                deny_prefixes: vec!["ao.task.".to_string()],
                deny_exact: vec!["ao.requirements.list".to_string()],
            },
            allow_elevated: false,
            policy_hash: String::new(),
            approval: None,
        };
        policy.policy_hash = execution_policy_hash(&policy);

        assert!(!is_tool_call_allowed_with_policy(
            "ao.task.list",
            &json!({}),
            &mcp,
            &policy
        ));
        assert!(!is_tool_call_allowed_with_policy(
            "ao.requirements.list",
            &json!({}),
            &mcp,
            &policy
        ));
        assert!(is_tool_call_allowed_with_policy(
            "ao.review.get",
            &json!({}),
            &mcp,
            &policy
        ));
    }

    #[test]
    fn sandbox_elevation_approval_is_single_use() {
        let temp = std::env::temp_dir().join(format!(
            "ao-policy-elevation-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir should be created");
        let run_id = RunId("run-sandbox-1".to_string());
        let mut policy = ExecutionPolicyEnforcement {
            sandbox_mode: SandboxMode::ReadOnly,
            tool_policy: ToolPolicyEnforcement::default(),
            allow_elevated: true,
            policy_hash: "policy-1".to_string(),
            approval: None,
        };
        let request_id =
            elevation_request_id(&run_id, &policy.policy_hash, "sandbox_read_only_override");
        policy.approval = Some(ElevationApproval {
            request_id,
            approved: true,
            approved_by: Some("operator".to_string()),
            comment: None,
        });
        let context = json!({
            "project_root": temp.to_string_lossy(),
            "workflow_id": "wf-1",
            "task_id": "TASK-1",
            "phase_id": "implementation",
            "agent_id": "implementation"
        });

        enforce_sandbox_mode_before_launch(
            &run_id,
            "codex",
            temp.to_string_lossy().as_ref(),
            Some(&context),
            &policy,
        )
        .expect("first approval use should pass");

        let err = enforce_sandbox_mode_before_launch(
            &run_id,
            "codex",
            temp.to_string_lossy().as_ref(),
            Some(&context),
            &policy,
        )
        .expect_err("approval replay should fail");
        assert!(err.to_string().contains("POLICY_VIOLATION"));
        assert!(err.to_string().contains("approval already consumed"));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn native_mcp_policy_rejects_unknown_cli_when_enforced() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "unknown-cli".to_string(),
            args: vec!["hello".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            }),
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-1".to_string());

        let err = apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect_err("unknown provider should fail closed");

        assert!(err.to_string().contains("no native enforcement adapter"));
    }

    #[test]
    fn native_mcp_policy_requires_transport_when_enforced() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "claude".to_string(),
            args: vec!["--print".to_string(), "hello".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-1b".to_string());

        let err = apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect_err("missing transport should fail closed");

        assert!(err
            .to_string()
            .contains("neither mcp.endpoint nor mcp.stdio.command"));
    }

    #[test]
    fn native_mcp_policy_adds_codex_mcp_server_override() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "codex".to_string(),
            args: vec![
                "exec".to_string(),
                "--json".to_string(),
                "hello".to_string(),
            ],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-2".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("codex policy should apply");

        let joined = invocation.args.join(" ");
        assert!(joined.contains("mcp_servers.ao.url=\"http://127.0.0.1:3101/mcp/ao\""));
    }

    #[test]
    fn native_mcp_policy_configures_claude_permission_mode() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "claude".to_string(),
            args: vec!["--print".to_string(), "hello".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-claude".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("claude policy should apply");

        assert!(invocation.args.windows(2).any(|pair| {
            pair[0] == "--permission-mode" && pair[1] == "bypassPermissions"
        }));
        assert!(invocation.args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(!invocation.args.iter().any(|arg| arg == "--tools"));
    }

    #[test]
    fn parse_codex_mcp_server_names_extracts_safe_names() {
        let payload = r#"
            [
              {"name":"ao"},
              {"name":"shortcut"},
              {"name":"bad.name"},
              {"name":"with space"}
            ]
        "#;
        assert_eq!(
            parse_codex_mcp_server_names(payload),
            vec!["ao".to_string(), "shortcut".to_string()]
        );
    }

    #[test]
    fn codex_native_lockdown_disables_non_target_servers() {
        let mut args = vec!["exec".to_string(), "--json".to_string(), "hello".to_string()];
        let configured_servers = vec!["shortcut".to_string(), "ao".to_string()];

        apply_codex_native_mcp_lockdown(
            &mut args,
            McpServerTransport::Http("http://127.0.0.1:3101/mcp/ao"),
            "ao",
            &configured_servers,
        );

        let joined = args.join(" ");
        assert!(joined.contains("mcp_servers.shortcut.enabled=false"));
        assert!(joined.contains("mcp_servers.ao.url=\"http://127.0.0.1:3101/mcp/ao\""));
        assert!(!joined.contains("mcp_servers.ao.enabled=false"));
    }

    #[test]
    fn codex_native_lockdown_sets_stdio_transport_when_configured() {
        let mut args = vec!["exec".to_string(), "--json".to_string(), "hello".to_string()];

        apply_codex_native_mcp_lockdown(
            &mut args,
            McpServerTransport::Stdio {
                command: "/Users/samishukri/ao-cli/target/debug/ao",
                args: &[
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            },
            "ao",
            &[],
        );

        let joined = args.join(" ");
        assert!(joined.contains(
            "mcp_servers.ao.command=\"/Users/samishukri/ao-cli/target/debug/ao\""
        ));
        assert!(joined.contains(
            "mcp_servers.ao.args=[\"--project-root\", \"/Users/samishukri/ao-cli\", \"mcp\", \"serve\"]"
        ));
        assert!(joined.contains("mcp_servers.ao.enabled=true"));
    }

    #[test]
    fn native_mcp_policy_sets_gemini_system_settings_path_for_stdio_transport() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "gemini".to_string(),
            args: vec!["--output-format".to_string(), "json".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            }),
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-3".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("gemini policy should apply");

        let settings_path = env
            .get("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
            .expect("gemini settings path should be set")
            .to_string();
        assert!(invocation
            .args
            .windows(2)
            .any(|pair| pair[0] == "--allowed-mcp-server-names" && pair[1] == "ao"));
        let settings = std::fs::read_to_string(&settings_path).expect("read gemini settings");
        assert!(
            settings.contains("\"AO_MCP_SCHEMA_DRAFT\":\"draft07\""),
            "expected draft07 env in gemini settings, got: {settings}"
        );
        assert!(
            settings.contains("\"type\":\"stdio\""),
            "expected stdio transport in gemini settings, got: {settings}"
        );
    }

    #[test]
    fn native_mcp_policy_sets_gemini_http_settings_without_schema_override() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "gemini".to_string(),
            args: vec!["--output-format".to_string(), "json".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-3-http".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("gemini policy should apply");

        let settings_path = env
            .get("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
            .expect("gemini settings path should be set")
            .to_string();
        let settings = std::fs::read_to_string(&settings_path).expect("read gemini settings");
        assert!(
            settings.contains("\"type\":\"http\""),
            "expected http transport in gemini settings, got: {settings}"
        );
        assert!(
            settings.contains("\"url\":\"http://127.0.0.1:3101/mcp/ao\""),
            "expected ao endpoint in gemini settings, got: {settings}"
        );
        assert!(
            !settings.contains("\"AO_MCP_SCHEMA_DRAFT\""),
            "did not expect schema override env for gemini http transport, got: {settings}"
        );
    }

    #[test]
    fn native_mcp_policy_sets_opencode_local_mcp_command_array() {
        let mut invocation = super::super::process_builder::CliInvocation {
            command: "opencode".to_string(),
            args: vec!["run".to_string(), "--format".to_string(), "json".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            }),
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-opencode".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("opencode policy should apply");

        let config_raw = env
            .get("OPENCODE_CONFIG_CONTENT")
            .expect("opencode config should be provided");
        let parsed: serde_json::Value =
            serde_json::from_str(config_raw).expect("opencode config should be valid JSON");
        assert_eq!(
            parsed.pointer("/mcp/ao/type").and_then(serde_json::Value::as_str),
            Some("local")
        );
        assert_eq!(
            parsed
                .pointer("/mcp/ao/command/0")
                .and_then(serde_json::Value::as_str),
            Some("/Users/samishukri/ao-cli/target/debug/ao")
        );
        assert_eq!(
            parsed
                .pointer("/mcp/ao/command/4")
                .and_then(serde_json::Value::as_str),
            Some("serve")
        );
        assert!(parsed.pointer("/mcp/ao/args").is_none());
    }
}
