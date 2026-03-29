use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use orchestrator_core::{
    load_active_workflow_summaries, load_blocked_task_summaries, load_next_task_by_priority,
    load_requirement_link_summaries_by_ids, load_stale_task_summaries, load_task_titles_by_ids,
    BlockedTaskSummary, RequirementLinkSummary, StaleTaskSummary, WorkflowActivitySummary,
};
use serde::Serialize;
use serde_json::Value;

use super::{WebApiError, WebApiService};

const NOW_SCHEMA: &str = "ao.now.v1";
const STALE_TASK_THRESHOLD_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize)]
struct NowSurface {
    schema: &'static str,
    generated_at: DateTime<Utc>,
    next_task: Option<NextTaskItem>,
    active_workflows: Vec<ActiveWorkflowItem>,
    blocked_items: Vec<BlockedItem>,
    stale_items: Vec<StaleItem>,
}

#[derive(Debug, Clone, Serialize)]
struct NextTaskItem {
    id: String,
    title: String,
    priority: String,
    status: String,
    linked_requirements: Vec<LinkedRequirement>,
}

#[derive(Debug, Clone, Serialize)]
struct LinkedRequirement {
    id: String,
    title: String,
    priority: String,
}

#[derive(Debug, Clone, Serialize)]
struct ActiveWorkflowItem {
    id: String,
    task_id: String,
    task_title: String,
    status: String,
    current_phase: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct BlockedItem {
    id: String,
    item_type: String,
    title: String,
    blocked_reason: Option<String>,
    blocked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
struct StaleItem {
    id: String,
    item_type: String,
    title: String,
    last_updated: DateTime<Utc>,
    days_stale: u32,
}

impl WebApiService {
    pub async fn now(&self) -> Result<Value, WebApiError> {
        let project_root = self.context.project_root.clone();
        let surface = tokio::task::spawn_blocking(move || load_now_surface(&project_root))
            .await
            .map_err(|e| WebApiError::new("spawn_blocking_failed", format!("failed to collect now surface: {e}"), 500))?
            .map_err(|e| WebApiError::from(e))?;

        serde_json::to_value(surface).map_err(|e| WebApiError::new("serialization_failed", format!("failed to serialize now surface: {e}"), 500))
    }
}

fn load_now_surface(project_root: &str) -> anyhow::Result<NowSurface> {
    let project_root = Path::new(project_root);
    let generated_at = Utc::now();
    let stale_before = generated_at - Duration::days(STALE_TASK_THRESHOLD_DAYS);

    let next_task = load_next_task_by_priority(project_root)?;
    let linked_requirements = match next_task.as_ref() {
        Some(task) => load_requirement_link_summaries_by_ids(project_root, &task.linked_requirements)?,
        None => Vec::new(),
    };

    let active_workflows = load_active_workflow_summaries(project_root)?;
    let active_task_ids: Vec<String> = active_workflows.iter().map(|workflow| workflow.task_id.clone()).collect();
    let active_task_titles = load_task_titles_by_ids(project_root, &active_task_ids)?;

    let blocked_items = load_blocked_task_summaries(project_root)?;
    let stale_items = load_stale_task_summaries(project_root, stale_before)?;

    Ok(build_now_surface(
        generated_at,
        build_next_task_item(next_task, linked_requirements),
        build_active_workflow_items(active_workflows, &active_task_titles),
        build_blocked_items(blocked_items),
        build_stale_items(generated_at, stale_items),
    ))
}

fn build_now_surface(
    generated_at: DateTime<Utc>,
    next_task: Option<NextTaskItem>,
    active_workflows: Vec<ActiveWorkflowItem>,
    blocked_items: Vec<BlockedItem>,
    stale_items: Vec<StaleItem>,
) -> NowSurface {
    NowSurface { schema: NOW_SCHEMA, generated_at, next_task, active_workflows, blocked_items, stale_items }
}

fn build_next_task_item(
    next_task: Option<orchestrator_core::OrchestratorTask>,
    linked_requirements: Vec<RequirementLinkSummary>,
) -> Option<NextTaskItem> {
    next_task.map(|task| NextTaskItem {
        id: task.id,
        title: task.title,
        priority: format!("{:?}", task.priority),
        status: format!("{:?}", task.status),
        linked_requirements: linked_requirements
            .into_iter()
            .map(|requirement| LinkedRequirement {
                id: requirement.requirement_id,
                title: requirement.title,
                priority: storage_label(requirement.priority.as_str()),
            })
            .collect(),
    })
}

fn build_active_workflow_items(
    active_workflows: Vec<WorkflowActivitySummary>,
    task_titles: &HashMap<String, String>,
) -> Vec<ActiveWorkflowItem> {
    active_workflows
        .into_iter()
        .map(|workflow| ActiveWorkflowItem {
            id: workflow.workflow_id,
            task_id: workflow.task_id.clone(),
            task_title: task_titles
                .get(workflow.task_id.as_str())
                .cloned()
                .unwrap_or_else(|| "Unknown task".to_string()),
            status: storage_label(workflow.status.as_str()),
            current_phase: Some(workflow.phase_id),
        })
        .collect()
}

fn build_blocked_items(blocked_tasks: Vec<BlockedTaskSummary>) -> Vec<BlockedItem> {
    blocked_tasks
        .into_iter()
        .map(|task| BlockedItem {
            id: task.task_id,
            item_type: "task".to_string(),
            title: task.title,
            blocked_reason: task.blocked_reason,
            blocked_at: task.blocked_at,
        })
        .collect()
}

fn build_stale_items(generated_at: DateTime<Utc>, stale_tasks: Vec<StaleTaskSummary>) -> Vec<StaleItem> {
    stale_tasks
        .into_iter()
        .filter_map(|task| {
            let days_stale = (generated_at.signed_duration_since(task.updated_at).num_seconds() / 86_400) as u32;
            (days_stale > STALE_TASK_THRESHOLD_DAYS as u32).then_some(StaleItem {
                id: task.task_id,
                item_type: "task".to_string(),
                title: task.title,
                last_updated: task.updated_at,
                days_stale,
            })
        })
        .collect()
}

fn storage_label(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = first.to_uppercase().collect::<String>();
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn next_task(id: &str, title: &str) -> NextTaskItem {
        NextTaskItem {
            id: id.to_string(),
            title: title.to_string(),
            priority: "Medium".to_string(),
            status: "Ready".to_string(),
            linked_requirements: Vec::new(),
        }
    }

    fn active_workflow(id: &str, task_id: &str, task_title: &str) -> ActiveWorkflowItem {
        ActiveWorkflowItem {
            id: id.to_string(),
            task_id: task_id.to_string(),
            task_title: task_title.to_string(),
            status: "Running".to_string(),
            current_phase: Some("implementation".to_string()),
        }
    }

    fn blocked_item(id: &str, title: &str) -> BlockedItem {
        BlockedItem {
            id: id.to_string(),
            item_type: "task".to_string(),
            title: title.to_string(),
            blocked_reason: Some("Waiting for review".to_string()),
            blocked_at: None,
        }
    }

    fn stale_item(id: &str, title: &str, days_stale: u32) -> StaleItem {
        StaleItem {
            id: id.to_string(),
            item_type: "task".to_string(),
            title: title.to_string(),
            last_updated: Utc::now() - ChronoDuration::days(i64::from(days_stale)),
            days_stale,
        }
    }

    #[test]
    fn test_build_now_surface_with_next_task() {
        let surface = build_now_surface(Utc::now(), Some(next_task("TASK-001", "Test task")), vec![], vec![], vec![]);

        assert!(surface.next_task.is_some());
        assert_eq!(surface.next_task.as_ref().unwrap().id, "TASK-001");
        assert_eq!(surface.next_task.as_ref().unwrap().title, "Test task");
        assert_eq!(surface.schema, NOW_SCHEMA);
    }

    #[test]
    fn test_build_now_surface_json_structure() {
        let now = Utc::now();
        let surface = build_now_surface(
            now,
            Some(next_task("TASK-001", "Test task")),
            vec![active_workflow("WF-001", "TASK-001", "Test task")],
            vec![blocked_item("TASK-002", "Blocked task")],
            vec![stale_item("TASK-003", "Stale task", 8)],
        );

        let json = serde_json::to_value(&surface).expect("should serialize");
        assert_eq!(json["schema"], NOW_SCHEMA);
        assert!(json["generated_at"].is_string());
        assert!(json["next_task"].is_object());
        assert!(json["active_workflows"].is_array());
        assert!(json["blocked_items"].is_array());
        assert!(json["stale_items"].is_array());
    }

    #[test]
    fn test_build_now_surface_without_next_task() {
        let surface = build_now_surface(Utc::now(), None, vec![], vec![], vec![]);

        assert!(surface.next_task.is_none());
        assert_eq!(surface.active_workflows.len(), 0);
        assert_eq!(surface.blocked_items.len(), 0);
        assert_eq!(surface.stale_items.len(), 0);
    }

    #[test]
    fn test_storage_label_formatting() {
        assert_eq!(storage_label("in_progress"), "InProgress");
        assert_eq!(storage_label("in-progress"), "InProgress");
        assert_eq!(storage_label("high"), "High");
        assert_eq!(storage_label("MEDIUM"), "MEDIUM");
    }
}
