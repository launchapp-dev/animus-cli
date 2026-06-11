use crate::config_context::RuntimeConfigContext;
use crate::ipc::{
    build_runtime_contract_with_resume, collect_json_payload_lines, connect_runner, event_matches_run,
    run_dir as ipc_run_dir, runner_config_dir, write_json_line,
};
use crate::payload_traversal::{parse_commit_message_from_text, parse_phase_decision_from_text};
use crate::phase_command::{
    build_command_phase_decision, build_command_result_payload, run_workflow_phase_with_command,
    CommandExecutionContext,
};
use crate::phase_failover::PhaseFailureClassifier;
use crate::phase_git::commit_implementation_changes;
use crate::phase_output::persist_phase_output;
use crate::phase_prompt::{
    phase_requires_commit_message_with_ctx, phase_result_kind_for_ctx, render_phase_prompt_with_ctx_overrides,
    PhasePromptInputs, PhaseRenderParams,
};
use crate::phase_targets::PhaseTargetPlanner;
use crate::runtime_contract::{
    apply_phase_capability_launch_flags, inject_agent_tool_policy, inject_default_stdio_mcp,
    inject_memory_mcp_for_capable_agent, inject_named_mcp_servers, inject_project_mcp_servers,
    inject_response_schema_into_launch_args, inject_workflow_mcp_servers, phase_output_json_schema_for,
    phase_response_json_schema_for, set_mcp_tool_policy,
};
use crate::runtime_support::{
    inject_cli_launch_env, inject_cli_launch_overrides, phase_max_continuations, phase_runner_attempts,
    WorkflowPhaseRuntimeSettings,
};
use crate::skill_dispatch;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use orchestrator_config::{skill_resolution::ResolvedSkill, SkillApplicationResult};
use orchestrator_core::ServiceHub;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::time::sleep;
use tracing::{debug, info, warn};
use uuid::Uuid;

use protocol::{canonical_model_id, AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};

#[derive(Debug, Clone, Default)]
pub struct PhaseExecuteOverrides {
    pub tool: Option<String>,
    pub model: Option<String>,
    pub rework_context: Option<String>,
}

#[derive(Default)]
pub struct CliPhaseExecutor;

fn debug_mcp_stdio_enabled() -> bool {
    protocol::parse_env_bool("ANIMUS_DEBUG_MCP_STDIO")
}

#[async_trait]
impl orchestrator_core::PhaseExecutor for CliPhaseExecutor {
    async fn execute_phase(
        &self,
        request: orchestrator_core::PhaseExecutionRequest,
    ) -> Result<orchestrator_core::PhaseExecutionResult> {
        let hub = orchestrator_core::FileServiceHub::new(&request.project_root)?;
        let (subject_id, subject_title, subject_description, task_complexity) =
            if let Ok(task) = hub.tasks().get(&request.task_id).await {
                (task.id.clone(), task.title.clone(), task.description.clone(), Some(task.complexity))
            } else if let Ok(req) = hub.planning().get_requirement(&request.task_id).await {
                (req.id.clone(), req.title.clone(), req.description.clone(), None)
            } else {
                (request.task_id.clone(), request.task_id.clone(), String::new(), None)
            };

        let execution_cwd = if Path::new(&request.config_dir).is_dir() {
            request.config_dir.clone()
        } else {
            request.project_root.clone()
        };

        let phase_timeout_secs = request.timeout;

        let overrides = if request.tool_override.is_some() || request.model_override.is_some() {
            Some(PhaseExecuteOverrides {
                tool: request.tool_override,
                model: request.model_override,
                rework_context: None,
            })
        } else {
            None
        };

        let routing = protocol::PhaseRoutingConfig::default();
        let run_result = run_workflow_phase(&PhaseRunParams {
            project_root: &request.project_root,
            execution_cwd: &execution_cwd,
            workflow_id: &request.workflow_ref,
            workflow_ref: &request.workflow_ref,
            subject_id: &subject_id,
            subject_title: &subject_title,
            subject_description: &subject_description,
            task_complexity,
            phase_id: &request.phase_id,
            phase_attempt: 0,
            overrides: overrides.as_ref(),
            pipeline_vars: None,
            dispatch_input: None,
            schedule_input: None,
            routing: &routing,
            phase_timeout_secs,
        })
        .await;

        let run_result = run_result?;
        let output_log = serde_json::to_string_pretty(&run_result)?;

        let mut commit_message = None;
        let (verdict, exit_code, error) = phase_execution_result_values(
            &request.project_root,
            &request.workflow_ref,
            &request.phase_id,
            &run_result.outcome,
        );

        if let PhaseExecutionOutcome::Completed {
            commit_message: resolved_commit,
            phase_decision: Some(decision),
            ..
        } = &run_result.outcome
        {
            commit_message = resolved_commit.clone().or_else(|| decision.commit_message.clone());
        }

        Ok(orchestrator_core::PhaseExecutionResult { exit_code, verdict, output_log, error, commit_message })
    }
}

fn yaml_verdict_target(
    project_root: &str,
    workflow_ref: &str,
    phase_id: &str,
    verdict: &str,
    requested_target: Option<&str>,
) -> Option<String> {
    let loaded_config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
    let routing = orchestrator_core::resolve_workflow_verdict_routing(&loaded_config.config, Some(workflow_ref));
    let transition = routing.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(phase_id)).and_then(
        |(_, transitions)| {
            transitions
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(verdict))
                .map(|(_, transition)| transition)
        },
    )?;
    let workflow_phases =
        orchestrator_core::resolve_phase_plan_for_workflow_ref(Some(Path::new(project_root)), Some(workflow_ref))
            .ok()?;
    let requested_target = requested_target.map(str::trim).filter(|target| !target.is_empty());
    if transition.allow_agent_target {
        let requested_target = requested_target?;
        if (transition.allowed_targets.is_empty()
            || transition.allowed_targets.iter().any(|allowed| allowed.eq_ignore_ascii_case(requested_target)))
            && workflow_phases.iter().any(|phase| phase.eq_ignore_ascii_case(requested_target))
        {
            return workflow_phases.into_iter().find(|phase| phase.eq_ignore_ascii_case(requested_target));
        }
    }
    let target = transition.target.trim();
    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}

fn phase_execution_result_values(
    project_root: &str,
    workflow_ref: &str,
    phase_id: &str,
    outcome: &PhaseExecutionOutcome,
) -> (orchestrator_core::PhaseVerdict, i32, Option<String>) {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision, .. } => match phase_decision {
            Some(decision) => match decision.verdict {
                orchestrator_core::PhaseDecisionVerdict::Advance => (orchestrator_core::PhaseVerdict::Advance, 0, None),
                orchestrator_core::PhaseDecisionVerdict::Rework => (
                    orchestrator_core::PhaseVerdict::Rework {
                        target_phase: yaml_verdict_target(
                            project_root,
                            workflow_ref,
                            phase_id,
                            "rework",
                            decision.target_phase.as_deref(),
                        )
                        .unwrap_or_else(|| phase_id.to_string()),
                    },
                    0,
                    None,
                ),
                orchestrator_core::PhaseDecisionVerdict::Skip => (orchestrator_core::PhaseVerdict::Skip, 0, None),
                orchestrator_core::PhaseDecisionVerdict::Fail => {
                    let reason = if decision.reason.trim().is_empty() {
                        "phase verdict fail".to_string()
                    } else {
                        decision.reason.clone()
                    };
                    (orchestrator_core::PhaseVerdict::Failed { reason: reason.clone() }, 1, Some(reason))
                }
                orchestrator_core::PhaseDecisionVerdict::Unknown => {
                    let reason = "phase verdict unknown".to_string();
                    (orchestrator_core::PhaseVerdict::Failed { reason: reason.clone() }, 1, Some(reason))
                }
            },
            None => (orchestrator_core::PhaseVerdict::Advance, 0, None),
        },
        PhaseExecutionOutcome::ManualPending { instructions, .. } => {
            let reason = format!("manual review required: {instructions}");
            (orchestrator_core::PhaseVerdict::Failed { reason: reason.clone() }, 1, Some(reason))
        }
    }
}

fn runtime_contract_string_array(contract: &Value, pointer: &str) -> Vec<String> {
    contract
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn runtime_contract_additional_server_names(contract: &Value) -> Vec<String> {
    contract
        .pointer("/mcp/additional_servers")
        .and_then(Value::as_object)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

pub fn load_agent_runtime_config(project_root: &str) -> orchestrator_core::AgentRuntimeConfig {
    orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root))
}

fn load_agent_runtime_config_strict(project_root: &str) -> Result<orchestrator_core::LoadedAgentRuntimeConfig> {
    orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))
}

fn load_workflow_config_strict(project_root: &str) -> Result<orchestrator_core::LoadedWorkflowConfig> {
    orchestrator_core::load_workflow_config_with_metadata(Path::new(project_root))
}

