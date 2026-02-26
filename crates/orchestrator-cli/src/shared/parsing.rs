use anyhow::{anyhow, Context, Result};
use orchestrator_core::{DependencyType, Priority, ProjectType, TaskStatus, TaskType};
use protocol::{AgentRunEvent, RunId};
use serde_json::Value;

use crate::{event_matches_run, run_dir};

const TASK_STATUS_EXPECTED: &str =
    "backlog|todo, ready, in-progress|in_progress, blocked, on-hold|on_hold, done, cancelled";
const TASK_TYPE_EXPECTED: &str = "feature, bugfix, hotfix, refactor, docs, test, chore, experiment";
const PRIORITY_EXPECTED: &str = "critical, high, medium, low";
const DEPENDENCY_TYPE_EXPECTED: &str =
    "blocks-by|blocks_by, blocked-by|blocked_by, related-to|related_to";
const PROJECT_TYPE_EXPECTED: &str = "web-app, mobile-app, desktop-app, full-stack-platform|full-stack|saas, library, infrastructure, other";
pub(crate) const COMMAND_HELP_HINT: &str = "run '<command> --help'";

fn invalid_value_error(domain: &str, value: &str, expected: &str) -> anyhow::Error {
    let value = value.trim();
    let normalized_value = if value.is_empty() { "<empty>" } else { value };
    anyhow!("invalid {domain} '{normalized_value}'; expected one of: {expected}; {COMMAND_HELP_HINT}")
}

pub(crate) fn parse_input_json_or<T, F>(input_json: Option<String>, fallback: F) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce() -> Result<T>,
{
    match input_json {
        Some(raw) => {
            serde_json::from_str::<T>(&raw).context("failed to parse --input-json payload as JSON")
        }
        None => fallback(),
    }
}

pub(crate) fn ensure_destructive_confirmation(
    confirm: Option<&str>,
    expected: &str,
    action: &str,
) -> Result<()> {
    let expected = expected.trim();
    if expected.is_empty() {
        anyhow::bail!("invalid confirmation token for {action}");
    }

    let provided = confirm.map(str::trim).filter(|value| !value.is_empty());
    if provided == Some(expected) {
        return Ok(());
    }

    anyhow::bail!(
        "CONFIRMATION_REQUIRED: rerun '{}' with --confirm {}; use --dry-run to preview changes",
        action,
        expected
    );
}

pub(crate) fn read_agent_status(
    project_root: &str,
    run_id: &str,
    jsonl_dir_override: Option<&str>,
) -> Result<Value> {
    let run_id = RunId(run_id.to_string());
    let events_path = run_dir(project_root, &run_id, jsonl_dir_override).join("events.jsonl");
    if !events_path.exists() {
        return Err(anyhow!(
            "no event log found for run {} at {}",
            run_id.0,
            events_path.display()
        ));
    }

    let mut event_count = 0usize;
    let mut status = "unknown".to_string();
    let mut exit_code: Option<i32> = None;
    let mut duration_ms: Option<u64> = None;
    let mut last_error: Option<String> = None;
    let mut started_at: Option<String> = None;

    let content = std::fs::read_to_string(&events_path)?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }
        event_count = event_count.saturating_add(1);

        match event {
            AgentRunEvent::Started { timestamp, .. } => {
                status = "running".to_string();
                started_at = Some(timestamp.0.to_rfc3339());
            }
            AgentRunEvent::OutputChunk { .. } => {
                if status == "unknown" {
                    status = "running".to_string();
                }
            }
            AgentRunEvent::Metadata { .. } => {}
            AgentRunEvent::Error { error, .. } => {
                status = "failed".to_string();
                last_error = Some(error);
            }
            AgentRunEvent::Finished {
                exit_code: code,
                duration_ms: duration,
                ..
            } => {
                exit_code = code;
                duration_ms = Some(duration);
                status = if code.unwrap_or_default() == 0 {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                };
            }
            AgentRunEvent::ToolCall { .. }
            | AgentRunEvent::ToolResult { .. }
            | AgentRunEvent::Artifact { .. }
            | AgentRunEvent::Thinking { .. } => {
                if status == "unknown" {
                    status = "running".to_string();
                }
            }
        }
    }

    Ok(serde_json::json!({
        "run_id": run_id.0,
        "status": status,
        "event_count": event_count,
        "started_at": started_at,
        "exit_code": exit_code,
        "duration_ms": duration_ms,
        "last_error": last_error,
        "events_path": events_path,
    }))
}

pub(crate) fn parse_task_status(value: &str) -> Result<TaskStatus> {
    let normalized = value.trim().to_ascii_lowercase();
    let status = match normalized.as_str() {
        "todo" | "backlog" => TaskStatus::Backlog,
        "ready" => TaskStatus::Ready,
        "in_progress" | "in-progress" => TaskStatus::InProgress,
        "done" => TaskStatus::Done,
        "blocked" => TaskStatus::Blocked,
        "on_hold" | "on-hold" => TaskStatus::OnHold,
        "cancelled" => TaskStatus::Cancelled,
        _ => return Err(invalid_value_error("status", value, TASK_STATUS_EXPECTED)),
    };

    Ok(status)
}

pub(crate) fn parse_task_type_opt(value: Option<&str>) -> Result<Option<TaskType>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase();
    let task_type = match normalized.as_str() {
        "feature" => TaskType::Feature,
        "bugfix" => TaskType::Bugfix,
        "hotfix" => TaskType::Hotfix,
        "refactor" => TaskType::Refactor,
        "docs" => TaskType::Docs,
        "test" => TaskType::Test,
        "chore" => TaskType::Chore,
        "experiment" => TaskType::Experiment,
        _ => return Err(invalid_value_error("task_type", value, TASK_TYPE_EXPECTED)),
    };

    Ok(Some(task_type))
}

