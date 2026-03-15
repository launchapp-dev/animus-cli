use std::collections::HashMap;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    evaluate_task_priority_policy, plan_task_priority_rebalance, services::ServiceHub, ListPageRequest,
    TaskCreateInput, TaskFilter, TaskPriorityPolicyReport, TaskPriorityRebalanceOptions, TaskQuery, TaskQuerySort,
    TaskStatus, TaskType, TaskUpdateInput, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT,
};
use serde::Serialize;

use crate::services::runtime::{stale_in_progress_summary, StaleInProgressSummary};
use crate::{
    ensure_destructive_confirmation, invalid_input_error, not_found_error, parse_dependency_type, parse_input_json_or,
    parse_priority_opt, parse_risk_opt, parse_task_query_sort_opt, parse_task_status, parse_task_type_opt, print_value,
    TaskCommand,
};

#[derive(Debug, Serialize)]
struct TaskStatsOutput {
    #[serde(flatten)]
    stats: orchestrator_core::TaskStatistics,
    stale_in_progress: StaleInProgressSummary,
    priority_policy: TaskPriorityPolicyReport,
}

const UNLINKED_REQUIREMENTS_WARNING: &str = "warning: creating non-chore task without linked requirements; pass --linked-requirement <REQ_ID> to improve traceability";

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn git_local_config_value(project_root: &Path, key: &str) -> Option<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["config", "--local", "--get", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn infer_human_assignee_identity(project_root: &Path) -> Option<String> {
    if let Some(user_id) = non_empty_env("AO_ASSIGNEE_USER_ID") {
        return Some(user_id);
    }
    if let Some(user_id) = non_empty_env("AO_USER_ID") {
        return Some(user_id);
    }
    if let Some(user_id) = git_local_config_value(project_root, "user.email") {
        return Some(user_id);
    }
    if let Some(user_id) = git_local_config_value(project_root, "user.name") {
        return Some(user_id);
    }
    non_empty_env("USER").or_else(|| non_empty_env("USERNAME"))
}

async fn set_task_status_with_assignee_inference(
    tasks: Arc<dyn orchestrator_core::TaskServiceApi>,
    task_id: &str,
    status: orchestrator_core::TaskStatus,
    project_root: &Path,
    validate: bool,
) -> Result<orchestrator_core::OrchestratorTask> {
    let status_updated = tasks.set_status(task_id, status, validate).await?;
    if status == orchestrator_core::TaskStatus::InProgress {
        if let Some(user_id) = infer_human_assignee_identity(project_root) {
            if let Ok(updated) = tasks.assign_human(task_id, user_id.clone(), user_id).await {
                return Ok(updated);
            }
        }
    }
    Ok(status_updated)
}

fn classify_task_service_error(e: anyhow::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("not found") {
        not_found_error(msg)
    } else {
        e
    }
}

fn has_non_empty_linked_requirements(input: &TaskCreateInput) -> bool {
    input.linked_requirements.iter().any(|requirement| !requirement.trim().is_empty())
}

fn should_warn_missing_linked_requirements(input: &TaskCreateInput) -> bool {
    input.task_type.unwrap_or(TaskType::Feature) != TaskType::Chore && !has_non_empty_linked_requirements(input)
}

fn build_task_query_from_list_args(args: crate::TaskListArgs) -> Result<TaskQuery> {
    Ok(TaskQuery {
        filter: TaskFilter {
            task_type: parse_task_type_opt(args.task_type.as_deref())?,
            status: match args.status {
                Some(status) => Some(parse_task_status(&status)?),
                None => None,
            },
            priority: parse_priority_opt(args.priority.as_deref())?,
            risk: parse_risk_opt(args.risk.as_deref())?,
            assignee_type: args.assignee_type,
            tags: if args.tag.is_empty() { None } else { Some(args.tag) },
            linked_requirement: args.linked_requirement,
            linked_architecture_entity: args.linked_architecture_entity,
            search_text: args.search,
        },
        page: ListPageRequest { limit: args.limit, offset: args.offset },
        sort: parse_task_query_sort_opt(args.sort.as_deref())?.unwrap_or_default(),
    })
}

fn build_task_query_from_prioritized_args(args: crate::TaskPrioritizedArgs) -> Result<TaskQuery> {
    Ok(TaskQuery {
        filter: TaskFilter {
            status: match args.status {
                Some(status) => Some(parse_task_status(&status)?),
                None => None,
            },
            priority: parse_priority_opt(args.priority.as_deref())?,
            assignee_type: args.assignee_type,
            search_text: args.search,
            ..Default::default()
        },
        page: ListPageRequest { limit: args.limit, offset: args.offset },
        sort: TaskQuerySort::Priority,
    })
}

pub(crate) async fn handle_task(
    command: TaskCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let tasks = hub.tasks();

    match command {
        TaskCommand::List(args) => {
            let page = tasks.query(build_task_query_from_list_args(args)?).await?;
            print_value(page.items, json)
        }
        TaskCommand::Prioritized(args) => {
            let page = tasks.query(build_task_query_from_prioritized_args(args)?).await?;
            print_value(page.items, json)
        }
        TaskCommand::Next => print_value(tasks.next_task().await?, json),
        TaskCommand::Stats(args) => {
            let task_list = tasks.list().await?;
            let stats = tasks.statistics().await?;
            let stale_in_progress = stale_in_progress_summary(&task_list, args.stale_threshold_hours, Utc::now());
            let priority_policy = evaluate_task_priority_policy(&task_list, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT)?;
            print_value(TaskStatsOutput { stats, stale_in_progress, priority_policy }, json)
        }
        TaskCommand::Get(args) => {
            let task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            print_value(task, json)
        }
        TaskCommand::Create(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(TaskCreateInput {
                    title: args.title,
                    description: args.description.unwrap_or_default(),
                    task_type: parse_task_type_opt(args.task_type.as_deref())?,
                    priority: parse_priority_opt(args.priority.as_deref())?,
                    created_by: Some(protocol::ACTOR_CLI.to_string()),
                    tags: Vec::new(),
                    linked_requirements: args.linked_requirement,
                    linked_architecture_entities: args.linked_architecture_entity,
                })
            })?;
            if should_warn_missing_linked_requirements(&input) {
                eprintln!("{UNLINKED_REQUIREMENTS_WARNING}");
            }
            print_value(tasks.create(input).await?, json)
        }
        TaskCommand::Update(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(TaskUpdateInput {
                    title: args.title,
                    description: args.description,
                    priority: parse_priority_opt(args.priority.as_deref())?,
                    status: match args.status {
                        Some(status) => Some(parse_task_status(&status)?),
                        None => None,
                    },
                    assignee: args.assignee,
                    tags: None,
                    updated_by: Some(protocol::ACTOR_CLI.to_string()),
                    deadline: None,
                    linked_architecture_entities: if args.replace_linked_architecture_entities
                        || !args.linked_architecture_entity.is_empty()
                    {
                        Some(args.linked_architecture_entity)
                    } else {
                        None
                    },
                })
            })?;
            print_value(tasks.update(&args.id, input).await?, json)
        }
        TaskCommand::Delete(args) => {
            let task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            if args.dry_run {
                let task_id = task.id.clone();
                let task_title = task.title.clone();
                let task_status = task.status;
                let task_paused = task.paused;
                let task_cancelled = task.cancelled;
                return print_value(
                    serde_json::json!({
                        "operation": "task.delete",
                        "target": {
                            "task_id": task_id.clone(),
                        },
                        "action": "task.delete",
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "delete task from project state",
                        ],
                        "next_step": format!(
                            "rerun 'ao task delete --id {} --confirm {}' to apply",
                            task_id,
                            task_id
                        ),
                        "task": {
                            "id": task_id.clone(),
                            "title": task_title,
                            "status": task_status,
                            "paused": task_paused,
                            "cancelled": task_cancelled,
                        },
                    }),
                    json,
                );
            }

            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "task delete", "--id")?;
            tasks.delete(&args.id).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": "task deleted",
                    "task_id": args.id,
                }),
                json,
            )
        }
        TaskCommand::Assign(args) => {
            let is_agent = args.assignee_type.as_deref() == Some("agent") || args.agent_role.is_some();
            if is_agent {
                let role = args.agent_role.unwrap_or(args.assignee);
                print_value(tasks.assign_agent(&args.id, role, args.model, args.updated_by).await?, json)
            } else {
                print_value(tasks.assign_human(&args.id, args.assignee, args.updated_by).await?, json)
            }
        }
        TaskCommand::ChecklistAdd(args) => {
            print_value(tasks.add_checklist_item(&args.id, args.description, args.updated_by).await?, json)
        }
        TaskCommand::ChecklistUpdate(args) => print_value(
            tasks.update_checklist_item(&args.id, &args.item_id, args.completed, args.updated_by).await?,
            json,
        ),
        TaskCommand::DependencyAdd(args) => {
            let dependency_type = parse_dependency_type(&args.dependency_type)?;
            print_value(
                tasks.add_dependency(&args.id, &args.dependency_id, dependency_type, args.updated_by).await?,
                json,
            )
        }
        TaskCommand::DependencyRemove(args) => {
            print_value(tasks.remove_dependency(&args.id, &args.dependency_id, args.updated_by).await?, json)
        }
        TaskCommand::Status(args) => {
            let status = parse_task_status(&args.status)?;
            print_value(
                set_task_status_with_assignee_inference(
                    tasks.clone(),
                    &args.id,
                    status,
                    Path::new(project_root),
                    true, // validate: true for CLI commands
                )
                .await?,
                json,
            )
        }
        TaskCommand::History(args) => {
            let task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            print_value(&task.dispatch_history, json)
        }
        TaskCommand::Pause(args) => {
            let mut task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            if task.paused {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "task is already paused",
                        "task_id": args.id,
                    }),
                    json,
                );
            }
            task.paused = true;
            task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} paused", args.id),
                }),
                json,
            )
        }
        TaskCommand::Resume(args) => {
            let mut task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            if !task.paused {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "task is not paused",
                        "task_id": args.id,
                    }),
                    json,
                );
            }
            task.paused = false;
            task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} resumed", args.id),
                }),
                json,
            )
        }
        TaskCommand::Cancel(args) => {
            let mut task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            if task.cancelled {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "task is already cancelled",
                        "task_id": args.id,
                    }),
                    json,
                );
            }
            if args.dry_run {
                let task_id = task.id.clone();
                return print_value(
                    serde_json::json!({
                        "operation": "task.cancel",
                        "target": { "task_id": task_id },
                        "action": "task.cancel",
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "mark task as cancelled",
                            "set task status to cancelled",
                        ],
                        "next_step": format!(
                            "rerun 'ao task cancel --id {} --confirm {}' to apply",
                            task_id, task_id
                        ),
                    }),
                    json,
                );
            }
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "task cancel", "--id")?;
            task.cancelled = true;
            task.status = TaskStatus::Cancelled;
            task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} cancelled", args.id),
                }),
                json,
            )
        }
        TaskCommand::Reopen(args) => {
            let mut task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            if !task.status.is_terminal() {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "task is not in a terminal state (done or cancelled)",
                        "task_id": args.id,
                        "current_status": task.status.to_string(),
                    }),
                    json,
                );
            }
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "task reopen", "--id")?;
            // Reopen bypasses terminal state validation by using validate: false
            // and explicitly setting status to Backlog
            task.status = TaskStatus::Backlog;
            task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
            // Clear terminal state metadata
            task.cancelled = false;
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} reopened to backlog", args.id),
                }),
                json,
            )
        }
        TaskCommand::SetPriority(args) => {
            let priority =
                parse_priority_opt(Some(args.priority.as_str()))?.ok_or_else(|| anyhow!("priority is required"))?;
            let mut task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            task.priority = priority;
            task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} priority set to {}", args.id, args.priority),
                }),
                json,
            )
        }
        TaskCommand::SetDeadline(args) => {
            let mut task = tasks.get(&args.id).await.map_err(classify_task_service_error)?;
            let normalized = args
                .deadline
                .as_deref()
                .map(|deadline| {
                    chrono::DateTime::parse_from_rfc3339(deadline)
                        .map(|value| value.with_timezone(&Utc).to_rfc3339())
                        .with_context(|| {
                            format!(
                                "invalid deadline format '{deadline}'; expected RFC 3339 timestamp such as 2026-03-01T09:30:00Z"
                            )
                        })
                })
                .transpose()?;
            task.deadline = normalized;
            task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} deadline updated", args.id),
                }),
                json,
            )
        }
        TaskCommand::RebalancePriority(args) => {
            const OPERATION: &str = "task.rebalance-priority";
            const CONFIRM_TOKEN: &str = "apply";
            let all_tasks = tasks.list().await?;
            let plan = plan_task_priority_rebalance(
                &all_tasks,
                TaskPriorityRebalanceOptions {
                    high_budget_percent: args.high_budget_percent,
                    essential_task_ids: args.essential_task_id,
                    nice_to_have_task_ids: args.nice_to_have_task_id,
                },
            )?;

            if !args.apply {
                return print_value(
                    serde_json::json!({
                        "operation": OPERATION,
                        "target": all_tasks.len().to_string(),
                        "action": OPERATION,
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "reserve critical for blocked active tasks",
                            "enforce high-priority budget for active tasks",
                            "rebalance remaining tasks to medium/low",
                        ],
                        "next_step": format!(
                            "rerun 'ao task rebalance-priority --apply --confirm {}' to apply",
                            CONFIRM_TOKEN
                        ),
                        "plan": plan,
                    }),
                    json,
                );
            }

            if args.confirm.as_deref().map(str::trim) != Some(CONFIRM_TOKEN) {
                return Err(invalid_input_error(format!(
                    "CONFIRMATION_REQUIRED: rerun 'ao task rebalance-priority --apply --confirm {CONFIRM_TOKEN}'; run without --apply to preview changes"
                )));
            }

            let mut tasks_by_id: HashMap<String, orchestrator_core::OrchestratorTask> =
                all_tasks.into_iter().map(|task| (task.id.clone(), task)).collect();
            for change in &plan.changes {
                if let Some(mut task) = tasks_by_id.remove(change.task_id.as_str()) {
                    task.priority = change.to;
                    task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
                    tasks.replace(task).await?;
                }
            }

            let changed_task_ids: Vec<String> = plan.changes.iter().map(|change| change.task_id.clone()).collect();
            print_value(
                serde_json::json!({
                    "success": true,
                    "operation": OPERATION,
                    "dry_run": false,
                    "applied": true,
                    "changed_count": changed_task_ids.len(),
                    "changed_task_ids": changed_task_ids,
                    "plan": plan,
                }),
                json,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{Assignee, InMemoryServiceHub, Priority, TaskStatus};
    use tempfile::TempDir;

    use protocol::test_utils::EnvVarGuard;

    fn init_git_repo(path: &TempDir) {
        let init =
            ProcessCommand::new("git").arg("init").current_dir(path.path()).status().expect("git init should run");
        assert!(init.success(), "git init should succeed");
    }

    fn git_config(path: &TempDir, key: &str, value: &str) {
        let status = ProcessCommand::new("git")
            .args(["config", "--local", key, value])
            .current_dir(path.path())
            .status()
            .expect("git config should run");
        assert!(status.success(), "git config should succeed");
    }

    fn task_create_input(task_type: Option<TaskType>, linked_requirements: Vec<&str>) -> TaskCreateInput {
        TaskCreateInput {
            title: "task".to_string(),
            description: String::new(),
            task_type,
            priority: None,
            created_by: None,
            tags: Vec::new(),
            linked_requirements: linked_requirements.into_iter().map(str::to_string).collect(),
            linked_architecture_entities: Vec::new(),
        }
    }

    #[test]
    fn warns_for_default_feature_tasks_without_links() {
        let input = task_create_input(None, Vec::new());
        assert!(should_warn_missing_linked_requirements(&input));
    }

    #[test]
    fn warns_for_non_chore_tasks_without_links() {
        let input = task_create_input(Some(TaskType::Feature), Vec::new());
        assert!(should_warn_missing_linked_requirements(&input));
    }

    #[test]
    fn does_not_warn_for_chore_tasks_without_links() {
        let input = task_create_input(Some(TaskType::Chore), Vec::new());
        assert!(!should_warn_missing_linked_requirements(&input));
    }

    #[test]
    fn does_not_warn_when_linked_requirements_are_present() {
        let input = task_create_input(Some(TaskType::Feature), vec!["REQ-123"]);
        assert!(!should_warn_missing_linked_requirements(&input));
    }

    #[test]
    fn warns_when_linked_requirements_are_blank() {
        let input = task_create_input(Some(TaskType::Feature), vec!["", "   "]);
        assert!(should_warn_missing_linked_requirements(&input));
    }

    #[test]
    fn does_not_warn_when_at_least_one_linked_requirement_is_non_blank() {
        let input = task_create_input(Some(TaskType::Feature), vec!["", "REQ-123", "   "]);
        assert!(!should_warn_missing_linked_requirements(&input));
    }

    #[test]
    fn infer_human_assignee_prefers_ao_assignee_user_id() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _ao_assignee = EnvVarGuard::set("AO_ASSIGNEE_USER_ID", Some("assignee-user"));
        let _ao_user = EnvVarGuard::set("AO_USER_ID", Some("ao-user"));
        let _user = EnvVarGuard::set("USER", Some("shell-user"));
        let _username = EnvVarGuard::set("USERNAME", Some("shell-username"));

        assert_eq!(
            infer_human_assignee_identity(Path::new("/tmp/ao-task-assignee-test")).as_deref(),
            Some("assignee-user")
        );
    }

    #[test]
    fn infer_human_assignee_prefers_git_identity_before_shell_user() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _ao_assignee = EnvVarGuard::set("AO_ASSIGNEE_USER_ID", None);
        let _ao_user = EnvVarGuard::set("AO_USER_ID", None);
        let _user = EnvVarGuard::set("USER", Some("shell-user"));
        let _username = EnvVarGuard::set("USERNAME", Some("shell-username"));

        let repo = TempDir::new().expect("temp dir should be created");
        init_git_repo(&repo);
        git_config(&repo, "user.email", "git-email@example.com");
        git_config(&repo, "user.name", "Git Name");

        assert_eq!(infer_human_assignee_identity(repo.path()).as_deref(), Some("git-email@example.com"));
    }

    #[tokio::test]
    async fn set_task_status_in_progress_assigns_human_when_identity_is_available() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _ao_assignee = EnvVarGuard::set("AO_ASSIGNEE_USER_ID", Some("operator@example.com"));
        let _ao_user = EnvVarGuard::set("AO_USER_ID", None);

        let hub = Arc::new(InMemoryServiceHub::new());
        let created = hub
            .tasks()
            .create(TaskCreateInput {
                title: "status-assignee".to_string(),
                description: "auto assign on in-progress".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let updated = set_task_status_with_assignee_inference(
            hub.tasks(),
            &created.id,
            TaskStatus::InProgress,
            Path::new("/tmp/ao-task-assignee-test"),
            false,
        )
        .await
        .expect("status update should succeed");
        assert_eq!(updated.status, TaskStatus::InProgress);
        assert_eq!(updated.assignee, Assignee::Human { user_id: "operator@example.com".to_string() });
        assert_eq!(updated.metadata.updated_by, "operator@example.com");
    }

    #[tokio::test]
    async fn set_task_status_in_progress_keeps_unassigned_when_identity_is_unavailable() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _ao_assignee = EnvVarGuard::set("AO_ASSIGNEE_USER_ID", None);
        let _ao_user = EnvVarGuard::set("AO_USER_ID", None);
        let _user = EnvVarGuard::set("USER", None);
        let _username = EnvVarGuard::set("USERNAME", None);
        let repo = TempDir::new().expect("temp dir should be created");

        let hub = Arc::new(InMemoryServiceHub::new());
        let created = hub
            .tasks()
            .create(TaskCreateInput {
                title: "status-unassigned".to_string(),
                description: "keep unassigned when no identity".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let updated = set_task_status_with_assignee_inference(
            hub.tasks(),
            &created.id,
            TaskStatus::InProgress,
            repo.path(),
            false,
        )
        .await
        .expect("status update should succeed");
        assert_eq!(updated.status, TaskStatus::InProgress);
        assert_eq!(updated.assignee, Assignee::Unassigned);
    }

    #[tokio::test]
    async fn set_task_status_non_in_progress_does_not_assign_human() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _ao_assignee = EnvVarGuard::set("AO_ASSIGNEE_USER_ID", Some("operator@example.com"));

        let hub = Arc::new(InMemoryServiceHub::new());
        let created = hub
            .tasks()
            .create(TaskCreateInput {
                title: "status-ready".to_string(),
                description: "no auto-assign outside in-progress".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let updated = set_task_status_with_assignee_inference(
            hub.tasks(),
            &created.id,
            TaskStatus::Ready,
            Path::new("/tmp/ao-task-assignee-test"),
            false,
        )
        .await
        .expect("status update should succeed");
        assert_eq!(updated.status, TaskStatus::Ready);
        assert_eq!(updated.assignee, Assignee::Unassigned);
    }
}
