use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;

use crate::{
    services::ServiceHub, RequirementStatus, REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
    REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
};

fn projected_requirement_status(workflow_ref: &str) -> Option<RequirementStatus> {
    if workflow_ref.eq_ignore_ascii_case(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF) {
        return Some(RequirementStatus::Planned);
    }

    if workflow_ref.eq_ignore_ascii_case(REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF) {
        return Some(RequirementStatus::InProgress);
    }

    None
}

pub async fn project_requirement_workflow_status(
    hub: Arc<dyn ServiceHub>,
    requirement_id: &str,
    workflow_ref: &str,
) -> Result<()> {
    let Some(status) = projected_requirement_status(workflow_ref) else {
        return Ok(());
    };

    let mut requirement = hub.planning().get_requirement(requirement_id).await?;
    requirement.status = status;
    requirement.updated_at = Utc::now();
    hub.planning().upsert_requirement(requirement).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        InMemoryServiceHub, RequirementItem, RequirementLinks, RequirementPriority,
        RequirementStatus,
    };

    async fn upsert_requirement(
        hub: &Arc<InMemoryServiceHub>,
        id: &str,
        status: RequirementStatus,
    ) -> RequirementItem {
        let now = Utc::now();
        let requirement = RequirementItem {
            id: id.to_string(),
            title: format!("Requirement {id}"),
            description: "Requirement status projection".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: Vec::new(),
            priority: RequirementPriority::Should,
            status,
            source: "test".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        };

        hub.planning()
            .upsert_requirement(requirement.clone())
            .await
            .expect("upsert requirement");
        requirement
    }

    #[tokio::test]
    async fn requirement_task_generation_projects_planned() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_requirement(&hub, "REQ-1", RequirementStatus::Refined).await;

        project_requirement_workflow_status(
            hub.clone(),
            "REQ-1",
            REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
        )
        .await
        .expect("projection should succeed");

        let updated = hub
            .planning()
            .get_requirement("REQ-1")
            .await
            .expect("requirement should exist");
        assert_eq!(updated.status, RequirementStatus::Planned);
    }

    #[tokio::test]
    async fn requirement_task_generation_run_projects_in_progress() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_requirement(&hub, "REQ-2", RequirementStatus::Refined).await;

        project_requirement_workflow_status(
            hub.clone(),
            "REQ-2",
            REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
        )
        .await
        .expect("projection should succeed");

        let updated = hub
            .planning()
            .get_requirement("REQ-2")
            .await
            .expect("requirement should exist");
        assert_eq!(updated.status, RequirementStatus::InProgress);
    }

    #[tokio::test]
    async fn unrelated_workflow_ref_does_not_mutate_requirement_status() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_requirement(&hub, "REQ-3", RequirementStatus::Refined).await;

        project_requirement_workflow_status(hub.clone(), "REQ-3", "standard")
            .await
            .expect("projection should ignore unrelated workflow refs");

        let updated = hub
            .planning()
            .get_requirement("REQ-3")
            .await
            .expect("requirement should exist");
        assert_eq!(updated.status, RequirementStatus::Refined);
    }
}
