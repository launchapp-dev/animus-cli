use super::*;

pub(super) fn list_projects(state: &CoreState) -> Vec<OrchestratorProject> {
    state.projects.values().cloned().collect()
}

pub(super) fn get_project(state: &CoreState, id: &str) -> Result<OrchestratorProject> {
    state
        .projects
        .get(id)
        .cloned()
        .ok_or_else(|| not_found(format!("project not found: {id}")))
}

pub(super) fn active_project(state: &CoreState) -> Option<OrchestratorProject> {
    let active_id = state.active_project_id.as_deref()?;
    state.projects.get(active_id).cloned()
}

pub(super) fn create_project(
    state: &mut CoreState,
    input: ProjectCreateInput,
    now: chrono::DateTime<Utc>,
) -> OrchestratorProject {
    let mut metadata = input.metadata.unwrap_or_default();
    if metadata.description.is_none() {
        metadata.description = input.description.clone();
    }

    let config = ProjectConfig {
        project_type: input.project_type.unwrap_or(ProjectType::Other),
        tech_stack: input.tech_stack,
        ..ProjectConfig::default()
    };

    let id = Uuid::new_v4().to_string();
    let project = OrchestratorProject {
        id: id.clone(),
        name: input.name,
        path: input.path,
        config,
        metadata,
        created_at: now,
        updated_at: now,
        archived: false,
    };

    state.projects.insert(id.clone(), project.clone());
    state.active_project_id = Some(id);
    project
}

pub(super) fn upsert_project(
    state: &mut CoreState,
    project: OrchestratorProject,
    now: chrono::DateTime<Utc>,
) -> OrchestratorProject {
    let mut project = project;
    if project.created_at.timestamp() == 0 {
        project.created_at = now;
    }
    project.updated_at = now;

    state.projects.insert(project.id.clone(), project.clone());
    if state.active_project_id.is_none() {
        state.active_project_id = Some(project.id.clone());
    }
    project
}

pub(super) fn load_project(state: &mut CoreState, id: &str) -> Result<OrchestratorProject> {
    if let Some(project) = state.projects.get(id).cloned() {
        state.active_project_id = Some(project.id.clone());
        return Ok(project);
    }

    let project = state
        .projects
        .values()
        .find(|project| project.path == id)
        .cloned()
        .ok_or_else(|| not_found(format!("project not found: {id}")))?;
    state.active_project_id = Some(project.id.clone());
    Ok(project)
}

pub(super) fn rename_project(
    state: &mut CoreState,
    id: &str,
    new_name: &str,
    now: chrono::DateTime<Utc>,
) -> Result<OrchestratorProject> {
    let project = state
        .projects
        .get_mut(id)
        .ok_or_else(|| not_found(format!("project not found: {id}")))?;
    project.name = new_name.to_string();
    project.updated_at = now;
    Ok(project.clone())
}

pub(super) fn archive_project(
    state: &mut CoreState,
    id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<OrchestratorProject> {
    let project = state
        .projects
        .get_mut(id)
        .ok_or_else(|| not_found(format!("project not found: {id}")))?;
    project.archived = true;
    project.updated_at = now;
    Ok(project.clone())
}

pub(super) fn remove_project(state: &mut CoreState, id: &str) -> Result<()> {
    state
        .projects
        .remove(id)
        .ok_or_else(|| not_found(format!("project not found: {id}")))?;
    if state.active_project_id.as_deref() == Some(id) {
        state.active_project_id = None;
    }
    Ok(())
}
