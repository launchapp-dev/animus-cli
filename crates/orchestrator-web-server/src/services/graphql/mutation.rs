use async_graphql::{Context, Object, Result, ID};
use orchestrator_web_api::WebApiService;
use serde_json::json;

use super::types::{
    GqlProject, GqlRequirement, GqlTask, GqlVision, GqlWorkflow, RawRequirement, RawTask,
    RawWorkflow,
};

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    // -----------------------------------------------------------------------
    // Task mutations
    // -----------------------------------------------------------------------

    async fn create_task(
        &self,
        ctx: &Context<'_>,
        title: String,
        description: Option<String>,
        task_type: Option<String>,
        priority: Option<String>,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "title": title,
            "description": description.unwrap_or_default(),
            "type": task_type,
            "priority": priority,
        });
        let val = api
            .tasks_create(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn update_task(
        &self,
        ctx: &Context<'_>,
        id: ID,
        title: Option<String>,
        description: Option<String>,
        task_type: Option<String>,
        priority: Option<String>,
        risk: Option<String>,
        scope: Option<String>,
        complexity: Option<String>,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let mut body = serde_json::Map::new();
        if let Some(v) = title {
            body.insert("title".into(), json!(v));
        }
        if let Some(v) = description {
            body.insert("description".into(), json!(v));
        }
        if let Some(v) = task_type {
            body.insert("type".into(), json!(v));
        }
        if let Some(v) = priority {
            body.insert("priority".into(), json!(v));
        }
        if let Some(v) = risk {
            body.insert("risk".into(), json!(v));
        }
        if let Some(v) = scope {
            body.insert("scope".into(), json!(v));
        }
        if let Some(v) = complexity {
            body.insert("complexity".into(), json!(v));
        }
        let val = api
            .tasks_patch(&id, serde_json::Value::Object(body))
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn update_task_status(
        &self,
        ctx: &Context<'_>,
        id: ID,
        status: String,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "status": status });
        let val = api
            .tasks_status(&id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn delete_task(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.tasks_delete(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn assign_agent(
        &self,
        ctx: &Context<'_>,
        id: ID,
        role: Option<String>,
        model: Option<String>,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "role": role, "model": model });
        let val = api
            .tasks_assign_agent(&id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn assign_human(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: String,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "name": name });
        let val = api
            .tasks_assign_human(&id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn checklist_add(
        &self,
        ctx: &Context<'_>,
        id: ID,
        description: String,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "description": description });
        let val = api
            .tasks_checklist_add(&id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn checklist_update(
        &self,
        ctx: &Context<'_>,
        id: ID,
        item_id: String,
        completed: Option<bool>,
        description: Option<String>,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "completed": completed,
            "description": description,
        });
        let val = api
            .tasks_checklist_update(&id, &item_id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn dependency_add(
        &self,
        ctx: &Context<'_>,
        id: ID,
        depends_on: String,
        dependency_type: Option<String>,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "depends_on": depends_on,
            "dependency_type": dependency_type,
        });
        let val = api
            .tasks_dependency_add(&id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    async fn dependency_remove(
        &self,
        ctx: &Context<'_>,
        id: ID,
        depends_on: String,
    ) -> Result<GqlTask> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .tasks_dependency_remove(&id, &depends_on, None)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawTask = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse task: {e}")))?;
        Ok(GqlTask(raw))
    }

    // -----------------------------------------------------------------------
    // Requirement mutations
    // -----------------------------------------------------------------------

    async fn create_requirement(
        &self,
        ctx: &Context<'_>,
        title: String,
        description: Option<String>,
        priority: Option<String>,
        requirement_type: Option<String>,
        acceptance_criteria: Option<Vec<String>>,
    ) -> Result<GqlRequirement> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "title": title,
            "description": description.unwrap_or_default(),
            "priority": priority,
            "type": requirement_type,
            "acceptance_criteria": acceptance_criteria.unwrap_or_default(),
        });
        let val = api
            .requirements_create(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawRequirement = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse requirement: {e}")))?;
        Ok(GqlRequirement(raw))
    }

    async fn update_requirement(
        &self,
        ctx: &Context<'_>,
        id: ID,
        title: Option<String>,
        description: Option<String>,
        priority: Option<String>,
        status: Option<String>,
        requirement_type: Option<String>,
        acceptance_criteria: Option<Vec<String>>,
    ) -> Result<GqlRequirement> {
        let api = ctx.data::<WebApiService>()?;
        let mut body = serde_json::Map::new();
        if let Some(v) = title {
            body.insert("title".into(), json!(v));
        }
        if let Some(v) = description {
            body.insert("description".into(), json!(v));
        }
        if let Some(v) = priority {
            body.insert("priority".into(), json!(v));
        }
        if let Some(v) = status {
            body.insert("status".into(), json!(v));
        }
        if let Some(v) = requirement_type {
            body.insert("type".into(), json!(v));
        }
        if let Some(v) = acceptance_criteria {
            body.insert("acceptance_criteria".into(), json!(v));
        }
        let val = api
            .requirements_patch(&id, serde_json::Value::Object(body))
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawRequirement = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse requirement: {e}")))?;
        Ok(GqlRequirement(raw))
    }

    async fn delete_requirement(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.requirements_delete(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn draft_requirement(
        &self,
        ctx: &Context<'_>,
        context: Option<String>,
    ) -> Result<GqlRequirement> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "context": context });
        let val = api
            .requirements_draft(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawRequirement = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse requirement: {e}")))?;
        Ok(GqlRequirement(raw))
    }

    async fn refine_requirement(
        &self,
        ctx: &Context<'_>,
        id: String,
        feedback: Option<String>,
    ) -> Result<GqlRequirement> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "id": id, "feedback": feedback });
        let val = api
            .requirements_refine(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawRequirement = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse requirement: {e}")))?;
        Ok(GqlRequirement(raw))
    }

    // -----------------------------------------------------------------------
    // Workflow mutations
    // -----------------------------------------------------------------------

    async fn run_workflow(
        &self,
        ctx: &Context<'_>,
        task_id: String,
        workflow_ref: Option<String>,
    ) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "task_id": task_id,
            "workflow_ref": workflow_ref,
        });
        let val = api
            .workflows_run(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawWorkflow = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse workflow: {e}")))?;
        Ok(GqlWorkflow(raw))
    }

    async fn pause_workflow(&self, ctx: &Context<'_>, id: ID) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_pause(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawWorkflow = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse workflow: {e}")))?;
        Ok(GqlWorkflow(raw))
    }

    async fn resume_workflow(
        &self,
        ctx: &Context<'_>,
        id: ID,
        feedback: Option<String>,
    ) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_resume(&id, feedback)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawWorkflow = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse workflow: {e}")))?;
        Ok(GqlWorkflow(raw))
    }

    async fn cancel_workflow(&self, ctx: &Context<'_>, id: ID) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_cancel(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawWorkflow = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse workflow: {e}")))?;
        Ok(GqlWorkflow(raw))
    }

    async fn approve_phase(
        &self,
        ctx: &Context<'_>,
        workflow_id: ID,
        phase_id: String,
        note: Option<String>,
    ) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_phase_approve(&workflow_id, &phase_id, note)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let raw: RawWorkflow = serde_json::from_value(val)
            .map_err(|e| async_graphql::Error::new(format!("failed to parse workflow: {e}")))?;
        Ok(GqlWorkflow(raw))
    }

    // -----------------------------------------------------------------------
    // Daemon mutations
    // -----------------------------------------------------------------------

    async fn daemon_start(&self, ctx: &Context<'_>) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.daemon_start()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn daemon_stop(&self, ctx: &Context<'_>) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.daemon_stop()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn daemon_pause(&self, ctx: &Context<'_>) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.daemon_pause()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn daemon_resume(&self, ctx: &Context<'_>) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.daemon_resume()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn daemon_clear_logs(&self, ctx: &Context<'_>) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.daemon_clear_logs()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Project mutations
    // -----------------------------------------------------------------------

    async fn create_project(
        &self,
        ctx: &Context<'_>,
        name: String,
        path: String,
        description: Option<String>,
        project_type: Option<String>,
    ) -> Result<GqlProject> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "name": name,
            "path": path,
            "description": description,
            "type": project_type,
        });
        let val = api
            .projects_create(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(GqlProject(val))
    }

    async fn update_project(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: Option<String>,
        description: Option<String>,
        project_type: Option<String>,
    ) -> Result<GqlProject> {
        let api = ctx.data::<WebApiService>()?;
        let mut body = serde_json::Map::new();
        if let Some(v) = name {
            body.insert("name".into(), json!(v));
        }
        if let Some(v) = description {
            body.insert("description".into(), json!(v));
        }
        if let Some(v) = project_type {
            body.insert("type".into(), json!(v));
        }
        let val = api
            .projects_patch(&id, serde_json::Value::Object(body))
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(GqlProject(val))
    }

    async fn delete_project(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        api.projects_delete(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn load_project(&self, ctx: &Context<'_>, id: ID) -> Result<GqlProject> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .projects_load(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(GqlProject(val))
    }

    async fn archive_project(&self, ctx: &Context<'_>, id: ID) -> Result<GqlProject> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .projects_archive(&id)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(GqlProject(val))
    }

    // -----------------------------------------------------------------------
    // Queue mutations
    // -----------------------------------------------------------------------

    async fn queue_reorder(
        &self,
        ctx: &Context<'_>,
        task_ids: Vec<String>,
    ) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "task_ids": task_ids });
        api.queue_reorder(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn queue_hold(
        &self,
        ctx: &Context<'_>,
        task_id: String,
        reason: Option<String>,
    ) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "reason": reason });
        api.queue_hold(&task_id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    async fn queue_release(&self, ctx: &Context<'_>, task_id: String) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({});
        api.queue_release(&task_id, body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Vision mutations
    // -----------------------------------------------------------------------

    async fn save_vision(
        &self,
        ctx: &Context<'_>,
        content: String,
    ) -> Result<GqlVision> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "content": content });
        let val = api
            .vision_save(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(GqlVision(val))
    }

    async fn refine_vision(
        &self,
        ctx: &Context<'_>,
        feedback: Option<String>,
    ) -> Result<GqlVision> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({ "feedback": feedback });
        let val = api
            .vision_refine(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(GqlVision(val))
    }

    // -----------------------------------------------------------------------
    // Review mutations
    // -----------------------------------------------------------------------

    async fn review_handoff(
        &self,
        ctx: &Context<'_>,
        target_role: String,
        question: String,
        context: Option<String>,
    ) -> Result<bool> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "target_role": target_role,
            "question": question,
            "context": context,
        });
        api.reviews_handoff(body)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        Ok(true)
    }
}
