mod event_persistence;
mod launch;
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
    AgentStatusQueryResponse, AgentStatusRequest, AgentStatusResponse, ModelStatusRequest, ModelStatusResponse, RunId,
    RunnerStatusResponse, Timestamp, PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::recording::{DecisionEvent, Recorder};
use crate::telemetry::RunnerMetrics;

pub use supervisor::Supervisor;

struct RunningAgent {
    cancel_tx: oneshot::Sender<()>,
    started_at: Timestamp,
    event_broadcast: broadcast::Sender<AgentRunEvent>,
}

struct FinishedAgent {
    started_at: Timestamp,
    completed_at: Timestamp,
    status: AgentStatus,
}

#[derive(Debug, Clone)]
pub struct CleanupMessage {
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

    pub fn is_run_active(&self, run_id: &RunId) -> bool {
        self.running_agents.contains_key(run_id)
    }

    pub fn subscribe_to_run(&self, run_id: &RunId) -> Option<broadcast::Receiver<AgentRunEvent>> {
        self.running_agents.get(run_id).map(|agent| agent.event_broadcast.subscribe())
    }

    pub fn handle_run_request(
        &mut self,
        req: AgentRunRequest,
        _event_tx: mpsc::Sender<AgentRunEvent>,
    ) -> broadcast::Receiver<AgentRunEvent> {
        let run_id = req.run_id.clone();
        let persistence = RunEventPersistence::new(&req.context, &run_id);
        let recorder = build_decision_recorder(&req, &run_id);
        let (broadcast_tx, broadcast_rx) = broadcast::channel::<AgentRunEvent>(256);
        let (run_event_tx, mut run_event_rx) = mpsc::channel::<AgentRunEvent>(100);
        let run_id_for_forwarder = run_id.clone();
        let broadcast_tx_for_forwarder = broadcast_tx.clone();

        tokio::spawn(async move {
            let mut persistence = persistence;
            let recorder = recorder;
            while let Some(event) = run_event_rx.recv().await {
                if let Err(err) = persistence.persist(&event) {
                    warn!(
                        run_id = %run_id_for_forwarder.0.as_str(),
                        error = %err,
                        "Failed to persist run event"
                    );
                }
                if let Some(rec) = recorder.as_ref() {
                    if let Some(decision) = decision_event_from_agent_event(&event) {
                        if let Err(err) = rec.record(&decision) {
                            warn!(
                                run_id = %run_id_for_forwarder.0.as_str(),
                                error = %err,
                                "Failed to append decision-log entry"
                            );
                        }
                    }
                }
                let is_terminal = matches!(event, AgentRunEvent::Finished { .. } | AgentRunEvent::Error { .. });
                let _ = broadcast_tx_for_forwarder.send(event);
                if is_terminal {
                    break;
                }
            }
        });

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let started_at = Timestamp::now();
        let replaced = self
            .running_agents
            .insert(
                run_id.clone(),
                RunningAgent { cancel_tx, started_at: started_at.clone(), event_broadcast: broadcast_tx },
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

        let cleanup_tx = self.cleanup_tx.clone();
        if let Some(replay_path) = resolve_replay_path(&req) {
            info!(
                run_id = %run_id.0.as_str(),
                path = %replay_path.display(),
                "Routing agent run through decision-log replay (production model bypassed)"
            );
            let run_id_for_task = run_id.clone();
            let consumer_provider = resolve_request_provider_id(&req);
            tokio::spawn(async move {
                let terminal_status =
                    drive_replay(replay_path, run_id_for_task.clone(), consumer_provider, run_event_tx, cancel_rx)
                        .await;
                if cleanup_tx.send(CleanupMessage { run_id: run_id_for_task.clone(), terminal_status }).await.is_err() {
                    warn!(run_id = %run_id_for_task.0.as_str(), "Failed to enqueue cleanup for replayed run");
                }
            });
        } else {
            let supervisor = Supervisor::new();
            tokio::spawn(async move {
                let terminal_status = supervisor.spawn_agent(req, run_event_tx, cancel_rx).await;
                if cleanup_tx.send(CleanupMessage { run_id: run_id.clone(), terminal_status }).await.is_err() {
                    warn!(run_id = %run_id.0.as_str(), "Failed to enqueue cleanup for run");
                }
            });
        }

        broadcast_rx
    }

    pub fn cancel_runs(&mut self, run_ids: &[RunId]) {
        for run_id in run_ids {
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
                    "Cancelled agent run due to client disconnect"
                );
            }
        }
    }

    pub fn cleanup_agent(&mut self, message: CleanupMessage) {
        let CleanupMessage { run_id, terminal_status } = message;
        if let Some(entry) = self.running_agents.remove(&run_id) {
            let terminal_status = normalize_terminal_status_for_cleanup(terminal_status, &run_id);
            let completed_at = Timestamp::now();
            let duration_ms = completed_at.0.signed_duration_since(entry.started_at.0).num_milliseconds().max(0) as u64;
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
                FinishedAgent { started_at: entry.started_at, completed_at, status: terminal_status },
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
        debug!(requested_models = req.models.len(), "Handling model status request");
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
            let elapsed_ms = now.0.signed_duration_since(entry.started_at.0).num_milliseconds().max(0) as u64;
            return AgentStatusQueryResponse::Status(AgentStatusResponse {
                run_id: req.run_id,
                status: AgentStatus::Running,
                elapsed_ms,
                started_at: entry.started_at.clone(),
                completed_at: None,
            });
        }

        if let Some(entry) = self.finished_agents.get(&req.run_id) {
            let elapsed_ms =
                entry.completed_at.0.signed_duration_since(entry.started_at.0).num_milliseconds().max(0) as u64;
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

fn resolve_replay_path(req: &AgentRunRequest) -> Option<std::path::PathBuf> {
    if let Some(value) = req.context.get("replay_session_path").and_then(|v| v.as_str()) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(std::path::PathBuf::from(trimmed));
        }
    }
    match std::env::var("ANIMUS_REPLAY_SESSION") {
        Ok(path) if !path.trim().is_empty() => Some(std::path::PathBuf::from(path)),
        _ => None,
    }
}

