use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use protocol::{AgentRunEvent, RunId};
use uuid::Uuid;

use crate::{persist_agent_event, persist_json_output, print_agent_event, print_value, run_dir, AgentRunArgs};

use super::provider_client::{session_request_from_args, start_session, to_agent_event};

pub(super) async fn handle_agent_run(
    args: AgentRunArgs,
    _hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let run_id = RunId(args.run_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()));
    let project_root_path = Path::new(project_root);

    let request = session_request_from_args(&args, project_root)?;
    let run = start_session(project_root_path, request).await?;

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
    if args.detach {
        if json {
            let response = serde_json::json!({ "run_id": run_id.0, "status": "submitted" });
            print_value(response, json)?;
        } else {
            eprintln!(
                "warning: --detach now drains the provider session inline under the v0.5.3 \
                 provider-only model; run {} will keep streaming until the provider finishes",
                run_id.0
            );
            if !args.save_jsonl {
                eprintln!("warning: --save-jsonl=false + --detach produces no log to inspect later");
            }
        }
        let run_dir_path =
            if args.save_jsonl { Some(run_dir(project_root, &run_id, args.jsonl_dir.as_deref())) } else { None };
        // Suppress streaming output and json envelope on subsequent
        // frames — the caller asked us to return rather than render to
        // their terminal. Errors still propagate so scripts can react
        // to provider failures.
        return stream_events_quiet(run, run_id, &args.tool, run_dir_path).await;
    }

    let run_dir_path =
        if args.save_jsonl { Some(run_dir(project_root, &run_id, args.jsonl_dir.as_deref())) } else { None };

    stream_events(run, run_id, &args, run_dir_path, json).await
}

async fn stream_events(
    mut run: animus_session_backend::session::SessionRun,
    run_id: RunId,
    args: &AgentRunArgs,
    run_dir_path: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    while let Some(session_event) = run.events.recv().await {
        let is_finished = matches!(session_event, animus_session_backend::session::SessionEvent::Finished { .. });
        let exit_code = if let animus_session_backend::session::SessionEvent::Finished { exit_code } = &session_event {
            *exit_code
        } else {
            None
        };
        // Only unrecoverable error frames terminate the loop early.
        // Recoverable errors are surfaced to the user / persisted but
        // the loop continues until the provider plugin reports
        // `Finished`. This matches v0.5.2's agent-runner semantics
        // (P2 finding from codex review).
        let unrecoverable_error_message = match &session_event {
            animus_session_backend::session::SessionEvent::Error { message, recoverable } if !recoverable => {
                Some(message.clone())
            }
            _ => None,
        };

        let event = to_agent_event(session_event, &run_id);

        if let Some(path) = &run_dir_path {
            persist_agent_event(path, &event)?;
            if let AgentRunEvent::OutputChunk { stream_type, text, .. } = &event {
                persist_json_output(path, *stream_type, text)?;
            }
        }

        if args.stream || json {
            print_agent_event(&event, json, &args.tool)?;
        }

        if let Some(message) = unrecoverable_error_message {
            return Err(anyhow!(message));
        }

        if is_finished {
            if !args.stream && !json {
                println!("run {} finished (exit_code={:?})", run_id.0, exit_code);
            }
            if exit_code.unwrap_or_default() != 0 {
                return Err(anyhow!("agent run exited with code {:?}", exit_code));
            }
            return Ok(());
        }
    }

    Err(anyhow!("provider session ended before run {} completed", run_id.0))
}

/// Synchronous variant of [`stream_events`] used by `--detach`: persists
/// events to the JSONL log (when `run_dir_path` is set) without printing
/// to stdout. Errors propagate so scripts can react to provider failures.
async fn stream_events_quiet(
    mut run: animus_session_backend::session::SessionRun,
    run_id: RunId,
    _tool: &str,
    run_dir_path: Option<PathBuf>,
) -> Result<()> {
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

        if let Some(path) = &run_dir_path {
            persist_agent_event(path, &event)?;
            if let AgentRunEvent::OutputChunk { stream_type, text, .. } = &event {
                persist_json_output(path, *stream_type, text)?;
            }
        }

        if let Some(message) = unrecoverable_error_message {
            return Err(anyhow!(message));
        }

        if is_finished {
            if exit_code.unwrap_or_default() != 0 {
                return Err(anyhow!("agent run exited with code {:?}", exit_code));
            }
            return Ok(());
        }
    }

    Err(anyhow!("provider session ended before run {} completed", run_id.0))
}
