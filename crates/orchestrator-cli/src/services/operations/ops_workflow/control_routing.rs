//! CLI-side `WorkflowRouting` adapter — bridges the daemon's control
//! surface back to the same `WorkflowServiceApi` + `dispatch_workflow_event`
//! helpers the CLI uses for its in-process code path.
//!
//! See the sibling [`crate::services::operations::ops_plugin::control_routing`]
//! and [`crate::services::runtime::runtime_daemon::control_routing`] modules
//! for the plugin/* and daemon/* equivalents.
//!
//! ## Shape conversion
//!
//! The wire types under [`animus_control_protocol::types`] are a small
//! subset of the in-tree [`orchestrator_core::OrchestratorWorkflow`].
//! [`workflow_summary_from_core`] does the projection: top-level summary
//! fields land in [`WireWorkflowRunSummary`]; the full
//! `OrchestratorWorkflow` is re-encoded as opaque JSON under
//! [`WireWorkflowRun::detail`] so wire consumers that need the rich
//! schema can still parse it.
//!
//! ## Error mapping
//!
//! Anyhow errors carry no machine-readable code. We surface them as
//! [`ControlError::Internal`] with the original message preserved (with
//! `{:#}` so the chain shows up).

use std::path::PathBuf;
use std::sync::Arc;

use animus_control_protocol::{
    types::{
        Unit, WorkflowCancelRequest as WireCancelRequest, WorkflowExecuteRequest as WireExecuteRequest,
        WorkflowGetRequest as WireGetRequest, WorkflowListRequest as WireListRequest,
        WorkflowListResponse as WireListResponse, WorkflowPauseRequest as WirePauseRequest,
        WorkflowResumeRequest as WireResumeRequest, WorkflowRun as WireWorkflowRun,
        WorkflowRunRequest as WireRunRequest, WorkflowRunStart as WireRunStart, WorkflowRunSummary as WireRunSummary,
        WorkflowStatus as WireWorkflowStatus,
    },
    ControlError,
};
use animus_subject_protocol_wire::SubjectId;
use async_trait::async_trait;
use orchestrator_core::{
    dispatch_workflow_event, FileServiceHub, ListPage, ListPageRequest, OrchestratorWorkflow, ServiceHub,
    WorkflowEvent, WorkflowFilter, WorkflowQuery, WorkflowRunInput, WorkflowStatus as CoreWorkflowStatus,
};
use orchestrator_daemon_runtime::control::WorkflowRouting;

/// Build a [`WorkflowRouting`] handle bound to `project_root`.
///
/// `project_root` is captured once at daemon startup and reused for every
/// routed call. The returned handle is `Clone` + `Send + Sync` via the
/// `Arc<dyn>` wrapper.
pub fn build_workflow_routing(project_root: PathBuf) -> Arc<dyn WorkflowRouting> {
    Arc::new(WorkflowRoutingImpl { project_root })
}

struct WorkflowRoutingImpl {
    project_root: PathBuf,
}

impl WorkflowRoutingImpl {
    fn project_root_str(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }

    fn hub(&self) -> Result<Arc<dyn ServiceHub>, ControlError> {
        let hub = FileServiceHub::new(&self.project_root_str())
            .map_err(|err| ControlError::Internal(format!("workflow routing: hub init: {err:#}")))?;
        Ok(Arc::new(hub))
    }
}

fn internal(err: anyhow::Error) -> ControlError {
    ControlError::Internal(format!("{err:#}"))
}

/// Map the core `WorkflowStatus` (snake_case + `Escalated`) into the wire
/// `WorkflowStatus` (kebab-case + no `Escalated`). Escalated runs are
/// reported on the wire as `Failed` — closest semantic match, and the
/// full status string survives in the opaque `detail` JSON.
fn core_status_to_wire(status: CoreWorkflowStatus) -> WireWorkflowStatus {
    match status {
        CoreWorkflowStatus::Pending => WireWorkflowStatus::Pending,
        CoreWorkflowStatus::Running => WireWorkflowStatus::Running,
        CoreWorkflowStatus::Paused => WireWorkflowStatus::Paused,
        CoreWorkflowStatus::Completed => WireWorkflowStatus::Completed,
        CoreWorkflowStatus::Failed | CoreWorkflowStatus::Escalated => WireWorkflowStatus::Failed,
        CoreWorkflowStatus::Cancelled => WireWorkflowStatus::Cancelled,
    }
}