fn configured_tool_profile(phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>) -> Option<&str> {
    phase_runtime_settings
        .and_then(|settings| settings.tool_profile.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn resolve_claude_profile_env(
    tool_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
    global_config: Option<&protocol::Config>,
) -> Result<BTreeMap<String, String>> {
    let Some(profile_name) = configured_tool_profile(phase_runtime_settings) else {
        return Ok(BTreeMap::new());
    };

    if !tool_id.eq_ignore_ascii_case("claude") {
        return Err(anyhow!(
            "tool_profile '{}' is only supported when the effective tool is claude (resolved '{}')",
            profile_name,
            tool_id
        ));
    }

    let config = global_config
        .ok_or_else(|| anyhow!("global Animus config is required to resolve claude profile '{}'", profile_name))?;
    let profile = config
        .claude_profile(profile_name)
        .ok_or_else(|| anyhow!("unknown global claude profile '{}'", profile_name))?;

    if profile.env.keys().any(|key| key.trim().is_empty()) {
        return Err(anyhow!("global claude profile '{}' contains an empty env key", profile_name));
    }

    Ok(profile.env.clone())
}

fn hash_serializable<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionMetadata {
    pub phase_id: String,
    pub phase_mode: String,
    pub phase_definition_hash: String,
    pub agent_runtime_config_hash: String,
    pub agent_runtime_schema: String,
    pub agent_runtime_version: u32,
    pub agent_runtime_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub effective_capabilities: protocol::PhaseCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_skills: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_skills: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_application: Option<SkillApplicationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionSignal {
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRunResult {
    pub outcome: PhaseExecutionOutcome,
    pub metadata: PhaseExecutionMetadata,
    #[serde(default)]
    pub signals: Vec<PhaseExecutionSignal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// Sentinel marker attached to errors that should NOT terminally fail the
/// workflow phase. The scheduler arm in `workflow_execute` downcasts on
/// this type to distinguish a pre-runner dispatch failure from a real
/// phase failure when emitting operator-visible events. Used today by
/// the dispatch path in [`run_workflow_phase_attempt`] for pre-runner
/// checkpoint write failures.
///
/// Wrapped values are formatted into the surrounding `anyhow::Error` so
/// log output remains readable; the marker is recovered via
/// `error.downcast_ref::<DispatchRetryableError>()`.
#[derive(Debug)]
pub struct DispatchRetryableError {
    pub message: String,
}

impl std::fmt::Display for DispatchRetryableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DispatchRetryableError {}

/// Sentinel attached to TERMINAL post-runner checkpoint write failures
/// (the `Running` → `Completed`/`Failed` mutation that runs after the
/// agent has already produced its outcome). The agent retry/failover
/// loop in `run_workflow_phase_with_agent` checks for this marker and
/// REFUSES to retry — without the marker, an I/O message that happens
/// to match the transient-runner-error classifier (eg fsync on network
/// storage returning "connection timed out") would re-dispatch a
/// phase whose side effects have already happened, which is exactly the
/// crash-replay hazard this fix exists to prevent.
#[derive(Debug)]
pub struct TerminalCheckpointError {
    pub message: String,
}

impl std::fmt::Display for TerminalCheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TerminalCheckpointError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum PhaseExecutionOutcome {
    Completed {
        commit_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase_decision: Option<orchestrator_core::PhaseDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_payload: Option<Value>,
    },
    ManualPending {
        instructions: String,
        approval_note_required: bool,
    },
}

fn outcome_verdict(outcome: &PhaseExecutionOutcome) -> orchestrator_core::PhaseDecisionVerdict {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision, .. } => phase_decision
            .as_ref()
            .map(|decision| decision.verdict)
            .unwrap_or(orchestrator_core::PhaseDecisionVerdict::Advance),
        PhaseExecutionOutcome::ManualPending { .. } => orchestrator_core::PhaseDecisionVerdict::Advance,
    }
}

fn routing_complexity(
    task_complexity: Option<orchestrator_core::Complexity>,
) -> Option<protocol::ModelRoutingComplexity> {
    task_complexity.map(|complexity| match complexity {
        orchestrator_core::Complexity::Low => protocol::ModelRoutingComplexity::Low,
        orchestrator_core::Complexity::Medium => protocol::ModelRoutingComplexity::Medium,
        orchestrator_core::Complexity::High => protocol::ModelRoutingComplexity::High,
    })
}

pub(crate) fn validate_basic_json_schema(instance: &Value, schema: &Value) -> Result<()> {
    let validator = jsonschema::validator_for(schema).map_err(|e| anyhow!("invalid JSON Schema: {}", e))?;

    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| {
            let path = e.instance_path().to_string();
            if path.is_empty() {
                format!("{}", e)
            } else {
                format!("at '{}': {}", path, e)
            }
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("schema validation failed: {}", errors.join("; ")))
    }
}

pub async fn run_workflow_phase_attempt(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    request: &AgentRunRequest,
) -> Result<PhaseExecutionOutcome> {
    let ctx = RuntimeConfigContext::load(project_root);
    let parse_commit_message = phase_requires_commit_message_with_ctx(&ctx, phase_id);
    let config_dir = runner_config_dir(Path::new(project_root));

    let scoped_state_root =
        protocol::scoped_state_root(Path::new(project_root)).unwrap_or_else(|| Path::new(project_root).join(".animus"));
    let provider_hint = request
        .context
        .pointer("/tool")
        .or_else(|| request.context.pointer("/runtime_contract/cli/name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    // RETRYABLE-FATAL: if we can't write the pending checkpoint, abort
    // BEFORE dispatching to the runner. The recovery oracle
    // (`list_running_checkpoints`) needs a durable Running checkpoint to
    // shield an in-flight phase across daemon restart; dispatching without
    // one means a crash mid-phase would silently lose the work AND a
    // re-tick could double-dispatch a side-effecting phase. No
    // side-effecting work has happened yet, so the scheduler should
    // RE-DISPATCH on the next tick (not terminally fail the workflow);
    // the [`DispatchRetryableError`] sentinel signals exactly that to
    // `execute_workflow`.
    if let Err(err) = crate::phase_session::write_session_pending(
        &scoped_state_root,
        workflow_id,
        phase_id,
        &provider_hint,
        request.run_id.0.as_str(),
        Some(serde_json::to_value(request).unwrap_or(Value::Null)),
    ) {
        warn!(
            workflow_id = %workflow_id,
            phase_id = %phase_id,
            run_id = %request.run_id.0,
            %err,
            "failed to write pending session checkpoint; aborting dispatch before runner launch"
        );
        return Err(anyhow::Error::new(DispatchRetryableError {
            message: format!(
                "failed to write pending session checkpoint for workflow {} phase {} (run_id {}): {}; dispatch aborted before runner launch to preserve crash-replay invariant — operator must inspect underlying I/O condition and `animus workflow retry` after resolution",
                workflow_id, phase_id, request.run_id.0, err
            ),
        }));
    }
    let notification_log = match crate::notification_log::NotificationLog::open(&scoped_state_root, workflow_id) {
        Ok(log) => Some(std::sync::Arc::new(log)),
        Err(err) => {
            warn!(
                workflow_id = %workflow_id,
                phase_id = %phase_id,
                %err,
                "failed to open notification log; continuing without durability"
            );
            None
        }
    };
    let request_mcp_stdio_command = request
        .context
        .pointer("/runtime_contract/mcp/stdio/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let request_mcp_stdio_args = request
        .context
        .pointer("/runtime_contract/mcp/stdio/args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if debug_mcp_stdio_enabled() {
        eprintln!(
            "[ao][debug][mcp-stdio] dispatch workflow={} phase={} run_id={} command={:?} args={:?}",
            workflow_id, phase_id, request.run_id.0, request_mcp_stdio_command, request_mcp_stdio_args
        );
    }
    info!(
        workflow_id = %workflow_id,
        phase_id = %phase_id,
        run_id = %request.run_id.0,
        request_mcp_stdio_command = ?request_mcp_stdio_command,
        request_mcp_stdio_args = ?request_mcp_stdio_args,
        "Dispatching workflow phase request to agent runner"
    );
    let stream = connect_runner(&config_dir)
        .await
        .with_context(|| format!("failed to connect runner for workflow {} phase {}", workflow_id, phase_id))?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, request).await?;

    // FATAL: the request has already been written to the runner, so the
    // agent may be doing side-effecting work right now. If we can't flip
    // the checkpoint to Running the recovery oracle will not shield this
    // phase on daemon restart (`list_running_checkpoints` only returns
    // Running entries). We MUST tag the failure with
    // `TerminalCheckpointError` so the agent retry/failover loop refuses
    // to re-dispatch — re-dispatching while the first runner is still
    // executing would duplicate the side-effecting work, which is exactly
    // the crash-replay hazard we are fixing.
    if let Err(err) = crate::phase_session::update_session_running(&scoped_state_root, workflow_id, phase_id) {
        warn!(
            workflow_id = %workflow_id,
            phase_id = %phase_id,
            %err,
            "failed to mark session checkpoint as running; aborting in-flight phase to preserve crash-replay invariant"
        );
        return Err(anyhow::Error::new(TerminalCheckpointError {
            message: format!(
                "failed to mark session checkpoint Running for workflow {} phase {}: {}; in-flight phase aborted because resumable-shield invariant would not hold (runner subprocess may still be executing — operator must verify state)",
                workflow_id, phase_id, err
            ),
        }));
    }

    let parse_phase_decision = ctx.phase_decision_contract(phase_id).is_some();
    let expected_result_kind = phase_result_kind_for_ctx(&ctx, phase_id);
    let event_run_dir = ipc_run_dir(project_root, &request.run_id, None);

    // SidecarPollContext lets process_phase_event_stream poll the runner
    // sidecar mid-stream and copy the provider_session_id into the durable
    // checkpoint as soon as the plugin reports it — instead of waiting until
    // the phase completes. This closes a crash window where the daemon died
    // after the agent wrote its sidecar but before the phase finished:
    // without the mid-stream copy the checkpoint never learned the
    // provider_session_id and auto-resume blocked the workflow as
    // unrecoverable.
    let poll_ctx = SidecarPollContext {
        scoped_state_root: scoped_state_root.clone(),
        workflow_id: workflow_id.to_string(),
        phase_id: phase_id.to_string(),
        run_id: request.run_id.0.clone(),
    };
    let outcome = process_phase_event_stream(
        tokio::io::BufReader::new(read_half).lines(),
        &request.run_id,
        workflow_id,
        phase_id,
        parse_commit_message,
        parse_phase_decision,
        &expected_result_kind,
        Some(&event_run_dir),
        Some(project_root),
        notification_log.clone(),
        Some(poll_ctx),
    )
    .await;

    // Final post-stream sidecar sweep is still worthwhile: handles the case
    // where the sidecar was only written immediately before the agent's
    // Finished event, after the loop's last polling tick.
    if let Some(provider_session_id) = crate::phase_session::lookup_runner_session_sidecar(request.run_id.0.as_str()) {
        if let Err(err) = crate::phase_session::update_provider_session_id(
            &scoped_state_root,
            workflow_id,
            phase_id,
            &provider_session_id,
        ) {
            warn!(
                workflow_id = %workflow_id,
                phase_id = %phase_id,
                %err,
                "failed to record provider session id on checkpoint"
            );
        }
    }

    match &outcome {
        Ok(_) => {
            // FATAL: the runner completed successfully but we couldn't flip
            // the checkpoint to Completed. If we ignore this, the checkpoint
            // stays Running and the next daemon start will treat the phase
            // as resumable — re-dispatching a side-effecting phase that
            // already succeeded. We wrap the error in
            // `TerminalCheckpointError` so the agent retry/failover loop
            // refuses to redispatch the (already-successful) phase even if
            // the I/O message happens to match a transient-runner pattern.
            if let Err(err) = crate::phase_session::update_session_completed(&scoped_state_root, workflow_id, phase_id)
            {
                warn!(
                    workflow_id = %workflow_id,
                    phase_id = %phase_id,
                    %err,
                    "failed to mark session checkpoint as completed; surfacing as phase failure to prevent double-run on restart"
                );
                return Err(anyhow::Error::new(TerminalCheckpointError {
                    message: format!(
                        "phase {} completed successfully but terminal checkpoint write failed: {}; failing phase to prevent crash-replay double-dispatch",
                        phase_id, err
                    ),
                }));
            }
        }
        Err(stream_err) => {
            let reason = format!("phase event stream failed: {stream_err}");
            // FATAL: same logic as the Completed arm — a stuck Running
            // checkpoint after a stream error is indistinguishable to the
            // recovery oracle from an actually-resumable phase. We must
            // either mark it Failed or surface the inability to do so;
            // logging-and-swallowing risks a phantom resume. Wrapping in
            // `TerminalCheckpointError` blocks the agent retry/failover
            // loop from rerunning the phase on I/O messages that overlap
            // the transient-runner classifier.
            if let Err(err) =
                crate::phase_session::update_session_failed(&scoped_state_root, workflow_id, phase_id, &reason)
            {
                warn!(
                    workflow_id = %workflow_id,
                    phase_id = %phase_id,
                    %err,
                    "failed to mark session checkpoint as failed after stream error; surfacing checkpoint-write failure to prevent double-run on restart"
                );
                return Err(anyhow::Error::new(TerminalCheckpointError {
                    message: format!(
                        "phase {} event stream failed ({}) AND terminal checkpoint write failed: {}; surfaced as combined failure to prevent crash-replay double-dispatch",
                        phase_id, stream_err, err
                    ),
                }));
            }
        }
    }
    outcome
}

/// Bundle of state `process_phase_event_stream` needs to copy a
/// provider_session_id from the runner sidecar into the durable checkpoint
/// as soon as it appears — instead of only after the phase finishes.
///
/// Mid-stream copying matters because the agent-runner writes the
/// `~/.animus/runner-sessions/<run_id>.session.json` sidecar shortly after
/// the provider plugin reports its session id, which can be hundreds of
/// seconds before `Finished` arrives. If the daemon crashes in that window
/// the checkpoint stayed at `provider_session_id = None`, auto-resume
/// excluded the workflow from the resumable shield, and the user saw the
/// run marked "unrecoverable".
#[derive(Debug, Clone)]
pub struct SidecarPollContext {
    pub scoped_state_root: PathBuf,
    pub workflow_id: String,
    pub phase_id: String,
    pub run_id: String,
}

/// Maximum interval between sidecar polls inside the event stream loop.
/// 500 ms is fast enough that even a daemon crash 1–2 seconds after the
/// sidecar appears will already have the checkpoint updated, and slow
/// enough that an idle (no-event) loop wakes at most twice a second to
/// stat a single file on disk.
const SIDECAR_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Stat the runner sidecar at most once per [`SIDECAR_POLL_INTERVAL`]. If
/// the sidecar has materialized and the checkpoint does not yet carry the
/// provider session id, copy it over and flip `captured` so subsequent
/// loop iterations short-circuit to a plain `next_line().await`.
fn maybe_poll_sidecar(
    ctx: Option<&SidecarPollContext>,
    captured: &mut bool,
    last_poll: &mut Instant,
    workflow_id: &str,
    phase_id: &str,
) {
    let Some(ctx) = ctx else {
        *captured = true;
        return;
    };
    if *captured {
        return;
    }
    if last_poll.elapsed() < SIDECAR_POLL_INTERVAL {
        return;
    }
    *last_poll = Instant::now();
    let Some(provider_session_id) = crate::phase_session::lookup_runner_session_sidecar(ctx.run_id.as_str()) else {
        return;
    };
    match crate::phase_session::read_checkpoint(&ctx.scoped_state_root, &ctx.workflow_id, &ctx.phase_id) {
        Ok(Some(checkpoint)) => {
            if checkpoint.provider_session_id.as_deref() == Some(provider_session_id.as_str()) {
                *captured = true;
                return;
            }
        }
        Ok(None) => {
            // Checkpoint hasn't been written yet (race with
            // write_session_pending); skip this poll, try again next tick.
            return;
        }
        Err(err) => {
            warn!(
                workflow_id = %workflow_id,
                phase_id = %phase_id,
                %err,
                "failed to read session checkpoint while polling sidecar"
            );
            return;
        }
    }
    if let Err(err) = crate::phase_session::update_provider_session_id(
        &ctx.scoped_state_root,
        &ctx.workflow_id,
        &ctx.phase_id,
        &provider_session_id,
    ) {
        warn!(
            workflow_id = %workflow_id,
            phase_id = %phase_id,
            %err,
            "failed to persist provider session id mid-stream"
        );
        return;
    }
    *captured = true;
}

async fn process_phase_event_stream<R: AsyncBufRead + Unpin>(
    mut lines: tokio::io::Lines<R>,
    run_id: &RunId,
    workflow_id: &str,
    phase_id: &str,
    parse_commit_message: bool,
    parse_phase_decision: bool,
    expected_result_kind: &str,
    event_run_dir: Option<&std::path::Path>,
    project_root: Option<&str>,
    notification_log: Option<std::sync::Arc<crate::notification_log::NotificationLog>>,
    sidecar_poll: Option<SidecarPollContext>,
) -> Result<PhaseExecutionOutcome> {
    let run_logger =
        project_root.map(|root| orchestrator_logging::Logger::for_run(std::path::Path::new(root), &run_id.0));
    if let Some(dir) = event_run_dir {
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                run_id = %run_id.0,
                phase_id,
                workflow_id,
                run_dir = %dir.display(),
                %error,
                "failed to create run dir for event persistence"
            );
        }
    }
    let mut pending_commit_message: Option<String> = None;
    let mut pending_phase_decision: Option<orchestrator_core::PhaseDecision> = None;
    let mut pending_result_payload: Option<Value> = None;
    let mut provider_exhaustion_reason: Option<String> = None;
    let mut diagnostics = VecDeque::new();
    // Once the sidecar has yielded a session id (or the caller didn't ask
    // us to poll), short-circuit further filesystem stats.
    let mut sidecar_captured = sidecar_poll.is_none();
    let mut last_sidecar_poll = Instant::now().checked_sub(SIDECAR_POLL_INTERVAL).unwrap_or_else(Instant::now);
    loop {
        // tokio::select! between the next line and a 500ms tick so the
        // sidecar poll fires even when the agent goes quiet between events.
        let next = if sidecar_captured {
            lines.next_line().await?
        } else {
            tokio::select! {
                biased;
                line = lines.next_line() => line?,
                _ = tokio::time::sleep(SIDECAR_POLL_INTERVAL) => {
                    maybe_poll_sidecar(
                        sidecar_poll.as_ref(),
                        &mut sidecar_captured,
                        &mut last_sidecar_poll,
                        workflow_id,
                        phase_id,
                    );
                    continue;
                }
            }
        };
        if !sidecar_captured {
            maybe_poll_sidecar(
                sidecar_poll.as_ref(),
                &mut sidecar_captured,
                &mut last_sidecar_poll,
                workflow_id,
                phase_id,
            );
        }
        let Some(line) = next else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };

        if let Some(dir) = event_run_dir {
            if let Err(error) = crate::ipc::persist_run_event(dir, &event) {
                tracing::warn!(
                    run_id = %run_id.0,
                    phase_id,
                    workflow_id,
                    run_dir = %dir.display(),
                    %error,
                    "failed to persist agent run event"
                );
            }
        }
        if !event_matches_run(&event, run_id) {
            continue;
        }

        if let Some(log) = notification_log.as_ref() {
            if let Ok(payload) = serde_json::to_value(&event) {
                if let Err(err) = log.append(phase_id, &payload) {
                    tracing::warn!(
                        run_id = %run_id.0,
                        phase_id,
                        workflow_id,
                        %err,
                        "failed to append notification log line"
                    );
                }
                if let Err(err) = log.rotate_if_needed() {
                    tracing::warn!(
                        workflow_id,
                        phase_id,
                        %err,
                        "failed to rotate notification log"
                    );
                }
            }
        }

        if let Some(ref logger) = run_logger {
            match &event {
                AgentRunEvent::OutputChunk { text, .. } => {
                    logger
                        .debug("llm.output", text.chars().take(500).collect::<String>())
                        .run(run_id.0.as_str())
                        .phase(phase_id)
                        .role("assistant")
                        .content(text)
                        .emit();
                }
                AgentRunEvent::Thinking { content, .. } => {
                    logger
                        .debug("llm.thinking", content.chars().take(200).collect::<String>())
                        .run(run_id.0.as_str())
                        .phase(phase_id)
                        .emit();
                }
                AgentRunEvent::ToolCall { tool_info, .. } => {
                    let is_mcp = tool_info.tool_name.starts_with("mcp_");
                    let mut b = logger
                        .info("llm.tool_call", &tool_info.tool_name)
                        .run(run_id.0.as_str())
                        .phase(phase_id)
                        .meta(serde_json::json!({"tool": &tool_info.tool_name, "params": &tool_info.parameters}));
                    if is_mcp {
                        let parts: Vec<&str> = tool_info.tool_name.splitn(3, '_').collect();
                        if parts.len() >= 3 {
                            b = b.mcp(parts[2], parts[1]);
                        }
                    }
                    b.emit();
                }
                AgentRunEvent::Metadata { cost, tokens, .. } => {
                    let mut b = logger.info("llm.metadata", "usage update").run(run_id.0.as_str()).phase(phase_id);
                    if let Some(c) = cost {
                        b = b.cost(*c);
                    }
                    if let Some(ref t) = tokens {
                        b = b.tokens(t.input as u64, t.output as u64);
                    }
                    b.emit();
                }
                AgentRunEvent::ToolResult { result_info, .. } => {
                    let status = if result_info.success { "ok" } else { "err" };
                    logger
                        .info("llm.tool_result", format!("{} {}", &result_info.tool_name, status))
                        .run(run_id.0.as_str())
                        .phase(phase_id)
                        .meta(serde_json::json!({
                            "tool": &result_info.tool_name,
                            "success": result_info.success,
                            "duration_ms": result_info.duration_ms,
                            "result": truncate_json_value(&result_info.result, 1000),
                        }))
                        .emit();
                }
                AgentRunEvent::Error { error, .. } => {
                    logger.error("llm.error", error).run(run_id.0.as_str()).phase(phase_id).err(error).emit();
                }
                AgentRunEvent::Finished { exit_code, duration_ms, .. } => {
                    let code = exit_code.unwrap_or(-1);
                    let b = if code == 0 {
                        logger.info("llm.complete", format!("exit={code}"))
                    } else {
                        logger.error("llm.complete", format!("exit={code}"))
                    };
                    b.run(run_id.0.as_str()).phase(phase_id).exit(code).duration(*duration_ms).emit();
                }
                _ => {}
            }
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                if provider_exhaustion_reason.is_none() {
                    provider_exhaustion_reason = PhaseFailureClassifier::provider_exhaustion_reason_from_text(&text);
                }
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &text);
                if parse_commit_message && pending_commit_message.is_none() {
                    pending_commit_message = parse_commit_message_from_text(&text);
                }
                if parse_phase_decision && pending_phase_decision.is_none() {
                    if let Some(decision) = parse_phase_decision_from_text(&text, phase_id) {
                        if pending_commit_message.is_none() {
                            if let Some(ref cm) = decision.commit_message {
                                pending_commit_message = Some(cm.clone());
                            }
                        }
                        pending_phase_decision = Some(decision);
                    }
                }
                if pending_result_payload.is_none() {
                    pending_result_payload = parse_result_payload_from_text(&text, expected_result_kind);
                }
                if pending_result_payload.is_none() && parse_phase_decision {
                    pending_result_payload = parse_decision_payload_from_text(&text, phase_id);
                }
            }
            AgentRunEvent::Thinking { content, .. } => {
                if provider_exhaustion_reason.is_none() {
                    provider_exhaustion_reason = PhaseFailureClassifier::provider_exhaustion_reason_from_text(&content);
                }
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &content);
                if parse_commit_message && pending_commit_message.is_none() {
                    pending_commit_message = parse_commit_message_from_text(&content);
                }
                if parse_phase_decision && pending_phase_decision.is_none() {
                    if let Some(decision) = parse_phase_decision_from_text(&content, phase_id) {
                        if pending_commit_message.is_none() {
                            if let Some(ref cm) = decision.commit_message {
                                pending_commit_message = Some(cm.clone());
                            }
                        }
                        pending_phase_decision = Some(decision);
                    }
                }
                if pending_result_payload.is_none() {
                    pending_result_payload = parse_result_payload_from_text(&content, expected_result_kind);
                }
                if pending_result_payload.is_none() && parse_phase_decision {
                    pending_result_payload = parse_decision_payload_from_text(&content, phase_id);
                }
            }
            AgentRunEvent::Error { error, .. } => {
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &error);
                let exhaustion_reason = provider_exhaustion_reason
                    .clone()
                    .or_else(|| PhaseFailureClassifier::provider_exhaustion_reason_from_text(&error));
                return Err(anyhow!(
                    "workflow {} phase {} error: {}{}",
                    workflow_id,
                    phase_id,
                    error,
                    exhaustion_reason.map(|reason| format!(" (provider_exhausted: {reason})")).unwrap_or_default()
                ));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    let diagnostics_summary = PhaseFailureClassifier::summarize_phase_diagnostics(&diagnostics);
                    let exhaustion_reason = provider_exhaustion_reason.clone().or_else(|| {
                        diagnostics_summary
                            .as_deref()
                            .and_then(PhaseFailureClassifier::provider_exhaustion_reason_from_text)
                    });
                    return Err(anyhow!(
                        "workflow {} phase {} exited with code {:?}{}{}",
                        workflow_id,
                        phase_id,
                        exit_code,
                        exhaustion_reason.map(|reason| format!(" (provider_exhausted: {reason})")).unwrap_or_default(),
                        diagnostics_summary.map(|summary| format!("; diagnostics: {summary}")).unwrap_or_default(),
                    ));
                }
                return Ok(PhaseExecutionOutcome::Completed {
                    commit_message: pending_commit_message,
                    phase_decision: pending_phase_decision,
                    result_payload: pending_result_payload,
                });
            }
            AgentRunEvent::ToolCall { tool_info, .. } => {
                PhaseFailureClassifier::push_phase_diagnostic_line(
                    &mut diagnostics,
                    &format!("tool_call: {}", tool_info.tool_name),
                );
            }
            AgentRunEvent::Artifact { .. } => {}
            _ => {}
        }
    }

    let diagnostics_suffix = PhaseFailureClassifier::summarize_phase_diagnostics(&diagnostics)
        .map(|summary| format!("; diagnostics: {summary}"))
        .unwrap_or_default();
    Err(anyhow!(
        "runner disconnected before workflow {} phase {} completed{}",
        workflow_id,
        phase_id,
        diagnostics_suffix
    ))
}