pub(crate) fn parse_priority_opt(value: Option<&str>) -> Result<Option<Priority>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase();
    let priority = match normalized.as_str() {
        "critical" => Priority::Critical,
        "high" => Priority::High,
        "medium" => Priority::Medium,
        "low" => Priority::Low,
        _ => return Err(invalid_value_error("priority", value, PRIORITY_EXPECTED)),
    };

    Ok(Some(priority))
}

pub(crate) fn parse_dependency_type(value: &str) -> Result<DependencyType> {
    let normalized = value.trim().to_ascii_lowercase();
    let dependency_type = match normalized.as_str() {
        "blocks-by" | "blocks_by" | "blocksby" => DependencyType::BlocksBy,
        "blocked-by" | "blocked_by" | "blockedby" => DependencyType::BlockedBy,
        "related-to" | "related_to" | "relatedto" => DependencyType::RelatedTo,
        _ => {
            return Err(invalid_value_error(
                "dependency_type",
                value,
                DEPENDENCY_TYPE_EXPECTED,
            ))
        }
    };

    Ok(dependency_type)
}

pub(crate) fn parse_project_type_opt(value: Option<&str>) -> Result<Option<ProjectType>> {
    let Some(value) = value else {
        return Ok(Some(ProjectType::Other));
    };

    let normalized = value.trim().to_ascii_lowercase();
    let project_type = match normalized.as_str() {
        "web-app" | "web_app" | "webapp" => ProjectType::WebApp,
        "mobile-app" | "mobile_app" | "mobileapp" => ProjectType::MobileApp,
        "desktop-app" | "desktop_app" | "desktopapp" => ProjectType::DesktopApp,
        "full-stack-platform"
        | "full_stack_platform"
        | "fullstackplatform"
        | "full-stack"
        | "full_stack"
        | "fullstack"
        | "saas" => ProjectType::FullStackPlatform,
        "library" => ProjectType::Library,
        "infrastructure" => ProjectType::Infrastructure,
        "other" | "greenfield" | "existing" => ProjectType::Other,
        _ => {
            return Err(invalid_value_error(
                "project_type",
                value,
                PROJECT_TYPE_EXPECTED,
            ))
        }
    };

    Ok(Some(project_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_project_type_accepts_saas_alias() {
        let parsed = parse_project_type_opt(Some("saas"))
            .expect("saas alias should parse")
            .expect("project type should be present");
        assert_eq!(parsed, ProjectType::FullStackPlatform);
    }

    #[test]
    fn parse_project_type_is_case_insensitive_and_trimmed() {
        let parsed = parse_project_type_opt(Some("  WeB-aPp  "))
            .expect("mixed-case value should parse")
            .expect("project type should be present");
        assert_eq!(parsed, ProjectType::WebApp);
    }

    #[test]
    fn parse_project_type_rejects_unknown_values() {
        let err = parse_project_type_opt(Some("nonsense")).expect_err("unknown value should fail");
        let message = err.to_string();
        assert!(message.contains("invalid project_type"));
        assert!(message.contains("expected one of"));
        assert!(message.contains("--help"));
    }

    #[test]
    fn parse_task_status_is_case_insensitive_and_trimmed() {
        assert_eq!(
            parse_task_status("  In_Progress  ").expect("mixed-case aliases should parse"),
            TaskStatus::InProgress
        );
    }

    #[test]
    fn parse_task_status_rejects_unknown_values_with_actionable_message() {
        let err = parse_task_status("invalid-status").expect_err("unknown status should fail");
        let message = err.to_string();
        assert!(message.contains("invalid status"));
        assert!(message.contains("expected one of"));
        assert!(message.contains("in-progress|in_progress"));
        assert!(message.contains("run '<command> --help'"));
    }

    #[test]
    fn parse_priority_rejects_unknown_values_with_actionable_message() {
        let err = parse_priority_opt(Some("urgent")).expect_err("unknown priority should fail");
        let message = err.to_string();
        assert!(message.contains("invalid priority"));
        assert!(message.contains("critical, high, medium, low"));
        assert!(message.contains("run '<command> --help'"));
    }

    #[test]
    fn parse_priority_rejects_empty_values_with_explicit_placeholder() {
        let err = parse_priority_opt(Some("   ")).expect_err("empty priority should fail");
        let message = err.to_string();
        assert!(message.contains("invalid priority '<empty>'"));
        assert!(message.contains("run '<command> --help'"));
    }

    #[test]
    fn parse_dependency_type_rejects_unknown_values_with_actionable_message() {
        let err =
            parse_dependency_type("unrelated").expect_err("unknown dependency type should fail");
        let message = err.to_string();
        assert!(message.contains("invalid dependency_type"));
        assert!(message.contains("blocks-by|blocks_by"));
        assert!(message.contains("run '<command> --help'"));
    }

    #[test]
    fn destructive_confirmation_accepts_matching_token() {
        ensure_destructive_confirmation(Some("TASK-123"), "TASK-123", "task.delete")
            .expect("matching token should pass");
    }

    #[test]
    fn destructive_confirmation_requires_exact_token() {
        let error = ensure_destructive_confirmation(Some("wrong"), "TASK-123", "task.delete")
            .expect_err("mismatched token should fail");
        let message = error.to_string();
        assert!(message.contains("CONFIRMATION_REQUIRED"));
        assert!(message.contains("--confirm TASK-123"));
        assert!(message.contains("--dry-run"));
    }
}