fn wire_status_to_core(status: WireWorkflowStatus) -> CoreWorkflowStatus {
    match status {
        WireWorkflowStatus::Pending => CoreWorkflowStatus::Pending,
        WireWorkflowStatus::Running => CoreWorkflowStatus::Running,
        WireWorkflowStatus::Paused => CoreWorkflowStatus::Paused,
        WireWorkflowStatus::Completed => CoreWorkflowStatus::Completed,
        WireWorkflowStatus::Failed => CoreWorkflowStatus::Failed,
        WireWorkflowStatus::Cancelled => CoreWorkflowStatus::Cancelled,
    }
}

fn workflow_summary_from_core(workflow: &OrchestratorWorkflow) -> WireRunSummary {
    let definition = workflow.workflow_ref.clone().unwrap_or_default();
    let subject_id =
        if workflow.task_id.is_empty() { None } else { Some(SubjectId::new(format!("task:{}", workflow.task_id))) };
    WireRunSummary {
        id: workflow.id.clone(),
        definition,
        status: core_status_to_wire(workflow.status),
        subject_id,
        started_at: workflow.started_at,
        finished_at: workflow.completed_at,
    }
}

fn workflow_to_wire_run(workflow: OrchestratorWorkflow) -> Result<WireWorkflowRun, ControlError> {
    let summary = workflow_summary_from_core(&workflow);
    let detail =
        serde_json::to_value(&workflow).map_err(|e| ControlError::Internal(format!("workflow detail encode: {e}")))?;
    Ok(WireWorkflowRun { summary, detail })
}

/// Extract the wire-side `params.vars` map into the in-tree
/// `HashMap<String, String>` consumed by
/// `WorkflowRunInput::with_vars`. The CLI side stuffs `--var KEY=VALUE`
/// pairs into `params["vars"]` as a `{key: string}` object; here we
/// reverse the projection. Non-object payloads are treated as empty so
/// older CLI binaries (no vars plumbing) keep round-tripping cleanly.
fn extract_vars_from_params(
    params: &std::collections::BTreeMap<String, serde_json::Value>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Some(serde_json::Value::Object(obj)) = params.get("vars") {
        for (k, v) in obj {
            let stringified = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out.insert(k.clone(), stringified);
        }
    }
    out
}

/// Extract the wire-side `params.overrides` map (packed by the CLI's
/// `try_workflow_run_via_control`) into the detached-runner execution
/// overrides, mirroring [`extract_vars_from_params`]. Missing or
/// non-object payloads degrade to defaults so older CLI binaries keep
/// round-tripping cleanly.
fn extract_runner_overrides_from_params(
    params: &std::collections::BTreeMap<String, serde_json::Value>,
) -> super::phases::DetachedRunnerOverrides {
    let Some(serde_json::Value::Object(obj)) = params.get("overrides") else {
        return super::phases::DetachedRunnerOverrides::default();
    };
    super::phases::DetachedRunnerOverrides {
        model: obj.get("model").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        tool: obj.get("tool").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        phase_timeout_secs: obj.get("phase_timeout_secs").and_then(serde_json::Value::as_u64),
        // The actor is NOT carried in `params.overrides` (untrusted wire field);
        // the caller sets `overrides.actor` from the authenticated `request.actor`.
        actor: None,
    }
}

#[async_trait]
impl WorkflowRouting for WorkflowRoutingImpl {
    async fn workflow_list(&self, request: WireListRequest) -> Result<WireListResponse, ControlError> {
        let hub = self.hub()?;
        let limit = request.limit.map(|v| v as usize);
        let offset: usize = request.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let query = WorkflowQuery {
            filter: WorkflowFilter {
                status: request.status.map(wire_status_to_core),
                // v0.7.0-rc.7: the workflow-type filter is now wire-expressible.
                workflow_ref: request.workflow_ref.clone(),
                task_id: None,
                phase_id: None,
                search_text: None,
            },
            page: ListPageRequest { limit, offset },
            sort: Default::default(),
        };
        let page: ListPage<OrchestratorWorkflow> = hub.workflows().query(query).await.map_err(internal)?;
        let total = page.total as u32;
        let runs: Vec<WireRunSummary> = page.items.iter().map(workflow_summary_from_core).collect();
        let next_cursor = page.next_offset.map(|n| n.to_string());
        Ok(WireListResponse { runs, next_cursor, total: Some(total) })
    }