async fn drive_replay(
    path: std::path::PathBuf,
    run_id: RunId,
    consumer_provider: Option<String>,
    event_tx: mpsc::Sender<AgentRunEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> AgentStatus {
    let _ = event_tx.send(AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() }).await;

    let source = match crate::recording::ReplaySource::open(&path) {
        Ok(source) => source,
        Err(err) => {
            let _ = event_tx
                .send(AgentRunEvent::Error {
                    run_id: run_id.clone(),
                    error: format!("Replay session open failed: {}", err),
                })
                .await;
            return AgentStatus::Failed;
        }
    };
    if let Some(expected) = consumer_provider.as_deref() {
        if let Err(err) = source.require_provider(expected) {
            let _ = event_tx
                .send(AgentRunEvent::Error {
                    run_id: run_id.clone(),
                    error: format!("Replay session provider guard rejected request: {}", err),
                })
                .await;
            return AgentStatus::Failed;
        }
    }
    let truncated = source.truncated_tail();
    let events = source.drain();

    let mut terminal: Option<AgentStatus> = None;
    for decision in events {
        if cancel_rx.try_recv().is_ok() {
            return AgentStatus::Terminated;
        }
        match decision {
            crate::recording::DecisionEvent::ResponseChunk { stream, text, .. } => {
                let stream_type = match stream.as_str() {
                    "stderr" => protocol::OutputStreamType::Stderr,
                    "system" => protocol::OutputStreamType::System,
                    _ => protocol::OutputStreamType::Stdout,
                };
                let _ = event_tx.send(AgentRunEvent::OutputChunk { run_id: run_id.clone(), stream_type, text }).await;
            }
            crate::recording::DecisionEvent::ToolCall { args, .. } => {
                if let Ok(tool_info) = serde_json::from_value::<protocol::ToolCallInfo>(args) {
                    let _ = event_tx.send(AgentRunEvent::ToolCall { run_id: run_id.clone(), tool_info }).await;
                }
            }
            crate::recording::DecisionEvent::ToolResult { result, .. } => {
                if let Ok(result_info) = serde_json::from_value::<protocol::ToolResultInfo>(result) {
                    let _ = event_tx.send(AgentRunEvent::ToolResult { run_id: run_id.clone(), result_info }).await;
                }
            }
            crate::recording::DecisionEvent::Error { message, .. } => {
                let _ = event_tx.send(AgentRunEvent::Error { run_id: run_id.clone(), error: message }).await;
                terminal = Some(AgentStatus::Failed);
                break;
            }
            crate::recording::DecisionEvent::Finished { exit_code, .. } => {
                let _ =
                    event_tx.send(AgentRunEvent::Finished { run_id: run_id.clone(), exit_code, duration_ms: 0 }).await;
                terminal = Some(if exit_code == Some(0) { AgentStatus::Completed } else { AgentStatus::Failed });
                break;
            }
            _ => {}
        }
    }

    if terminal.is_none() {
        let reason = if truncated {
            "Replay session ended without Finished marker (log was truncated)"
        } else {
            "Replay session ended without Finished marker (incomplete decision log)"
        };
        let _ = event_tx.send(AgentRunEvent::Error { run_id: run_id.clone(), error: reason.to_string() }).await;
        return AgentStatus::Failed;
    }
    terminal.unwrap_or(AgentStatus::Failed)
}

