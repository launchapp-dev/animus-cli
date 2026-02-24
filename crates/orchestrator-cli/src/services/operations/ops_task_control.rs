use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{services::ServiceHub, TaskStatus};

use crate::{parse_priority_opt, print_value, TaskControlCommand};

pub(crate) async fn handle_task_control(
    command: TaskControlCommand,
    hub: Arc<dyn ServiceHub>,
    json: bool,
) -> Result<()> {
    let tasks = hub.tasks();
    match command {
        TaskControlCommand::Pause(args) => {
            let mut task = tasks.get(&args.task_id).await?;
            if task.paused {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "Task is already paused",
                        "task_id": args.task_id,
                    }),
                    json,
                );
            }
            task.paused = true;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("Task {} paused", args.task_id),
                }),
                json,
            )
        }
        TaskControlCommand::Resume(args) => {
            let mut task = tasks.get(&args.task_id).await?;
            if !task.paused {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "Task is not paused",
                        "task_id": args.task_id,
                    }),
                    json,
                );
            }
            task.paused = false;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("Task {} resumed", args.task_id),
                }),
                json,
            )
        }
        TaskControlCommand::Cancel(args) => {
            let mut task = tasks.get(&args.task_id).await?;
            if task.cancelled {
                return print_value(
                    serde_json::json!({
                        "success": false,
                        "message": "Task is already cancelled",
                        "task_id": args.task_id,
                    }),
                    json,
                );
            }
            task.cancelled = true;
            task.status = TaskStatus::Cancelled;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("Task {} cancelled", args.task_id),
                }),
                json,
            )
        }
        TaskControlCommand::SetPriority(args) => {
            let priority = parse_priority_opt(Some(args.priority.as_str()))?
                .ok_or_else(|| anyhow!("priority is required"))?;
            let mut task = tasks.get(&args.task_id).await?;
            task.priority = priority;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("Task {} priority set to {}", args.task_id, args.priority),
                }),
                json,
            )
        }
        TaskControlCommand::SetDeadline(args) => {
            let mut task = tasks.get(&args.task_id).await?;
            let normalized = args
                .deadline
                .as_deref()
                .map(|deadline| {
                    chrono::DateTime::parse_from_rfc3339(deadline)
                        .map(|value| value.with_timezone(&Utc).to_rfc3339())
                        .with_context(|| format!("invalid deadline format: {deadline}"))
                })
                .transpose()?;
            task.deadline = normalized;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("Task {} deadline updated", args.task_id),
                }),
                json,
            )
        }
    }
}
