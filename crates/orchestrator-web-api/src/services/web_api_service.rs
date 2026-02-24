use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use chrono::Utc;
use orchestrator_core::{
    AgentHandoffRequestInput, DaemonStatus, DependencyType, FileServiceHub, HandoffTargetRole,
    Priority, ProjectCreateInput, ProjectMetadata, ProjectType, RequirementStatus, RiskLevel,
    ServiceHub, TaskCreateInput, TaskFilter, TaskStatus, TaskType, TaskUpdateInput,
    WorkflowRunInput,
};
use orchestrator_web_contracts::DaemonEventRecord;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::models::{WebApiContext, WebApiError};

const EVENT_SCHEMA: &str = "ao.daemon.event.v1";
const DEFAULT_UPDATED_BY: &str = "ao-web";

#[derive(Clone)]
pub struct WebApiService {
    context: Arc<WebApiContext>,
    event_tx: broadcast::Sender<DaemonEventRecord>,
    next_seq: Arc<AtomicU64>,
}

impl WebApiService {
    pub fn new(context: Arc<WebApiContext>) -> Self {
        let (event_tx, _event_rx) = broadcast::channel(1024);
        let max_seq = read_max_seq_for_project(&context.project_root).unwrap_or(0);

        Self {
            context,
            event_tx,
            next_seq: Arc::new(AtomicU64::new(max_seq)),
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEventRecord> {
        self.event_tx.subscribe()
    }

    pub fn read_events_since(
        &self,
        after_seq: Option<u64>,
    ) -> Result<Vec<DaemonEventRecord>, WebApiError> {
        let mut records = read_events_for_project(&self.context.project_root)?;
        if let Some(after_seq) = after_seq {
            records.retain(|record| record.seq > after_seq);
        }
        Ok(records)
    }

    pub async fn system_info(&self) -> Result<Value, WebApiError> {
        let status = self.context.hub.daemon().status().await?;
        let daemon_running = matches!(
            status,
            DaemonStatus::Starting
                | DaemonStatus::Running
                | DaemonStatus::Paused
                | DaemonStatus::Stopping
        );
        let daemon_status = enum_as_string(&status)?;

        Ok(json!({
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "version": self.context.app_version,
            "daemon_running": daemon_running,
            "daemon_status": daemon_status,
            "project_root": self.context.project_root,
        }))
    }

    pub async fn daemon_status(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.daemon().status().await?))
    }

