use anyhow::{anyhow, Context, Result};
use orchestrator_core::{DependencyType, Priority, ProjectType, TaskStatus, TaskType};
use protocol::{AgentRunEvent, RunId};
use serde_json::Value;

use crate::{event_matches_run, run_dir};

pub(crate) fn parse_input_json_or<T, F>(input_json: Option<String>, fallback: F) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce() -> Result<T>,
{
    match input_json {
        Some(raw) => {
            serde_json::from_str::<T>(&raw).context("failed to parse --input-json payload")
        }
        None => fallback(),
    }
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
    let status = match value {
        "todo" | "backlog" => TaskStatus::Backlog,
        "ready" => TaskStatus::Ready,
        "in_progress" | "in-progress" => TaskStatus::InProgress,
        "done" => TaskStatus::Done,
        "blocked" => TaskStatus::Blocked,
        "on_hold" | "on-hold" => TaskStatus::OnHold,
        "cancelled" => TaskStatus::Cancelled,
        _ => anyhow::bail!("invalid status: {value}"),
    };

    Ok(status)
}

pub(crate) fn parse_task_type_opt(value: Option<&str>) -> Result<Option<TaskType>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let task_type = match value {
        "feature" => TaskType::Feature,
        "bugfix" => TaskType::Bugfix,
        "hotfix" => TaskType::Hotfix,
        "refactor" => TaskType::Refactor,
        "docs" => TaskType::Docs,
        "test" => TaskType::Test,
        "chore" => TaskType::Chore,
        "experiment" => TaskType::Experiment,
        _ => anyhow::bail!("invalid task_type: {value}"),
    };

    Ok(Some(task_type))
}

pub(crate) fn parse_priority_opt(value: Option<&str>) -> Result<Option<Priority>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let priority = match value {
        "critical" => Priority::Critical,
        "high" => Priority::High,
        "medium" => Priority::Medium,
        "low" => Priority::Low,
        _ => anyhow::bail!("invalid priority: {value}"),
    };

    Ok(Some(priority))
}

pub(crate) fn parse_dependency_type(value: &str) -> Result<DependencyType> {
    let dependency_type = match value {
        "blocks-by" | "blocks_by" | "blocksby" => DependencyType::BlocksBy,
        "blocked-by" | "blocked_by" | "blockedby" => DependencyType::BlockedBy,
        "related-to" | "related_to" | "relatedto" => DependencyType::RelatedTo,
        _ => anyhow::bail!("invalid dependency_type: {value}"),
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
        _ => anyhow::bail!("invalid project_type: {}", value.trim()),
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
        assert!(err.to_string().contains("invalid project_type"));
    }
}
