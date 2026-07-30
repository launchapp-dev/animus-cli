use std::sync::Arc;

use animus_actor::Actor;
use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;

use crate::print_value;
use crate::services::plugin_clients;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result_for_actor;
use animus_workflow_runner_protocol as workflow_proto;

#[derive(Debug)]
pub(crate) struct WorkflowExecuteArgs {
    pub(crate) workflow_id: Option<String>,
    pub(crate) title: Option<String>,
    /// Generic, kind-correct subject dispatch for the BaaS `--subject-id` path.
    /// When set (and not re-attaching to an existing workflow), it is relayed
    /// verbatim on the runner request as `subject_dispatch` — the protocol
    /// requires generic (non-task/requirement) subject backends to use the
    /// dispatch envelope — so the subject resolves under its real kind via
    /// `<kind>/get` instead of being coerced to `task`.
    pub(crate) subject_dispatch: Option<protocol::SubjectDispatch>,
    pub(crate) description: Option<String>,
    pub(crate) workflow_ref: Option<String>,
    pub(crate) phase: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) tool: Option<String>,
    pub(crate) phase_timeout_secs: Option<u64>,
    pub(crate) input_json: Option<String>,
    pub(crate) vars: Vec<String>,
    /// Transport-asserted caller identity, relayed verbatim into the runner
    /// `WorkflowExecuteRequest` so the runner can scope subject/journal/config
    /// plugins to the user.
    ///
    /// TRUST BOUNDARY: this is populated ONLY from an authenticated inbound
    /// control request. Local CLI invocations leave it `None` — the actor is
    /// NEVER synthesized from local context, workflow YAML, agent output, or
    /// subject content.
    pub(crate) actor: Option<Actor>,
}