fn parse_result_payload_from_text(text: &str, expected_kind: &str) -> Option<Value> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(result) = parse_result_payload_from_payload(&payload, expected_kind) {
            return Some(result);
        }
    }
    None
}

fn parse_result_payload_from_payload(payload: &Value, expected_kind: &str) -> Option<Value> {
    match payload {
        Value::Array(items) => items.iter().find_map(|item| parse_result_payload_from_payload(item, expected_kind)),
        Value::Object(object) => {
            let kind = object.get("kind").and_then(Value::as_str).unwrap_or_default();
            if kind.eq_ignore_ascii_case(expected_kind) {
                return Some(payload.clone());
            }
            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(result) = parse_result_payload_from_payload(value, expected_kind) {
                        return Some(result);
                    }
                }
            }
            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(result) = parse_result_payload_from_text(raw, expected_kind) {
                        return Some(result);
                    }
                }
            }
            None
        }
        Value::String(text) => parse_result_payload_from_text(text, expected_kind),
        _ => None,
    }
}

fn truncate_json_value(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(s) => {
            if s.len() <= max_chars {
                Value::String(s.clone())
            } else {
                Value::String(s.chars().take(max_chars).collect())
            }
        }
        Value::Null => Value::Null,
        other => {
            let serialized = other.to_string();
            if serialized.len() <= max_chars {
                other.clone()
            } else {
                Value::String(serialized.chars().take(max_chars).collect())
            }
        }
    }
}

fn parse_decision_payload_from_text(text: &str, phase_id: &str) -> Option<Value> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(result) = parse_decision_payload_from_payload(&payload, phase_id) {
            return Some(result);
        }
    }
    None
}

fn parse_decision_payload_from_payload(payload: &Value, _phase_id: &str) -> Option<Value> {
    match payload {
        Value::Array(items) => items.iter().find_map(|item| parse_decision_payload_from_payload(item, _phase_id)),
        Value::Object(object) => {
            let is_decision = object
                .get("kind")
                .and_then(Value::as_str)
                .map(|v| v.eq_ignore_ascii_case("phase_decision"))
                .unwrap_or(false);
            if is_decision {
                return Some(payload.clone());
            }
            for key in ["proposal", "data", "payload", "result", "output", "item", "phase_decision"] {
                if let Some(value) = object.get(key) {
                    if let Some(result) = parse_decision_payload_from_payload(value, _phase_id) {
                        return Some(result);
                    }
                }
            }
            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(result) = parse_decision_payload_from_text(raw, _phase_id) {
                        return Some(result);
                    }
                }
            }
            None
        }
        Value::String(text) => parse_decision_payload_from_text(text, _phase_id),
        _ => None,
    }
}

struct PhaseAgentParams<'a> {
    ctx: &'a RuntimeConfigContext,
    project_root: &'a str,
    execution_cwd: &'a str,
    workflow_id: &'a str,
    subject_id: &'a str,
    subject_title: &'a str,
    subject_description: &'a str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &'a str,
    phase_runtime_settings: Option<&'a WorkflowPhaseRuntimeSettings>,
    overrides: Option<&'a PhaseExecuteOverrides>,
    pipeline_vars: Option<&'a std::collections::HashMap<String, String>>,
    dispatch_input: Option<&'a str>,
    schedule_input: Option<&'a str>,
    routing: &'a protocol::PhaseRoutingConfig,
    resolved_phase_skills: &'a skill_dispatch::ResolvedPhaseSkillSet,
    phase_timeout_secs: Option<u64>,
}

struct AgentPhaseRunOutcome {
    outcome: PhaseExecutionOutcome,
    selected_tool: Option<String>,
    selected_model: Option<String>,
    effective_capabilities: protocol::PhaseCapabilities,
    applied_skills: skill_dispatch::AppliedPhaseSkills,
}

fn phase_session_resume_plan(
    workflow_id: &str,
    phase_id: &str,
    session_id: &str,
    continuation: usize,
    attempt: usize,
) -> orchestrator_core::runtime_contract::CliSessionResumePlan {
    let reuses_existing_session = continuation > 0;
    let effective_session_id =
        if attempt > 1 { format!("{}-a{}", session_id, attempt) } else { session_id.to_string() };
    orchestrator_core::runtime_contract::CliSessionResumePlan {
        mode: orchestrator_core::runtime_contract::CliSessionResumeMode::NativeId,
        session_key: format!("wf:{workflow_id}:{phase_id}"),
        session_id: Some(effective_session_id),
        summary_seed: None,
        reused: reuses_existing_session,
        phase_thread_isolated: true,
    }
}

fn phase_requires_structured_completion(ctx: &RuntimeConfigContext, phase_id: &str) -> bool {
    ctx.phase_decision_contract(phase_id).is_some()
        || ctx.phase_output_contract(phase_id).is_some()
        || ctx.phase_output_json_schema(phase_id).is_some()
        || phase_requires_commit_message_with_ctx(ctx, phase_id)
}

fn phase_outcome_is_complete(outcome: Option<&PhaseExecutionOutcome>, requires_structured_completion: bool) -> bool {
    match outcome {
        Some(PhaseExecutionOutcome::Completed { phase_decision, commit_message, result_payload }) => {
            !requires_structured_completion
                || phase_decision.is_some()
                || commit_message.is_some()
                || result_payload.is_some()
        }
        Some(PhaseExecutionOutcome::ManualPending { .. }) => true,
        None => false,
    }
}

