use async_graphql::{Object, SimpleObject, ID};
use serde::Deserialize;

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlPhaseExecution {
    pub phase_id: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub attempt: i32,
    pub error_message: Option<String>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDecision {
    pub timestamp: String,
    pub phase_id: String,
    pub source: String,
    pub decision: String,
    pub target_phase: Option<String>,
    pub reason: String,
    pub confidence: f64,
    pub risk: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDaemonHealth {
    pub healthy: bool,
    pub status: String,
    pub runner_connected: bool,
    pub runner_pid: Option<i32>,
    pub active_agents: i32,
    pub daemon_pid: Option<i32>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlAgentRun {
    pub run_id: String,
    pub task_id: Option<String>,
    pub task_title: Option<String>,
    pub workflow_id: Option<String>,
    pub phase_id: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawTask {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "type", default)]
    pub task_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub linked_requirements: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRequirement {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub linked_task_ids: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWorkflow {
    pub id: String,
    pub task_id: String,
    #[serde(default)]
    pub pipeline_id: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub current_phase: Option<String>,
    #[serde(default)]
    pub phases: Vec<RawPhaseExecution>,
    #[serde(default)]
    pub decision_history: Vec<RawDecision>,
    #[serde(default)]
    pub total_reworks: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPhaseExecution {
    pub phase_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDecision {
    #[serde(default)]
    pub timestamp: String,
    pub phase_id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub target_phase: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub risk: String,
}

pub struct GqlTask(pub RawTask);
pub struct GqlRequirement(pub RawRequirement);
pub struct GqlWorkflow(pub RawWorkflow);

#[Object]
impl GqlTask {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn title(&self) -> &str {
        &self.0.title
    }
    async fn description(&self) -> &str {
        &self.0.description
    }
    async fn task_type(&self) -> &str {
        &self.0.task_type
    }
    async fn status(&self) -> &str {
        &self.0.status
    }
    async fn priority(&self) -> &str {
        &self.0.priority
    }
    async fn tags(&self) -> &[String] {
        &self.0.tags
    }
    async fn linked_requirement_ids(&self) -> &[String] {
        &self.0.linked_requirements
    }
    async fn requirements(
        &self,
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlRequirement>> {
        let api = ctx.data::<orchestrator_web_api::WebApiService>()?;
        let mut result = Vec::new();
        for req_id in &self.0.linked_requirements {
            if let Ok(val) = api.requirements_get(req_id).await {
                if let Ok(raw) = serde_json::from_value::<RawRequirement>(val) {
                    result.push(GqlRequirement(raw));
                }
            }
        }
        Ok(result)
    }
}

#[Object]
impl GqlRequirement {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn title(&self) -> &str {
        &self.0.title
    }
    async fn description(&self) -> &str {
        &self.0.description
    }
    async fn priority(&self) -> &str {
        &self.0.priority
    }
    async fn status(&self) -> &str {
        &self.0.status
    }
    async fn tags(&self) -> &[String] {
        &self.0.tags
    }
    async fn linked_task_ids(&self) -> &[String] {
        &self.0.linked_task_ids
    }
}

#[Object]
impl GqlWorkflow {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn task_id(&self) -> &str {
        &self.0.task_id
    }
    async fn pipeline_id(&self) -> Option<&str> {
        self.0.pipeline_id.as_deref()
    }
    async fn status(&self) -> &str {
        &self.0.status
    }
    async fn current_phase(&self) -> Option<&str> {
        self.0.current_phase.as_deref()
    }
    async fn total_reworks(&self) -> i32 {
        self.0.total_reworks as i32
    }
    async fn phases(&self) -> Vec<GqlPhaseExecution> {
        self.0
            .phases
            .iter()
            .map(|p| GqlPhaseExecution {
                phase_id: p.phase_id.clone(),
                status: p.status.clone(),
                started_at: p.started_at.clone(),
                completed_at: p.completed_at.clone(),
                attempt: p.attempt as i32,
                error_message: p.error_message.clone(),
            })
            .collect()
    }
    async fn decisions(
        &self,
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlDecision>> {
        let api = ctx.data::<orchestrator_web_api::WebApiService>()?;
        match api.workflows_decisions(&self.0.id).await {
            Ok(val) => {
                let decisions: Vec<RawDecision> =
                    serde_json::from_value(val).unwrap_or_default();
                Ok(decisions
                    .into_iter()
                    .map(|d| GqlDecision {
                        timestamp: d.timestamp,
                        phase_id: d.phase_id,
                        source: d.source,
                        decision: d.decision,
                        target_phase: d.target_phase,
                        reason: d.reason,
                        confidence: d.confidence as f64,
                        risk: d.risk,
                    })
                    .collect())
            }
            Err(_) => Ok(vec![]),
        }
    }
}
