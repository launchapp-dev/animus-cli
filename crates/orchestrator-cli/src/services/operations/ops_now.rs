use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use orchestrator_core::{
    load_active_workflow_summaries, load_blocked_task_summaries, load_next_task_by_priority,
    load_requirement_link_summaries_by_ids, load_stale_task_summaries, load_task_titles_by_ids,
    BlockedTaskSummary, RequirementLinkSummary, StaleTaskSummary, WorkflowActivitySummary,
};
use serde::Serialize;

use crate::print_value;

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

pub(crate) async fn handle_now(project_root: &str, json: bool) -> Result<()> {
    let now_surface = collect_now_surface(project_root).await?;

    if json {
        return print_value(now_surface, true);
    }

    println!("{}", render_now_surface(&now_surface));
    Ok(())
}

async fn collect_now_surface(project_root: &str) -> Result<NowSurface> {
    let project_root = project_root.to_string();
    tokio::task::spawn_blocking(move || load_now_surface(project_root.as_str()))
        .await
        .map_err(|error| anyhow!("failed to collect now surface: {error}"))?
}

fn load_now_surface(project_root: &str) -> Result<NowSurface> {
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
    value.split(['_', '-'])
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

fn render_now_surface(surface: &NowSurface) -> String {
    let mut output = String::new();

    output.push_str("AO Now/Inbox Surface\n");
    output.push_str(&format!("Generated At: {}\n\n", surface.generated_at.to_rfc3339()));

    output.push_str("Next Task\n");
    if let Some(ref next) = surface.next_task {
        output.push_str(&format!("  id: {}\n", next.id));
        output.push_str(&format!("  title: {}\n", next.title));
        output.push_str(&format!("  priority: {}\n", next.priority));
        output.push_str(&format!("  status: {}\n", next.status));
        if !next.linked_requirements.is_empty() {
            output.push_str("  linked_requirements:\n");
            for req in &next.linked_requirements {
                output.push_str(&format!("    - id={} title={} priority={}\n", req.id, req.title, req.priority));
            }
        }
    } else {
        output.push_str("  none\n");
    }
    output.push_str("\n");

    output.push_str("Active Workflows\n");
    if surface.active_workflows.is_empty() {
        output.push_str("  none\n");
    } else {
        for w in &surface.active_workflows {
            output.push_str(&format!(
                "  - id={} task_id={} task_title={} phase={}\n",
                w.id,
                w.task_id,
                w.task_title,
                w.current_phase.as_deref().unwrap_or("unknown")
            ));
        }
    }
    output.push_str("\n");

    output.push_str("Blocked Items\n");
    if surface.blocked_items.is_empty() {
        output.push_str("  none\n");
    } else {
        for item in &surface.blocked_items {
            output.push_str(&format!("  - id={} type={} title={}\n", item.id, item.item_type, item.title));
            if let Some(reason) = &item.blocked_reason {
                output.push_str(&format!("    reason: {}\n", reason));
            }
        }
    }
    output.push_str("\n");

    output.push_str("Stale Items (>7 days in progress)\n");
    if surface.stale_items.is_empty() {
        output.push_str("  none\n");
    } else {
        for item in &surface.stale_items {
            output.push_str(&format!(
                "  - id={} type={} title={} days_stale={}\n",
                item.id, item.item_type, item.title, item.days_stale
            ));
        }
    }

    output
}

#[cfg(test)]
mod tests;