    pub async fn daemon_health(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.daemon().health().await?))
    }

    pub async fn daemon_logs(&self, limit: Option<usize>) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.daemon().logs(limit).await?))
    }

    pub async fn daemon_start(&self) -> Result<Value, WebApiError> {
        self.context.hub.daemon().start().await?;
        self.publish_event("daemon-start", json!({ "message": "daemon started" }));
        Ok(json!({ "message": "daemon started" }))
    }

    pub async fn daemon_stop(&self) -> Result<Value, WebApiError> {
        self.context.hub.daemon().stop().await?;
        self.publish_event("daemon-stop", json!({ "message": "daemon stopped" }));
        Ok(json!({ "message": "daemon stopped" }))
    }

    pub async fn daemon_pause(&self) -> Result<Value, WebApiError> {
        self.context.hub.daemon().pause().await?;
        self.publish_event("daemon-pause", json!({ "message": "daemon paused" }));
        Ok(json!({ "message": "daemon paused" }))
    }

    pub async fn daemon_resume(&self) -> Result<Value, WebApiError> {
        self.context.hub.daemon().resume().await?;
        self.publish_event("daemon-resume", json!({ "message": "daemon resumed" }));
        Ok(json!({ "message": "daemon resumed" }))
    }

    pub async fn daemon_clear_logs(&self) -> Result<Value, WebApiError> {
        self.context.hub.daemon().clear_logs().await?;
        self.publish_event(
            "daemon-clear-logs",
            json!({ "message": "daemon logs cleared" }),
        );
        Ok(json!({ "message": "daemon logs cleared" }))
    }

    pub async fn daemon_agents(&self) -> Result<Value, WebApiError> {
        let active_agents = self.context.hub.daemon().active_agents().await?;
        Ok(json!({ "active_agents": active_agents }))
    }

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
        self.publish_event(
            "project-create",
            json!({ "project_id": project.id, "project_name": project.name }),
        );
        Ok(json!(project))
    }

    pub async fn projects_load(&self, id: &str) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().load(id).await?;
        self.publish_event(
            "project-load",
            json!({ "project_id": project.id, "project_name": project.name }),
        );
        Ok(json!(project))
    }

    pub async fn projects_patch(&self, id: &str, body: Value) -> Result<Value, WebApiError> {
        let request: ProjectPatchRequest = parse_json_body(body)?;
        let name = request
            .name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                WebApiError::new("invalid_input", "projects patch requires non-empty name", 2)
            })?;

        let project = self.context.hub.projects().rename(id, &name).await?;
        self.publish_event(
            "project-rename",
            json!({ "project_id": project.id, "project_name": project.name }),
        );
        Ok(json!(project))
    }

    pub async fn projects_archive(&self, id: &str) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().archive(id).await?;
        self.publish_event(
            "project-archive",
            json!({ "project_id": project.id, "project_name": project.name }),
        );
        Ok(json!(project))
    }

    pub async fn projects_delete(&self, id: &str) -> Result<Value, WebApiError> {
        self.context.hub.projects().remove(id).await?;
        self.publish_event("project-delete", json!({ "project_id": id }));
        Ok(json!({ "message": "project removed", "id": id }))
    }

    pub async fn requirements_list(&self) -> Result<Value, WebApiError> {
        Ok(json!(
            self.context.hub.planning().list_requirements().await?
        ))
    }

    pub async fn requirements_get(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(
            self.context.hub.planning().get_requirement(id).await?
        ))
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

    pub async fn tasks_list(
        &self,
        task_type: Option<String>,
        status: Option<String>,
        priority: Option<String>,
        risk: Option<String>,
        assignee_type: Option<String>,
        tags: Vec<String>,
        linked_requirement: Option<String>,
        search: Option<String>,
    ) -> Result<Value, WebApiError> {
        let task_filter = build_task_filter(
            task_type,
            status,
            priority,
            risk,
            assignee_type,
            tags,
            linked_requirement,
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
        linked_requirement: Option<String>,
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
            linked_requirement,
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
            linked_requirements: request.linked_requirements,
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
            updated_by: Some(
                request
                    .updated_by
                    .unwrap_or_else(|| DEFAULT_UPDATED_BY.to_string()),
            ),
            deadline: request.deadline,
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
        let task = self.context.hub.tasks().set_status(id, status).await?;
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

    pub async fn workflows_list(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.workflows().list().await?))
    }

    pub async fn project_workflows(&self, id: &str) -> Result<Value, WebApiError> {
        let project = self.context.hub.projects().get(id).await?;
        let hub = FileServiceHub::new(&project.path)?;
        let workflows = hub.workflows().list().await?;

        Ok(json!({
            "project": project,
            "workflows": workflows,
        }))
    }

    pub async fn workflows_get(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.workflows().get(id).await?))
    }

    pub async fn workflows_decisions(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.workflows().decisions(id).await?))
    }

    pub async fn workflows_checkpoints(&self, id: &str) -> Result<Value, WebApiError> {
        Ok(json!(
            self.context.hub.workflows().list_checkpoints(id).await?
        ))
    }

    pub async fn workflows_get_checkpoint(
        &self,
        id: &str,
        checkpoint: usize,
    ) -> Result<Value, WebApiError> {
        Ok(json!(
            self.context
                .hub
                .workflows()
                .get_checkpoint(id, checkpoint)
                .await?
        ))
    }

    pub async fn workflows_run(&self, body: Value) -> Result<Value, WebApiError> {
        let request: WorkflowRunRequest = parse_json_body(body)?;
        let workflow = self
            .context
            .hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: request.task_id,
                pipeline_id: request.pipeline_id,
            })
            .await?;
        self.publish_event(
            "workflow-run",
            json!({ "workflow_id": workflow.id, "task_id": workflow.task_id }),
        );
        Ok(json!(workflow))
    }

    pub async fn workflows_resume(&self, id: &str) -> Result<Value, WebApiError> {
        let workflow = self.context.hub.workflows().resume(id).await?;
        self.publish_event(
            "workflow-resume",
            json!({ "workflow_id": workflow.id, "status": workflow.status }),
        );
        Ok(json!(workflow))
    }

    pub async fn workflows_pause(&self, id: &str) -> Result<Value, WebApiError> {
        let workflow = self.context.hub.workflows().pause(id).await?;
        self.publish_event(
            "workflow-pause",
            json!({ "workflow_id": workflow.id, "status": workflow.status }),
        );
        Ok(json!(workflow))
    }

    pub async fn workflows_cancel(&self, id: &str) -> Result<Value, WebApiError> {
        let workflow = self.context.hub.workflows().cancel(id).await?;
        self.publish_event(
            "workflow-cancel",
            json!({ "workflow_id": workflow.id, "status": workflow.status }),
        );
        Ok(json!(workflow))
    }

    pub async fn reviews_handoff(&self, body: Value) -> Result<Value, WebApiError> {
        let request: ReviewHandoffRequest = parse_json_body(body)?;
        let target_role = parse_handoff_target_role(&request.target_role)?;
        let input = AgentHandoffRequestInput {
            handoff_id: request.handoff_id,
            run_id: request.run_id,
            target_role,
            question: request.question,
            context: request.context,
        };

        let result = self.context.hub.review().request_handoff(input).await?;
        self.publish_event(
            "review-handoff",
            json!({
                "handoff_id": result.handoff_id,
                "run_id": result.run_id,
                "target_role": result.target_role.as_str(),
                "status": result.status,
            }),
        );
        Ok(json!(result))
    }

    async fn project_requirements_snapshot(
        &self,
        project: &orchestrator_core::OrchestratorProject,
    ) -> Value {
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
            let status_key = requirement_status_key(requirement.status);
            *by_status.entry(status_key.clone()).or_default() += 1;
            let updated_at = requirement.updated_at.to_rfc3339();
            if latest_updated_at
                .as_ref()
                .map(|current| updated_at > *current)
                .unwrap_or(true)
            {
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

    fn publish_event(&self, event_type: &str, data: Value) {
        let next_seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let record = DaemonEventRecord {
            schema: EVENT_SCHEMA.to_string(),
            id: Uuid::new_v4().to_string(),
            seq: next_seq,
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            project_root: Some(self.context.project_root.clone()),
            data,
        };

        let _ = self.event_tx.send(record);
    }
}

#[derive(Debug, Deserialize)]
struct ProjectCreateRequest {
    name: String,
    path: String,
    #[serde(default)]
    project_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tech_stack: Vec<String>,
    #[serde(default)]
    metadata: Option<ProjectMetadata>,
}

#[derive(Debug, Deserialize)]
struct ProjectPatchRequest {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskCreateRequest {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    task_type: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    linked_requirements: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TaskPatchRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    updated_by: Option<String>,
    #[serde(default)]
    deadline: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskStatusRequest {
    status: String,
}

#[derive(Debug, Deserialize)]
struct TaskAssignAgentRequest {
    role: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskAssignHumanRequest {
    user_id: String,
    #[serde(default)]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskChecklistAddRequest {
    description: String,
    #[serde(default)]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskChecklistUpdateRequest {
    completed: bool,
    #[serde(default)]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskDependencyAddRequest {
    dependency_id: String,
    dependency_type: String,
    #[serde(default)]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskDependencyRemoveRequest {
    #[serde(default)]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkflowRunRequest {
    task_id: String,
    #[serde(default)]
    pipeline_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReviewHandoffRequest {
    #[serde(default)]
    handoff_id: Option<String>,
    run_id: String,
    target_role: String,
    question: String,
    #[serde(default)]
    context: Value,
}

fn parse_json_body<T: DeserializeOwned>(body: Value) -> Result<T, WebApiError> {
    serde_json::from_value(body).map_err(|error| {
        WebApiError::new("invalid_input", format!("invalid JSON body: {error}"), 2)
    })
}

fn enum_as_string<T: serde::Serialize>(value: &T) -> Result<String, WebApiError> {
    let serialized = serde_json::to_value(value)
        .map_err(|error| WebApiError::from(anyhow!("failed to serialize enum: {error}")))?;
    Ok(serialized
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "unknown".to_string()))
}

fn build_task_filter(
    task_type: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    risk: Option<String>,
    assignee_type: Option<String>,
    tags: Vec<String>,
    linked_requirement: Option<String>,
    search: Option<String>,
) -> Result<TaskFilter, WebApiError> {
    Ok(TaskFilter {
        task_type: parse_task_type_opt(task_type.as_deref())?,
        status: status
            .as_deref()
            .map(parse_task_status)
            .transpose()
            .map_err(WebApiError::from)?,
        priority: parse_priority_opt(priority.as_deref())?,
        risk: parse_risk_opt(risk.as_deref())?,
        assignee_type,
        tags: if tags.is_empty() { None } else { Some(tags) },
        linked_requirement,
        search_text: search,
    })
}

fn is_empty_task_filter(filter: &TaskFilter) -> bool {
    filter.task_type.is_none()
        && filter.status.is_none()
        && filter.priority.is_none()
        && filter.risk.is_none()
        && filter.assignee_type.is_none()
        && filter.tags.is_none()
        && filter.linked_requirement.is_none()
        && filter.search_text.is_none()
}

fn parse_task_status(value: &str) -> Result<TaskStatus, WebApiError> {
    let parsed = match value {
        "todo" | "backlog" => TaskStatus::Backlog,
        "ready" => TaskStatus::Ready,
        "in_progress" | "in-progress" => TaskStatus::InProgress,
        "done" => TaskStatus::Done,
        "blocked" => TaskStatus::Blocked,
        "on_hold" | "on-hold" => TaskStatus::OnHold,
        "cancelled" => TaskStatus::Cancelled,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid status: {value}"),
                2,
            ))
        }
    };
    Ok(parsed)
}

fn parse_handoff_target_role(value: &str) -> Result<HandoffTargetRole, WebApiError> {
    HandoffTargetRole::try_from(value)
        .map_err(|error| WebApiError::new("invalid_input", format!("{error}"), 2))
}

fn parse_task_type_opt(value: Option<&str>) -> Result<Option<TaskType>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let parsed = match value {
        "feature" => TaskType::Feature,
        "bugfix" => TaskType::Bugfix,
        "hotfix" => TaskType::Hotfix,
        "refactor" => TaskType::Refactor,
        "docs" => TaskType::Docs,
        "test" => TaskType::Test,
        "chore" => TaskType::Chore,
        "experiment" => TaskType::Experiment,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid task_type: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

fn parse_priority_opt(value: Option<&str>) -> Result<Option<Priority>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let parsed = match value {
        "critical" => Priority::Critical,
        "high" => Priority::High,
        "medium" => Priority::Medium,
        "low" => Priority::Low,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid priority: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

fn parse_risk_opt(value: Option<&str>) -> Result<Option<RiskLevel>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let parsed = match value {
        "high" => RiskLevel::High,
        "medium" => RiskLevel::Medium,
        "low" => RiskLevel::Low,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid risk: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

fn parse_dependency_type(value: &str) -> Result<DependencyType, WebApiError> {
    let parsed = match value {
        "blocks-by" | "blocks_by" | "blocksby" => DependencyType::BlocksBy,
        "blocked-by" | "blocked_by" | "blockedby" => DependencyType::BlockedBy,
        "related-to" | "related_to" | "relatedto" => DependencyType::RelatedTo,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid dependency_type: {value}"),
                2,
            ))
        }
    };

    Ok(parsed)
}

fn parse_project_type_opt(value: Option<&str>) -> Result<Option<ProjectType>, WebApiError> {
    let Some(value) = value else {
        return Ok(Some(ProjectType::Other));
    };

    let normalized = value.trim().to_ascii_lowercase();
    let parsed = match normalized.as_str() {
        "web-app" | "web_app" | "webapp" => ProjectType::WebApp,
        "mobile-app" | "mobile_app" | "mobileapp" => ProjectType::MobileApp,
        "desktop-app" | "desktop_app" | "desktopapp" => ProjectType::DesktopApp,
        "full-stack-platform"
        | "full_stack_platform"
        | "fullstackplatform"
        | "full-stack"
        | "full_stack"
        | "fullstack"
        | "saas" => ProjectType::FullStackPlatform,
        "library" => ProjectType::Library,
        "infrastructure" => ProjectType::Infrastructure,
        "other" | "greenfield" | "existing" => ProjectType::Other,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid project_type: {}", value.trim()),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

fn requirement_status_key(status: RequirementStatus) -> String {
    match status {
        RequirementStatus::Draft => "draft",
        RequirementStatus::Refined => "refined",
        RequirementStatus::Planned => "planned",
        RequirementStatus::InProgress => "in-progress",
        RequirementStatus::Done => "done",
        RequirementStatus::PoReview => "po-review",
        RequirementStatus::EmReview => "em-review",
        RequirementStatus::NeedsRework => "needs-rework",
        RequirementStatus::Approved => "approved",
        RequirementStatus::Implemented => "implemented",
        RequirementStatus::Deprecated => "deprecated",
    }
    .to_string()
}

fn daemon_events_log_path() -> PathBuf {
    protocol::Config::global_config_dir().join("daemon-events.jsonl")
}

fn read_max_seq_for_project(project_root: &str) -> Result<u64, WebApiError> {
    let records = read_events_for_project(project_root)?;
    Ok(records.iter().map(|record| record.seq).max().unwrap_or(0))
}

fn read_events_for_project(project_root: &str) -> Result<Vec<DaemonEventRecord>, WebApiError> {
    let path = daemon_events_log_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read daemon events: {}", path.display()))?;

    let mut parsed_records = Vec::new();

    for (line_number, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let fallback_seq = (line_number as u64).saturating_add(1);

        let mut record = match serde_json::from_str::<DaemonEventRecord>(trimmed) {
            Ok(record) => record,
            Err(_) => match serde_json::from_str::<Value>(trimmed) {
                Ok(raw) => value_to_event_record(raw, fallback_seq),
                Err(_) => continue,
            },
        };

        if record.seq == 0 {
            record.seq = fallback_seq;
        }

        if record.schema.trim().is_empty() {
            record.schema = EVENT_SCHEMA.to_string();
        }

        if record.id.trim().is_empty() {
            record.id = Uuid::new_v4().to_string();
        }

        if record.timestamp.trim().is_empty() {
            record.timestamp = Utc::now().to_rfc3339();
        }

        if record.event_type.trim().is_empty() {
            record.event_type = "unknown".to_string();
        }

        if record
            .project_root
            .as_ref()
            .map(|root| root == project_root)
            .unwrap_or(true)
        {
            parsed_records.push(record);
        }
    }

    parsed_records.sort_by_key(|record| record.seq);
    Ok(parsed_records)
}

fn value_to_event_record(value: Value, fallback_seq: u64) -> DaemonEventRecord {
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or(EVENT_SCHEMA)
        .to_string();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let seq = value
        .get("seq")
        .and_then(Value::as_u64)
        .unwrap_or(fallback_seq);
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let event_type = value
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let project_root = value
        .get("project_root")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let data = value.get("data").cloned().unwrap_or_else(|| json!({}));

    DaemonEventRecord {
        schema,
        id,
        seq,
        timestamp,
        event_type,
        project_root,
        data,
    }
}

#[allow(dead_code)]
fn sanitize_relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let mut safe_path = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Normal(segment) => safe_path.push(segment),
            Component::CurDir => continue,
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => return None,
        }
    }

    Some(safe_path)
}
