use orchestrator_core::{
    workflow_ref_for_task, FileServiceHub, ServiceHub, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
    STANDARD_WORKFLOW_REF,
};
use protocol::orchestrator::WorkflowSubject;
use serde_json::{json, Value};

use super::{parsing::parse_json_body, requests::WorkflowRunRequest, WebApiError, WebApiService};

async fn resolve_workflow_run_dispatch(
    hub: &dyn ServiceHub,
    project_root: &str,
    request: WorkflowRunRequest,
) -> Result<protocol::SubjectDispatch, WebApiError> {
    match (request.task_id, request.requirement_id, request.title) {
        (Some(task_id), None, None) => {
            let task = hub.tasks().get(&task_id).await.map_err(WebApiError::from)?;
            Ok(protocol::SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                request
                    .workflow_ref
                    .unwrap_or_else(|| workflow_ref_for_task(&task)),
                "web-api-run",
                chrono::Utc::now(),
            ))
        }
        (None, Some(requirement_id), None) => {
            hub.planning()
                .get_requirement(&requirement_id)
                .await
                .map_err(WebApiError::from)?;
            let workflow_ref = match request.workflow_ref {
                Some(workflow_ref) => workflow_ref,
                None => resolve_requirement_workflow_ref(project_root)
                    .map_err(|message| WebApiError::new("invalid_input", message, 2))?,
            };
            Ok(protocol::SubjectDispatch::for_requirement(
                requirement_id,
                workflow_ref,
                "web-api-run",
            ))
        }
        (None, None, Some(title)) => Ok(protocol::SubjectDispatch::for_custom(
            title,
            request.description.unwrap_or_default(),
            request
                .workflow_ref
                .unwrap_or_else(|| STANDARD_WORKFLOW_REF.to_string()),
            None,
            "web-api-run",
        )),
        (None, None, None) => Err(WebApiError::new(
            "invalid_input",
            "one of task_id, requirement_id, or title must be provided".to_string(),
            2,
        )),
        _ => Err(WebApiError::new(
            "invalid_input",
            "task_id, requirement_id, and title are mutually exclusive".to_string(),
            2,
        )),
    }
}

fn resolve_requirement_workflow_ref(project_root: &str) -> Result<String, String> {
    let root = std::path::Path::new(project_root);
    orchestrator_core::ensure_workflow_config_compiled(root).map_err(|error| error.to_string())?;
    let workflow_config =
        orchestrator_core::load_workflow_config(root).map_err(|error| error.to_string())?;
    workflow_config
        .workflows
        .iter()
        .any(|workflow| {
            workflow
                .id
                .eq_ignore_ascii_case(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF)
        })
        .then(|| REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string())
        .ok_or_else(|| {
            format!(
                "requirement workflow '{}' is not configured for requirement subjects",
                REQUIREMENT_TASK_GENERATION_WORKFLOW_REF
            )
        })
}

impl WebApiService {
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
        let dispatch = resolve_workflow_run_dispatch(
            self.context.hub.as_ref(),
            &self.context.project_root,
            request,
        )
        .await?;
        let workflow = self
            .context
            .hub
            .workflows()
            .run(dispatch.to_workflow_run_input())
            .await?;
        let subject_id = match &workflow.subject {
            WorkflowSubject::Task { id } | WorkflowSubject::Requirement { id } => id.clone(),
            WorkflowSubject::Custom { title, .. } => title.clone(),
        };
        self.publish_event(
            "workflow-run",
            json!({
                "workflow_id": workflow.id,
                "subject_id": subject_id,
                "task_id": workflow.task_id,
            }),
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
}