fn build_decision_recorder(req: &AgentRunRequest, run_id: &RunId) -> Option<Recorder> {
    let project_root = req.context.get("project_root").and_then(|v| v.as_str())?;
    let durability = recorder_durability_from_env();
    let recorder = match Recorder::for_run_with_durability(project_root, &run_id.0, durability) {
        Ok(Some(rec)) => rec,
        Ok(None) => return None,
        Err(err) => {
            warn!(
                run_id = %run_id.0.as_str(),
                error = %err,
                "Failed to open decision log for run"
            );
            return None;
        }
    };

    // Emit the cross-provider session header FIRST so ReplaySource can
    // refuse mismatched-provider replays.
    let provider_id = resolve_request_provider_id(req).unwrap_or_else(|| "unknown".to_string());
    let header = crate::recording::DecisionEvent::session_header(provider_id, req.model.0.as_str());
    if let Err(err) = recorder.record(&header) {
        warn!(
            run_id = %run_id.0.as_str(),
            error = %err,
            "Failed to record session header"
        );
    }

    let prompt_text = req.context.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let runtime_contract = req.context.get("runtime_contract").cloned();
    let prompt_event = DecisionEvent::prompt(req.model.0.as_str(), prompt_text, runtime_contract);
    if let Err(err) = recorder.record(&prompt_event) {
        warn!(
            run_id = %run_id.0.as_str(),
            error = %err,
            "Failed to record initial prompt event"
        );
    }
    Some(recorder)
}

/// Resolve the provider id used for the cross-provider replay guard.
///
/// Mirrors [`Supervisor`]'s actual launch precedence (codex round-4 P2):
/// 1. `context.runtime_contract.cli.name`
/// 2. `context.tool`
/// 3. The supervisor's `"claude"` fallback (only when both overrides are
///    absent). We do NOT consult the model id — Supervisor doesn't either,
///    so deriving from `tool_for_model_id` would tag the recording with a
///    different provider than the actually-launched CLI.
fn resolve_request_provider_id(req: &AgentRunRequest) -> Option<String> {
    if let Some(name) = req
        .context
        .pointer("/runtime_contract/cli/name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(name.to_string());
    }
    if let Some(tool) = req.context.get("tool").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        return Some(tool.to_string());
    }
    // Supervisor falls back to `claude` here; the guard MUST agree so
    // recordings and consumer-side require_provider stay in lockstep.
    Some("claude".to_string())
}

fn recorder_durability_from_env() -> crate::recording::Durability {
    use crate::recording::Durability;
    match std::env::var("ANIMUS_RECORDER_DURABILITY").ok().as_deref() {
        Some("flush_only") => Durability::FlushOnly,
        Some("fsync_per_event") => Durability::FsyncPerEvent,
        Some(other) if other.starts_with("fsync_every_") => {
            let n_str = other.trim_start_matches("fsync_every_");
            n_str.parse::<usize>().map(Durability::FsyncEveryN).unwrap_or_else(|_| Durability::production_default())
        }
        _ => Durability::production_default(),
    }
}

