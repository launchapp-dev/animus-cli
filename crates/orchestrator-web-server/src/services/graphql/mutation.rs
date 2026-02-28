use async_graphql::{Context, Object, Result, ID};
use orchestrator_web_api::WebApiService;
use serde_json::json;

use super::types::{GqlTask, GqlWorkflow, RawTask, RawWorkflow};

pub struct MutationRoot;

#[Object]
impl MutationRoot {
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

    async fn run_workflow(
        &self,
        ctx: &Context<'_>,
        task_id: String,
        pipeline_id: Option<String>,
    ) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let body = json!({
            "task_id": task_id,
            "pipeline_id": pipeline_id,
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

    async fn resume_workflow(&self, ctx: &Context<'_>, id: ID) -> Result<GqlWorkflow> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_resume(&id)
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
}
