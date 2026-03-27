use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use orchestrator_core::{OrchestratorTask, ServiceHub, TaskStatus};
use serde::Serialize;

use crate::print_value;

const NOW_SCHEMA: &str = "ao.now.v1";
const STALE_IN_PROGRESS_THRESHOLD_HOURS: i64 = 4;

#[derive(Debug, Clone, Serialize)]
struct NowSurface {
    schema: &'static str,
    project_root: String,
    generated_at: DateTime<Utc>,
    next_task: Option<NextTaskEntry>,
    blocked_items: Vec<BlockedItem>,
    stale_items: Vec<StaleItem>,
}

#[derive(Debug, Clone, Serialize)]
struct NextTaskEntry {
    id: String,
    title: String,
    priority: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    linked_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct BlockedItem {
    id: String,
    title: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StaleItem {
    id: String,
    title: String,
    age_hours: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
}

pub(crate) async fn handle_now(hub: Arc<dyn ServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let tasks_service = hub.tasks();
    let tasks_result = tasks_service.list().await;

    let tasks = match tasks_result {
        Ok(tasks) => tasks,
        Err(error) => {
            let surface = NowSurface {
                schema: NOW_SCHEMA,
                project_root: project_root.to_string(),
                generated_at: Utc::now(),
                next_task: None,
                blocked_items: vec![],
                stale_items: vec![],
            };

            if json {
                return print_value(surface, true);
            }

            eprintln!("Error fetching tasks: {}", error);
            return Err(error);
        }
    };

    let next_task = extract_next_task(&tasks);
    let blocked_items = extract_blocked_items(&tasks);
    let stale_items = extract_stale_items(&tasks);

    let surface = NowSurface {
        schema: NOW_SCHEMA,
        project_root: project_root.to_string(),
        generated_at: Utc::now(),
        next_task,
        blocked_items,
        stale_items,
    };

    if json {
        return print_value(surface, true);
    }

    println!("{}", render_now_surface(&surface));
    Ok(())
}

fn extract_next_task(tasks: &[OrchestratorTask]) -> Option<NextTaskEntry> {
    let mut ready_tasks: Vec<_> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Ready)
        .collect();

    ready_tasks.sort_by(|left, right| {
        let left_priority = task_priority_value(left.priority);
        let right_priority = task_priority_value(right.priority);
        match right_priority.cmp(&left_priority) {
            std::cmp::Ordering::Equal => left.id.cmp(&right.id),
            ordering => ordering,
        }
    });

    ready_tasks.into_iter().next().map(|task| NextTaskEntry {
        id: task.id.clone(),
        title: task.title.clone(),
        priority: task.priority.as_str().to_string(),
        linked_requirements: task.linked_requirements.clone(),
    })
}

fn extract_blocked_items(tasks: &[OrchestratorTask]) -> Vec<BlockedItem> {
    let mut blocked: Vec<BlockedItem> = tasks
        .iter()
        .filter(|task| task.status.is_blocked())
        .map(|task| BlockedItem {
            id: task.id.clone(),
            title: task.title.clone(),
            status: format!("{:?}", task.status),
            blocked_reason: task.blocked_reason.clone(),
        })
        .collect();

    blocked.sort_by(|left, right| left.id.cmp(&right.id));
    blocked
}

fn extract_stale_items(tasks: &[OrchestratorTask]) -> Vec<StaleItem> {
    let now = Utc::now();
    let threshold = Duration::hours(STALE_IN_PROGRESS_THRESHOLD_HOURS);

    let mut stale: Vec<StaleItem> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .filter_map(|task| {
            let updated_at = task.metadata.updated_at;
            let age = now.signed_duration_since(updated_at);
            if age > threshold {
                let age_hours = age.num_hours();
                Some(StaleItem {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    age_hours,
                    updated_at: Some(updated_at),
                })
            } else {
                None
            }
        })
        .collect();

    stale.sort_by(|left, right| {
        match (right.updated_at, left.updated_at) {
            (Some(right_at), Some(left_at)) => right_at.cmp(&left_at).then_with(|| left.id.cmp(&right.id)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.id.cmp(&right.id),
        }
    });

    stale
}

fn task_priority_value(priority: orchestrator_core::Priority) -> i32 {
    match priority {
        orchestrator_core::Priority::Critical => 5,
        orchestrator_core::Priority::High => 4,
        orchestrator_core::Priority::Medium => 3,
        orchestrator_core::Priority::Low => 2,
    }
}

fn render_now_surface(surface: &NowSurface) -> String {
    let mut output = String::new();
    use std::fmt::Write as _;

    let _ = writeln!(&mut output, "AO Now - What needs attention");
    let _ = writeln!(&mut output, "Project Root: {}", surface.project_root);
    let _ = writeln!(&mut output, "Generated At: {}", surface.generated_at.to_rfc3339());
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Next Task");
    if let Some(task) = &surface.next_task {
        let _ = writeln!(&mut output, "  id: {}", task.id);
        let _ = writeln!(&mut output, "  title: {}", task.title);
        let _ = writeln!(&mut output, "  priority: {}", task.priority);
        if !task.linked_requirements.is_empty() {
            let _ = writeln!(&mut output, "  linked_requirements: {}", task.linked_requirements.join(", "));
        }
    } else {
        let _ = writeln!(&mut output, "  none");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Blocked Items");
    if surface.blocked_items.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for item in &surface.blocked_items {
            let reason = item.blocked_reason.as_deref().unwrap_or("n/a");
            let _ = writeln!(
                &mut output,
                "  - id={} title={} status={} reason={}",
                item.id, item.title, item.status, reason
            );
        }
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Stale Items (> {} hours in progress)", STALE_IN_PROGRESS_THRESHOLD_HOURS);
    if surface.stale_items.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for item in &surface.stale_items {
            let updated = item.updated_at.as_ref().map(|dt| dt.to_rfc3339()).unwrap_or_else(|| "unknown".to_string());
            let _ = writeln!(
                &mut output,
                "  - id={} title={} age_hours={} updated_at={}",
                item.id, item.title, item.age_hours, updated
            );
        }
    }

    output
}