fn decision_event_from_agent_event(event: &AgentRunEvent) -> Option<DecisionEvent> {
    match event {
        AgentRunEvent::OutputChunk { stream_type, text, .. } => {
            let stream = match stream_type {
                protocol::OutputStreamType::Stdout => "stdout",
                protocol::OutputStreamType::Stderr => "stderr",
                protocol::OutputStreamType::System => "system",
            };
            Some(DecisionEvent::response_chunk(stream, text.clone()))
        }
        AgentRunEvent::ToolCall { tool_info, .. } => {
            let value = serde_json::to_value(tool_info).unwrap_or(serde_json::Value::Null);
            let name = value
                .get("tool_name")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("name").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            Some(DecisionEvent::tool_call(name, value))
        }
        AgentRunEvent::ToolResult { result_info, .. } => {
            let value = serde_json::to_value(result_info).unwrap_or(serde_json::Value::Null);
            let name = value
                .get("tool_name")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("name").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            Some(DecisionEvent::tool_result(name, value))
        }
        AgentRunEvent::Metadata { cost, tokens, .. } => {
            let payload = serde_json::json!({
                "cost": cost,
                "tokens": tokens,
            });
            Some(DecisionEvent::metadata(payload))
        }
        AgentRunEvent::Finished { exit_code, .. } => Some(DecisionEvent::finished(*exit_code)),
        AgentRunEvent::Error { error, .. } => Some(DecisionEvent::error(error.clone())),
        AgentRunEvent::Started { .. } | AgentRunEvent::Artifact { .. } | AgentRunEvent::Thinking { .. } => None,
    }
}

