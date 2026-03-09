use chrono::Utc;
use orchestrator_core::{
    FileServiceHub, RequirementItem, RequirementPriority, RequirementStatus,
    RequirementsDraftInput, RequirementsRefineInput, ServiceHub,
};
use serde_json::{json, Value};

use super::{
    parsing::{
        build_requirement_filter, normalize_optional_string, normalize_string_list,
        parse_json_body, parse_requirement_priority, parse_requirement_priority_opt,
        parse_requirement_status, parse_requirement_status_opt, parse_requirement_type_opt,
    },
    requests::{
        RequirementCreateRequest, RequirementPatchRequest, RequirementsDraftRequest,
        RequirementsRefineRequest,
    },
    WebApiError, WebApiService, DEFAULT_REQUIREMENT_SOURCE,
};

impl WebApiService {
    pub async fn requirements_list(
        &self,
        status: Option<String>,
        priority: Option<String>,
        requirement_type: Option<String>,
        category: Option<String>,
        tags: Vec<String>,
        labels: Vec<String>,
        area: Option<String>,
        source: Option<String>,
        epic_id: Option<String>,
        linked_task_id: Option<String>,
        search: Option<String>,
    ) -> Result<Value, WebApiError> {
        let filter = build_requirement_filter(
            status,
            priority,
            requirement_type,
            category,
            tags,
            labels,
            area,
            source,
            epic_id,
            linked_task_id,
            search,
        )?;
        Ok(json!(
            self.context
                .hub
                .planning()
                .list_requirements_filtered(filter)
                .await?
        ))
    }

