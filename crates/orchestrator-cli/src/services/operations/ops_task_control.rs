use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    plan_task_priority_rebalance, services::ServiceHub, TaskPriorityRebalanceOptions, TaskStatus,
};

use crate::{
    ensure_destructive_confirmation, invalid_input_error, parse_priority_opt, print_value,
    TaskControlCommand,
};

const PRIORITY_REBALANCE_OPERATION: &str = "task-control.rebalance-priority";
const PRIORITY_REBALANCE_CONFIRM_TOKEN: &str = "apply";

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
                        "message": "task is already paused",
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
                    "message": format!("task {} paused", args.task_id),
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
                        "message": "task is not paused",
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
                    "message": format!("task {} resumed", args.task_id),
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
                        "message": "task is already cancelled",
                        "task_id": args.task_id,
                    }),
                    json,
                );
            }
            if args.dry_run {
                let task_id = task.id.clone();
                let task_status = task.status.clone();
                let task_paused = task.paused;
                let task_cancelled = task.cancelled;
                return print_value(
                    serde_json::json!({
                        "operation": "task-control.cancel",
                        "target": {
                            "task_id": task_id.clone(),
                        },
                        "action": "task-control.cancel",
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "mark task as cancelled",
                            "set task status to cancelled",
                        ],
                        "next_step": format!(
                            "rerun 'ao task-control cancel --task-id {} --confirm {}' to apply",
                            task_id,
                            task_id
                        ),
                        "task": {
                            "task_id": task_id,
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
                &args.task_id,
                "task-control cancel",
                "--task-id",
            )?;
            task.cancelled = true;
            task.status = TaskStatus::Cancelled;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} cancelled", args.task_id),
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
                    "message": format!("task {} priority set to {}", args.task_id, args.priority),
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
                        .with_context(|| {
                            format!(
                                "invalid deadline format '{deadline}'; expected RFC 3339 timestamp such as 2026-03-01T09:30:00Z"
                            )
                        })
                })
                .transpose()?;
            task.deadline = normalized;
            task.metadata.updated_by = "ao-cli".to_string();
            tasks.replace(task).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": format!("task {} deadline updated", args.task_id),
                }),
                json,
            )
        }
        TaskControlCommand::RebalancePriority(args) => {
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
                        "operation": PRIORITY_REBALANCE_OPERATION,
                        "target": {
                            "task_count": all_tasks.len(),
                        },
                        "action": PRIORITY_REBALANCE_OPERATION,
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "reserve critical for blocked active tasks",
                            "enforce high-priority budget for active tasks",
                            "rebalance remaining tasks to medium/low",
                        ],
                        "next_step": format!(
                            "rerun 'ao task-control rebalance-priority --apply --confirm {}' to apply",
                            PRIORITY_REBALANCE_CONFIRM_TOKEN
                        ),
                        "plan": plan,
                    }),
                    json,
                );
            }

            ensure_priority_rebalance_confirmation(args.confirm.as_deref())?;

            let mut tasks_by_id: HashMap<String, orchestrator_core::OrchestratorTask> = all_tasks
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect();
            for change in &plan.changes {
                if let Some(mut task) = tasks_by_id.remove(change.task_id.as_str()) {
                    task.priority = change.to;
                    task.metadata.updated_by = "ao-cli".to_string();
                    tasks.replace(task).await?;
                }
            }

            let changed_task_ids: Vec<String> = plan
                .changes
                .iter()
                .map(|change| change.task_id.clone())
                .collect();
            print_value(
                serde_json::json!({
                    "success": true,
                    "operation": PRIORITY_REBALANCE_OPERATION,
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

fn ensure_priority_rebalance_confirmation(confirm: Option<&str>) -> Result<()> {
    if confirm.map(str::trim) == Some(PRIORITY_REBALANCE_CONFIRM_TOKEN) {
        return Ok(());
    }

    Err(invalid_input_error(format!(
        "CONFIRMATION_REQUIRED: rerun 'ao task-control rebalance-priority --apply --confirm {token}'; run without --apply to preview changes",
        token = PRIORITY_REBALANCE_CONFIRM_TOKEN
    )))
}
