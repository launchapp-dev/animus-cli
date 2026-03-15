use anyhow::Result;
use orchestrator_core::{
    DependencyType, Priority, ProjectType, RequirementPriority, RequirementQuerySort, RequirementStatus,
    RequirementType, RiskLevel, TaskQuerySort, TaskStatus, TaskType, WorkflowQuerySort, WorkflowStatus,
};
use protocol::{AgentRunEvent, RunId};
use serde_json::Value;

use crate::{ensure_safe_run_id, event_matches_run, invalid_input_error, not_found_error, run_dir};

const TASK_STATUS_EXPECTED: &str = "backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled";
const TASK_TYPE_EXPECTED: &str = "feature|bugfix|hotfix|refactor|docs|test|chore|experiment";
const PRIORITY_EXPECTED: &str = "critical|high|medium|low";
const TASK_SORT_EXPECTED: &str = "priority|updated-at|updated_at|created-at|created_at|id";
const REQUIREMENT_PRIORITY_EXPECTED: &str = "must|should|could|wont|won't";
const REQUIREMENT_STATUS_EXPECTED: &str = "draft|refined|planned|in-progress|in_progress|done";
const REQUIREMENT_CATEGORY_EXPECTED: &str = "documentation|usability|runtime|integration|quality|release|security";
const REQUIREMENT_TYPE_EXPECTED: &str =
    "product|functional|non-functional|nonfunctional|non_functional|technical|other";
const REQUIREMENT_SORT_EXPECTED: &str = "id|updated-at|updated_at|priority|status";
const WORKFLOW_STATUS_EXPECTED: &str = "pending|running|paused|completed|failed|escalated|cancelled";
const WORKFLOW_SORT_EXPECTED: &str = "started-at|started_at|status|workflow-ref|workflow_ref|id";
const DEPENDENCY_TYPE_EXPECTED: &str =
    "blocks-by|blocks_by|blocksby|blocked-by|blocked_by|blockedby|related-to|related_to|relatedto";
const PROJECT_TYPE_EXPECTED: &str =
    "web-app|mobile-app|desktop-app|full-stack-platform|full-stack|saas|library|infrastructure|other";
pub(crate) const COMMAND_HELP_HINT: &str = "run the same command with --help";

fn invalid_value_error(domain: &str, value: &str, expected: &str) -> anyhow::Error {
    let value = value.trim();
    let normalized_value = if value.is_empty() { "<empty>" } else { value };
    invalid_input_error(format!(
        "invalid {domain} '{normalized_value}'; expected one of: {expected}; {COMMAND_HELP_HINT}"
    ))
}

pub(crate) fn parse_input_json_or<T, F>(input_json: Option<String>, fallback: F) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce() -> Result<T>,
{
    match input_json {
        Some(raw) => serde_json::from_str::<T>(&raw).map_err(|error| {
            invalid_input_error(format!(
                "failed to parse --input-json payload as JSON: {error}; {COMMAND_HELP_HINT} for the expected shape"
            ))
        }),
        None => fallback(),
    }
}

pub(crate) fn ensure_destructive_confirmation(
    confirm: Option<&str>,
    expected: &str,
    command_path: &str,
    id_flag: &str,
) -> Result<()> {
    let expected = expected.trim();
    if expected.is_empty() {
        return Err(invalid_input_error(format!("invalid confirmation token for {command_path}")));
    }

    let command_path = command_path.trim();
    if command_path.is_empty() {
        return Err(invalid_input_error("invalid confirmation command path"));
    }

    let id_flag = id_flag.trim();
    if id_flag.is_empty() || !id_flag.starts_with("--") {
        return Err(invalid_input_error(format!("invalid confirmation id flag for {command_path}")));
    }

    let provided = confirm.map(str::trim).filter(|value| !value.is_empty());
    if provided == Some(expected) {
        return Ok(());
    }

    Err(invalid_input_error(format!(
        "CONFIRMATION_REQUIRED: rerun 'ao {command_path} {id_flag} {expected} --confirm {expected}'; use --dry-run to preview changes"
    )))
}