    pub async fn requirements_get(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(
            self.context.hub.planning().get_requirement(id).await?
        ))
    }

    pub async fn requirements_create(&self, body: Value) -> Result<Value, WebApiError> {
        let request: RequirementCreateRequest = parse_json_body(body)?;
        let mut title = request.title.trim().to_string();
        if title.is_empty() {
            return Err(WebApiError::new(
                "invalid_input",
                "requirement title is required",
                2,
            ));
        }
        title.shrink_to_fit();

        let mut requirement_id = String::new();
        if let Some(id) = normalize_optional_string(request.id) {
            if self
                .context
                .hub
                .planning()
                .get_requirement(&id)
                .await
                .is_ok()
            {
                return Err(WebApiError::new(
                    "conflict",
                    format!("requirement already exists: {id}"),
                    4,
                ));
            }
            requirement_id = id;
        }

        let now = Utc::now();
        let requirement = RequirementItem {
            id: requirement_id,
            title,
            description: request.description.unwrap_or_default(),
            body: normalize_optional_string(request.body),
            legacy_id: None,
            category: normalize_optional_string(request.category),
            requirement_type: parse_requirement_type_opt(request.requirement_type.as_deref())?,
            acceptance_criteria: normalize_string_list(request.acceptance_criteria),
            priority: parse_requirement_priority_opt(request.priority.as_deref())?
                .unwrap_or(RequirementPriority::Should),
            status: parse_requirement_status_opt(request.status.as_deref())?
                .unwrap_or(RequirementStatus::Draft),
            source: normalize_optional_string(request.source)
                .unwrap_or_else(|| DEFAULT_REQUIREMENT_SOURCE.to_string()),
            tags: normalize_string_list(request.tags),
            labels: normalize_string_list(request.labels),
            area: normalize_optional_string(request.area),
            external_ref: normalize_optional_string(request.external_ref),
            linked_epic_ids: normalize_string_list(request.linked_epic_ids),
            links: Default::default(),
            comments: Vec::new(),
            relative_path: normalize_optional_string(request.relative_path),
            linked_task_ids: normalize_string_list(request.linked_task_ids),
            created_at: now,
            updated_at: now,
        };

        let created = self
            .context
            .hub
            .planning()
            .upsert_requirement(requirement)
            .await?;
        self.publish_event(
            "requirement-create",
            json!({ "requirement_id": created.id, "status": created.status }),
        );
        Ok(json!(created))
    }

    pub async fn requirements_patch(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: RequirementPatchRequest = parse_json_body(body)?;
        let mut requirement = self.context.hub.planning().get_requirement(id).await?;

        if let Some(title) = request.title {
            let title = title.trim().to_string();
            if title.is_empty() {
                return Err(WebApiError::new(
                    "invalid_input",
                    "requirement title must be non-empty when provided",
                    2,
                ));
            }
            requirement.title = title;
        }

        if let Some(description) = request.description {
            requirement.description = description;
        }

        if let Some(body) = request.body {
            requirement.body = normalize_optional_string(Some(body));
        }

        if let Some(category) = request.category {
            requirement.category = normalize_optional_string(Some(category));
        }

        if let Some(requirement_type) = request.requirement_type {
            requirement.requirement_type =
                parse_requirement_type_opt(Some(requirement_type.as_str()))?;
        }

        if let Some(criteria) = request.acceptance_criteria {
            requirement.acceptance_criteria = normalize_string_list(criteria);
        }

        if let Some(priority) = request.priority {
            requirement.priority = parse_requirement_priority(&priority)?;
        }

        if let Some(status) = request.status {
            requirement.status = parse_requirement_status(&status)?;
        }

        if let Some(source) = request.source {
            requirement.source = normalize_optional_string(Some(source))
                .unwrap_or_else(|| DEFAULT_REQUIREMENT_SOURCE.to_string());
        }

        if let Some(tags) = request.tags {
            requirement.tags = normalize_string_list(tags);
        }

        if let Some(labels) = request.labels {
            requirement.labels = normalize_string_list(labels);
        }

        if let Some(area) = request.area {
            requirement.area = normalize_optional_string(Some(area));
        }

        if let Some(external_ref) = request.external_ref {
            requirement.external_ref = normalize_optional_string(Some(external_ref));
        }

        if let Some(linked_epic_ids) = request.linked_epic_ids {
            requirement.linked_epic_ids = normalize_string_list(linked_epic_ids);
        }

        if let Some(linked_task_ids) = request.linked_task_ids {
            requirement.linked_task_ids = normalize_string_list(linked_task_ids);
        }

        if let Some(relative_path) = request.relative_path {
            requirement.relative_path = normalize_optional_string(Some(relative_path));
        }

        let updated = self
            .context
            .hub
            .planning()
            .upsert_requirement(requirement)
            .await?;
        self.publish_event(
            "requirement-update",
            json!({ "requirement_id": updated.id, "status": updated.status }),
        );
        Ok(json!(updated))
    }

    pub async fn requirements_delete(&self, id: &str) -> Result<Value, WebApiError> {
        self.context.hub.planning().delete_requirement(id).await?;
        self.publish_event("requirement-delete", json!({ "requirement_id": id }));
        Ok(json!({ "message": "requirement deleted", "id": id }))
    }

    pub async fn requirements_draft(&self, body: Value) -> Result<Value, WebApiError> {
        let request: RequirementsDraftRequest = parse_json_body(body)?;
        let input = RequirementsDraftInput {
            include_codebase_scan: request.include_codebase_scan,
            append_only: request.append_only,
            max_requirements: request.max_requirements.unwrap_or_default(),
        };

        let result = self
            .context
            .hub
            .planning()
            .draft_requirements(input)
            .await?;
        self.publish_event(
            "requirements-draft",
            json!({ "appended_count": result.appended_count }),
        );
        Ok(json!(result))
    }

    pub async fn requirements_refine(&self, body: Value) -> Result<Value, WebApiError> {
        let request: RequirementsRefineRequest = parse_json_body(body)?;
        let requirement_ids = normalize_string_list(request.requirement_ids);
        let focus = normalize_optional_string(request.focus);

        let refined = self
            .context
            .hub
            .planning()
            .refine_requirements(RequirementsRefineInput {
                requirement_ids: requirement_ids.clone(),
                focus: focus.clone(),
            })
            .await?;

        let mut updated_ids: Vec<String> = refined
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect();
        updated_ids.sort();
        updated_ids.dedup();

        self.publish_event(
            "requirements-refine",
            json!({
                "scope": if requirement_ids.is_empty() { "all" } else { "selected" },
                "updated_count": updated_ids.len(),
            }),
        );

        Ok(json!({
            "requirements": refined,
            "updated_ids": updated_ids,
            "requested_ids": requirement_ids,
            "scope": if requirement_ids.is_empty() { "all" } else { "selected" },
            "focus": focus,
        }))
    }

    pub async fn project_requirement_get(
        &self,
        project_id: &str,
        requirement_id: &str,
    ) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().get(project_id).await?;
        let hub = FileServiceHub::new(&project.path)?;
        let requirement = hub.planning().get_requirement(requirement_id).await?;
        let markdown = requirement
            .body
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| requirement.description.clone());

        Ok(json!({
            "project_id": project.id,
            "project_name": project.name,
            "project_path": project.path,
            "requirement": requirement,
            "markdown": markdown,
        }))
    }
}
