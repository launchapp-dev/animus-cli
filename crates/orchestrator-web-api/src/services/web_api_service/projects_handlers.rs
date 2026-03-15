use std::collections::BTreeMap;

use orchestrator_core::{FileServiceHub, ProjectCreateInput, ServiceHub};
use serde_json::{json, Value};

use super::{
    parsing::{parse_json_body, parse_project_type_opt},
    requests::{ProjectCreateRequest, ProjectPatchRequest},
    WebApiError, WebApiService,
};

impl WebApiService {
    pub async fn projects_list(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.projects().list().await?))
    }

    pub async fn projects_active(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.projects().active().await?))
    }

    pub async fn projects_get(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.projects().get(id).await?))
    }

    pub async fn projects_create(&self, body: Value) -> Result<Value, WebApiError> {
        let request: ProjectCreateRequest = parse_json_body(body)?;
        let input = ProjectCreateInput {
            name: request.name,
            path: request.path,
            project_type: parse_project_type_opt(request.project_type.as_deref())?,
            description: request.description,
            tech_stack: request.tech_stack,
            metadata: request.metadata,
        };
        let project = self.context.hub.projects().create(input).await?;
        self.publish_event("project-create", json!({ "project_id": project.id, "project_name": project.name }));
        Ok(json!(project))
    }

    pub async fn projects_load(&self, id: &str) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().load(id).await?;
        self.publish_event("project-load", json!({ "project_id": project.id, "project_name": project.name }));
        Ok(json!(project))
    }

    pub async fn projects_patch(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: ProjectPatchRequest = parse_json_body(body)?;
        let name = request
            .name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| WebApiError::new("invalid_input", "projects patch requires non-empty name", 2))?;

        let project = self.context.hub.projects().rename(id, &name).await?;
        self.publish_event("project-rename", json!({ "project_id": project.id, "project_name": project.name }));
        Ok(json!(project))
    }

    pub async fn projects_archive(&self, id: &str) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().archive(id).await?;
        self.publish_event("project-archive", json!({ "project_id": project.id, "project_name": project.name }));
        Ok(json!(project))
    }

    pub async fn projects_delete(&self, id: &str) -> Result<Value, WebApiError> {
        self.context.hub.projects().remove(id).await?;
        self.publish_event("project-delete", json!({ "project_id": id }));
        Ok(json!({ "message": "project removed", "id": id }))
    }

    pub async fn projects_requirements(&self) -> Result<Value, WebApiError> {
        let mut projects = self.context.hub.projects().list().await?;
        projects.sort_by(|left, right| left.name.cmp(&right.name));

        let mut snapshots = Vec::with_capacity(projects.len());
        for project in &projects {
            snapshots.push(self.project_requirements_snapshot(project).await);
        }

        Ok(json!(snapshots))
    }

    pub async fn projects_requirements_by_id(&self, id: &str) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().get(id).await?;
        Ok(self.project_requirements_snapshot(&project).await)
    }

    async fn project_requirements_snapshot(&self, project: &orchestrator_core::OrchestratorProject) -> Value {
        let mut snapshot = json!({
            "project_id": project.id,
            "project_name": project.name,
            "project_path": project.path,
            "project_archived": project.archived,
            "requirement_count": 0,
            "by_status": {},
            "latest_updated_at": null,
            "requirements": [],
        });

        let hub = match FileServiceHub::new(&project.path) {
            Ok(hub) => hub,
            Err(error) => {
                snapshot["error"] = json!(error.to_string());
                return snapshot;
            }
        };

        let requirements = match hub.planning().list_requirements().await {
            Ok(requirements) => requirements,
            Err(error) => {
                snapshot["error"] = json!(error.to_string());
                return snapshot;
            }
        };

        let mut by_status = BTreeMap::<String, usize>::new();
        let mut latest_updated_at = None::<String>;
        let mut requirement_rows = Vec::with_capacity(requirements.len());

        for requirement in requirements {
            let status_key = requirement.status.to_string();
            *by_status.entry(status_key.clone()).or_default() += 1;
            let updated_at = requirement.updated_at.to_rfc3339();
            if latest_updated_at.as_ref().map(|current| updated_at > *current).unwrap_or(true) {
                latest_updated_at = Some(updated_at.clone());
            }

            requirement_rows.push(json!({
                "id": requirement.id,
                "title": requirement.title,
                "description": requirement.description,
                "status": status_key,
                "priority": requirement.priority,
                "updated_at": updated_at,
                "task_links": requirement.links.tasks.len() + requirement.linked_task_ids.len(),
                "workflow_links": requirement.links.workflows.len(),
                "test_links": requirement.links.tests.len(),
                "relative_path": requirement.relative_path,
            }));
        }

        snapshot["requirement_count"] = json!(requirement_rows.len());
        snapshot["by_status"] = json!(by_status);
        snapshot["latest_updated_at"] = json!(latest_updated_at);
        snapshot["requirements"] = json!(requirement_rows);
        snapshot
    }
}
