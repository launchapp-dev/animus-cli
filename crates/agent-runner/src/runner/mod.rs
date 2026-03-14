mod event_persistence;
mod lifecycle;
mod mcp_policy;
mod process;
mod process_builder;
mod process_signals;
mod session_process;
mod stream;
mod stream_bridge;
mod supervisor;

use event_persistence::RunEventPersistence;
use protocol::{
    AgentRunEvent, AgentRunRequest, AgentStatus, AgentStatusErrorCode, AgentStatusErrorResponse,
    AgentStatusQueryResponse, AgentStatusRequest, AgentStatusResponse, ModelStatusRequest,
    ModelStatusResponse, RunId, RunnerStatusResponse, Timestamp, PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::telemetry::RunnerMetrics;

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

#[derive(Debug, Clone)]
pub(crate) struct CleanupMessage {
    pub run_id: RunId,
    pub terminal_status: AgentStatus,
}

pub struct Runner {
    running_agents: HashMap<RunId, RunningAgent>,
    finished_agents: HashMap<RunId, FinishedAgent>,
    cleanup_tx: mpsc::Sender<CleanupMessage>,
    pub metrics: Arc<RunnerMetrics>,
}

impl Runner {
    pub fn new(cleanup_tx: mpsc::Sender<CleanupMessage>) -> Self {
        Self {
            running_agents: HashMap::new(),
            finished_agents: HashMap::new(),
            cleanup_tx,
            metrics: Arc::new(RunnerMetrics::new()),
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

        self.metrics.record_start();

        let supervisor = Supervisor::new();
        let cleanup_tx = self.cleanup_tx.clone();
        tokio::spawn(async move {
            let terminal_status = supervisor.spawn_agent(req, run_event_tx, cancel_rx).await;
            if cleanup_tx
                .send(CleanupMessage {
                    run_id: run_id.clone(),
                    terminal_status,
                })
                .await
                .is_err()
            {
                warn!(run_id = %run_id.0.as_str(), "Failed to enqueue cleanup for run");
            }
        });
    }

    pub fn cleanup_agent(&mut self, message: CleanupMessage) {
        let CleanupMessage {
            run_id,
            terminal_status,
        } = message;
        if let Some(entry) = self.running_agents.remove(&run_id) {
            let terminal_status = normalize_terminal_status_for_cleanup(terminal_status, &run_id);
            let completed_at = Timestamp::now();
            let duration_ms = completed_at
                .0
                .signed_duration_since(entry.started_at.0)
                .num_milliseconds()
                .max(0) as u64;
            match terminal_status {
                AgentStatus::Completed => self.metrics.record_completion(duration_ms),
                AgentStatus::Terminated => self.metrics.record_cancellation(),
                AgentStatus::Timeout => {
                    self.metrics.record_timeout();
                    self.metrics.record_failure(duration_ms);
                }
                _ => self.metrics.record_failure(duration_ms),
            }
            self.finished_agents.insert(
                run_id.clone(),
                FinishedAgent {
                    started_at: entry.started_at,
                    completed_at,
                    status: terminal_status,
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
            protocol_version: PROTOCOL_VERSION.to_string(),
            build_id: runner_build_id(),
            metrics: serde_json::to_value(self.metrics.snapshot()).ok(),
        }
    }

    pub fn handle_agent_status(&self, req: AgentStatusRequest) -> AgentStatusQueryResponse {
        if let Some(entry) = self.running_agents.get(&req.run_id) {
            let now = Timestamp::now();
            let elapsed_ms = now
                .0
                .signed_duration_since(entry.started_at.0)
                .num_milliseconds()
                .max(0) as u64;
            return AgentStatusQueryResponse::Status(AgentStatusResponse {
                run_id: req.run_id,
                status: AgentStatus::Running,
                elapsed_ms,
                started_at: entry.started_at.clone(),
                completed_at: None,
            });
        }

        if let Some(entry) = self.finished_agents.get(&req.run_id) {
            let elapsed_ms = entry
                .completed_at
                .0
                .signed_duration_since(entry.started_at.0)
                .num_milliseconds()
                .max(0) as u64;
            return AgentStatusQueryResponse::Status(AgentStatusResponse {
                run_id: req.run_id,
                status: entry.status,
                elapsed_ms,
                started_at: entry.started_at.clone(),
                completed_at: Some(entry.completed_at.clone()),
            });
        }

        let run_id = req.run_id;
        AgentStatusQueryResponse::Error(AgentStatusErrorResponse {
            message: format!("run not found: {}", run_id.0),
            run_id,
            code: AgentStatusErrorCode::NotFound,
        })
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

fn normalize_terminal_status_for_cleanup(status: AgentStatus, run_id: &RunId) -> AgentStatus {
    match status {
        AgentStatus::Completed
        | AgentStatus::Failed
        | AgentStatus::Timeout
        | AgentStatus::Terminated => status,
        AgentStatus::Starting | AgentStatus::Running | AgentStatus::Paused => {
            warn!(
                run_id = %run_id.0.as_str(),
                status = ?status,
                "Cleanup received non-terminal status; coercing to failed"
            );
            AgentStatus::Failed
        }
    }
}

fn runner_build_id() -> Option<String> {
    std::env::var("AO_RUNNER_BUILD_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_runner() -> Runner {
        let (cleanup_tx, _cleanup_rx) = mpsc::channel(1);
        Runner::new(cleanup_tx)
    }

    fn insert_running_agent(runner: &mut Runner, run_id: &RunId) {
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        runner.running_agents.insert(
            run_id.clone(),
            RunningAgent {
                cancel_tx,
                started_at: Timestamp::now(),
            },
        );
    }

    #[test]
    fn cleanup_agent_persists_terminal_status_from_cleanup_message() {
        let mut runner = make_runner();
        let run_id = RunId("run-cleanup-failed".to_string());
        insert_running_agent(&mut runner, &run_id);

        runner.cleanup_agent(CleanupMessage {
            run_id: run_id.clone(),
            terminal_status: AgentStatus::Failed,
        });

        let finished = runner
            .finished_agents
            .get(&run_id)
            .expect("run should be persisted in finished map");
        assert_eq!(finished.status, AgentStatus::Failed);
    }

    #[test]
    fn handle_agent_status_returns_failed_after_failed_cleanup() {
        let mut runner = make_runner();
        let run_id = RunId("run-query-failed".to_string());
        insert_running_agent(&mut runner, &run_id);

        runner.cleanup_agent(CleanupMessage {
            run_id: run_id.clone(),
            terminal_status: AgentStatus::Failed,
        });

        let response = runner.handle_agent_status(AgentStatusRequest {
            run_id: run_id.clone(),
        });
        match response {
            AgentStatusQueryResponse::Status(status) => {
                assert_eq!(status.run_id, run_id);
                assert_eq!(status.status, AgentStatus::Failed);
                assert!(status.completed_at.is_some());
            }
            AgentStatusQueryResponse::Error(_) => panic!("expected status response"),
        }
    }

    #[test]
    fn cleanup_agent_coerces_non_terminal_status_to_failed() {
        let mut runner = make_runner();
        let run_id = RunId("run-cleanup-running".to_string());
        insert_running_agent(&mut runner, &run_id);

        runner.cleanup_agent(CleanupMessage {
            run_id: run_id.clone(),
            terminal_status: AgentStatus::Running,
        });

        let finished = runner
            .finished_agents
            .get(&run_id)
            .expect("run should be persisted in finished map");
        assert_eq!(finished.status, AgentStatus::Failed);
    }

    #[test]
    fn cleanup_agent_does_not_override_terminated_status() {
        let mut runner = make_runner();
        let run_id = RunId("run-terminated".to_string());
        insert_running_agent(&mut runner, &run_id);

        assert!(runner.stop_agent(&run_id));
        runner.cleanup_agent(CleanupMessage {
            run_id: run_id.clone(),
            terminal_status: AgentStatus::Completed,
        });

        let finished = runner
            .finished_agents
            .get(&run_id)
            .expect("terminated run should be persisted in finished map");
        assert_eq!(finished.status, AgentStatus::Terminated);
    }

    #[test]
    fn handle_agent_status_returns_not_found_for_unknown_run() {
        let runner = make_runner();
        let run_id = RunId("run-missing".to_string());
        let response = runner.handle_agent_status(AgentStatusRequest {
            run_id: run_id.clone(),
        });

        match response {
            AgentStatusQueryResponse::Error(error) => {
                assert_eq!(error.run_id, run_id);
                assert_eq!(error.code, AgentStatusErrorCode::NotFound);
                assert_eq!(error.message, "run not found: run-missing");
            }
            AgentStatusQueryResponse::Status(_) => panic!("expected not_found error"),
        }
    }
}