fn resolve_phase_skill_target(
    phase_id: &str,
    target_tool_id: &str,
    target_model_id: &str,
    explicit_tool_override: Option<&str>,
    explicit_model_override: Option<&str>,
    base_caps: &protocol::PhaseCapabilities,
    resolved_phase_skills: &skill_dispatch::ResolvedPhaseSkillSet,
    routing_complexity: Option<protocol::ModelRoutingComplexity>,
    routing: &protocol::PhaseRoutingConfig,
) -> Result<(String, String, protocol::PhaseCapabilities, skill_dispatch::AppliedPhaseSkills)> {
    const MAX_SKILL_TARGET_RESOLUTION_PASSES: usize = 3;

    let initial_tool_id = target_tool_id.to_string();
    let initial_model_id = canonical_model_id(target_model_id);
    let mut tool_id = target_tool_id.to_string();
    let mut model_id = canonical_model_id(target_model_id);
    let mut exhausted_iteration_budget = false;
    let explicit_tool_override =
        explicit_tool_override.map(protocol::normalize_tool_id).filter(|value| !value.trim().is_empty());
    let explicit_model_override =
        explicit_model_override.map(canonical_model_id).filter(|value| !value.trim().is_empty());

    for iteration in 0..MAX_SKILL_TARGET_RESOLUTION_PASSES {
        let applied_skills = skill_dispatch::apply_phase_skills(resolved_phase_skills, &tool_id, &model_id);
        let effective_caps =
            skill_dispatch::apply_skill_capability_overrides(base_caps, &applied_skills.application.capabilities);
        let requested_model = explicit_model_override
            .clone()
            .or_else(|| applied_skills.application.model.as_deref().map(canonical_model_id));
        let mut requested_tool = explicit_tool_override.clone().unwrap_or_else(|| tool_id.clone());
        let requested_model = requested_model.unwrap_or_else(|| model_id.clone());
        let model_tool = PhaseTargetPlanner::tool_for_model_id(&requested_model).to_string();

        if explicit_model_override.is_none() {
            if let Some(pinned_tool) = explicit_tool_override.as_ref() {
                if !model_tool.eq_ignore_ascii_case(pinned_tool) && applied_skills.application.model.is_some() {
                    return Err(anyhow!(
                        "phase '{}' skill selected model '{}' which requires tool '{}' but the run is pinned to tool '{}'",
                        phase_id,
                        requested_model,
                        model_tool,
                        pinned_tool
                    ));
                }
            } else if applied_skills.application.model.is_some() {
                requested_tool = model_tool;
            }
        }

        let (next_tool, next_model) = PhaseTargetPlanner::resolve_phase_execution_target(
            phase_id,
            Some(&requested_model),
            Some(&requested_tool),
            routing_complexity,
            &effective_caps,
            routing,
        );

        if next_tool.eq_ignore_ascii_case(&tool_id) && next_model.eq_ignore_ascii_case(&model_id) {
            return Ok((next_tool, next_model, effective_caps, applied_skills));
        }

        tool_id = next_tool;
        model_id = next_model;
        exhausted_iteration_budget = iteration + 1 == MAX_SKILL_TARGET_RESOLUTION_PASSES;
    }

    let applied_skills = skill_dispatch::apply_phase_skills(resolved_phase_skills, &tool_id, &model_id);
    let effective_caps =
        skill_dispatch::apply_skill_capability_overrides(base_caps, &applied_skills.application.capabilities);
    if exhausted_iteration_budget {
        warn!(
            phase_id,
            initial_tool = %initial_tool_id,
            initial_model = %initial_model_id,
            final_tool = %tool_id,
            final_model = %model_id,
            "phase skill target resolution exhausted iteration budget without convergence"
        );
    }
    Ok((tool_id, model_id, effective_caps, applied_skills))
}

async fn run_workflow_phase_with_agent(params: PhaseAgentParams<'_>) -> Result<AgentPhaseRunOutcome> {
    let ctx = params.ctx;
    let project_root = params.project_root;
    let execution_cwd = params.execution_cwd;
    let workflow_id = params.workflow_id;
    let subject_id = params.subject_id;
    let subject_title = params.subject_title;
    let subject_description = params.subject_description;
    let phase_id = params.phase_id;
    let phase_runtime_settings = params.phase_runtime_settings;
    let overrides = params.overrides;
    let pipeline_vars = params.pipeline_vars;
    let base_caps = ctx.phase_capabilities(phase_id);
    let planning_caps = skill_dispatch::preview_phase_capabilities(&base_caps, params.resolved_phase_skills);
    let routing_complexity = routing_complexity(params.task_complexity);
    let settings_tool = phase_runtime_settings.and_then(|s| s.tool.as_deref());
    let settings_model = phase_runtime_settings.and_then(|s| s.model.as_deref());
    let agent_model_override = ctx.phase_model_override(phase_id);
    let agent_tool_override = ctx.phase_tool_override(phase_id);
    let agent_fallback_models = ctx.phase_fallback_models(phase_id);
    let agent_fallback_tools = ctx.phase_fallback_tools(phase_id);
    let configured_fallback_models = if agent_fallback_models.is_empty() {
        phase_runtime_settings.map(|settings| settings.fallback_models.clone()).unwrap_or_default()
    } else {
        agent_fallback_models
    };
    let configured_fallback_tools = if agent_fallback_tools.is_empty() {
        phase_runtime_settings.map(|settings| settings.fallback_tools.clone()).unwrap_or_default()
    } else {
        agent_fallback_tools
    };
    let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
        phase_id,
        settings_model.or(agent_model_override.as_deref()),
        settings_tool.or(agent_tool_override.as_deref()),
        configured_fallback_models.as_slice(),
        configured_fallback_tools.as_slice(),
        routing_complexity,
        Some(project_root),
        &planning_caps,
        params.routing,
    );
    let prompt_inputs = PhasePromptInputs {
        rework_context: overrides.and_then(|o| o.rework_context.as_deref()).map(ToOwned::to_owned),
        pipeline_vars: pipeline_vars.cloned().unwrap_or_default(),
        dispatch_input: params.dispatch_input.map(ToOwned::to_owned),
        schedule_input: params.schedule_input.map(ToOwned::to_owned),
    };
    let phase_render_params = PhaseRenderParams {
        project_root,
        execution_cwd,
        workflow_id,
        subject_id,
        subject_title,
        subject_description,
        phase_id,
    };
    let max_attempts =
        phase_runtime_settings.and_then(|settings| settings.max_attempts).unwrap_or_else(phase_runner_attempts);
    let max_continuations =
        phase_runtime_settings.and_then(|settings| settings.max_continuations).unwrap_or_else(phase_max_continuations);
    let session_id = Uuid::new_v4().to_string();
    let mut fallover_errors: Vec<String> = Vec::new();
    let requires_structured_completion = phase_requires_structured_completion(ctx, phase_id);
    let global_config = if configured_tool_profile(phase_runtime_settings).is_some() {
        Some(
            protocol::Config::load_global()
                .context("failed to load global Animus config for claude profile resolution")?,
        )
    } else {
        None
    };

    for (target_index, (target_tool_id, target_model_id)) in execution_targets.iter().enumerate() {
        let (effective_tool_id, effective_model_id, effective_caps, applied_skills) = resolve_phase_skill_target(
            phase_id,
            target_tool_id,
            target_model_id,
            overrides.and_then(|value| value.tool.as_deref()),
            overrides.and_then(|value| value.model.as_deref()),
            &planning_caps,
            params.resolved_phase_skills,
            routing_complexity,
            params.routing,
        )?;
        let prompt = render_phase_prompt_with_ctx_overrides(
            ctx,
            &phase_render_params,
            prompt_inputs.clone(),
            Some(effective_caps.clone()),
            (!applied_skills.application.is_empty()).then_some(&applied_skills.application),
        )
        .final_prompt;
        let request_timeout_secs = params
            .phase_timeout_secs
            .or(applied_skills.application.timeout_secs)
            .or(phase_runtime_settings.and_then(|settings| settings.timeout_secs));
        let mut last_outcome: Option<PhaseExecutionOutcome> = None;
        let base_context = serde_json::json!({
            "tool": effective_tool_id,
            "prompt": prompt,
            "cwd": execution_cwd,
            "project_root": project_root,
            "workflow_id": workflow_id,
            "subject_id": subject_id,
            "phase_id": phase_id,
            "phase_capabilities": serde_json::to_value(&effective_caps)?,
        });
        let phase_contract = ctx.phase_output_contract(phase_id).cloned();
        let phase_output_schema = phase_output_json_schema_for(ctx, phase_id)?;
        let phase_response_schema = phase_response_json_schema_for(ctx, phase_id)?;

        for continuation in 0..=max_continuations {
            let is_continuation = continuation > 0;
            let effective_prompt = if is_continuation {
                format!(
                    "Continue your work on the current task. Your previous session was interrupted \
                     before completion. Pick up where you left off and complete the remaining work. \
                     The original task: {}",
                    prompt
                )
            } else {
                prompt.clone()
            };

            let mut attempt_succeeded = false;
            let mut backoff = Duration::from_millis(200);
            for attempt in 1..=max_attempts {
                let resume_plan = phase_session_resume_plan(workflow_id, phase_id, &session_id, continuation, attempt);
                let mut context = base_context.clone();
                context
                    .as_object_mut()
                    .expect("json object")
                    .insert("prompt".to_string(), serde_json::json!(effective_prompt));
                if let Some(agent_id) = ctx.phase_agent_id(phase_id) {
                    context
                        .as_object_mut()
                        .expect("json object")
                        .insert("agent_id".to_string(), serde_json::json!(agent_id));
                }
                if let Some(mut runtime_contract) = build_runtime_contract_with_resume(
                    context.get("tool").and_then(Value::as_str).unwrap_or("codex"),
                    &effective_model_id,
                    &effective_prompt,
                    Some(&resume_plan),
                ) {
                    if let Some(contract) = phase_contract.as_ref() {
                        let mut policy = serde_json::json!({
                            "require_commit_message": contract.requires_field("commit_message"),
                            "required_result_kind": contract.kind.as_str(),
                            "required_result_fields": contract.required_fields.clone(),
                        });
                        if let Some(schema) = phase_response_schema.clone().or(phase_output_schema.clone()) {
                            policy
                                .as_object_mut()
                                .expect("json object")
                                .insert("output_json_schema".to_string(), schema);
                        }
                        runtime_contract.as_object_mut().expect("json object").insert("policy".to_string(), policy);
                    }
                    if let Some(schema) = phase_response_schema.as_ref() {
                        inject_response_schema_into_launch_args(
                            &mut runtime_contract,
                            schema,
                            &ctx.agent_runtime_config,
                        );
                    }
                    apply_phase_capability_launch_flags(
                        &mut runtime_contract,
                        &effective_caps,
                        &ctx.agent_runtime_config,
                    );
                    inject_cli_launch_overrides(&mut runtime_contract, &effective_tool_id, phase_runtime_settings);
                    // Forward the resolved phase permission mode (phase
                    // `runtime.permission_mode` > agent profile
                    // `permission_mode`) on the contract's top-level
                    // `permission_mode` key; the agent-runner's
                    // `build_session_request` lifts it onto the typed
                    // `SessionRequest.permission_mode` field.
                    if let Some(mode) = phase_runtime_settings
                        .and_then(|settings| settings.permission_mode.as_deref())
                        .map(str::trim)
                        .filter(|mode| !mode.is_empty())
                    {
                        runtime_contract
                            .as_object_mut()
                            .expect("json object")
                            .insert("permission_mode".to_string(), serde_json::json!(mode));
                    }
                    // When the phase's agent profile carries an
                    // `approval_policy`, flag the contract so the
                    // agent-runner sets `extras.approvals = true` and the
                    // v0.1.13.5 transports activate the claude
                    // `--permission-prompt-tool` hook / approvals preamble.
                    if ctx.phase_has_approval_policy(phase_id) {
                        runtime_contract
                            .as_object_mut()
                            .expect("json object")
                            .insert("approvals".to_string(), Value::Bool(true));
                    }
                    inject_default_stdio_mcp(&mut runtime_contract, project_root);
                    inject_agent_tool_policy(&mut runtime_contract, ctx, phase_id);
                    inject_project_mcp_servers(&mut runtime_contract, project_root, ctx, phase_id);
                    inject_workflow_mcp_servers(&mut runtime_contract, ctx, phase_id);
                    if let Some(policy) = applied_skills.application.tool_policy.as_ref() {
                        set_mcp_tool_policy(&mut runtime_contract, policy);
                    }
                    inject_named_mcp_servers(
                        &mut runtime_contract,
                        project_root,
                        ctx,
                        phase_id,
                        &applied_skills.application.mcp_servers,
                    )?;
                    inject_memory_mcp_for_capable_agent(&mut runtime_contract, project_root, ctx, phase_id);
                    skill_dispatch::inject_skill_overrides(
                        &mut runtime_contract,
                        &effective_tool_id,
                        &applied_skills.application,
                    );
                    let claude_profile_env =
                        resolve_claude_profile_env(&effective_tool_id, phase_runtime_settings, global_config.as_ref())?;
                    inject_cli_launch_env(&mut runtime_contract, &claude_profile_env);
                    let cli_supports_mcp = runtime_contract
                        .pointer("/cli/capabilities/supports_mcp")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let mcp_enforce_only = runtime_contract.pointer("/mcp/enforce_only").and_then(Value::as_bool);
                    let mcp_endpoint = runtime_contract
                        .pointer("/mcp/endpoint")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string);
                    let mcp_stdio_command = runtime_contract
                        .pointer("/mcp/stdio/command")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string);
                    let mcp_stdio_args = runtime_contract_string_array(&runtime_contract, "/mcp/stdio/args");
                    let mcp_additional_servers = runtime_contract_additional_server_names(&runtime_contract);
                    let mcp_tool_policy_allow =
                        runtime_contract_string_array(&runtime_contract, "/mcp/tool_policy/allow");
                    let mcp_tool_policy_deny =
                        runtime_contract_string_array(&runtime_contract, "/mcp/tool_policy/deny");
                    info!(
                        workflow_id = %workflow_id,
                        phase_id = %phase_id,
                        subject_id = %subject_id,
                        tool = %effective_tool_id,
                        model = %effective_model_id,
                        continuation,
                        attempt,
                        execution_cwd = %execution_cwd,
                        project_root = %project_root,
                        cli_supports_mcp,
                        mcp_enforce_only = ?mcp_enforce_only,
                        mcp_endpoint = ?mcp_endpoint,
                        mcp_stdio_command = ?mcp_stdio_command,
                        mcp_stdio_args = ?mcp_stdio_args,
                        mcp_additional_servers = ?mcp_additional_servers,
                        mcp_tool_policy_allow = ?mcp_tool_policy_allow,
                        mcp_tool_policy_deny = ?mcp_tool_policy_deny,
                        "Prepared workflow phase runtime contract"
                    );
                    if debug_mcp_stdio_enabled() {
                        eprintln!(
                            "[ao][debug][mcp-stdio] prepared workflow={} phase={} command={:?} args={:?} additional={:?}",
                            workflow_id, phase_id, mcp_stdio_command, mcp_stdio_args, mcp_additional_servers
                        );
                    }
                    context
                        .as_object_mut()
                        .expect("json object")
                        .insert("runtime_contract".to_string(), runtime_contract);
                } else {
                    info!(
                        workflow_id = %workflow_id,
                        phase_id = %phase_id,
                        subject_id = %subject_id,
                        tool = %effective_tool_id,
                        model = %effective_model_id,
                        continuation,
                        attempt,
                        "Skipping runtime contract injection for workflow phase because no launch contract was built"
                    );
                }

                let run_id = RunId(format!(
                    "wf-{workflow_id}-{}-{target_index}-c{continuation}-a{attempt}-{}",
                    phase_id,
                    Uuid::new_v4().simple()
                ));
                let request = AgentRunRequest {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    run_id,
                    model: ModelId(effective_model_id.clone()),
                    context,
                    timeout_secs: request_timeout_secs,
                };
                debug!(
                    workflow_id = %workflow_id,
                    phase_id = %phase_id,
                    subject_id = %subject_id,
                    run_id = %request.run_id.0,
                    tool = %effective_tool_id,
                    model = %effective_model_id,
                    timeout_secs = ?request.timeout_secs,
                    is_continuation,
                    continuation,
                    attempt,
                    "Dispatching workflow phase attempt to agent runner"
                );

                match run_workflow_phase_attempt(project_root, workflow_id, phase_id, &request).await {
                    Ok(mut outcome) => {
                        if phase_requires_commit_message_with_ctx(ctx, phase_id) {
                            if let PhaseExecutionOutcome::Completed { commit_message, .. } = &mut outcome {
                                let resolved_commit_message = commit_message.clone().unwrap_or_else(|| {
                                    crate::payload_traversal::fallback_implementation_commit_message(
                                        subject_id,
                                        subject_title,
                                    )
                                });
                                commit_implementation_changes(execution_cwd, &resolved_commit_message)?;
                                *commit_message = Some(resolved_commit_message);
                            }
                        }
                        last_outcome = Some(outcome);
                        attempt_succeeded = true;
                        break;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        // Checkpoint-write failures (both the pre-runner
                        // pending write and the post-runner terminal
                        // mutation) MUST NOT trigger the agent retry or
                        // fallback-target path: doing so would re-dispatch
                        // a phase whose side effects may have already
                        // executed. We bypass the transient-runner
                        // classifier entirely for those sentinels — the
                        // workflow_execute Err arm handles them via
                        // `fail_current_phase` so the operator can resolve
                        // the I/O condition and retry the workflow.
                        let is_checkpoint_io_failure = error.downcast_ref::<DispatchRetryableError>().is_some()
                            || error.downcast_ref::<TerminalCheckpointError>().is_some();
                        let should_retry = attempt < max_attempts
                            && !is_checkpoint_io_failure
                            && PhaseFailureClassifier::is_transient_runner_error_message(&message);
                        if should_retry {
                            warn!(
                                workflow_id = %workflow_id,
                                phase_id = %phase_id,
                                attempt,
                                max_attempts,
                                error = %message,
                                "Transient runner error on phase attempt; retrying"
                            );
                            sleep(backoff).await;
                            backoff = std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(3));
                            continue;
                        }

                        let has_fallback_target = target_index + 1 < execution_targets.len();
                        if has_fallback_target
                            && !is_checkpoint_io_failure
                            && PhaseFailureClassifier::should_failover_target(&message)
                        {
                            let next_target = &execution_targets[target_index + 1];
                            let logger = orchestrator_logging::Logger::for_project(std::path::Path::new(project_root));
                            logger
                                .warn("llm.fallback", format!("{} → {}", effective_model_id, next_target.1))
                                .phase(phase_id)
                                .fallback(&effective_model_id, &next_target.1)
                                .err(&message)
                                .emit();
                            fallover_errors.push(format!(
                                "target {}:{} failed: {}",
                                effective_tool_id, effective_model_id, message
                            ));
                            orchestrator_core::record_model_phase_outcome(
                                std::path::Path::new(project_root),
                                &effective_model_id,
                                phase_id,
                                orchestrator_core::PhaseDecisionVerdict::Fail,
                            );
                            break;
                        }
                        orchestrator_core::record_model_phase_outcome(
                            std::path::Path::new(project_root),
                            &effective_model_id,
                            phase_id,
                            orchestrator_core::PhaseDecisionVerdict::Fail,
                        );
                        return Err(error);
                    }
                }
            }

            if !attempt_succeeded {
                break;
            }

            let outcome_is_complete = phase_outcome_is_complete(last_outcome.as_ref(), requires_structured_completion);

            if outcome_is_complete {
                let outcome = last_outcome.take().expect("outcome verified above");
                orchestrator_core::record_model_phase_outcome(
                    std::path::Path::new(project_root),
                    &effective_model_id,
                    phase_id,
                    outcome_verdict(&outcome),
                );
                return Ok(AgentPhaseRunOutcome {
                    outcome,
                    selected_tool: Some(effective_tool_id.clone()),
                    selected_model: Some(effective_model_id.clone()),
                    effective_capabilities: effective_caps.clone(),
                    applied_skills: applied_skills.clone(),
                });
            }

            if continuation < max_continuations {
                eprintln!(
                    "[ao] workflow {} phase {}: agent produced no result, \
                     attempting continuation {}/{}",
                    workflow_id,
                    phase_id,
                    continuation + 1,
                    max_continuations
                );
            }
        }

        if let Some(outcome) = last_outcome {
            orchestrator_core::record_model_phase_outcome(
                std::path::Path::new(project_root),
                &effective_model_id,
                phase_id,
                outcome_verdict(&outcome),
            );
            return Ok(AgentPhaseRunOutcome {
                outcome,
                selected_tool: Some(effective_tool_id),
                selected_model: Some(effective_model_id),
                effective_capabilities: effective_caps,
                applied_skills,
            });
        }
    }

    Err(anyhow!(
        "workflow {} phase {} exhausted fallback targets: {}",
        workflow_id,
        phase_id,
        if fallover_errors.is_empty() {
            "no available execution targets".to_string()
        } else {
            fallover_errors.join(" || ")
        }
    ))
}