    async fn workflow_get(&self, request: WireGetRequest) -> Result<WireWorkflowRun, ControlError> {
        let hub = self.hub()?;
        let workflow = hub.workflows().get(&request.id).await.map_err(internal)?;
        workflow_to_wire_run(workflow)
    }

    async fn workflow_run(&self, request: WireRunRequest) -> Result<WireRunStart, ControlError> {
        let hub = self.hub()?;
        // Extract the wire-side `params.vars` map (set by the CLI's
        // `try_workflow_run_via_control`) so user-supplied
        // `--var KEY=VALUE` pairs survive the control round-trip and
        // reach `WorkflowRunInput::with_vars` — matching the local
        // path. Any non-string values are stringified via
        // `Value::to_string()` so callers always see a valid scalar.
        let vars = extract_vars_from_params(&request.params);
        let mut overrides = extract_runner_overrides_from_params(&request.params);
        // TRUST BOUNDARY: `request.actor` is the transport-asserted caller
        // identity from the authenticated control request — the ONLY trusted
        // source. The kernel relays it opaquely; it never reads an actor from
        // workflow config, agent output, or subject content. This path hands
        // execution to a DETACHED workflow_runner child that rebuilds its
        // request from the persisted record (which carries no actor), so the
        // actor is relayed to that child via the trusted WORKFLOW_ACTOR_ENV
        // handoff carried in the runner overrides.
        //
        // TODO(codex-p2): the actor reaches the detached RUNNER, but the core
        // bootstrap below (`start_workflow_with_runner` -> `hub.workflows().run`)
        // still resolves the default-workflow-ref / skip-guards / phase plan from
        // `load_workflow_config_or_default(actor=None)` (the global config
        // partition). For an actor-scoped `config_source` (per-user default
        // workflow or actor-only definitions) the record can be created from the
        // global view. Closing this needs `actor` threaded through the core
        // `WorkflowServiceApi::run` + phase-plan resolution (a cross-crate change
        // beyond this wave's kernel-threading scope); tracked for the
        // runner-integration / per-actor-config wave.
        overrides.actor = request.actor;
        let input = WorkflowRunInput::for_task(request.task_id, request.definition).with_vars(vars);
        // Post-v0.5 the daemon has no in-process workflow executor: a bare
        // `workflows.run(...)` would only bootstrap a Running record that
        // nothing drives. Hand execution to a detached workflow_runner
        // spawn (same path as the local CLI `workflow run`).
        let workflow = super::phases::start_workflow_with_runner(hub, &self.project_root_str(), input, overrides)
            .await
            .map_err(internal)?;
        Ok(WireRunStart {
            workflow_id: workflow.id,
            status: core_status_to_wire(workflow.status),
            started_at: workflow.started_at,
        })
    }

    async fn workflow_execute(&self, request: WireExecuteRequest) -> Result<WireRunStart, ControlError> {
        let hub = self.hub()?;
        let subject_id_str = request.subject_id.as_ref().map(|sid| sid.as_str().to_string());
        // Strip the optional "task:" prefix from subject ids so a wire
        // caller can pass either `task:TASK-1` or `TASK-1`.
        let task_id =
            subject_id_str.map(|s| s.strip_prefix("task:").map(str::to_string).unwrap_or(s)).unwrap_or_default();
        if task_id.is_empty() {
            return Err(ControlError::InvalidRequest(
                "workflow/execute requires a task subject_id for the CLI control routing".to_string(),
            ));
        }
        let vars = extract_vars_from_params(&request.params);
        let mut overrides = extract_runner_overrides_from_params(&request.params);
        // TRUST BOUNDARY: see `workflow_run` above — `request.actor` is relayed
        // opaquely from the authenticated control request only, and reaches the
        // detached workflow_runner child via the trusted WORKFLOW_ACTOR_ENV
        // handoff carried in the runner overrides.
        overrides.actor = request.actor;
        let input = WorkflowRunInput::for_task(task_id, Some(request.definition)).with_vars(vars);
        let workflow = super::phases::start_workflow_with_runner(hub, &self.project_root_str(), input, overrides)
            .await
            .map_err(internal)?;
        Ok(WireRunStart {
            workflow_id: workflow.id,
            status: core_status_to_wire(workflow.status),
            started_at: workflow.started_at,
        })
    }

