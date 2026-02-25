use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{services::ServiceHub, TaskCreateInput, TaskFilter, TaskUpdateInput};

use crate::{
    ensure_destructive_confirmation, parse_dependency_type, parse_input_json_or,
    parse_priority_opt, parse_task_status, parse_task_type_opt, print_value, TaskCommand,
};

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
        TaskCommand::Stats => print_value(tasks.statistics().await?, json),
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
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: args.linked_architecture_entity,
                })
            })?;
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
                return print_value(
                    serde_json::json!({
                        "action": "task.delete",
                        "dry_run": true,
                        "destructive": true,
                        "task": {
                            "id": task.id,
                            "title": task.title,
                            "status": task.status,
                            "paused": task.paused,
                            "cancelled": task.cancelled,
                        },
                    }),
                    json,
                );
            }

            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "task delete")?;
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
