use std::collections::HashMap;

use orchestrator_core::types::WorkflowStatus;
use serde_json::{json, Value};

use super::{WebApiError, WebApiService};

impl WebApiService {
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
        self.context.hub.daemon().start(Default::default()).await?;
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
        self.publish_event("daemon-clear-logs", json!({ "message": "daemon logs cleared" }));
        Ok(json!({ "message": "daemon logs cleared" }))
    }

    pub async fn daemon_agents(&self) -> Result<Value, WebApiError> {
        let active_agents = self.context.hub.daemon().active_agents().await?;
        let workflows = self.context.hub.workflows().list().await.unwrap_or_default();
        let tasks = self.context.hub.tasks().list().await.unwrap_or_default();

        let task_titles: HashMap<&str, &str> = tasks.iter().map(|t| (t.id.as_str(), t.title.as_str())).collect();

        let mut running: Vec<_> = workflows.iter().filter(|w| w.status == WorkflowStatus::Running).collect();
        running.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.task_id.cmp(&b.task_id)));

        let attributed = active_agents.min(running.len());
        let agents: Vec<Value> = running
            .into_iter()
            .take(attributed)
            .map(|w| {
                json!({
                    "workflow_id": w.id,
                    "task_id": w.task_id,
                    "task_title": task_titles.get(w.task_id.as_str()).copied().unwrap_or("Unknown task"),
                    "phase": w.current_phase,
                    "phase_index": w.current_phase_index,
                    "status": "running",
                    "started_at": w.started_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(json!({
            "active_agents": active_agents,
            "agents": agents,
        }))
    }
}
