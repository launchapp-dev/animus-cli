mod event_persistence;
mod lifecycle;
mod process;
mod process_builder;
mod stream;
mod stream_bridge;
mod supervisor;

use event_persistence::RunEventPersistence;
use protocol::{
    AgentRunEvent, AgentRunRequest, AgentStatus, AgentStatusRequest, AgentStatusResponse,
    ModelStatusRequest, ModelStatusResponse, RunId, RunnerStatusResponse, Timestamp,
};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

pub use supervisor::Supervisor;

struct RunningAgent {
    cancel_tx: oneshot::Sender<()>,
    started_at: Timestamp,
}

struct FinishedAgent {
    started_at: Timestamp,
    completed_at: Timestamp,
    status: AgentStatus,
}

pub struct Runner {
    running_agents: HashMap<RunId, RunningAgent>,
    finished_agents: HashMap<RunId, FinishedAgent>,
    cleanup_tx: mpsc::Sender<RunId>,
}

impl Runner {
    pub fn new(cleanup_tx: mpsc::Sender<RunId>) -> Self {
        Self {
            running_agents: HashMap::new(),
            finished_agents: HashMap::new(),
            cleanup_tx,
        }
    }

    pub fn handle_run_request(
        &mut self,
        req: AgentRunRequest,
        event_tx: mpsc::Sender<AgentRunEvent>,
    ) {
        let run_id = req.run_id.clone();
        let persistence = RunEventPersistence::new(&req.context, &run_id);
        let (run_event_tx, mut run_event_rx) = mpsc::channel::<AgentRunEvent>(100);
        let downstream_event_tx = event_tx.clone();
        let run_id_for_forwarder = run_id.clone();

        tokio::spawn(async move {
            let mut persistence = persistence;
            while let Some(event) = run_event_rx.recv().await {
                if let Err(err) = persistence.persist(&event) {
                    warn!(
                        run_id = %run_id_for_forwarder.0.as_str(),
                        error = %err,
                        "Failed to persist run event"
                    );
                }
                if downstream_event_tx.send(event).await.is_err() {
                    debug!(
                        run_id = %run_id_for_forwarder.0.as_str(),
                        "Run event receiver dropped; continuing persistence without downstream forwarding"
                    );
                }
            }
        });

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let started_at = Timestamp::now();
        let replaced = self
            .running_agents
            .insert(
                run_id.clone(),
                RunningAgent {
                    cancel_tx,
                    started_at: started_at.clone(),
                },
            )
            .is_some();
        self.finished_agents.remove(&run_id);
        if replaced {
            warn!(
                run_id = %run_id.0.as_str(),
                "Run ID replaced an existing active agent"
            );
        }
        info!(
            run_id = %run_id.0.as_str(),
            active_agents = self.running_agents.len(),
            "Registered agent run request"
        );

        let supervisor = Supervisor::new();
        let cleanup_tx = self.cleanup_tx.clone();
        tokio::spawn(async move {
            supervisor.spawn_agent(req, run_event_tx, cancel_rx).await;
            if cleanup_tx.send(run_id.clone()).await.is_err() {
                warn!(run_id = %run_id.0.as_str(), "Failed to enqueue cleanup for run");
            }
        });
    }

    pub fn cleanup_agent(&mut self, run_id: &RunId) {
        if let Some(entry) = self.running_agents.remove(run_id) {
            self.finished_agents.insert(
                run_id.clone(),
                FinishedAgent {
                    started_at: entry.started_at,
                    completed_at: Timestamp::now(),
                    status: AgentStatus::Completed,
                },
            );
            info!(
                run_id = %run_id.0.as_str(),
                active_agents = self.running_agents.len(),
                "Cleaned up finished agent run"
            );
        } else {
            debug!(
                run_id = %run_id.0.as_str(),
                active_agents = self.running_agents.len(),
                "Cleanup requested for unknown run ID"
            );
        }
    }

    pub async fn handle_model_status(&self, req: ModelStatusRequest) -> ModelStatusResponse {
        debug!(
            requested_models = req.models.len(),
            "Handling model status request"
        );
        crate::providers::check_model_status(req).await
    }

    pub fn handle_runner_status(&self) -> RunnerStatusResponse {
        RunnerStatusResponse {
            active_agents: self.running_agents.len(),
        }
    }

    pub fn handle_agent_status(&self, req: AgentStatusRequest) -> AgentStatusResponse {
        if let Some(entry) = self.running_agents.get(&req.run_id) {
            let now = Timestamp::now();
            let elapsed_ms = now
                .0
                .signed_duration_since(entry.started_at.0)
                .num_milliseconds()
                .max(0) as u64;
            return AgentStatusResponse {
                run_id: req.run_id,
                status: AgentStatus::Running,
                elapsed_ms,
                started_at: entry.started_at.clone(),
                completed_at: None,
            };
        }

        if let Some(entry) = self.finished_agents.get(&req.run_id) {
            let elapsed_ms = entry
                .completed_at
                .0
                .signed_duration_since(entry.started_at.0)
                .num_milliseconds()
                .max(0) as u64;
            return AgentStatusResponse {
                run_id: req.run_id,
                status: entry.status,
                elapsed_ms,
                started_at: entry.started_at.clone(),
                completed_at: Some(entry.completed_at.clone()),
            };
        }

        let now = Timestamp::now();
        AgentStatusResponse {
            run_id: req.run_id,
            status: AgentStatus::Failed,
            elapsed_ms: 0,
            started_at: now.clone(),
            completed_at: Some(now),
        }
    }

    pub fn stop_agent(&mut self, run_id: &RunId) -> bool {
        if let Some(entry) = self.running_agents.remove(run_id) {
            let _ = entry.cancel_tx.send(());
            self.finished_agents.insert(
                run_id.clone(),
                FinishedAgent {
                    started_at: entry.started_at,
                    completed_at: Timestamp::now(),
                    status: AgentStatus::Terminated,
                },
            );
            info!(
                run_id = %run_id.0.as_str(),
                active_agents = self.running_agents.len(),
                "Sent cancellation signal to running agent"
            );
            true
        } else {
            warn!(
                run_id = %run_id.0.as_str(),
                active_agents = self.running_agents.len(),
                "Stop requested for non-running agent"
            );
            false
        }
    }
}
