use std::sync::Arc;

use async_graphql::{Context, EmptySubscription, Schema, SimpleObject, InputObject, ID};
use serde::{Deserialize, Serialize};
use orchestrator_core::ServiceHub;

pub struct QueryRoot;

pub struct MutationRoot;

pub type AppSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

#[derive(SimpleObject, Clone, Serialize, Deserialize)]
pub struct GqlTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub task_type: String,
    pub risk: String,
    pub complexity: String,
    pub scope: String,
}

#[derive(SimpleObject, Clone, Serialize, Deserialize)]
pub struct GqlRequirement {
    pub id: String,
    pub title: String,
    pub description: String,
    pub priority: String,
    pub status: String,
    pub requirement_type: String,
}

#[derive(SimpleObject, Clone, Serialize, Deserialize)]
pub struct GqlWorkflow {
    pub id: String,
    pub status: String,
    pub current_phase: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(SimpleObject, Clone, Serialize, Deserialize)]
pub struct GqlDaemonStatus {
    pub healthy: bool,
    pub status: String,
    pub runner_connected: bool,
    pub active_agents: usize,
    pub max_agents: Option<usize>,
    pub project_root: Option<String>,
}

#[derive(InputObject)]
pub struct TaskFilterInput {
    pub status: Option<String>,
    pub priority: Option<String>,
    pub task_type: Option<String>,
}

#[derive(InputObject)]
pub struct CreateTaskInput {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub task_type: Option<String>,
}

impl QueryRoot {
    async fn tasks(&self, ctx: &Context<'_>, filter: Option<TaskFilterInput>) -> async_graphql::Result<Vec<GqlTask>> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        let tasks = hub.tasks().list().await.map_err(|e| async_graphql::Error::new(e.to_string()))?;
        
        let mut result: Vec<GqlTask> = tasks.into_iter().map(|t| GqlTask {
            id: t.id,
            title: t.title,
            description: t.description.unwrap_or_default(),
            status: format!("{:?}", t.status),
            priority: format!("{:?}", t.priority),
            task_type: format!("{:?}", t.task_type),
            risk: format!("{:?}", t.risk),
            complexity: format!("{:?}", t.complexity),
            scope: format!("{:?}", t.scope),
        }).collect();
        
        if let Some(f) = filter {
            if let Some(status) = f.status {
                result.retain(|t| t.status.to_lowercase() == status.to_lowercase());
            }
            if let Some(priority) = f.priority {
                result.retain(|t| t.priority.to_lowercase() == priority.to_lowercase());
            }
            if let Some(task_type) = f.task_type {
                result.retain(|t| t.task_type.to_lowercase() == task_type.to_lowercase());
            }
        }
        
        Ok(result)
    }

    async fn task(&self, ctx: &Context<'_>, id: ID) -> async_graphql::Result<Option<GqlTask>> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        match hub.tasks().get(&id.to_string()).await {
            Ok(Some(t)) => Ok(Some(GqlTask {
                id: t.id,
                title: t.title,
                description: t.description.unwrap_or_default(),
                status: format!("{:?}", t.status),
                priority: format!("{:?}", t.priority),
                task_type: format!("{:?}", t.task_type),
                risk: format!("{:?}", t.risk),
                complexity: format!("{:?}", t.complexity),
                scope: format!("{:?}", t.scope),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(async_graphql::Error::new(e.to_string())),
        }
    }

    async fn requirements(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<GqlRequirement>> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        let requirements = hub.requirements().list().await.map_err(|e| async_graphql::Error::new(e.to_string()))?;
        
        Ok(requirements.into_iter().map(|r| GqlRequirement {
            id: r.id,
            title: r.title,
            description: r.description,
            priority: format!("{:?}", r.priority),
            status: format!("{:?}", r.status),
            requirement_type: format!("{:?}", r.requirement_type.unwrap_or(orchestrator_core::types::RequirementType::Other)),
        }).collect())
    }

    async fn requirement(&self, ctx: &Context<'_>, id: ID) -> async_graphql::Result<Option<GqlRequirement>> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        match hub.requirements().get(&id.to_string()).await {
            Ok(Some(r)) => Ok(Some(GqlRequirement {
                id: r.id,
                title: r.title,
                description: r.description,
                priority: format!("{:?}", r.priority),
                status: format!("{:?}", r.status),
                requirement_type: format!("{:?}", r.requirement_type.unwrap_or(orchestrator_core::types::RequirementType::Other)),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(async_graphql::Error::new(e.to_string())),
        }
    }

    async fn workflows(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<GqlWorkflow>> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        let workflows = hub.workflows().list().await.map_err(|e| async_graphql::Error::new(e.to_string()))?;
        
        Ok(workflows.into_iter().map(|w| GqlWorkflow {
            id: w.id,
            status: format!("{:?}", w.status),
            current_phase: w.current_phase,
            started_at: w.started_at.map(|d| d.to_rfc3339()),
            completed_at: w.completed_at.map(|d| d.to_rfc3339()),
        }).collect())
    }

    async fn workflow(&self, ctx: &Context<'_>, id: ID) -> async_graphql::Result<Option<GqlWorkflow>> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        match hub.workflows().get(&id.to_string()).await {
            Ok(Some(w)) => Ok(Some(GqlWorkflow {
                id: w.id,
                status: format!("{:?}", w.status),
                current_phase: w.current_phase,
                started_at: w.started_at.map(|d| d.to_rfc3339()),
                completed_at: w.completed_at.map(|d| d.to_rfc3339()),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(async_graphql::Error::new(e.to_string())),
        }
    }

    async fn daemon_status(&self, ctx: &Context<'_>) -> async_graphql::Result<GqlDaemonStatus> {
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        let daemon = hub.daemon();
        let health = daemon.health().await.map_err(|e| async_graphql::Error::new(e.to_string()))?;
        
        Ok(GqlDaemonStatus {
            healthy: health.healthy,
            status: format!("{:?}", health.status),
            runner_connected: health.runner_connected,
            active_agents: health.active_agents,
            max_agents: health.max_agents,
            project_root: health.project_root,
        })
    }
}

impl MutationRoot {
    async fn create_task(&self, ctx: &Context<'_>, input: CreateTaskInput) -> async_graphql::Result<GqlTask> {
        use orchestrator_core::TaskCreateInput;
        
        let hub = ctx.data::<Arc<dyn ServiceHub>>()?;
        
        let task_input = TaskCreateInput {
            title: input.title,
            description: input.description,
            task_type: None,
            priority: None,
            created_by: Some("graphql".to_string()),
            tags: Vec::new(),
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
        };
        
        let task = hub.tasks()
            .create(task_input)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        
        Ok(GqlTask {
            id: task.id,
            title: task.title,
            description: task.description.unwrap_or_default(),
            status: format!("{:?}", task.status),
            priority: format!("{:?}", task.priority),
            task_type: format!("{:?}", task.task_type),
            risk: format!("{:?}", task.risk),
            complexity: format!("{:?}", task.complexity),
            scope: format!("{:?}", task.scope),
        })
    }
}

pub fn create_schema(hub: Arc<dyn ServiceHub>) -> AppSchema {
    Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .data(hub)
        .finish()
}
