use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{
    evaluate_task_priority_policy, services::ServiceHub, TaskCreateInput, TaskFilter,
    TaskPriorityPolicyReport, TaskType, TaskUpdateInput, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT,
};
use serde::Serialize;

use crate::services::runtime::{stale_in_progress_summary, StaleInProgressSummary};
use crate::{
    ensure_destructive_confirmation, parse_dependency_type, parse_input_json_or,
    parse_priority_opt, parse_task_status, parse_task_type_opt, print_value, TaskCommand,
};

#[derive(Debug, Serialize)]
struct TaskStatsOutput {
    #[serde(flatten)]
    stats: orchestrator_core::TaskStatistics,
    stale_in_progress: StaleInProgressSummary,
    priority_policy: TaskPriorityPolicyReport,
}

const UNLINKED_REQUIREMENTS_WARNING: &str = "warning: creating non-chore task without linked requirements; pass --linked-requirement <REQ_ID> to improve traceability";

fn has_non_empty_linked_requirements(input: &TaskCreateInput) -> bool {
    input
        .linked_requirements
        .iter()
        .any(|requirement| !requirement.trim().is_empty())
}

fn should_warn_missing_linked_requirements(input: &TaskCreateInput) -> bool {
    input.task_type.unwrap_or(TaskType::Feature) != TaskType::Chore
        && !has_non_empty_linked_requirements(input)
}

pub(crate) async fn handle_task(
    command: TaskCommand,
    hub: Arc<dyn ServiceHub>,
    json: bool,
) -> Result<()> {
    let tasks = hub.tasks();

    match command {
        TaskCommand::List(args) => {
            let filter = TaskFilter {
                task_type: parse_task_type_opt(args.task_type.as_deref())?,
                status: match args.status {
                    Some(status) => Some(parse_task_status(&status)?),
                    None => None,
                },
                priority: parse_priority_opt(args.priority.as_deref())?,
                risk: None,
                assignee_type: args.assignee_type,
                tags: if args.tag.is_empty() {
                    None
                } else {
                    Some(args.tag)
                },
                linked_requirement: args.linked_requirement,
                linked_architecture_entity: args.linked_architecture_entity,
                search_text: args.search,
            };

            if filter.task_type.is_none()
                && filter.status.is_none()
                && filter.priority.is_none()
                && filter.risk.is_none()
                && filter.assignee_type.is_none()
                && filter.tags.is_none()
                && filter.linked_requirement.is_none()
                && filter.linked_architecture_entity.is_none()
                && filter.search_text.is_none()
            {
                print_value(tasks.list().await?, json)
            } else {
                print_value(tasks.list_filtered(filter).await?, json)
            }
        }
        TaskCommand::Prioritized => print_value(tasks.list_prioritized().await?, json),
        TaskCommand::Next => print_value(tasks.next_task().await?, json),
        TaskCommand::Stats(args) => {
            let task_list = tasks.list().await?;
            let stats = tasks.statistics().await?;
            let stale_in_progress =
                stale_in_progress_summary(&task_list, args.stale_threshold_hours, Utc::now());
            let priority_policy =
                evaluate_task_priority_policy(&task_list, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT)?;
            print_value(
                TaskStatsOutput {
                    stats,
                    stale_in_progress,
                    priority_policy,
                },
                json,
            )
        }
        TaskCommand::Get(args) => print_value(tasks.get(&args.id).await?, json),
        TaskCommand::Create(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(TaskCreateInput {
                    title: args.title,
                    description: args.description,
                    task_type: parse_task_type_opt(args.task_type.as_deref())?,
                    priority: parse_priority_opt(args.priority.as_deref())?,
                    created_by: Some("ao-cli".to_string()),
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
                    updated_by: Some("ao-cli".to_string()),
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
            let task = tasks.get(&args.id).await?;
            if args.dry_run {
                let task_id = task.id.clone();
                let task_title = task.title.clone();
                let task_status = task.status.clone();
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

            ensure_destructive_confirmation(
                args.confirm.as_deref(),
                &args.id,
                "task delete",
                "--id",
            )?;
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
            print_value(tasks.assign(&args.id, args.assignee).await?, json)
        }
        TaskCommand::AssignAgent(args) => print_value(
            tasks
                .assign_agent(&args.id, args.role, args.model, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::AssignHuman(args) => print_value(
            tasks
                .assign_human(&args.id, args.user_id, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::ChecklistAdd(args) => print_value(
            tasks
                .add_checklist_item(&args.id, args.description, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::ChecklistUpdate(args) => print_value(
            tasks
                .update_checklist_item(&args.id, &args.item_id, args.completed, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::DependencyAdd(args) => {
            let dependency_type = parse_dependency_type(&args.dependency_type)?;
            print_value(
                tasks
                    .add_dependency(
                        &args.id,
                        &args.dependency_id,
                        dependency_type,
                        args.updated_by,
                    )
                    .await?,
                json,
            )
        }
        TaskCommand::DependencyRemove(args) => print_value(
            tasks
                .remove_dependency(&args.id, &args.dependency_id, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::Status(args) => {
            let status = parse_task_status(&args.status)?;
            print_value(tasks.set_status(&args.id, status).await?, json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_create_input(
        task_type: Option<TaskType>,
        linked_requirements: Vec<&str>,
    ) -> TaskCreateInput {
        TaskCreateInput {
            title: "task".to_string(),
            description: String::new(),
            task_type,
            priority: None,
            created_by: None,
            tags: Vec::new(),
            linked_requirements: linked_requirements
                .into_iter()
                .map(str::to_string)
                .collect(),
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
}