pub(crate) fn read_agent_status(project_root: &str, run_id: &str, jsonl_dir_override: Option<&str>) -> Result<Value> {
    ensure_safe_run_id(run_id)?;
    let run_id = RunId(run_id.to_string());
    let events_path = run_dir(project_root, &run_id, jsonl_dir_override).join("events.jsonl");
    if !events_path.exists() {
        return Err(not_found_error(format!("no event log found for run {} at {}", run_id.0, events_path.display())));
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
            AgentRunEvent::Finished { exit_code: code, duration_ms: duration, .. } => {
                exit_code = code;
                duration_ms = Some(duration);
                status = if code.unwrap_or_default() == 0 { "completed".to_string() } else { "failed".to_string() };
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
    value.parse().map_err(|_| invalid_value_error("status", value, TASK_STATUS_EXPECTED))
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
        _ => return Err(invalid_value_error("task type", value, TASK_TYPE_EXPECTED)),
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

pub(crate) fn parse_task_query_sort_opt(value: Option<&str>) -> Result<Option<TaskQuerySort>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let sort = match normalized.as_str() {
        "priority" => TaskQuerySort::Priority,
        "updated-at" => TaskQuerySort::UpdatedAt,
        "created-at" => TaskQuerySort::CreatedAt,
        "id" => TaskQuerySort::Id,
        _ => return Err(invalid_value_error("task sort", value, TASK_SORT_EXPECTED)),
    };

    Ok(Some(sort))
}

pub(crate) fn parse_requirement_priority_opt(value: Option<&str>) -> Result<Option<RequirementPriority>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase();
    let priority = match normalized.as_str() {
        "must" => RequirementPriority::Must,
        "should" => RequirementPriority::Should,
        "could" => RequirementPriority::Could,
        "wont" | "won't" => RequirementPriority::Wont,
        _ => return Err(invalid_value_error("requirement priority", value, REQUIREMENT_PRIORITY_EXPECTED)),
    };

    Ok(Some(priority))
}

pub(crate) fn parse_requirement_status_opt(value: Option<&str>) -> Result<Option<RequirementStatus>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let status = match normalized.as_str() {
        "draft" => RequirementStatus::Draft,
        "refined" => RequirementStatus::Refined,
        "planned" => RequirementStatus::Planned,
        "in-progress" => RequirementStatus::InProgress,
        "done" => RequirementStatus::Done,
        _ => return Err(invalid_value_error("requirement status", value, REQUIREMENT_STATUS_EXPECTED)),
    };

    Ok(Some(status))
}

pub(crate) fn parse_requirement_category_opt(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase();
    let category = match normalized.as_str() {
        "documentation" | "usability" | "runtime" | "integration" | "quality" | "release" | "security" => normalized,
        _ => return Err(invalid_value_error("requirement category", value, REQUIREMENT_CATEGORY_EXPECTED)),
    };

    Ok(Some(category))
}

pub(crate) fn parse_requirement_type_opt(value: Option<&str>) -> Result<Option<RequirementType>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let requirement_type = match normalized.as_str() {
        "product" => RequirementType::Product,
        "functional" => RequirementType::Functional,
        "non-functional" => RequirementType::NonFunctional,
        "technical" => RequirementType::Technical,
        "other" => RequirementType::Other,
        _ => return Err(invalid_value_error("requirement type", value, REQUIREMENT_TYPE_EXPECTED)),
    };

    Ok(Some(requirement_type))
}

pub(crate) fn parse_requirement_query_sort_opt(value: Option<&str>) -> Result<Option<RequirementQuerySort>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let sort = match normalized.as_str() {
        "id" => RequirementQuerySort::Id,
        "updated-at" => RequirementQuerySort::UpdatedAt,
        "priority" => RequirementQuerySort::Priority,
        "status" => RequirementQuerySort::Status,
        _ => return Err(invalid_value_error("requirement sort", value, REQUIREMENT_SORT_EXPECTED)),
    };

    Ok(Some(sort))
}

pub(crate) fn parse_risk_opt(value: Option<&str>) -> Result<Option<RiskLevel>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase();
    let risk = match normalized.as_str() {
        "high" => RiskLevel::High,
        "medium" => RiskLevel::Medium,
        "low" => RiskLevel::Low,
        _ => return Err(invalid_value_error("risk level", value, "high|medium|low")),
    };

    Ok(Some(risk))
}

pub(crate) fn parse_workflow_status_opt(value: Option<&str>) -> Result<Option<WorkflowStatus>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let status = match normalized.as_str() {
        "pending" => WorkflowStatus::Pending,
        "running" => WorkflowStatus::Running,
        "paused" => WorkflowStatus::Paused,
        "completed" => WorkflowStatus::Completed,
        "failed" => WorkflowStatus::Failed,
        "escalated" => WorkflowStatus::Escalated,
        "cancelled" => WorkflowStatus::Cancelled,
        _ => return Err(invalid_value_error("workflow status", value, WORKFLOW_STATUS_EXPECTED)),
    };

    Ok(Some(status))
}

