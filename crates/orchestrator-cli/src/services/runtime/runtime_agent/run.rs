use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use protocol::{AgentRunEvent, RunId};
use serde::Serialize;
use uuid::Uuid;

use crate::{persist_agent_event, persist_json_output, print_agent_event, print_value, run_dir, AgentRunArgs};

use super::provider_client::{session_request_from_args, start_session, to_agent_event};
use animus_runtime_shared::phase_session::list_running_checkpoints;

pub(crate) struct AgentRunApplicationRequest {
    args: AgentRunArgs,
    run_id: RunId,
}

impl AgentRunApplicationRequest {
    pub(crate) fn new(mut args: AgentRunArgs) -> Self {
        let run_id = RunId(args.run_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()));
        args.run_id = Some(run_id.0.clone());
        Self { args, run_id }
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id.0
    }

    pub(crate) fn detach(&self) -> bool {
        self.args.detach
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentRunApplicationView {
    pub(crate) run_id: String,
    pub(crate) status: &'static str,
    pub(crate) event_count: usize,
    pub(crate) exit_code: Option<i32>,
    pub(crate) events_path: Option<String>,
}

pub(super) async fn handle_agent_run(
    args: AgentRunArgs,
    _hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let request = AgentRunApplicationRequest::new(args);
    let run_id = request.run_id().to_string();
    let detach = request.detach();
    let stream = request.args.stream;
    let tool = request.args.tool.clone();

    // v0.5.3: the agent-runner sidecar used to host the provider session
    // out-of-process so `--detach` could safely return while the run kept
    // streaming events into the JSONL log. Under the provider-only model
    // the CLI hosts the plugin process itself, so if we returned right
    // away the Tokio runtime would shut down and abort the provider
    // before its first frame landed on disk. Two honest modes:
    //
    //   * `--detach` + `--save-jsonl` (the default): drain synchronously
    //     so the JSONL log is complete by the time `animus agent run`
    //     returns; suppress stdout streaming so the caller's stdout
    //     still looks "submitted-then-quiet". `--json` still emits the
    //     submission envelope first.
    //   * `--detach --save-jsonl=false`: warn that the run cannot be
    //     detached under v0.5.3 because there is no log to inspect
    //     later, and fall through to the synchronous path. Operators who
    //     actually need fire-and-forget should hand the request to the
    //     daemon's plugin host via `animus workflow run` instead.
    if detach {
        if json {
            let response = serde_json::json!({ "run_id": run_id, "status": "submitted" });
            print_value(response, json)?;
        } else {
            eprintln!(
                "warning: --detach now drains the provider session inline under the v0.5.3 \
                 provider-only model; run {} will keep streaming until the provider finishes",
                run_id
            );
            if !request.args.save_jsonl {
                eprintln!("warning: --save-jsonl=false + --detach produces no log to inspect later");
            }
        }
    }

    let result = agent_run_application(request, project_root, |event| {
        if !detach && (stream || json) {
            print_agent_event(event, json, &tool)?;
        }
        Ok(())
    })
    .await?;

    if !detach && !stream && !json {
        println!("run {run_id} finished (exit_code={:?})", result.exit_code);
    }
    Ok(())
}

/// Execute an ad-hoc provider session through the shared typed application
/// boundary. The provider plugin remains the live session owner and the
/// scoped JSONL directory remains the durable status owner; callers only
/// choose how observed events are rendered.
pub(crate) async fn agent_run_application<F>(
    request: AgentRunApplicationRequest,
    project_root: &str,
    mut observe: F,
) -> Result<AgentRunApplicationView>
where
    F: FnMut(&AgentRunEvent) -> Result<()>,
{
    let AgentRunApplicationRequest { args, run_id } = request;
    let project_root_path = Path::new(project_root);
    let session_request = session_request_from_args(&args, project_root)?;
    // v0.7 TASK-166 Phase 2: when the run resolves to a NON-LOCAL environment
    // (config `environment_routing:` or the ANIMUS_ENVIRONMENT_EXEC override),
    // the harness executes inside that environment plugin instead of the host.
    // The default resolution is `None`, taking the unchanged local path.
    let mut run = match super::environment_exec::resolve_exec_environment(project_root_path, &session_request.tool) {
        Some(environment) => {
            let checkpoint_target = environment_checkpoint_target(project_root_path, &run_id)?;
            super::environment_exec::start_environment_session(
                project_root_path,
                &environment,
                &session_request,
                checkpoint_target,
            )?
        }
        None => start_session(project_root_path, session_request).await?,
    };
    let run_dir_path =
        if args.save_jsonl { Some(run_dir(project_root, &run_id, args.jsonl_dir.as_deref())) } else { None };
    let mut event_count = 0usize;

    while let Some(session_event) = run.events.recv().await {
        let is_finished = matches!(session_event, animus_session_backend::session::SessionEvent::Finished { .. });
        let exit_code = if let animus_session_backend::session::SessionEvent::Finished { exit_code } = &session_event {
            *exit_code
        } else {
            None
        };
        let unrecoverable_error_message = match &session_event {
            animus_session_backend::session::SessionEvent::Error { message, recoverable } if !recoverable => {
                Some(message.clone())
            }
            _ => None,
        };
        let event = to_agent_event(session_event, &run_id);
        event_count = event_count.saturating_add(1);

        if let Some(path) = &run_dir_path {
            persist_agent_event(path, &event)?;
            if let AgentRunEvent::OutputChunk { stream_type, text, .. } = &event {
                persist_json_output(path, *stream_type, text)?;
            }
        }
        observe(&event)?;

        if let Some(message) = unrecoverable_error_message {
            return Err(anyhow!(message));
        }
        if is_finished {
            if exit_code.unwrap_or_default() != 0 {
                return Err(anyhow!("agent run exited with code {:?}", exit_code));
            }
            return Ok(AgentRunApplicationView {
                run_id: run_id.0,
                status: if args.detach { "submitted" } else { "completed" },
                event_count,
                exit_code,
                events_path: run_dir_path.map(|path| path.join("events.jsonl").display().to_string()),
            });
        }
    }

    Err(anyhow!("provider session ended before run {} completed", run_id.0))
}

fn environment_checkpoint_target(
    project_root: &Path,
    run_id: &RunId,
) -> Result<Option<super::environment_exec::EnvironmentCheckpointTarget>> {
    let Some(scoped_root) = protocol::repository_scope::scoped_state_root(project_root) else {
        return Ok(None);
    };
    let checkpoint = list_running_checkpoints(&scoped_root)?
        .into_iter()
        .map(|(_, checkpoint)| checkpoint)
        .find(|checkpoint| checkpoint.run_id == run_id.0);
    let Some(checkpoint) = checkpoint else {
        // Ad-hoc agent runs have no phase checkpoint. Their normal pipeline
        // still tears down; workflow-linked runs must always have been
        // checkpointed before their agent child is launched.
        tracing::debug!(run_id = %run_id.0, "prepared environment belongs to an ad-hoc run; no phase checkpoint to bind");
        return Ok(None);
    };
    Ok(Some(super::environment_exec::EnvironmentCheckpointTarget {
        scoped_root,
        workflow_id: checkpoint.workflow_id,
        phase_id: checkpoint.phase_id,
    }))
}