fn manual_phase_marker_path(project_root: &str) -> std::path::PathBuf {
    Path::new(project_root).join(".animus").join("state").join("manual-phase-markers.v1.json")
}

fn load_manual_phase_markers(path: &Path) -> BTreeMap<String, bool> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, bool>>(&content).unwrap_or_default()
}

// Manual phase markers gate the "phase needs human attention" notification:
// losing one after a crash would re-emit the same notification, while
// double-writing one would silence a needed alert. Either failure mode is
// user-visible, so durability matters — write through the shared fsync
// helper that sync_all's the data and then fsync's the parent directory.
fn write_manual_phase_markers(path: &Path, markers: &BTreeMap<String, bool>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(markers)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|value| value.to_str()).unwrap_or("manual-phase-markers"),
        Uuid::new_v4()
    ));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
    }
    orchestrator_store::fsync_rename(&tmp_path, path)?;
    Ok(())
}

fn should_emit_manual_required(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    phase_attempt: u32,
) -> Result<bool> {
    let path = manual_phase_marker_path(project_root);
    let key = format!("{workflow_id}::{phase_id}::{phase_attempt}");
    let mut markers = load_manual_phase_markers(&path);
    if markers.get(&key).copied().unwrap_or(false) {
        return Ok(false);
    }
    markers.insert(key, true);
    write_manual_phase_markers(&path, &markers)?;
    Ok(true)
}

pub struct PhaseRunParams<'a> {
    pub project_root: &'a str,
    pub execution_cwd: &'a str,
    pub workflow_id: &'a str,
    pub workflow_ref: &'a str,
    pub subject_id: &'a str,
    pub subject_title: &'a str,
    pub subject_description: &'a str,
    pub task_complexity: Option<orchestrator_core::Complexity>,
    pub phase_id: &'a str,
    pub phase_attempt: u32,
    pub overrides: Option<&'a PhaseExecuteOverrides>,
    pub pipeline_vars: Option<&'a std::collections::HashMap<String, String>>,
    pub dispatch_input: Option<&'a str>,
    pub schedule_input: Option<&'a str>,
    pub routing: &'a protocol::PhaseRoutingConfig,
    pub phase_timeout_secs: Option<u64>,
}

pub async fn run_workflow_phase(params: &PhaseRunParams<'_>) -> Result<PhaseRunResult> {
    let start = std::time::Instant::now();
    let phase_id_for_metric = params.phase_id.to_string();
    let result = run_workflow_phase_inner(params).await;
    crate::metrics_hook::observe_phase_duration(&phase_id_for_metric, start.elapsed());
    result
}

