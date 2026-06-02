use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;

use crate::print_value;
use crate::services::plugin_clients;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use animus_workflow_runner_protocol as workflow_proto;

#[derive(Debug)]
pub(crate) struct WorkflowExecuteArgs {
    pub(crate) workflow_id: Option<String>,
    pub(crate) task_id: Option<String>,
    pub(crate) requirement_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) workflow_ref: Option<String>,
    pub(crate) phase: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) tool: Option<String>,
    pub(crate) phase_timeout_secs: Option<u64>,
    pub(crate) input_json: Option<String>,
    pub(crate) vars: Vec<String>,
}

pub(crate) async fn handle_workflow_execute(
    mut args: WorkflowExecuteArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    if args.requirement_id.is_some() && args.workflow_ref.is_none() {
        args.workflow_ref = Some(super::resolve_requirement_workflow_ref(project_root)?);
    }
    if args.workflow_id.is_some() && !args.vars.is_empty() {
        anyhow::bail!(
            "--var cannot be used with --workflow-id; persisted workflow vars are authoritative for existing workflows"
        );
    }
    let vars = super::parse_workflow_vars(&args.vars)?;

    hub.daemon().start(Default::default()).await?;

    let task_id_for_sync = args.task_id.clone();
    let phase_filter = args.phase.clone();
    let task_id_for_output = args.task_id.clone();
    let requirement_id_for_output = args.requirement_id.clone();

    // v0.5.1 fold-in: route `workflow/execute` exclusively through the
    // installed `workflow_runner` plugin. The in-tree fallback path
    // was removed; daemon preflight enforces plugin presence at
    // startup. When invoked outside the daemon (e.g. `animus workflow
    // execute ...` on a fresh checkout) and no plugin is installed,
    // we surface an actionable error rather than falling through to
    // a runtime that the rest of v0.5.1 no longer exercises.
    let plugin_input_json: Option<serde_json::Value> =
        args.input_json.as_deref().map(serde_json::from_str).transpose()?;
    let plugin_request = workflow_proto::WorkflowExecuteRequest {
        workflow_id: args.workflow_id.clone(),
        subject_dispatch: None,
        subject_ref: None,
        task_id: args.task_id.clone(),
        requirement_id: args.requirement_id.clone(),
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
        if let (Some(task_id), Some(status)) = (task_id_for_sync.as_deref(), parsed_status) {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                plugin_result.subject_id.as_str(),
                Some(task_id),
                Some(plugin_result.workflow_ref.as_str()),
                Some(plugin_result.workflow_id.as_str()),
                status,
                None,
            )
            .await;
        }
    }
    if json {
        return print_value(
            serde_json::json!({
                "workflow_id": plugin_result.workflow_id,
                "workflow_ref": plugin_result.workflow_ref,
                "workflow_status": plugin_result.workflow_status,
                "subject_id": plugin_result.subject_id,
                "task_id": task_id_for_output,
                "requirement_id": requirement_id_for_output,
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
