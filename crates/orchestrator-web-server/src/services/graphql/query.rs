use async_graphql::{Context, Object, Result, ID};
use orchestrator_web_api::WebApiService;

use super::types::{
    GqlAgentRun, GqlDaemonHealth, GqlRequirement, GqlTask, GqlWorkflow, RawRequirement, RawTask,
    RawWorkflow,
};

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn tasks(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        task_type: Option<String>,
        priority: Option<String>,
        search: Option<String>,
    ) -> Result<Vec<GqlTask>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .tasks_list(
                task_type,
                status,
                priority,
                None,
                None,
                vec![],
                None,
                None,
                search,
            )
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let tasks: Vec<RawTask> = serde_json::from_value(val).unwrap_or_default();
        Ok(tasks.into_iter().map(GqlTask).collect())
    }

    async fn task(&self, ctx: &Context<'_>, id: ID) -> Result<Option<GqlTask>> {
        let api = ctx.data::<WebApiService>()?;
        match api.tasks_get(&id).await {
            Ok(val) => {
                let raw: RawTask = serde_json::from_value(val).map_err(|e| {
                    async_graphql::Error::new(format!("failed to parse task: {e}"))
                })?;
                Ok(Some(GqlTask(raw)))
            }
            Err(e) if e.code == "not_found" => Ok(None),
            Err(e) => Err(async_graphql::Error::new(e.message.clone())),
        }
    }

    async fn requirements(&self, ctx: &Context<'_>) -> Result<Vec<GqlRequirement>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .requirements_list()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let reqs: Vec<RawRequirement> = serde_json::from_value(val).unwrap_or_default();
        Ok(reqs.into_iter().map(GqlRequirement).collect())
    }

    async fn requirement(&self, ctx: &Context<'_>, id: ID) -> Result<Option<GqlRequirement>> {
        let api = ctx.data::<WebApiService>()?;
        match api.requirements_get(&id).await {
            Ok(val) => {
                let raw: RawRequirement = serde_json::from_value(val).map_err(|e| {
                    async_graphql::Error::new(format!("failed to parse requirement: {e}"))
                })?;
                Ok(Some(GqlRequirement(raw)))
            }
            Err(e) if e.code == "not_found" => Ok(None),
            Err(e) => Err(async_graphql::Error::new(e.message.clone())),
        }
    }

    async fn workflows(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
    ) -> Result<Vec<GqlWorkflow>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_list()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let mut workflows: Vec<RawWorkflow> = serde_json::from_value(val).unwrap_or_default();
        if let Some(status_filter) = status {
            workflows.retain(|w| w.status == status_filter);
        }
        Ok(workflows.into_iter().map(GqlWorkflow).collect())
    }

    async fn workflow(&self, ctx: &Context<'_>, id: ID) -> Result<Option<GqlWorkflow>> {
        let api = ctx.data::<WebApiService>()?;
        match api.workflows_get(&id).await {
            Ok(val) => {
                let raw: RawWorkflow = serde_json::from_value(val).map_err(|e| {
                    async_graphql::Error::new(format!("failed to parse workflow: {e}"))
                })?;
                Ok(Some(GqlWorkflow(raw)))
            }
            Err(e) if e.code == "not_found" => Ok(None),
            Err(e) => Err(async_graphql::Error::new(e.message.clone())),
        }
    }

    async fn daemon_health(&self, ctx: &Context<'_>) -> Result<GqlDaemonHealth> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .daemon_health()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let health = serde_json::from_value::<serde_json::Value>(val).unwrap_or_default();
        Ok(GqlDaemonHealth {
            healthy: health.get("healthy").and_then(|v| v.as_bool()).unwrap_or(false),
            status: health
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            runner_connected: health
                .get("runner_connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            runner_pid: health
                .get("runner_pid")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
            active_agents: health
                .get("active_agents")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            daemon_pid: health
                .get("daemon_pid")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        })
    }

    async fn agent_runs(&self, ctx: &Context<'_>) -> Result<Vec<GqlAgentRun>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .daemon_agents()
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))?;
        let agents = val
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(agents
            .iter()
            .filter_map(|a| {
                let run_id = a.get("run_id").and_then(|v| v.as_str())?.to_string();
                Some(GqlAgentRun {
                    run_id,
                    task_id: a
                        .get("task_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    task_title: a
                        .get("task_title")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    workflow_id: a
                        .get("workflow_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    phase_id: a
                        .get("phase_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    status: a
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                })
            })
            .collect())
    }
}