pub(crate) async fn handle_workflow_execute(
    args: WorkflowExecuteArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    if args.workflow_id.is_some() && !args.vars.is_empty() {
        anyhow::bail!(
            "--var cannot be used with --workflow-id; persisted workflow vars are authoritative for existing workflows"
        );
    }
    let vars = super::parse_workflow_vars(&args.vars)?;

    // Re-attach invocations (`--workflow-id`, including the detached
    // children spawned by the async `workflow run` path) must not mutate
    // daemon lifecycle state: `daemon().start` persists a Running status
    // without a daemon pid, which would make `animus status` report a
    // healthy daemon after the child exits.
    if args.workflow_id.is_none() {
        hub.daemon().start(Default::default()).await?;
    }

    let phase_filter = args.phase.clone();

    // Re-attaching to an existing workflow record: the persisted record is
    // authoritative for subject, input, and vars. Register this process in
    // the workflow-runner pid registry so the daemon's orphan reconciler
    // treats the run as live for as long as we drive the plugin.
    let existing_workflow = match args.workflow_id.as_deref() {
        Some(workflow_id) => Some(hub.workflows().get(workflow_id).await?),
        None => None,
    };
    // Best-effort: a registry write failure must not abort execution —
    // the workflow would otherwise have no driver at all even though the
    // caller was told the runner started. Without the entry the orphan
    // reconciler may cancel a long run, which is the lesser failure mode.
    let _runner_pid_guard = existing_workflow.as_ref().and_then(|workflow| {
        match super::phases::WorkflowRunnerPidGuard::register(project_root, &workflow.id) {
            Ok(guard) => Some(guard),
            Err(error) => {
                tracing::warn!(workflow_id = %workflow.id, error = %error, "failed to register workflow runner pid");
                None
            }
        }
    });

    // v0.5.1 fold-in: route `workflow/execute` exclusively through the
    // installed `workflow_runner` plugin. The in-tree fallback path
    // was removed; daemon preflight enforces plugin presence at
    // startup. When invoked outside the daemon (e.g. `animus workflow
    // execute ...` on a fresh checkout) and no plugin is installed,
    // we surface an actionable error rather than falling through to
    // a runtime that the rest of v0.5.1 no longer exercises.
    let plugin_input_json: Option<serde_json::Value> =
        args.input_json.as_deref().map(serde_json::from_str).transpose()?;
    let plugin_request = if let Some(existing) = existing_workflow.as_ref() {
        let mut request = super::phases::workflow_execute_request_for_existing(existing);
        if let Some(execution) = execution_fence_from_environment(Some(&existing.id))? {
            if let Some(persisted) = request.execution_fence.as_ref() {
                anyhow::ensure!(
                    persisted == &execution,
                    "persisted workflow execution fence does not match daemon spawn authority"
                );
            }
            request.execution_fence = Some(execution);
        }
        if args.workflow_ref.is_some() {
            request.workflow_ref = args.workflow_ref.clone();
        }
        if plugin_input_json.is_some() {
            request.input = plugin_input_json;
        }
        request.model = args.model.clone();
        request.tool = args.tool.clone();
        request.phase_timeout_secs = args.phase_timeout_secs;
        request.phase_filter = phase_filter.clone();
        // Relay the transport-asserted actor (if any) verbatim. The persisted
        // record carries no actor, so this is the only place a re-attach run
        // can carry the caller identity.
        request.actor = args.actor.clone();
        request
    } else {
        workflow_proto::WorkflowExecuteRequest {
            workflow_id: None,
            execution_fence: execution_fence_from_environment(None)?,
            subject_dispatch: args.subject_dispatch.clone(),
            subject_ref: None,
            // task/requirement are ordinary subjects now: a concrete-kind subject
            // arrives as `subject_dispatch` (from `--subject-id`); only a freeform
            // `--title` custom run travels via `title`.
            task_id: None,
            requirement_id: None,
            title: args.title.clone(),
            description: args.description.clone(),
            workflow_ref: args.workflow_ref.clone(),
            input: plugin_input_json,
            vars,
            model: args.model.clone(),
            tool: args.tool.clone(),
            phase_timeout_secs: args.phase_timeout_secs,
            phase_filter: phase_filter.clone(),
            phase_routing: None,
            mcp_config: None,
            // Relayed verbatim from the authenticated control request; `None`
            // for local CLI invocations (never synthesized).
            actor: args.actor.clone(),
        }
    };
    let project_root_path = std::path::Path::new(project_root);
    let plugin_result =
        plugin_clients::call_workflow_execute(project_root_path, &plugin_request).await?.ok_or_else(|| {
            anyhow!(
                "no workflow_runner plugin installed - run `animus plugin install-defaults` (or install \
                 `launchapp-dev/animus-workflow-runner-default`) before invoking `animus workflow execute`"
            )
        })?;

    let parsed_status = match workflow_proto::workflow_status::parse(plugin_result.workflow_status.as_str()) {
        workflow_proto::workflow_status::Parsed::Completed => Some(orchestrator_core::WorkflowStatus::Completed),
        workflow_proto::workflow_status::Parsed::Failed => Some(orchestrator_core::WorkflowStatus::Failed),
        workflow_proto::workflow_status::Parsed::Escalated => Some(orchestrator_core::WorkflowStatus::Escalated),
        workflow_proto::workflow_status::Parsed::Cancelled => Some(orchestrator_core::WorkflowStatus::Cancelled),
        workflow_proto::workflow_status::Parsed::Paused
        | workflow_proto::workflow_status::Parsed::Pending
        | workflow_proto::workflow_status::Parsed::Running
        | workflow_proto::workflow_status::Parsed::Unknown(_) => None,
    };
    if phase_filter.is_none() {
        if let (true, Some(status)) = (existing_workflow.is_some(), parsed_status) {
            // workflow-id-only invocations (detached async-run children,
            // manual `--sync --workflow-id` resumes) carry no bound subject on
            // the args; derive the subject from the persisted record so the
            // subject status projection still lands. Fresh `--subject-id` runs
            // are projected by the workflow_runner plugin that owns the run.
            if let Ok(reloaded) = hub.workflows().get(plugin_result.workflow_id.as_str()).await {
                project_terminal_workflow_result_for_actor(
                    hub.clone(),
                    project_root,
                    reloaded.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
                    Some(reloaded.task_id.as_str()),
                    reloaded.workflow_ref.as_deref(),
                    Some(reloaded.id.as_str()),
                    status,
                    reloaded.failure_reason.as_deref(),
                    args.actor.as_ref(),
                )
                .await;
            }
        }
    }
    if json {
        return print_value(
            serde_json::json!({
                "workflow_id": plugin_result.workflow_id,
                "workflow_ref": plugin_result.workflow_ref,
                "workflow_status": plugin_result.workflow_status,
                "subject_id": plugin_result.subject_id,
                "execution_cwd": plugin_result.execution_cwd,
                "phases_requested": plugin_result.phases_requested,
                "total_duration_secs": plugin_result.total_duration_secs,
                "results": plugin_result.phase_results,
                "post_success": plugin_result.post_success,
                "via": "plugin_host",
            }),
            true,
        );
    }
    Ok(())
}

fn execution_fence_from_environment(
    expected_workflow_id: Option<&str>,
) -> Result<Option<animus_execution_protocol::ExecutionFence>> {
    let Some(raw) = std::env::var_os(orchestrator_daemon_runtime::ANIMUS_EXECUTION_FENCE_JSON_ENV) else {
        return Ok(None);
    };
    let execution: animus_execution_protocol::ExecutionFence =
        serde_json::from_str(&raw.to_string_lossy()).map_err(|error| {
            anyhow!("invalid {}: {error}", orchestrator_daemon_runtime::ANIMUS_EXECUTION_FENCE_JSON_ENV)
        })?;
    execution.validate().map_err(anyhow::Error::msg)?;
    if let Some(expected) = expected_workflow_id {
        anyhow::ensure!(
            execution.workflow_id == expected,
            "{} workflow id does not match requested workflow",
            orchestrator_daemon_runtime::ANIMUS_EXECUTION_FENCE_JSON_ENV
        );
    }
    Ok(Some(execution))
}
