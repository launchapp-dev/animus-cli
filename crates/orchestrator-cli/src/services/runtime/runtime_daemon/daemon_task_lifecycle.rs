use super::*;
pub use orchestrator_daemon_runtime::{
    promote_backlog_tasks_to_ready, retry_failed_task_workflows,
};

pub async fn ensure_tasks_for_unplanned_requirements(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let requirements = hub.planning().list_requirements().await?;
    let tasks = hub.tasks().list().await?;

    let unplanned: Vec<String> = requirements
        .iter()
        .filter(|req| {
            !matches!(
                req.status,
                RequirementStatus::Done
                    | RequirementStatus::Implemented
                    | RequirementStatus::Deprecated
            )
        })
        .filter(|req| !requirement_has_active_tasks(req, &tasks))
        .map(|req| req.id.clone())
        .take(1)
        .collect();

    if unplanned.is_empty() {
        return Ok(0);
    }

    let summary = ensure_ai_generated_tasks_for_requirements(hub, project_root, &unplanned).await?;
    Ok(summary.requirements_generated)
}