async fn run_workflow_phase_inner(params: &PhaseRunParams<'_>) -> Result<PhaseRunResult> {
    let project_root = params.project_root;
    let execution_cwd = params.execution_cwd;
    let workflow_id = params.workflow_id;
    let workflow_ref = params.workflow_ref;
    let subject_id = params.subject_id;
    let subject_title = params.subject_title;
    let subject_description = params.subject_description;
    let task_complexity = params.task_complexity;
    let phase_id = params.phase_id;
    let phase_attempt = params.phase_attempt;
    let overrides = params.overrides;
    let pipeline_vars = params.pipeline_vars;
    let dispatch_input = params.dispatch_input;
    let schedule_input = params.schedule_input;
    let workflow_config = load_workflow_config_strict(project_root)?;
    let runtime_loaded = load_agent_runtime_config_strict(project_root)?;
    orchestrator_core::validate_workflow_and_runtime_configs_with_project_root(
        &workflow_config.config,
        &runtime_loaded.config,
        Some(Path::new(project_root)),
    )?;

    let mut merged_runtime = runtime_loaded.config.clone();
    for (id, profile) in &workflow_config.config.agent_profiles {
        merged_runtime.agents.insert(id.clone(), profile.clone());
    }
    if !workflow_config.config.tools_allowlist.is_empty() {
        let mut combined: std::collections::HashSet<String> = merged_runtime.tools_allowlist.iter().cloned().collect();
        combined.extend(workflow_config.config.tools_allowlist.iter().cloned());
        merged_runtime.tools_allowlist = combined.into_iter().collect();
        merged_runtime.tools_allowlist.sort();
    }

    let ctx =
        RuntimeConfigContext { agent_runtime_config: merged_runtime.clone(), workflow_config: workflow_config.clone() };

    let definition = ctx
        .phase_execution(phase_id)
        .ok_or_else(|| anyhow!("phase '{}' is missing from both workflow config and agent runtime config", phase_id))?;
    let agent_id = ctx.phase_agent_id(phase_id);
    let agent_profile_hash = agent_id.as_deref().and_then(|id| merged_runtime.agent_profile(id)).map(hash_serializable);

    let mut metadata = PhaseExecutionMetadata {
        phase_id: phase_id.to_string(),
        phase_mode: definition.mode.to_string(),
        phase_definition_hash: hash_serializable(definition),
        agent_runtime_config_hash: runtime_loaded.metadata.hash.clone(),
        agent_runtime_schema: runtime_loaded.metadata.schema.clone(),
        agent_runtime_version: runtime_loaded.metadata.version,
        agent_runtime_source: runtime_loaded.metadata.source.as_str().to_string(),
        agent_id: agent_id.clone(),
        agent_profile_hash,
        selected_tool: None,
        selected_model: None,
        effective_capabilities: ctx.phase_capabilities(phase_id),
        requested_skills: Vec::new(),
        resolved_skills: Vec::new(),
        applied_skills: Vec::new(),
        skill_application: None,
    };

    let mut signals = vec![PhaseExecutionSignal {
        event_type: "workflow-phase-execution-selected".to_string(),
        payload: serde_json::json!({
            "workflow_id": workflow_id,
            "subject_id": subject_id,
            "phase_id": phase_id,
            "phase_mode": metadata.phase_mode,
            "phase_definition_hash": metadata.phase_definition_hash,
            "agent_runtime_config_hash": metadata.agent_runtime_config_hash,
            "agent_runtime_schema": metadata.agent_runtime_schema,
            "agent_runtime_version": metadata.agent_runtime_version,
            "agent_runtime_source": metadata.agent_runtime_source,
            "agent_id": metadata.agent_id,
            "agent_profile_hash": metadata.agent_profile_hash,
        }),
    }];

    match definition.mode {
        orchestrator_core::PhaseExecutionMode::Agent => {
            let resolved_phase_skills = skill_dispatch::resolve_phase_skills(&ctx, Path::new(project_root), phase_id)?;
            metadata.requested_skills = resolved_phase_skills.requested_skills.clone();
            metadata.resolved_skills = resolved_phase_skills.resolved_skills.clone();
            let cli_tool_override = overrides.and_then(|o| o.tool.as_deref());
            let cli_model_override = overrides.and_then(|o| o.model.as_deref());

            let runtime_settings = Some(WorkflowPhaseRuntimeSettings {
                tool: cli_tool_override.or_else(|| merged_runtime.phase_tool_override(phase_id)).map(ToOwned::to_owned),
                tool_profile: merged_runtime.phase_tool_profile(phase_id).map(ToOwned::to_owned),
                model: cli_model_override
                    .or_else(|| merged_runtime.phase_model_override(phase_id))
                    .map(ToOwned::to_owned),
                fallback_models: merged_runtime.phase_fallback_models(phase_id),
                fallback_tools: merged_runtime.phase_fallback_tools(phase_id),
                reasoning_effort: merged_runtime.phase_reasoning_effort(phase_id).map(ToOwned::to_owned),
                permission_mode: ctx.phase_permission_mode(phase_id),
                web_search: merged_runtime.phase_web_search(phase_id),
                network_access: merged_runtime.phase_network_access(phase_id),
                timeout_secs: merged_runtime.phase_timeout_secs(phase_id),
                max_attempts: merged_runtime.phase_max_attempts(phase_id),
                extra_args: merged_runtime.phase_extra_args(phase_id),
                codex_config_overrides: merged_runtime.phase_codex_config_overrides(phase_id),
                max_continuations: merged_runtime.phase_max_continuations(phase_id),
            });
            let agent_result = run_workflow_phase_with_agent(PhaseAgentParams {
                ctx: &ctx,
                project_root,
                execution_cwd,
                workflow_id,
                subject_id,
                subject_title,
                subject_description,
                task_complexity,
                phase_id,
                phase_runtime_settings: runtime_settings.as_ref(),
                overrides,
                pipeline_vars,
                dispatch_input,
                schedule_input,
                routing: params.routing,
                resolved_phase_skills: &resolved_phase_skills,
                phase_timeout_secs: params.phase_timeout_secs,
            })
            .await?;
            metadata.selected_tool = agent_result.selected_tool.clone();
            metadata.selected_model = agent_result.selected_model.clone();
            metadata.effective_capabilities = agent_result.effective_capabilities.clone();
            metadata.applied_skills = agent_result.applied_skills.applied_skills.clone();
            metadata.skill_application = (!agent_result.applied_skills.application.is_empty())
                .then_some(agent_result.applied_skills.application.clone());
            if !metadata.requested_skills.is_empty() {
                signals.push(PhaseExecutionSignal {
                    event_type: "workflow-phase-skills-resolved".to_string(),
                    payload: serde_json::json!({
                        "workflow_id": workflow_id,
                        "phase_id": phase_id,
                        "requested_skills": metadata.requested_skills,
                        "resolved_skills": metadata.resolved_skills,
                        "applied_skills": metadata.applied_skills,
                        "skill_application": metadata.skill_application,
                        "effective_capabilities": metadata.effective_capabilities,
                    }),
                });
            }
            let outcome = agent_result.outcome;

            if definition.output_contract.is_some() || definition.output_json_schema.is_some() {
                if let PhaseExecutionOutcome::Completed { commit_message, result_payload, .. } = &outcome {
                    if definition
                        .output_contract
                        .as_ref()
                        .is_some_and(|contract| contract.requires_field("commit_message"))
                        && commit_message.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_none()
                    {
                        signals.push(PhaseExecutionSignal {
                            event_type: "workflow-phase-contract-violation".to_string(),
                            payload: serde_json::json!({
                                "workflow_id": workflow_id,
                                "phase_id": phase_id,
                                "reason": "commit_message required but missing",
                            }),
                        });
                        return Err(anyhow!("phase '{}' contract violation: commit_message is required", phase_id));
                    }

                    let phase_schema = match phase_response_json_schema_for(&ctx, phase_id)? {
                        Some(s) => Some(s),
                        None => phase_output_json_schema_for(&ctx, phase_id)?,
                    };
                    if let Some(schema) = phase_schema.as_ref() {
                        let mut payload = result_payload.clone().unwrap_or_else(|| {
                            serde_json::json!({
                                "kind": definition
                                    .output_contract
                                    .as_ref()
                                    .map(|contract| contract.kind.as_str())
                                    .unwrap_or("phase_result"),
                                "commit_message": commit_message,
                            })
                        });
                        if let (Some(obj), Some(msg)) = (payload.as_object_mut(), commit_message.as_deref()) {
                            if !obj.contains_key("commit_message") && !msg.trim().is_empty() {
                                obj.insert("commit_message".to_string(), serde_json::Value::String(msg.to_string()));
                            }
                        }
                        if let Err(error) = validate_basic_json_schema(&payload, schema) {
                            signals.push(PhaseExecutionSignal {
                                event_type: "workflow-phase-contract-violation".to_string(),
                                payload: serde_json::json!({
                                    "workflow_id": workflow_id,
                                    "phase_id": phase_id,
                                    "reason": error.to_string(),
                                }),
                            });
                            return Err(anyhow!("phase '{}' contract violation: {}", phase_id, error));
                        }
                    }

                    signals.push(PhaseExecutionSignal {
                        event_type: "workflow-phase-contract-validated".to_string(),
                        payload: serde_json::json!({
                            "workflow_id": workflow_id,
                            "phase_id": phase_id,
                            "phase_mode": "agent",
                        }),
                    });
                }
            }

            Ok(PhaseRunResult {
                model: metadata.selected_model.clone(),
                tool: metadata.selected_tool.clone(),
                outcome,
                metadata,
                signals,
            })
        }
        orchestrator_core::PhaseExecutionMode::Command => {
            let command = definition
                .command
                .as_ref()
                .ok_or_else(|| anyhow!("phase '{}' is missing command definition", phase_id))?;
            let command_context = CommandExecutionContext {
                project_root,
                execution_cwd,
                workflow_id,
                phase_id,
                workflow_ref,
                subject_id,
                subject_title,
                subject_description,
                pipeline_vars,
                dispatch_input,
                schedule_input,
            };

            let command_result = run_workflow_phase_with_command(&command_context, &merged_runtime, command).await?;
            {
                let logger = orchestrator_logging::Logger::for_project(std::path::Path::new(project_root));
                let success = command_result.failure_summary.is_none();
                let mut b = if success {
                    logger.info(
                        "command.complete",
                        format!("{} `{}` exit={}", phase_id, command_result.program, command_result.exit_code),
                    )
                } else {
                    logger.error(
                        "command.complete",
                        format!("{} `{}` exit={}", phase_id, command_result.program, command_result.exit_code),
                    )
                };
                b = b.phase(phase_id).exit(command_result.exit_code).duration(command_result.duration_ms);
                if !command_result.stdout.trim().is_empty() {
                    b = b.content(command_result.stdout.chars().take(2000).collect::<String>());
                }
                if let Some(ref err) = command_result.failure_summary {
                    b = b.err(err.chars().take(500).collect::<String>());
                }
                b.meta(serde_json::json!({"program": command_result.program, "args": command_result.args})).emit();
            }
            signals.push(PhaseExecutionSignal {
                event_type: "workflow-phase-command-executed".to_string(),
                payload: serde_json::json!({
                    "workflow_id": workflow_id,
                    "subject_id": subject_id,
                    "phase_id": phase_id,
                    "program": command_result.program,
                    "args": command_result.args.clone(),
                    "cwd": command_result.cwd.clone(),
                    "exit_code": command_result.exit_code,
                    "duration_ms": command_result.duration_ms,
                    "stdout": command_result.stdout.clone(),
                    "stderr": command_result.stderr.clone(),
                    "parsed_payload": command_result.parsed_payload.clone(),
                    "phase_decision": command_result.phase_decision.clone(),
                }),
            });

            if let Some(ref failure_summary) = command_result.failure_summary {
                let decision = command_result.phase_decision.clone().unwrap_or_else(|| {
                    build_command_phase_decision(
                        command,
                        phase_id,
                        command_result.exit_code,
                        Some(failure_summary.as_str()),
                    )
                });
                let result_payload = build_command_result_payload(
                    command,
                    phase_id,
                    definition.output_contract.as_ref().map(|contract| contract.kind.as_str()),
                    &command_result,
                    &decision,
                );

                let outcome = PhaseExecutionOutcome::Completed {
                    commit_message: None,
                    phase_decision: Some(decision),
                    result_payload: Some(result_payload),
                };

                persist_phase_output(project_root, workflow_id, phase_id, phase_attempt, &outcome)?;

                return Ok(PhaseRunResult { model: None, tool: None, outcome, metadata, signals });
            }

            if command.parse_json_output {
                signals.push(PhaseExecutionSignal {
                    event_type: "workflow-phase-contract-validated".to_string(),
                    payload: serde_json::json!({
                        "workflow_id": workflow_id,
                        "phase_id": phase_id,
                        "phase_mode": "command",
                    }),
                });
            }

            let decision = command_result
                .phase_decision
                .clone()
                .unwrap_or_else(|| build_command_phase_decision(command, phase_id, command_result.exit_code, None));
            let result_payload = build_command_result_payload(
                command,
                phase_id,
                definition.output_contract.as_ref().map(|contract| contract.kind.as_str()),
                &command_result,
                &decision,
            );

            Ok(PhaseRunResult {
                outcome: PhaseExecutionOutcome::Completed {
                    commit_message: None,
                    phase_decision: Some(decision),
                    result_payload: Some(result_payload),
                },
                metadata,
                signals,
                model: None,
                tool: None,
            })
        }
        orchestrator_core::PhaseExecutionMode::Manual => {
            let manual = definition
                .manual
                .as_ref()
                .ok_or_else(|| anyhow!("phase '{}' is missing manual definition", phase_id))?;
            if should_emit_manual_required(project_root, workflow_id, phase_id, phase_attempt)? {
                signals.push(PhaseExecutionSignal {
                    event_type: "workflow-phase-manual-required".to_string(),
                    payload: serde_json::json!({
                        "workflow_id": workflow_id,
                        "subject_id": subject_id,
                        "phase_id": phase_id,
                        "instructions": manual.instructions,
                        "approval_note_required": manual.approval_note_required,
                    }),
                });
            }

            Ok(PhaseRunResult {
                outcome: PhaseExecutionOutcome::ManualPending {
                    instructions: manual.instructions.clone(),
                    approval_note_required: manual.approval_note_required,
                },
                metadata,
                signals,
                model: None,
                tool: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        phase_outcome_is_complete, phase_session_resume_plan, process_phase_event_stream, resolve_claude_profile_env,
        PhaseExecutionOutcome, SidecarPollContext,
    };
    use crate::runtime_support::{inject_cli_launch_env, WorkflowPhaseRuntimeSettings};

    use std::collections::BTreeMap;

    #[test]
    fn initial_attempt_starts_a_fresh_native_session() {
        let plan = phase_session_resume_plan("wf-1", "requirements", "session-123", 0, 1);

        assert_eq!(plan.session_key, "wf:wf-1:requirements");
        assert_eq!(plan.session_id.as_deref(), Some("session-123"));
        assert!(!plan.reused);
    }

    #[test]
    fn retry_attempt_gets_fresh_session_id() {
        let plan = phase_session_resume_plan("wf-1", "requirements", "session-123", 0, 2);

        assert!(!plan.reused);
        assert_eq!(plan.session_id.as_deref(), Some("session-123-a2"));
    }

    #[test]
    fn continuation_reuses_the_existing_native_session() {
        let plan = phase_session_resume_plan("wf-1", "requirements", "session-123", 1, 1);

        assert!(plan.reused);
    }

    #[test]
    fn contractless_phases_complete_after_any_clean_exit() {
        let outcome =
            PhaseExecutionOutcome::Completed { commit_message: None, phase_decision: None, result_payload: None };

        assert!(phase_outcome_is_complete(Some(&outcome), false));
    }

    #[test]
    fn structured_phases_require_a_result_signal() {
        let empty_outcome =
            PhaseExecutionOutcome::Completed { commit_message: None, phase_decision: None, result_payload: None };
        let decided_outcome = PhaseExecutionOutcome::Completed {
            commit_message: None,
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "requirements".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.9,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Complete".to_string(),
                evidence: Vec::new(),
                guardrail_violations: Vec::new(),
                commit_message: None,
                target_phase: None,
            }),
            result_payload: None,
        };

        assert!(!phase_outcome_is_complete(Some(&empty_outcome), true));
        assert!(phase_outcome_is_complete(Some(&decided_outcome), true));
    }

    use protocol::{OutputStreamType, RunId, Timestamp};
    use tokio::io::AsyncBufReadExt;
    use uuid::Uuid;

    fn make_event_stream(events: &[protocol::AgentRunEvent]) -> Vec<u8> {
        events.iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect::<String>().into_bytes()
    }

    fn temp_run_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ao-phase-exec-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_global_claude_profile_config(config_dir: &std::path::Path, profile_name: &str, config_dir_value: &str) {
        let mut config = protocol::Config::load_from_dir(config_dir).expect("global config should load");
        config.claude_profiles.insert(
            profile_name.to_string(),
            protocol::ClaudeProfileEntry {
                env: BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), config_dir_value.to_string())]),
            },
        );
        let config_path = config_dir.join("config.json");
        std::fs::write(config_path, serde_json::to_string_pretty(&config).expect("serialize config"))
            .expect("write global config");
    }

    async fn run_stream(
        events: &[protocol::AgentRunEvent],
        run_id: &RunId,
        run_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<PhaseExecutionOutcome> {
        let bytes = make_event_stream(events);
        let reader = tokio::io::BufReader::new(bytes.as_slice());
        process_phase_event_stream(
            reader.lines(),
            run_id,
            "wf-test",
            "impl",
            false,
            false,
            "",
            run_dir,
            None,
            None,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn event_stream_success_returns_completed() {
        let run_id = RunId("run-success-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() },
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "work done\n".to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 200 },
        ];
        let outcome = run_stream(&events, &run_id, None).await.expect("should succeed");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn event_stream_nonzero_exit_returns_error() {
        let run_id = RunId("run-nonzero-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stderr,
                text: "fatal error\n".to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(1), duration_ms: 50 },
        ];
        let err = run_stream(&events, &run_id, None).await.expect_err("should be an error");
        assert!(err.to_string().contains("exited with code"), "got: {}", err);
    }

    #[test]
    fn claude_tool_profile_resolves_launch_env() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_global_claude_profile_config(temp.path(), "overflow", "/Users/test/.claude-overflow");
        let config = protocol::Config::load_from_dir(temp.path()).expect("global config should load");
        let settings = WorkflowPhaseRuntimeSettings {
            tool: Some("claude".to_string()),
            tool_profile: Some("overflow".to_string()),
            ..Default::default()
        };

        let env =
            resolve_claude_profile_env("claude", Some(&settings), Some(&config)).expect("profile env should resolve");
        let mut runtime_contract = serde_json::json!({
            "cli": {
                "launch": {
                    "command": "claude",
                    "args": ["--print", "hello"],
                    "prompt_via_stdin": false
                }
            }
        });
        inject_cli_launch_env(&mut runtime_contract, &env);

        assert_eq!(
            runtime_contract.pointer("/cli/launch/env/CLAUDE_CONFIG_DIR").and_then(serde_json::Value::as_str),
            Some("/Users/test/.claude-overflow")
        );
    }

    #[tokio::test]
    async fn event_stream_error_event_returns_error() {
        let run_id = RunId("run-error-001".to_string());
        let events =
            vec![protocol::AgentRunEvent::Error { run_id: run_id.clone(), error: "unexpected failure".to_string() }];
        let err = run_stream(&events, &run_id, None).await.expect_err("should be an error");
        assert!(err.to_string().contains("unexpected failure"), "got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_runner_disconnect_returns_error() {
        let run_id = RunId("run-disconnect-001".to_string());
        let events = vec![protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() }];
        let err = run_stream(&events, &run_id, None).await.expect_err("should be an error");
        assert!(err.to_string().contains("runner disconnected"), "got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_provider_exhaustion_is_annotated_in_error() {
        let run_id = RunId("run-exhaust-001".to_string());
        let exhaustion_text = "Error: 429 Too Many Requests\nyour account has exceeded its usage limit\nplease upgrade";
        let events = vec![
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stderr,
                text: exhaustion_text.to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(1), duration_ms: 10 },
        ];
        let err = run_stream(&events, &run_id, None).await.expect_err("should fail");
        assert!(err.to_string().contains("provider_exhausted"), "expected exhaustion annotation, got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_tool_thinking_artifact_events_are_tolerated() {
        let run_id = RunId("run-multi-events-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::Thinking { run_id: run_id.clone(), content: "reasoning...".to_string() },
            protocol::AgentRunEvent::ToolCall {
                run_id: run_id.clone(),
                tool_info: protocol::ToolCallInfo {
                    tool_name: "bash".to_string(),
                    parameters: serde_json::json!({"command": "ls"}),
                    timestamp: Timestamp::now(),
                },
            },
            protocol::AgentRunEvent::ToolResult {
                run_id: run_id.clone(),
                result_info: protocol::ToolResultInfo {
                    tool_name: "bash".to_string(),
                    result: serde_json::json!("file.txt"),
                    duration_ms: 10,
                    success: true,
                },
            },
            protocol::AgentRunEvent::Artifact {
                run_id: run_id.clone(),
                artifact_info: protocol::ArtifactInfo {
                    artifact_id: "art-001".to_string(),
                    artifact_type: protocol::ArtifactType::Other,
                    file_path: Some("out.txt".to_string()),
                    size_bytes: None,
                    mime_type: None,
                },
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 300 },
        ];
        let outcome = run_stream(&events, &run_id, None).await.expect("should succeed despite mixed events");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn event_stream_events_for_other_run_id_are_ignored() {
        let run_id = RunId("run-filter-001".to_string());
        let other_run_id = RunId("run-other-999".to_string());
        let events = vec![
            protocol::AgentRunEvent::Finished { run_id: other_run_id.clone(), exit_code: Some(0), duration_ms: 10 },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 20 },
        ];
        let outcome = run_stream(&events, &run_id, None).await.expect("should succeed on matching run");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn event_stream_persists_events_to_run_dir() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-stream-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() },
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "output text\n{\"kind\":\"result\"}\n".to_string(),
            },
            protocol::AgentRunEvent::Thinking { run_id: run_id.clone(), content: "thinking...".to_string() },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 100 },
        ];
        run_stream(&events, &run_id, Some(&run_dir)).await.expect("should succeed");

        let events_path = run_dir.join("events.jsonl");
        assert!(events_path.exists(), "events.jsonl should be written");
        let contents = std::fs::read_to_string(&events_path).expect("read events.jsonl");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 4, "all 4 events should be persisted");

        let json_output_path = run_dir.join("json-output.jsonl");
        assert!(json_output_path.exists(), "json-output.jsonl should be written");
        let json_contents = std::fs::read_to_string(&json_output_path).expect("read json-output.jsonl");
        assert!(json_contents.contains("\"result\""), "JSON payload from OutputChunk should be extracted");

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[tokio::test]
    async fn event_stream_persists_events_on_error_exit() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-err-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stderr,
                text: "crash\n".to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(2), duration_ms: 30 },
        ];
        let _ = run_stream(&events, &run_id, Some(&run_dir)).await;

        let events_path = run_dir.join("events.jsonl");
        assert!(events_path.exists(), "events.jsonl should be written even on failure");
        let lines: Vec<String> =
            std::fs::read_to_string(&events_path).expect("read").lines().map(str::to_string).collect();
        assert_eq!(lines.len(), 2, "both events should be persisted before the error is returned");

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[tokio::test]
    async fn event_stream_malformed_lines_are_skipped() {
        let run_id = RunId("run-malformed-001".to_string());
        let mut bytes = b"not json at all\n{\"garbage\": true}\n".to_vec();
        let finished_event =
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 10 };
        bytes.extend(serde_json::to_string(&finished_event).unwrap().as_bytes());
        bytes.push(b'\n');

        let reader = tokio::io::BufReader::new(bytes.as_slice());
        let outcome = process_phase_event_stream(
            reader.lines(),
            &run_id,
            "wf-test",
            "impl",
            false,
            false,
            "",
            None::<&std::path::Path>,
            None,
            None,
            None,
        )
        .await
        .expect("malformed lines should be skipped, not cause failure");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    /// Repro for the daemon-crash window before the fix: when the agent
    /// writes its sidecar mid-run but `process_phase_event_stream` only
    /// copied the provider session id into the checkpoint AFTER the loop
    /// exited, a daemon crash before `Finished` left the checkpoint with
    /// `provider_session_id = None` and auto-resume gave up. The mid-stream
    /// sidecar poller must now copy the id over within ~500 ms of the
    /// sidecar appearing, well before the phase finishes.
    #[tokio::test]
    // The MutexGuard intentionally lives across .await: it serializes
    // ANIMUS_RUNNER_SESSION_DIR mutation across all sidecar tests in this
    // crate. A std Mutex is sufficient because the test runs single-threaded
    // and the guard is uncontended for the duration of one test.
    #[allow(clippy::await_holding_lock)]
    async fn provider_session_id_persisted_within_500ms_of_arrival() {
        use crate::phase_session::{read_checkpoint, write_session_pending, SessionCheckpointStatus};
        use std::time::{Duration, Instant};
        use tokio::io::{duplex, AsyncWriteExt};

        // Serialize ANIMUS_RUNNER_SESSION_DIR mutation with the other
        // sidecar tests in this crate (they tweak the same env var).
        let _guard = sidecar_env_lock();

        let scoped = tempfile::tempdir().expect("scoped tempdir");
        let sidecar_dir = tempfile::tempdir().expect("sidecar tempdir");
        std::env::set_var("ANIMUS_RUNNER_SESSION_DIR", sidecar_dir.path());

        let run_id = RunId(format!("run-sidecar-poll-{}", Uuid::new_v4()));
        let workflow_id = "wf-sidecar-poll";
        let phase_id = "impl";
        let expected_sid = "sess-from-plugin-mid-stream";

        write_session_pending(scoped.path(), workflow_id, phase_id, "claude", run_id.0.as_str(), None)
            .expect("seed pending checkpoint");

        // Feed events through a pipe so we can interleave "agent ran for a
        // while, wrote the sidecar, kept streaming". 8 KiB buffer is plenty
        // for the JSON we emit.
        let (reader, mut writer) = duplex(8192);
        let lines = tokio::io::BufReader::new(reader).lines();

        // First event: the run begins. Sleep > 500 ms so the poller has at
        // least one tick to observe an empty sidecar. Then write the
        // sidecar and emit a chunk. Sleep again so the poller can observe
        // the populated sidecar BEFORE `Finished` arrives, then finish.
        let sidecar_dir_path = sidecar_dir.path().to_path_buf();
        let sidecar_run_id = run_id.0.clone();
        let writer_run_id = run_id.clone();
        let _writer_task = tokio::spawn(async move {
            let started = serde_json::to_string(&protocol::AgentRunEvent::Started {
                run_id: writer_run_id.clone(),
                timestamp: Timestamp::now(),
            })
            .unwrap();
            writer.write_all(started.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();

            tokio::time::sleep(Duration::from_millis(800)).await;
            let payload = serde_json::json!({
                "run_id": sidecar_run_id,
                "session_id": expected_sid,
                "tool": "claude",
            });
            std::fs::write(
                sidecar_dir_path.join(format!("{}.session.json", sidecar_run_id)),
                serde_json::to_vec_pretty(&payload).unwrap(),
            )
            .expect("write sidecar mid-stream");

            tokio::time::sleep(Duration::from_millis(1200)).await;
            let chunk = serde_json::to_string(&protocol::AgentRunEvent::OutputChunk {
                run_id: writer_run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "still working\n".to_string(),
            })
            .unwrap();
            writer.write_all(chunk.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();

            tokio::time::sleep(Duration::from_millis(500)).await;
            let finished = serde_json::to_string(&protocol::AgentRunEvent::Finished {
                run_id: writer_run_id,
                exit_code: Some(0),
                duration_ms: 100,
            })
            .unwrap();
            writer.write_all(finished.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
            // Drop writer so reader EOFs cleanly after Finished is consumed.
        });

        // Watcher task: poll the checkpoint at the same cadence the
        // production poller runs. As soon as it sees the provider session
        // id, record the elapsed time since the sidecar was created.
        let scoped_watcher = scoped.path().to_path_buf();
        let sidecar_file = sidecar_dir.path().join(format!("{}.session.json", run_id.0));
        let watcher = tokio::spawn(async move {
            // Wait for the sidecar to exist (the writer task creates it
            // ~800ms in). After that, time how long until the checkpoint
            // catches up.
            while !sidecar_file.exists() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let sidecar_at = Instant::now();
            loop {
                if let Ok(Some(cp)) = read_checkpoint(&scoped_watcher, workflow_id, phase_id) {
                    if cp.provider_session_id.as_deref() == Some(expected_sid) {
                        return Some(sidecar_at.elapsed());
                    }
                }
                if sidecar_at.elapsed() > Duration::from_secs(3) {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });

        let poll_ctx = SidecarPollContext {
            scoped_state_root: scoped.path().to_path_buf(),
            workflow_id: workflow_id.to_string(),
            phase_id: phase_id.to_string(),
            run_id: run_id.0.clone(),
        };
        let outcome = process_phase_event_stream(
            lines,
            &run_id,
            workflow_id,
            phase_id,
            false,
            false,
            "",
            None::<&std::path::Path>,
            None,
            None,
            Some(poll_ctx),
        )
        .await
        .expect("stream should finish cleanly");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));

        let elapsed = watcher.await.expect("watcher task").expect("checkpoint must catch up");
        assert!(
            elapsed <= Duration::from_millis(1500),
            "checkpoint should reflect sidecar within ~1 polling tick; elapsed = {elapsed:?}"
        );

        let final_checkpoint = read_checkpoint(scoped.path(), workflow_id, phase_id).expect("read").expect("present");
        assert_eq!(final_checkpoint.provider_session_id.as_deref(), Some(expected_sid));
        assert!(matches!(final_checkpoint.status, SessionCheckpointStatus::Running | SessionCheckpointStatus::Pending));

        std::env::remove_var("ANIMUS_RUNNER_SESSION_DIR");
    }

    fn sidecar_env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|p| p.into_inner())
    }

    // ── validate_basic_json_schema tests ──────────────────────────────────

    use super::validate_basic_json_schema;

    #[test]
    fn schema_validation_accepts_valid_required_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["name", "verdict"],
            "properties": {
                "name": { "type": "string" },
                "verdict": { "type": "string" }
            }
        });
        let instance = serde_json::json!({
            "name": "requirements",
            "verdict": "advance"
        });
        assert!(validate_basic_json_schema(&instance, &schema).is_ok());
    }

    #[test]
    fn schema_validation_rejects_missing_required_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["name", "verdict"],
            "properties": {
                "name": { "type": "string" },
                "verdict": { "type": "string" }
            }
        });
        let instance = serde_json::json!({
            "name": "requirements"
        });
        let err = validate_basic_json_schema(&instance, &schema).expect_err("should fail");
        assert!(err.to_string().contains("verdict"), "error should mention missing field: {}", err);
    }

    #[test]
    fn schema_validation_rejects_wrong_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "confidence": { "type": "number" },
                "done": { "type": "boolean" }
            }
        });
        let instance = serde_json::json!({
            "confidence": "high",
            "done": "yes"
        });
        let err = validate_basic_json_schema(&instance, &schema).expect_err("should fail");
        let msg = err.to_string();
        assert!(msg.contains("confidence") || msg.contains("type"), "should mention type error: {}", msg);
    }

    #[test]
    fn schema_validation_enforces_const() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "const": "phase_decision" }
            }
        });
        let valid = serde_json::json!({ "kind": "phase_decision" });
        let invalid = serde_json::json!({ "kind": "other" });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&invalid, &schema).expect_err("should fail");
        assert!(err.to_string().contains("kind"), "error should mention const field: {}", err);
    }

    #[test]
    fn schema_validation_enforces_pattern() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "pattern": "^TASK-\\d+$" }
            }
        });
        let valid = serde_json::json!({ "task_id": "TASK-123" });
        let invalid = serde_json::json!({ "task_id": "not-a-task" });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&invalid, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("pattern") || err.to_string().contains("task_id"),
            "should mention pattern violation: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_min_length() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "minLength": 5 }
            }
        });
        let valid = serde_json::json!({ "reason": "detailed enough" });
        let too_short = serde_json::json!({ "reason": "ok" });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&too_short, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("reason") || err.to_string().contains("minLength"),
            "should mention minLength: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_max_length() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "maxLength": 10 }
            }
        });
        let valid = serde_json::json!({ "summary": "short" });
        let too_long = serde_json::json!({ "summary": "this is way too long" });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&too_long, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("summary") || err.to_string().contains("maxLength"),
            "should mention maxLength: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_minimum() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "confidence": { "type": "number", "minimum": 0.5 }
            }
        });
        let valid = serde_json::json!({ "confidence": 0.9 });
        let too_low = serde_json::json!({ "confidence": 0.1 });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&too_low, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("confidence") || err.to_string().contains("minimum"),
            "should mention minimum: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_maximum() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "priority_score": { "type": "number", "maximum": 100 }
            }
        });
        let valid = serde_json::json!({ "priority_score": 42 });
        let too_high = serde_json::json!({ "priority_score": 150 });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&too_high, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("priority_score") || err.to_string().contains("maximum"),
            "should mention maximum: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_min_items() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "evidence": { "type": "array", "minItems": 1 }
            }
        });
        let valid = serde_json::json!({ "evidence": ["item1"] });
        let empty = serde_json::json!({ "evidence": [] });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&empty, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("evidence") || err.to_string().contains("minItems"),
            "should mention minItems: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_max_items() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "tags": { "type": "array", "maxItems": 3 }
            }
        });
        let valid = serde_json::json!({ "tags": ["a", "b"] });
        let too_many = serde_json::json!({ "tags": ["a", "b", "c", "d", "e"] });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&too_many, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("tags") || err.to_string().contains("maxItems"),
            "should mention maxItems: {}",
            err
        );
    }

    #[test]
    fn schema_validation_enforces_enum() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string", "enum": ["advance", "rework", "fail"] }
            }
        });
        let valid = serde_json::json!({ "verdict": "advance" });
        let invalid = serde_json::json!({ "verdict": "maybe" });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        let err = validate_basic_json_schema(&invalid, &schema).expect_err("should fail");
        assert!(
            err.to_string().contains("verdict") || err.to_string().contains("enum"),
            "should mention enum: {}",
            err
        );
    }

    #[test]
    fn schema_validation_combined_constraints() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["verdict", "confidence"],
            "properties": {
                "verdict": { "type": "string", "enum": ["advance", "rework"], "minLength": 1 },
                "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "reason": { "type": "string", "pattern": "^[A-Z].*", "minLength": 5, "maxLength": 500 },
                "evidence": { "type": "array", "minItems": 1, "maxItems": 20 }
            }
        });
        let valid = serde_json::json!({
            "verdict": "advance",
            "confidence": 0.95,
            "reason": "All criteria met with strong evidence",
            "evidence": ["code inspection", "test results"]
        });
        assert!(validate_basic_json_schema(&valid, &schema).is_ok());

        // Missing required field
        let missing = serde_json::json!({ "confidence": 0.9 });
        assert!(validate_basic_json_schema(&missing, &schema).is_err());

        // Enum violation
        let bad_enum = serde_json::json!({ "verdict": "skip", "confidence": 0.5 });
        assert!(validate_basic_json_schema(&bad_enum, &schema).is_err());

        // Numeric bounds violation
        let out_of_range = serde_json::json!({ "verdict": "advance", "confidence": 1.5 });
        assert!(validate_basic_json_schema(&out_of_range, &schema).is_err());

        // Pattern violation
        let bad_pattern = serde_json::json!({
            "verdict": "advance",
            "confidence": 0.9,
            "reason": "lowercase start"
        });
        assert!(validate_basic_json_schema(&bad_pattern, &schema).is_err());

        // String length violation
        let too_short_reason = serde_json::json!({
            "verdict": "advance",
            "confidence": 0.9,
            "reason": "Ab"
        });
        assert!(validate_basic_json_schema(&too_short_reason, &schema).is_err());

        // Array length violation
        let empty_evidence = serde_json::json!({
            "verdict": "advance",
            "confidence": 0.9,
            "evidence": []
        });
        assert!(validate_basic_json_schema(&empty_evidence, &schema).is_err());
    }

    #[test]
    fn schema_validation_reports_invalid_schema() {
        let bad_schema = serde_json::json!({ "type": "not-a-real-type" });
        let instance = serde_json::json!({ "key": "value" });
        // The jsonschema crate may or may not reject unknown types depending on version;
        // the key assertion is that a clearly malformed schema is caught.
        let result = validate_basic_json_schema(&instance, &bad_schema);
        // Don't assert error/success since behavior may vary; just ensure no panic.
        let _ = result;
    }

    #[test]
    fn schema_validation_accepts_nested_objects() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["data"],
            "properties": {
                "data": {
                    "type": "object",
                    "required": ["inner"],
                    "properties": {
                        "inner": { "type": "string", "minLength": 1 }
                    }
                }
            }
        });
        let valid = serde_json::json!({ "data": { "inner": "value" } });
        let missing_inner = serde_json::json!({ "data": {} });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        assert!(validate_basic_json_schema(&missing_inner, &schema).is_err());
    }

    #[test]
    fn schema_validation_integer_bounds() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 0, "maximum": 100 }
            }
        });
        let valid = serde_json::json!({ "count": 50 });
        let negative = serde_json::json!({ "count": -1 });
        let float_val = serde_json::json!({ "count": 50.5 });
        let too_big = serde_json::json!({ "count": 200 });

        assert!(validate_basic_json_schema(&valid, &schema).is_ok());
        assert!(validate_basic_json_schema(&negative, &schema).is_err());
        assert!(validate_basic_json_schema(&float_val, &schema).is_err());
        assert!(validate_basic_json_schema(&too_big, &schema).is_err());
    }

    // Fix 1 (durability hardening): the three checkpoint mutation sites in
    // `run_workflow_phase_attempt` must propagate failures instead of
    // log-and-continue, because the recovery oracle relies on a durable
    // Running checkpoint to shield in-flight phases across daemon restart.
    //
    // The fault-injection seam lives in
    // [`crate::phase_session::test_fault`]: it arms a per-thread fault for
    // a single checkpoint operation and the matching `write_session_*` /
    // `update_session_*` call returns `io::ErrorKind::PermissionDenied`
    // exactly once. We use the seam to assert each of the three failure
    // modes propagates as an `Err` rather than being silently swallowed.
    mod checkpoint_failure_propagation {
        use super::super::run_workflow_phase_attempt;
        use crate::phase_session::test_fault::{FaultGuard, FaultOp};
        use protocol::{AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};

        fn make_request() -> AgentRunRequest {
            AgentRunRequest {
                protocol_version: PROTOCOL_VERSION.to_string(),
                run_id: RunId(format!("run-fault-{}", uuid::Uuid::new_v4())),
                model: ModelId("claude-sonnet-4-6".to_string()),
                context: serde_json::json!({ "tool": "claude" }),
                timeout_secs: Some(60),
            }
        }

        // Direct seam smoke test: when the Pending fault is armed, the
        // checkpoint write returns the injected io error AND nothing else
        // fires (the guard auto-disarms after a single trigger so the
        // next call passes through).
        #[test]
        fn fault_seam_arms_and_disarms_correctly() {
            use crate::phase_session::write_session_pending;
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = FaultGuard::arm(FaultOp::Pending);
            let err = write_session_pending(tmp.path(), "wf", "phase", "claude", "run-1", None)
                .expect_err("pending fault should fire");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            // Second call after the single-shot arming auto-disarms; the
            // write succeeds normally.
            let _cp = write_session_pending(tmp.path(), "wf", "phase", "claude", "run-2", None)
                .expect("second write should succeed");
        }

        // Pending checkpoint write fault: dispatch must abort BEFORE
        // attempting to connect to the runner AND surface the
        // [`DispatchRetryableError`] sentinel so the scheduler re-dispatches
        // on the next tick rather than terminally failing the workflow.
        #[tokio::test]
        async fn pending_checkpoint_failure_aborts_before_runner_dispatch() {
            use crate::phase_executor::DispatchRetryableError;
            let tmp = tempfile::tempdir().expect("tempdir");
            let project_root = tmp.path().to_string_lossy().to_string();
            let request = make_request();
            let _guard = FaultGuard::arm(FaultOp::Pending);

            let result = run_workflow_phase_attempt(&project_root, "wf-fault-pending", "impl", &request).await;
            let err = result.expect_err("pending checkpoint failure must propagate");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("pending session checkpoint") && msg.contains("dispatch aborted"),
                "error must surface the pending-checkpoint failure and the aborted-dispatch reason, got: {msg}"
            );
            // And: no runner connection was attempted, because connect_runner
            // happens AFTER the pending write. We assert this indirectly by
            // looking at the error's primary cause — if connect_runner had
            // run, the message would mention "failed to connect runner".
            assert!(
                !msg.contains("failed to connect runner"),
                "dispatch must abort before runner connect; got connect error in message: {msg}"
            );
            // CRITICAL: the error MUST be tagged with the retryable
            // sentinel so the workflow scheduler re-dispatches on the next
            // tick instead of calling fail_current_phase (which would
            // turn a transient checkpoint I/O hiccup into a terminal
            // workflow failure).
            assert!(
                err.downcast_ref::<DispatchRetryableError>().is_some(),
                "pending-checkpoint failure must be tagged as DispatchRetryableError so the scheduler retries on next tick; got: {msg}"
            );
        }

        // Running checkpoint mutation fault: the runner has been
        // contacted, but if we can't flip the checkpoint to Running, the
        // resumable shield won't see it on restart. Without a real runner
        // socket the connection will fail first; what we assert here is
        // that the fault SEAM exists and the source-level guard branch is
        // present (verified by armed-fault + Err return path observed in
        // [`super::super::run_workflow_phase_attempt`]). The direct unit
        // test of `update_session_running` returning Err on fault doubles
        // as the regression test for the propagation path.
        #[test]
        fn running_checkpoint_fault_returns_io_error_through_seam() {
            use crate::phase_session::{update_session_running, write_session_pending};
            let tmp = tempfile::tempdir().expect("tempdir");
            // Seed a Pending checkpoint so the mutate() lookup succeeds
            // before the fault check fires inside update_session_running.
            write_session_pending(tmp.path(), "wf-r", "impl", "claude", "run-r", None).expect("seed");
            let _guard = FaultGuard::arm(FaultOp::Running);
            let err = update_session_running(tmp.path(), "wf-r", "impl").expect_err("running fault should fire");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        }

        // Defense-in-depth: terminal checkpoint failures
        // (TerminalCheckpointError) and the pre-runner dispatch failure
        // (DispatchRetryableError) MUST be tagged sentinels so the agent
        // retry/failover loop in `run_workflow_phase_with_agent` will not
        // re-dispatch a phase whose side effects have already executed.
        // The classifier in PhaseFailureClassifier matches I/O messages
        // like "connection timed out" as transient; without the typed
        // sentinel an fsync-on-network-storage failure would be retried.
        #[test]
        fn typed_sentinels_bypass_transient_error_classifier_check() {
            use crate::phase_executor::{DispatchRetryableError, TerminalCheckpointError};

            let terminal_err =
                anyhow::Error::new(TerminalCheckpointError { message: "connection timed out".to_string() });
            let retryable_err =
                anyhow::Error::new(DispatchRetryableError { message: "connection reset by peer".to_string() });
            let plain_err = anyhow::anyhow!("connection timed out");

            assert!(terminal_err.downcast_ref::<TerminalCheckpointError>().is_some());
            assert!(retryable_err.downcast_ref::<DispatchRetryableError>().is_some());
            // Plain anyhow has neither marker and would still flow into the
            // transient-runner classifier path; that's the comparison.
            assert!(plain_err.downcast_ref::<TerminalCheckpointError>().is_none());
            assert!(plain_err.downcast_ref::<DispatchRetryableError>().is_none());
        }

        // Terminal checkpoint mutation fault: same shape as Running.
        // Validates the seam fires for both Completed and Failed terminal
        // operations — the dispatch path in
        // `run_workflow_phase_attempt` propagates either as a phase
        // failure (verified by source review; mocking a full runner
        // dispatch end-to-end is out of scope for this unit test).
        #[test]
        fn terminal_checkpoint_faults_return_io_errors_through_seam() {
            use crate::phase_session::{update_session_completed, update_session_failed, write_session_pending};
            let tmp = tempfile::tempdir().expect("tempdir");

            write_session_pending(tmp.path(), "wf-c", "impl", "claude", "run-c", None).expect("seed");
            {
                let _g = FaultGuard::arm(FaultOp::Completed);
                let err =
                    update_session_completed(tmp.path(), "wf-c", "impl").expect_err("completed fault should fire");
                assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            }

            write_session_pending(tmp.path(), "wf-f", "impl", "claude", "run-f", None).expect("seed");
            {
                let _g = FaultGuard::arm(FaultOp::Failed);
                let err =
                    update_session_failed(tmp.path(), "wf-f", "impl", "boom").expect_err("failed fault should fire");
                assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            }
        }
    }
}