    async fn workflow_pause(&self, request: WirePauseRequest) -> Result<Unit, ControlError> {
        let hub = self.hub()?;
        let root = self.project_root_str();
        let task_store = orchestrator_daemon_runtime::resolve_task_projection_store(&root, hub.clone()).await;
        let _ = dispatch_workflow_event(
            hub,
            task_store.as_ref(),
            &root,
            WorkflowEvent::Pause { workflow_id: request.id, reason_detail: None },
        )
        .await
        .map_err(internal)?;
        Ok(Unit::default())
    }

    async fn workflow_resume(&self, request: WireResumeRequest) -> Result<Unit, ControlError> {
        let hub = self.hub()?;
        // Same contract as the local CLI resume: spawn the workflow_runner
        // for the resumed workflow so the Running record has a live driver
        // and the orphan reconciler does not cancel it on the next tick.
        let _ =
            super::phases::resume_workflow_with_runner(hub, &self.project_root_str(), &request.id, request.feedback)
                .await
                .map_err(internal)?;
        Ok(Unit::default())
    }

    async fn workflow_cancel(&self, request: WireCancelRequest) -> Result<Unit, ControlError> {
        let hub = self.hub()?;
        let root = self.project_root_str();
        let workflow = hub.workflows().get(&request.id).await.map_err(internal)?;
        crate::services::runtime::teardown_retained_environment_for_cancel(&root, &workflow).map_err(internal)?;
        let task_store = orchestrator_daemon_runtime::resolve_task_projection_store(&root, hub.clone()).await;
        let _ =
            dispatch_workflow_event(hub, task_store.as_ref(), &root, WorkflowEvent::Cancel { workflow_id: request.id })
                .await
                .map_err(internal)?;
        Ok(Unit::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn extract_vars_from_params_round_trips_workflow_run_var_pairs() {
        // The CLI's `try_workflow_run_via_control` packs `--var KEY=VALUE`
        // pairs into `params["vars"]` as a `{key: string}` object. The
        // daemon-side `workflow_run` adapter must extract them back into
        // the in-tree `HashMap<String, String>` consumed by
        // `WorkflowRunInput::with_vars` — otherwise `--var` is silently
        // dropped on the control path while the local path honors it.
        let mut params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        params.insert(
            "vars".to_string(),
            json!({
                "release_name": "Mercury",
                "rollout_pct": "25",
            }),
        );

        let extracted = extract_vars_from_params(&params);
        assert_eq!(extracted.len(), 2, "both vars should survive the control round-trip");
        assert_eq!(extracted.get("release_name").map(String::as_str), Some("Mercury"));
        assert_eq!(extracted.get("rollout_pct").map(String::as_str), Some("25"));
    }

    #[test]
    fn extract_runner_overrides_from_params_round_trips_and_defaults() {
        // The CLI packs --model / --tool / --phase-timeout-secs into
        // `params["overrides"]`; the daemon side must reverse the
        // projection so control-wire runs honor the same execution
        // overrides as the local async path.
        let mut params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        params.insert(
            "overrides".to_string(),
            json!({ "model": "claude-sonnet-4-6", "tool": "claude", "phase_timeout_secs": 120 }),
        );
        let overrides = extract_runner_overrides_from_params(&params);
        assert_eq!(overrides.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(overrides.tool.as_deref(), Some("claude"));
        assert_eq!(overrides.phase_timeout_secs, Some(120));

        // Older CLI binaries send no overrides key; degrade to defaults.
        let empty: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let overrides = extract_runner_overrides_from_params(&empty);
        assert!(overrides.model.is_none() && overrides.tool.is_none() && overrides.phase_timeout_secs.is_none());
    }

    #[test]
    fn extract_vars_from_params_returns_empty_when_vars_key_absent() {
        // Older CLI binaries (pre-vars-plumbing fix) ship an empty
        // params map. The adapter must degrade to "no vars" rather
        // than panic so the wire stays backward-compatible.
        let params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        assert!(extract_vars_from_params(&params).is_empty());
    }

    #[test]
    fn extract_vars_from_params_stringifies_non_string_values() {
        // We accept whatever the CLI sends; future callers may send
        // numeric or bool values via raw JSON-RPC. Stringify so the
        // downstream YAML interpolation sees a consistent scalar.
        let mut params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        params.insert("vars".to_string(), json!({ "retries": 3, "force": true }));
        let extracted = extract_vars_from_params(&params);
        assert_eq!(extracted.get("retries").map(String::as_str), Some("3"));
        assert_eq!(extracted.get("force").map(String::as_str), Some("true"));
    }
}
