use async_graphql::{Context, Object, Result, ID};
use orchestrator_web_api::WebApiService;

use super::gql_err;
use super::types::{
    GqlAgentRun, GqlDaemonHealth, GqlDaemonLog, GqlDaemonStatus, GqlPhaseOutput, GqlProject,
    GqlQueueEntry, GqlQueueStats, GqlRequirement, GqlRequirementConnection, GqlSystemInfo,
    GqlTask, GqlTaskConnection, GqlTaskStats, GqlVision, GqlWorkflow, GqlWorkflowCheckpoint,
    GqlWorkflowConnection, GqlWorkflowDefinition, RawRequirement, RawTask, RawWorkflow,
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
            .map_err(gql_err)?;
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
            Err(e) => Err(gql_err(e)),
        }
    }

    async fn tasks_prioritized(&self, ctx: &Context<'_>) -> Result<Vec<GqlTask>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .tasks_prioritized()
            .await
            .map_err(gql_err)?;
        let tasks: Vec<RawTask> = serde_json::from_value(val).unwrap_or_default();
        Ok(tasks.into_iter().map(GqlTask).collect())
    }

    async fn tasks_next(&self, ctx: &Context<'_>) -> Result<Option<GqlTask>> {
        let api = ctx.data::<WebApiService>()?;
        match api.tasks_next().await {
            Ok(val) => {
                let raw: RawTask = serde_json::from_value(val).map_err(|e| {
                    async_graphql::Error::new(format!("failed to parse task: {e}"))
                })?;
                Ok(Some(GqlTask(raw)))
            }
            Err(e) if e.code == "not_found" => Ok(None),
            Err(e) => Err(gql_err(e)),
        }
    }

    async fn task_stats(&self, ctx: &Context<'_>) -> Result<GqlTaskStats> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .tasks_stats()
            .await
            .map_err(gql_err)?;
        Ok(GqlTaskStats(val))
    }

    async fn requirements(&self, ctx: &Context<'_>) -> Result<Vec<GqlRequirement>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .requirements_list()
            .await
            .map_err(gql_err)?;
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
            Err(e) => Err(gql_err(e)),
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
            .map_err(gql_err)?;
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
            Err(e) => Err(gql_err(e)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn tasks_paginated(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        task_type: Option<String>,
        priority: Option<String>,
        search: Option<String>,
        #[graphql(default = 50)] limit: i32,
        #[graphql(default = 0)] offset: i32,
    ) -> Result<GqlTaskConnection> {
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
            .map_err(gql_err)?;
        let tasks: Vec<RawTask> = serde_json::from_value(val).unwrap_or_default();
        let total_count = tasks.len() as i32;
        let offset = (offset.max(0) as usize).min(tasks.len());
        let limit = limit.max(1) as usize;
        let items: Vec<GqlTask> = tasks
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(GqlTask)
            .collect();
        Ok(GqlTaskConnection { items, total_count })
    }

    async fn requirements_paginated(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: i32,
        #[graphql(default = 0)] offset: i32,
    ) -> Result<GqlRequirementConnection> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .requirements_list()
            .await
            .map_err(gql_err)?;
        let reqs: Vec<RawRequirement> = serde_json::from_value(val).unwrap_or_default();
        let total_count = reqs.len() as i32;
        let offset = (offset.max(0) as usize).min(reqs.len());
        let limit = limit.max(1) as usize;
        let items: Vec<GqlRequirement> = reqs
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(GqlRequirement)
            .collect();
        Ok(GqlRequirementConnection { items, total_count })
    }

    async fn workflows_paginated(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        #[graphql(default = 50)] limit: i32,
        #[graphql(default = 0)] offset: i32,
    ) -> Result<GqlWorkflowConnection> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_list()
            .await
            .map_err(gql_err)?;
        let mut workflows: Vec<RawWorkflow> = serde_json::from_value(val).unwrap_or_default();
        if let Some(status_filter) = status {
            workflows.retain(|w| w.status == status_filter);
        }
        let total_count = workflows.len() as i32;
        let offset = (offset.max(0) as usize).min(workflows.len());
        let limit = limit.max(1) as usize;
        let items: Vec<GqlWorkflow> = workflows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(GqlWorkflow)
            .collect();
        Ok(GqlWorkflowConnection { items, total_count })
    }

    async fn workflow_checkpoints(
        &self,
        ctx: &Context<'_>,
        workflow_id: ID,
    ) -> Result<Vec<GqlWorkflowCheckpoint>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_checkpoints(&workflow_id)
            .await
            .map_err(gql_err)?;
        let checkpoints: Vec<serde_json::Value> =
            serde_json::from_value(val).unwrap_or_default();
        Ok(checkpoints
            .into_iter()
            .map(|c| GqlWorkflowCheckpoint {
                id: c
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                phase: c
                    .get("phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                timestamp: c.get("timestamp").and_then(|v| v.as_str()).map(String::from),
                data: c.get("data").map(|v| v.to_string()),
            })
            .collect())
    }

    async fn phase_output(
        &self,
        ctx: &Context<'_>,
        workflow_id: ID,
        phase_id: Option<String>,
        tail: Option<i32>,
    ) -> Result<GqlPhaseOutput> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .workflows_phase_output(&workflow_id, phase_id.as_deref(), tail)
            .await
            .map_err(gql_err)?;
        let lines: Vec<String> = val
            .get("lines")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let resolved_phase_id = val
            .get("phase_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let has_more = val
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(GqlPhaseOutput {
            lines,
            phase_id: resolved_phase_id,
            has_more,
        })
    }

    async fn ready_tasks(
        &self,
        ctx: &Context<'_>,
        search: Option<String>,
        limit: Option<i32>,
    ) -> Result<Vec<GqlTask>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .tasks_list(None, None, None, None, None, vec![], None, None, search)
            .await
            .map_err(gql_err)?;
        let all_tasks: Vec<RawTask> = serde_json::from_value(val).unwrap_or_default();
        let priority_order = |p: &str| -> u8 {
            match p.to_lowercase().as_str() {
                "critical" => 0,
                "high" => 1,
                "medium" => 2,
                "low" => 3,
                _ => 4,
            }
        };
        let mut ready: Vec<RawTask> = all_tasks
            .into_iter()
            .filter(|t| {
                let s = t.status.to_lowercase();
                s == "ready" || s == "backlog" || s == "todo"
            })
            .collect();
        ready.sort_by(|a, b| priority_order(&a.priority).cmp(&priority_order(&b.priority)));
        let max = limit.unwrap_or(25).max(1) as usize;
        ready.truncate(max);
        Ok(ready.into_iter().map(GqlTask).collect())
    }

    async fn workflow_definitions(&self, ctx: &Context<'_>) -> Result<Vec<GqlWorkflowDefinition>> {
        let api = ctx.data::<WebApiService>()?;
        match api.workflow_definitions().await {
            Ok(val) => {
                let defs: Vec<serde_json::Value> =
                    serde_json::from_value(val).unwrap_or_default();
                Ok(defs
                    .into_iter()
                    .map(|d| GqlWorkflowDefinition {
                        id: d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        name: d.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        description: d.get("description").and_then(|v| v.as_str()).map(String::from),
                        phases: d
                            .get("phases")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect())
            }
            Err(e) => Err(gql_err(e)),
        }
    }

    async fn daemon_health(&self, ctx: &Context<'_>) -> Result<GqlDaemonHealth> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .daemon_health()
            .await
            .map_err(gql_err)?;
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

    async fn daemon_status(&self, ctx: &Context<'_>) -> Result<GqlDaemonStatus> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .daemon_status()
            .await
            .map_err(gql_err)?;
        Ok(GqlDaemonStatus(val))
    }

    async fn daemon_logs(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
    ) -> Result<Vec<GqlDaemonLog>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .daemon_logs(limit.map(|l| l as usize))
            .await
            .map_err(gql_err)?;
        let logs: Vec<serde_json::Value> = serde_json::from_value(val).unwrap_or_default();
        Ok(logs
            .into_iter()
            .map(|l| GqlDaemonLog {
                timestamp: l.get("timestamp").and_then(|v| v.as_str()).map(String::from),
                level: l.get("level").and_then(|v| v.as_str()).map(String::from),
                message: l.get("message").and_then(|v| v.as_str()).map(String::from),
                fields: l.get("fields").map(|v| v.to_string()),
            })
            .collect())
    }

    async fn agent_runs(&self, ctx: &Context<'_>) -> Result<Vec<GqlAgentRun>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .daemon_agents()
            .await
            .map_err(gql_err)?;
        let agents = val.as_array().cloned().unwrap_or_default();
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

    async fn projects(&self, ctx: &Context<'_>) -> Result<Vec<GqlProject>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .projects_list()
            .await
            .map_err(gql_err)?;
        let projects: Vec<serde_json::Value> = serde_json::from_value(val).unwrap_or_default();
        Ok(projects.into_iter().map(GqlProject).collect())
    }

    async fn project(&self, ctx: &Context<'_>, id: ID) -> Result<Option<GqlProject>> {
        let api = ctx.data::<WebApiService>()?;
        match api.projects_get(&id).await {
            Ok(val) => Ok(Some(GqlProject(val))),
            Err(e) if e.code == "not_found" => Ok(None),
            Err(e) => Err(gql_err(e)),
        }
    }

    async fn projects_active(&self, ctx: &Context<'_>) -> Result<Vec<GqlProject>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .projects_active()
            .await
            .map_err(gql_err)?;
        let projects: Vec<serde_json::Value> = serde_json::from_value(val).unwrap_or_default();
        Ok(projects.into_iter().map(GqlProject).collect())
    }

    async fn vision(&self, ctx: &Context<'_>) -> Result<Option<GqlVision>> {
        let api = ctx.data::<WebApiService>()?;
        match api.vision_get().await {
            Ok(val) => Ok(Some(GqlVision(val))),
            Err(e) if e.code == "not_found" => Ok(None),
            Err(e) => Err(gql_err(e)),
        }
    }

    async fn queue(&self, ctx: &Context<'_>) -> Result<Vec<GqlQueueEntry>> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .queue_list()
            .await
            .map_err(gql_err)?;
        let entries: Vec<serde_json::Value> = serde_json::from_value(val).unwrap_or_default();
        Ok(entries.into_iter().map(GqlQueueEntry).collect())
    }

    async fn queue_stats(&self, ctx: &Context<'_>) -> Result<GqlQueueStats> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .queue_stats()
            .await
            .map_err(gql_err)?;
        Ok(GqlQueueStats(val))
    }

    async fn system_info(&self, ctx: &Context<'_>) -> Result<GqlSystemInfo> {
        let api = ctx.data::<WebApiService>()?;
        let val = api
            .system_info()
            .await
            .map_err(gql_err)?;
        Ok(GqlSystemInfo {
            platform: val.get("platform").and_then(|v| v.as_str()).map(String::from),
            arch: val.get("arch").and_then(|v| v.as_str()).map(String::from),
            version: val.get("version").and_then(|v| v.as_str()).map(String::from),
            daemon_status: val
                .get("daemon_status")
                .and_then(|v| v.as_str())
                .map(String::from),
            project_root: val
                .get("project_root")
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    }
}
