use orchestrator_core::{FileServiceHub, ServiceHub, TaskCreateInput, TaskUpdateInput};
use serde_json::{json, Value};

use super::{
    parsing::{
        build_task_filter, is_empty_task_filter, parse_dependency_type, parse_json_body,
        parse_priority_opt, parse_task_status, parse_task_type_opt,
    },
    requests::{
        TaskAssignAgentRequest, TaskAssignHumanRequest, TaskChecklistAddRequest,
        TaskChecklistUpdateRequest, TaskCreateRequest, TaskDependencyAddRequest,
        TaskDependencyRemoveRequest, TaskPatchRequest, TaskStatusRequest,
    },
    WebApiError, WebApiService, DEFAULT_UPDATED_BY,
};

impl WebApiService {
    pub async fn tasks_list(
        &self,
        task_type: Option<String>,
        status: Option<String>,
        priority: Option<String>,
        risk: Option<String>,
        assignee_type: Option<String>,
        tags: Vec<String>,
        labels: Vec<String>,
        linked_requirements: Vec<String>,
        requirement_search: Option<String>,
        epic_id: Option<String>,
        parent_task_id: Option<String>,
        has_parent: Option<bool>,
        has_children: Option<bool>,
        area: Option<String>,
        related_task_id: Option<String>,
        relation_type: Option<String>,
        linked_architecture_entity: Option<String>,
        search: Option<String>,
    ) -> Result<Value, WebApiError> {
        let task_filter = build_task_filter(
            task_type,
            status,
            priority,
            risk,
            assignee_type,
            tags,
            labels,
            linked_requirements,
            requirement_search,
            epic_id,
            parent_task_id,
            has_parent,
            has_children,
            area,
            related_task_id,
            relation_type,
            linked_architecture_entity,
            search,
        )?;

        if is_empty_task_filter(&task_filter) {
            return Ok(json!(self.context.hub.tasks().list().await?));
        }

        Ok(json!(
            self.context.hub.tasks().list_filtered(task_filter).await?
        ))
    }

    pub async fn project_tasks(
        &self,
        id: &str,
        task_type: Option<String>,
        status: Option<String>,
        priority: Option<String>,
        risk: Option<String>,
        assignee_type: Option<String>,
        tags: Vec<String>,
        labels: Vec<String>,
        linked_requirements: Vec<String>,
        requirement_search: Option<String>,
        epic_id: Option<String>,
        parent_task_id: Option<String>,
        has_parent: Option<bool>,
        has_children: Option<bool>,
        area: Option<String>,
        related_task_id: Option<String>,
        relation_type: Option<String>,
        linked_architecture_entity: Option<String>,
        search: Option<String>,
    ) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().get(id).await?;
        let hub = FileServiceHub::new(&project.path)?;
        let task_filter = build_task_filter(
            task_type,
            status,
            priority,
            risk,
            assignee_type,
            tags,
            labels,
            linked_requirements,
            requirement_search,
            epic_id,
            parent_task_id,
            has_parent,
            has_children,
            area,
            related_task_id,
            relation_type,
            linked_architecture_entity,
            search,
        )?;

        let tasks = if is_empty_task_filter(&task_filter) {
            hub.tasks().list().await?
        } else {
            hub.tasks().list_filtered(task_filter).await?
        };