pub(crate) fn parse_workflow_query_sort_opt(value: Option<&str>) -> Result<Option<WorkflowQuerySort>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let sort = match normalized.as_str() {
        "started-at" => WorkflowQuerySort::StartedAt,
        "status" => WorkflowQuerySort::Status,
        "workflow-ref" => WorkflowQuerySort::WorkflowRef,
        "id" => WorkflowQuerySort::Id,
        _ => return Err(invalid_value_error("workflow sort", value, WORKFLOW_SORT_EXPECTED)),
    };

    Ok(Some(sort))
}

pub(crate) fn parse_dependency_type(value: &str) -> Result<DependencyType> {
    let normalized = value.trim().to_ascii_lowercase();
    let dependency_type = match normalized.as_str() {
        "blocks-by" | "blocks_by" | "blocksby" => DependencyType::BlocksBy,
        "blocked-by" | "blocked_by" | "blockedby" => DependencyType::BlockedBy,
        "related-to" | "related_to" | "relatedto" => DependencyType::RelatedTo,
        _ => return Err(invalid_value_error("dependency type", value, DEPENDENCY_TYPE_EXPECTED)),
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
        _ => return Err(invalid_value_error("project type", value, PROJECT_TYPE_EXPECTED)),
    };

    Ok(Some(project_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentRunEvent, RunId, Timestamp};

    use protocol::test_utils::EnvVarGuard;

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
        assert!(message.contains("invalid project type"));
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
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_priority_rejects_unknown_values_with_actionable_message() {
        let err = parse_priority_opt(Some("urgent")).expect_err("unknown priority should fail");
        let message = err.to_string();
        assert!(message.contains("invalid priority"));
        assert!(message.contains(PRIORITY_EXPECTED));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_priority_rejects_empty_values_with_explicit_placeholder() {
        let err = parse_priority_opt(Some("   ")).expect_err("empty priority should fail");
        let message = err.to_string();
        assert!(message.contains("invalid priority '<empty>'"));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_task_query_sort_accepts_aliases() {
        assert_eq!(parse_task_query_sort_opt(Some("updated_at")).unwrap(), Some(TaskQuerySort::UpdatedAt));
    }

    #[test]
    fn parse_requirement_filters_accept_aliases() {
        assert_eq!(parse_requirement_priority_opt(Some("won't")).unwrap(), Some(RequirementPriority::Wont));
        assert_eq!(parse_requirement_status_opt(Some("in_progress")).unwrap(), Some(RequirementStatus::InProgress));
        assert_eq!(parse_requirement_type_opt(Some("non_functional")).unwrap(), Some(RequirementType::NonFunctional));
        assert_eq!(
            parse_requirement_query_sort_opt(Some("updated_at")).unwrap(),
            Some(RequirementQuerySort::UpdatedAt)
        );
    }

    #[test]
    fn parse_workflow_filters_accept_aliases() {
        assert_eq!(parse_workflow_status_opt(Some("running")).unwrap(), Some(WorkflowStatus::Running));
        assert_eq!(parse_workflow_query_sort_opt(Some("workflow_ref")).unwrap(), Some(WorkflowQuerySort::WorkflowRef));
    }

    #[test]
    fn parse_dependency_type_rejects_unknown_values_with_actionable_message() {
        let err = parse_dependency_type("unrelated").expect_err("unknown dependency type should fail");
        let message = err.to_string();
        assert!(message.contains("invalid dependency type"));
        assert!(message.contains("blocks-by|blocks_by|blocksby"));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn parse_input_json_or_reports_help_hint_on_invalid_json() {
        let err =
            parse_input_json_or::<serde_json::Value, _>(Some("{invalid".to_string()), || Ok(serde_json::Value::Null))
                .expect_err("invalid json should fail");
        let message = err.to_string();
        assert!(message.contains("failed to parse --input-json payload as JSON"));
        assert!(message.contains(COMMAND_HELP_HINT));
    }

    #[test]
    fn destructive_confirmation_accepts_matching_token() {
        ensure_destructive_confirmation(Some("TASK-123"), "TASK-123", "task delete", "--id")
            .expect("matching token should pass");
    }

    #[test]
    fn destructive_confirmation_requires_exact_token() {
        let error = ensure_destructive_confirmation(Some("wrong"), "TASK-123", "task delete", "--id")
            .expect_err("mismatched token should fail");
        let message = error.to_string();
        assert!(message.contains("CONFIRMATION_REQUIRED"));
        assert!(message.contains("ao task delete --id TASK-123 --confirm TASK-123"));
        assert!(message.contains("--dry-run"));
    }

    #[test]
    fn destructive_confirmation_requires_non_empty_command_path() {
        let error = ensure_destructive_confirmation(None, "TASK-123", "   ", "--id")
            .expect_err("empty command path should fail");
        assert!(error.to_string().contains("invalid confirmation command path"));
    }

    #[test]
    fn destructive_confirmation_requires_long_form_id_flag() {
        let error = ensure_destructive_confirmation(None, "TASK-123", "task delete", "id")
            .expect_err("non long-form id flag should fail");
        assert!(error.to_string().contains("invalid confirmation id flag for task delete"));
    }

    #[test]
    fn read_agent_status_reads_scoped_events_and_reports_path() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");

        let run_id = "trace-run-status-010";
        let run_id_value = RunId(run_id.to_string());
        let events_path = run_dir(project_root.to_string_lossy().as_ref(), &run_id_value, None).join("events.jsonl");

        let started = serde_json::to_string(&AgentRunEvent::Started {
            run_id: run_id_value.clone(),
            timestamp: Timestamp::now(),
        })
        .expect("started event should serialize");
        let other_started = serde_json::to_string(&AgentRunEvent::Started {
            run_id: RunId("other-run".to_string()),
            timestamp: Timestamp::now(),
        })
        .expect("started event should serialize");
        let finished = serde_json::to_string(&AgentRunEvent::Finished {
            run_id: run_id_value.clone(),
            exit_code: Some(0),
            duration_ms: 42,
        })
        .expect("finished event should serialize");
        std::fs::create_dir_all(events_path.parent().expect("events path should include parent directory"))
            .expect("events directory should be created");
        std::fs::write(&events_path, format!("{started}\n{other_started}\n{finished}\n"))
            .expect("events file should be written");

        let status = read_agent_status(project_root.to_string_lossy().as_ref(), run_id, None)
            .expect("status should be read from fallback event log");
        assert_eq!(status.get("status").and_then(Value::as_str), Some("completed"));
        assert_eq!(status.get("event_count").and_then(Value::as_u64), Some(2));
        assert_eq!(status.get("duration_ms").and_then(Value::as_u64), Some(42));
        assert_eq!(status.get("events_path").and_then(Value::as_str), Some(events_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn read_agent_status_keeps_lookup_repo_scoped_under_global_runner_scope() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));
        let override_dir = temp.path().join("override-config");
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", Some(override_dir.to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");

        let run_id = "trace-run-status-global-scope";
        let run_id_value = RunId(run_id.to_string());
        let canonical_events_path =
            run_dir(project_root.to_string_lossy().as_ref(), &run_id_value, None).join("events.jsonl");
        let override_events_path = override_dir.join("runs").join(run_id).join("events.jsonl");

        let started = serde_json::to_string(&AgentRunEvent::Started {
            run_id: run_id_value.clone(),
            timestamp: Timestamp::now(),
        })
        .expect("started event should serialize");
        let finished = serde_json::to_string(&AgentRunEvent::Finished {
            run_id: run_id_value.clone(),
            exit_code: Some(0),
            duration_ms: 99,
        })
        .expect("finished event should serialize");
        std::fs::create_dir_all(canonical_events_path.parent().expect("events path should include parent directory"))
            .expect("canonical events directory should be created");
        std::fs::write(&canonical_events_path, format!("{started}\n{finished}\n"))
            .expect("canonical events file should be written");
        std::fs::create_dir_all(
            override_events_path.parent().expect("override events path should include parent directory"),
        )
        .expect("override events directory should be created");
        std::fs::write(
            &override_events_path,
            format!(
                "{}\n",
                serde_json::to_string(&AgentRunEvent::Error {
                    run_id: run_id_value.clone(),
                    error: "override-runner-state".to_string(),
                })
                .expect("error event should serialize")
            ),
        )
        .expect("override events file should be written");

        let status = read_agent_status(project_root.to_string_lossy().as_ref(), run_id, None)
            .expect("status should resolve from canonical scoped path");
        assert_eq!(status.get("status").and_then(Value::as_str), Some("completed"));
        assert_eq!(status.get("event_count").and_then(Value::as_u64), Some(2));
        assert_eq!(
            status.get("events_path").and_then(Value::as_str),
            Some(canonical_events_path.to_string_lossy().as_ref())
        );
        assert_ne!(
            status.get("events_path").and_then(Value::as_str),
            Some(override_events_path.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn read_agent_status_rejects_unsafe_run_id() {
        let err = read_agent_status("/tmp/project", "../escape", None).expect_err("unsafe run id should be rejected");
        assert!(err.to_string().contains("invalid run_id"));
    }
}