fn normalize_terminal_status_for_cleanup(status: AgentStatus, run_id: &RunId) -> AgentStatus {
    match status {
        AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Timeout | AgentStatus::Terminated => status,
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

fn normalize_runner_build_id(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn runner_build_id() -> Option<String> {
    normalize_runner_build_id(std::env::var("ANIMUS_RUNNER_BUILD_ID").ok())
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
        let (broadcast_tx, _) = broadcast::channel(16);
        runner.running_agents.insert(
            run_id.clone(),
            RunningAgent { cancel_tx, started_at: Timestamp::now(), event_broadcast: broadcast_tx },
        );
    }

    #[test]
    fn cleanup_agent_persists_terminal_status_from_cleanup_message() {
        let mut runner = make_runner();
        let run_id = RunId("run-cleanup-failed".to_string());
        insert_running_agent(&mut runner, &run_id);

        runner.cleanup_agent(CleanupMessage { run_id: run_id.clone(), terminal_status: AgentStatus::Failed });

        let finished = runner.finished_agents.get(&run_id).expect("run should be persisted in finished map");
        assert_eq!(finished.status, AgentStatus::Failed);
    }

    #[test]
    fn handle_agent_status_returns_failed_after_failed_cleanup() {
        let mut runner = make_runner();
        let run_id = RunId("run-query-failed".to_string());
        insert_running_agent(&mut runner, &run_id);

        runner.cleanup_agent(CleanupMessage { run_id: run_id.clone(), terminal_status: AgentStatus::Failed });

        let response = runner.handle_agent_status(AgentStatusRequest { run_id: run_id.clone() });
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

        runner.cleanup_agent(CleanupMessage { run_id: run_id.clone(), terminal_status: AgentStatus::Running });

        let finished = runner.finished_agents.get(&run_id).expect("run should be persisted in finished map");
        assert_eq!(finished.status, AgentStatus::Failed);
    }

    #[test]
    fn cleanup_agent_does_not_override_terminated_status() {
        let mut runner = make_runner();
        let run_id = RunId("run-terminated".to_string());
        insert_running_agent(&mut runner, &run_id);

        assert!(runner.stop_agent(&run_id));
        runner.cleanup_agent(CleanupMessage { run_id: run_id.clone(), terminal_status: AgentStatus::Completed });

        let finished = runner.finished_agents.get(&run_id).expect("terminated run should be persisted in finished map");
        assert_eq!(finished.status, AgentStatus::Terminated);
    }

    #[test]
    fn handle_agent_status_returns_not_found_for_unknown_run() {
        let runner = make_runner();
        let run_id = RunId("run-missing".to_string());
        let response = runner.handle_agent_status(AgentStatusRequest { run_id: run_id.clone() });

        match response {
            AgentStatusQueryResponse::Error(error) => {
                assert_eq!(error.run_id, run_id);
                assert_eq!(error.code, AgentStatusErrorCode::NotFound);
                assert_eq!(error.message, "run not found: run-missing");
            }
            AgentStatusQueryResponse::Status(_) => panic!("expected not_found error"),
        }
    }

    #[test]
    fn cancel_runs_terminates_active_agents() {
        let mut runner = make_runner();
        let run_id = RunId("run-cancel-test".to_string());
        insert_running_agent(&mut runner, &run_id);

        runner.cancel_runs(&[run_id.clone()]);

        assert!(!runner.is_run_active(&run_id));
        let finished = runner.finished_agents.get(&run_id).expect("should be finished");
        assert_eq!(finished.status, AgentStatus::Terminated);
    }

    #[test]
    fn subscribe_to_active_run_returns_receiver() {
        let mut runner = make_runner();
        let run_id = RunId("run-subscribe-test".to_string());
        insert_running_agent(&mut runner, &run_id);

        assert!(runner.subscribe_to_run(&run_id).is_some());
        assert!(runner.subscribe_to_run(&RunId("nonexistent".to_string())).is_none());
    }

    #[test]
    fn normalize_runner_build_id_trims_runtime_values() {
        assert_eq!(normalize_runner_build_id(Some("  build-123  ".to_string())), Some("build-123".to_string()));
    }

    #[test]
    fn normalize_runner_build_id_rejects_empty_values() {
        assert_eq!(normalize_runner_build_id(Some("   ".to_string())), None);
        assert_eq!(normalize_runner_build_id(None), None);
    }

    #[test]
    fn resolve_request_provider_id_prefers_runtime_contract_cli_name_over_tool() {
        use protocol::{AgentRunRequest, ModelId, PROTOCOL_VERSION};
        // Supervisor: runtime_contract.cli.name beats context.tool. The
        // guard must agree (codex round-4 P2).
        let req = AgentRunRequest {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: RunId("r".to_string()),
            model: ModelId("claude-sonnet-4-6".to_string()),
            context: serde_json::json!({
                "tool": "codex",
                "runtime_contract": {"cli": {"name": "gemini"}}
            }),
            timeout_secs: None,
        };
        assert_eq!(resolve_request_provider_id(&req).as_deref(), Some("gemini"));
    }

    #[test]
    fn resolve_request_provider_id_uses_context_tool_when_runtime_contract_absent() {
        use protocol::{AgentRunRequest, ModelId, PROTOCOL_VERSION};
        let req = AgentRunRequest {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: RunId("r".to_string()),
            model: ModelId("claude-sonnet-4-6".to_string()),
            context: serde_json::json!({"tool": "codex"}),
            timeout_secs: None,
        };
        assert_eq!(resolve_request_provider_id(&req).as_deref(), Some("codex"));
    }

    #[test]
    fn resolve_request_provider_id_defaults_to_claude_when_no_override() {
        use protocol::{AgentRunRequest, ModelId, PROTOCOL_VERSION};
        // Supervisor's `.unwrap_or("claude")` is the final fallback; the
        // guard returns the same default so recording and consumer sides
        // agree even for requests with neither override set.
        let req = AgentRunRequest {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: RunId("r".to_string()),
            model: ModelId("claude-sonnet-4-6".to_string()),
            context: serde_json::json!({}),
            timeout_secs: None,
        };
        assert_eq!(resolve_request_provider_id(&req).as_deref(), Some("claude"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_run_request_routes_through_replay_when_path_in_context() {
        use crate::recording::{DecisionEvent, Recorder};
        use protocol::{AgentRunRequest, ModelId, OutputStreamType, PROTOCOL_VERSION};
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let replay_path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_at(&replay_path).expect("recorder");
        recorder.record(&DecisionEvent::prompt("claude-sonnet", "hi", None)).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "alpha")).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "beta")).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);

        let (cleanup_tx, mut cleanup_rx) = mpsc::channel::<CleanupMessage>(4);
        let mut runner = Runner::new(cleanup_tx);

        let req = AgentRunRequest {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: RunId("replay-run-1".to_string()),
            model: ModelId("claude-sonnet".to_string()),
            context: serde_json::json!({
                "replay_session_path": replay_path.to_string_lossy(),
                "prompt": "hi",
            }),
            timeout_secs: None,
        };

        let (event_tx, _event_rx) = mpsc::channel::<AgentRunEvent>(16);
        let mut broadcast_rx = runner.handle_run_request(req, event_tx);

        let mut chunks: Vec<String> = Vec::new();
        let mut finished = false;
        while let Ok(evt) = tokio::time::timeout(std::time::Duration::from_secs(5), broadcast_rx.recv()).await {
            let evt = match evt {
                Ok(e) => e,
                Err(_) => break,
            };
            match evt {
                AgentRunEvent::OutputChunk { stream_type: OutputStreamType::Stdout, text, .. } => {
                    chunks.push(text);
                }
                AgentRunEvent::Finished { exit_code, .. } => {
                    finished = true;
                    assert_eq!(exit_code, Some(0));
                    break;
                }
                AgentRunEvent::Error { error, .. } => panic!("unexpected error event: {error}"),
                _ => {}
            }
        }
        assert!(finished, "replay must end with Finished");
        assert_eq!(chunks, vec!["alpha".to_string(), "beta".to_string()]);

        if let Ok(Some(msg)) = tokio::time::timeout(std::time::Duration::from_secs(5), cleanup_rx.recv()).await {
            assert_eq!(msg.run_id.0, "replay-run-1");
            assert!(matches!(msg.terminal_status, AgentStatus::Completed));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_run_request_replay_without_finished_marker_fails() {
        use crate::recording::{DecisionEvent, Recorder};
        use protocol::{AgentRunRequest, ModelId, PROTOCOL_VERSION};
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let replay_path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_at(&replay_path).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "alpha")).unwrap();
        drop(recorder);

        let (cleanup_tx, mut cleanup_rx) = mpsc::channel::<CleanupMessage>(4);
        let mut runner = Runner::new(cleanup_tx);
        let req = AgentRunRequest {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: RunId("replay-incomplete".to_string()),
            model: ModelId("m".to_string()),
            context: serde_json::json!({"replay_session_path": replay_path.to_string_lossy()}),
            timeout_secs: None,
        };
        let (event_tx, _event_rx) = mpsc::channel::<AgentRunEvent>(16);
        let mut broadcast_rx = runner.handle_run_request(req, event_tx);

        let mut saw_error = false;
        while let Ok(evt) = tokio::time::timeout(std::time::Duration::from_secs(5), broadcast_rx.recv()).await {
            let evt = match evt {
                Ok(e) => e,
                Err(_) => break,
            };
            if let AgentRunEvent::Error { error, .. } = evt {
                assert!(error.contains("Finished"));
                saw_error = true;
                break;
            }
        }
        assert!(saw_error, "incomplete replay must surface an Error event");

        if let Ok(Some(msg)) = tokio::time::timeout(std::time::Duration::from_secs(5), cleanup_rx.recv()).await {
            assert!(matches!(msg.terminal_status, AgentStatus::Failed));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_run_request_replay_yields_tool_call_and_result_events() {
        use crate::recording::{DecisionEvent, Recorder};
        use protocol::{AgentRunRequest, ModelId, ToolCallInfo, ToolResultInfo, PROTOCOL_VERSION};
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let replay_path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_at(&replay_path).expect("recorder");
        let tool_call = ToolCallInfo {
            tool_name: "read_file".to_string(),
            parameters: serde_json::json!({"path": "/tmp/a"}),
            timestamp: Timestamp::now(),
        };
        let tool_result = ToolResultInfo {
            tool_name: "read_file".to_string(),
            result: serde_json::json!({"content": "hello"}),
            duration_ms: 1,
            success: true,
        };
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        recorder
            .record(&DecisionEvent::tool_call(tool_call.tool_name.clone(), serde_json::to_value(&tool_call).unwrap()))
            .unwrap();
        recorder
            .record(&DecisionEvent::tool_result(
                tool_result.tool_name.clone(),
                serde_json::to_value(&tool_result).unwrap(),
            ))
            .unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);

        let (cleanup_tx, _cleanup_rx) = mpsc::channel::<CleanupMessage>(4);
        let mut runner = Runner::new(cleanup_tx);
        let req = AgentRunRequest {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: RunId("replay-tools".to_string()),
            model: ModelId("m".to_string()),
            context: serde_json::json!({"replay_session_path": replay_path.to_string_lossy()}),
            timeout_secs: None,
        };
        let (event_tx, _event_rx) = mpsc::channel::<AgentRunEvent>(16);
        let mut broadcast_rx = runner.handle_run_request(req, event_tx);

        let mut saw_call = false;
        let mut saw_result = false;
        while let Ok(evt) = tokio::time::timeout(std::time::Duration::from_secs(5), broadcast_rx.recv()).await {
            let evt = match evt {
                Ok(e) => e,
                Err(_) => break,
            };
            match evt {
                AgentRunEvent::ToolCall { tool_info, .. } => {
                    assert_eq!(tool_info.tool_name, "read_file");
                    saw_call = true;
                }
                AgentRunEvent::ToolResult { result_info, .. } => {
                    assert_eq!(result_info.tool_name, "read_file");
                    assert!(result_info.success);
                    saw_result = true;
                }
                AgentRunEvent::Finished { .. } => break,
                _ => {}
            }
        }
        assert!(saw_call, "replay must yield AgentRunEvent::ToolCall");
        assert!(saw_result, "replay must yield AgentRunEvent::ToolResult");
    }
}