        Ok(json!({
            "project": project,
            "tasks": tasks,
        }))
    }

    pub async fn tasks_prioritized(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.tasks().list_prioritized().await?))
    }

    pub async fn tasks_next(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.tasks().next_task().await?))
    }

    pub async fn tasks_stats(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.tasks().statistics().await?))
    }

    pub async fn tasks_get(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.tasks().get(id).await?))
    }

    pub async fn tasks_create(&self, body: Value) -> Result<Value, WebApiError> {
        let request: TaskCreateRequest = parse_json_body(body)?;
        let input = TaskCreateInput {
            title: request.title,
            description: request.description,
            task_type: parse_task_type_opt(request.task_type.as_deref())?,
            priority: parse_priority_opt(request.priority.as_deref())?,
            created_by: Some(
                request
                    .created_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            ),
            tags: request.tags,
            labels: request.labels,
            area: request.area,
            external_ref: request.external_ref,
            epic_id: request.epic_id,
            parent_task_id: request.parent_task_id,
            linked_requirements: request.linked_requirements,
            linked_architecture_entities: request.linked_architecture_entities,
        };

        let task = self.context.hub.tasks().create(input).await?;
        self.publish_event(
            "task-create",
            json!({ "task_id": task.id, "status": task.status }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_patch(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: TaskPatchRequest = parse_json_body(body)?;
        let input = TaskUpdateInput {
            title: request.title,
            description: request.description,
            priority: parse_priority_opt(request.priority.as_deref())?,
            status: request
                .status
                .as_deref()
                .map(parse_task_status)
                .transpose()?,
            assignee: request.assignee,
            tags: request.tags,
            labels: request.labels,
            area: request.area,
            external_ref: request.external_ref,
            epic_id: request.epic_id,
            parent_task_id: request.parent_task_id,
            linked_requirements: request.linked_requirements,
            updated_by: Some(
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            ),
            deadline: request.deadline,
            linked_architecture_entities: request.linked_architecture_entities,
        };

        let task = self.context.hub.tasks().update(id, input).await?;
        self.publish_event(
            "task-update",
            json!({ "task_id": task.id, "status": task.status }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_delete(&self, id: &str) -> Result<Value, WebApiError> {
        self.context.hub.tasks().delete(id).await?;
        self.publish_event("task-delete", json!({ "task_id": id }));
        Ok(json!({ "message": "task deleted", "id": id }))
    }

    pub async fn tasks_status(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: TaskStatusRequest = parse_json_body(body)?;
        let status = parse_task_status(&request.status)?;
        let task = self
            .context
            .hub
            .tasks()
            .set_status(id, status, true)
            .await?;
        self.publish_event(
            "task-status",
            json!({ "task_id": task.id, "status": task.status }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_assign_agent(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: TaskAssignAgentRequest = parse_json_body(body)?;
        let task = self
            .context
            .hub
            .tasks()
            .assign_agent(
                id,
                request.role,
                request.model,
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            )
            .await?;
        self.publish_event(
            "task-assign-agent",
            json!({ "task_id": task.id, "assignee": task.assignee }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_assign_human(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: TaskAssignHumanRequest = parse_json_body(body)?;
        let task = self
            .context
            .hub
            .tasks()
            .assign_human(
                id,
                request.user_id,
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            )
            .await?;
        self.publish_event(
            "task-assign-human",
            json!({ "task_id": task.id, "assignee": task.assignee }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_checklist_add(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: TaskChecklistAddRequest = parse_json_body(body)?;
        let task = self
            .context
            .hub
            .tasks()
            .add_checklist_item(
                id,
                request.description,
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            )
            .await?;
        self.publish_event(
            "task-checklist-add",
            json!({ "task_id": task.id, "checklist_count": task.checklist.len() }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_checklist_update(
        &self,
        id: &str,
        item_id: &str,
        body: Value,
    ) -> Result<Value, WebApiError> {
        let request: TaskChecklistUpdateRequest = parse_json_body(body)?;
        let task = self
            .context
            .hub
            .tasks()
            .update_checklist_item(
                id,
                item_id,
                request.completed,
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            )
            .await?;
        self.publish_event(
            "task-checklist-update",
            json!({ "task_id": task.id, "item_id": item_id, "completed": request.completed }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_dependency_add(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: TaskDependencyAddRequest = parse_json_body(body)?;
        let dependency_type = parse_dependency_type(&request.dependency_type)?;
        let task = self
            .context
            .hub
            .tasks()
            .add_dependency(
                id,
                &request.dependency_id,
                dependency_type,
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            )
            .await?;
        self.publish_event(
            "task-dependency-add",
            json!({ "task_id": task.id, "dependency_id": request.dependency_id }),
        );
        Ok(json!(task))
    }

    pub async fn tasks_dependency_remove(
        &self,
        id: &str,
        dependency_id: &str,
        body: Option<Value>,
    ) -> Result<Value, WebApiError> {
        let updated_by = match body {
            Some(value) => {
                let request: TaskDependencyRemoveRequest = parse_json_body(value)?;
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string())
            }
            None => DEFAULT_UPDATED_BY.to_string(),
        };

        let task = self
            .context
            .hub
            .tasks()
            .remove_dependency(id, dependency_id, updated_by)
            .await?;

        self.publish_event(
            "task-dependency-remove",
            json!({ "task_id": task.id, "dependency_id": dependency_id }),
        );
        Ok(json!(task))
    }
}
