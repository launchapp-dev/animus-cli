use super::*;

pub async fn bootstrap_from_vision_if_needed(
    hub: Arc<dyn ServiceHub>,
    include_codebase_scan: bool,
    ai_task_generation: bool,
) -> Result<()> {
    let planning = hub.planning();
    let Some(vision) = planning.get_vision().await? else {
        return Ok(());
    };

    let tasks = hub.tasks().list().await?;
    if !tasks.is_empty() {
        return Ok(());
    }

    let mut requirements = planning.list_requirements().await?;
    if requirements.is_empty() {
        let draft = planning
            .draft_requirements(RequirementsDraftInput {
                include_codebase_scan,
                append_only: true,
                max_requirements: bootstrap_max_requirements(),
            })
            .await?;
        requirements = draft.requirements;
    }

    if requirements.is_empty() {
        return Ok(());
    }

    if requirements.iter().any(requirement_needs_refinement) {
        let requirement_ids = requirements
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect();
        planning
            .refine_requirements(RequirementsRefineInput {
                requirement_ids,
                focus: Some(
                    "Production-quality scope with measurable outcomes, QA gates, and delivery readiness."
                        .to_string(),
                ),
            })
            .await?;
        requirements = planning.list_requirements().await?;
    }

    let mut requirement_ids: Vec<String> = requirements
        .iter()
        .filter(|requirement| !requirement.source.eq_ignore_ascii_case("baseline"))
        .map(|requirement| requirement.id.clone())
        .collect();
    if requirement_ids.is_empty() {
        requirement_ids = requirements
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect();
    }
    if ai_task_generation {
        ensure_ai_generated_tasks_for_requirements(
            hub.clone(),
            &vision.project_root,
            &requirement_ids,
        )
        .await?;
    }
    planning
        .execute_requirements(RequirementsExecutionInput {
            requirement_ids,
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        })
        .await?;

    Ok(())
}
