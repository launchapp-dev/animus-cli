use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::{build_guarded_list_result, AoMcpServer, ListGuardInput};
use crate::services::operations::{
    workflow_cancel_application, workflow_checkpoints_list_application, workflow_config_agent_remove_application,
    workflow_config_agent_set_application, workflow_config_get_application, workflow_config_set_application,
    workflow_config_source_application, workflow_config_validate_application,
    workflow_config_workflow_remove_application, workflow_config_workflow_set_application,
    workflow_decisions_application, workflow_definitions_list_application, workflow_dispatch_application,
    workflow_execute_application, workflow_get_application, workflow_launch_application, workflow_list_application,
    workflow_pause_application, workflow_phase_approve_application, workflow_phase_get_application,
    workflow_phase_reject_application, workflow_phases_list_application, workflow_resume_application,
    WorkflowControlApplicationRequest, WorkflowDispatchApplicationRequest, WorkflowExecuteArgs,
    WorkflowLaunchApplicationRequest, WorkflowListApplicationRequest,
};
use anyhow::Result;
use orchestrator_core::{services::ServiceHub, FileServiceHub};
use rmcp::model::CallToolResult;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::Arc;

fn workflow_input(input: Option<Value>, input_json: Option<String>) -> Result<Option<Value>> {
    match (input, input_json) {
        (Some(_), Some(_)) => Err(crate::invalid_input_error("input and input_json are mutually exclusive")),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(raw)) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| crate::invalid_input_error(format!("input_json: {error}"))),
        (None, None) => Ok(None),
    }
}

fn workflow_hub(project_root: &str) -> Result<Arc<dyn ServiceHub>> {
    Ok(Arc::new(FileServiceHub::new(project_root)?))
}

fn structured_or_compat_json<T: DeserializeOwned>(
    structured: Option<Value>,
    input_json: Option<String>,
    structured_field: &str,
) -> Result<T> {
    let value = match (structured, input_json) {
        (Some(value), None) => value,
        (None, Some(raw)) => {
            serde_json::from_str(&raw).map_err(|error| crate::invalid_input_error(format!("input_json: {error}")))?
        }
        (Some(_), Some(_)) => {
            return Err(crate::invalid_input_error(format!("{structured_field} and input_json are mutually exclusive")))
        }
        (None, None) => {
            return Err(crate::invalid_input_error(format!("one of {structured_field} or input_json is required")))
        }
    };
    serde_json::from_value(value).map_err(|error| crate::invalid_input_error(format!("{structured_field}: {error}")))
}

impl AoMcpServer {
    async fn run_workflow_application<F>(
        &self,
        tool_name: &str,
        project_root: Option<String>,
        call: F,
    ) -> CallToolResult
    where
        F: AsyncFnOnce(String, Option<&animus_actor::Actor>) -> Result<Value>,
    {
        self.audit_actor_tool_decision(tool_name, true, "forward");
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match call(project_root, self.pinned_actor()).await {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(err) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &err)),
        }
    }

    pub(super) async fn workflow_list_inproc(
        &self,
        input: super::WorkflowListInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let guard = ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens };
        let request = WorkflowListApplicationRequest {
            status: input.status,
            workflow_ref: input.workflow_ref,
            subject_id: input.subject_id,
            phase_id: input.phase_id,
            search: input.search,
            sort: input.sort,
            // MCP pagination is applied after actor filtering by the common
            // list guard, so fetch the complete actor-visible application set.
            limit: None,
            offset: 0,
        };
        Ok(self
            .run_workflow_application("animus.workflow.list", project_root, async |root, actor| {
                let items = workflow_list_application(workflow_hub(&root)?, &root, request, actor).await?;
                build_guarded_list_result("animus.workflow.list", Value::Array(items), guard)
            })
            .await)
    }

    pub(super) async fn workflow_run_inproc(
        &self,
        input: super::WorkflowRunInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.run", project_root, async |root, actor| {
                let dispatch = workflow_dispatch_application(
                    &root,
                    WorkflowDispatchApplicationRequest {
                        title: input.title,
                        subject_id: input.subject_id,
                        description: input.description,
                        workflow_ref: input.workflow_ref,
                        input: workflow_input(input.input, input.input_json)?,
                        vars: input.vars,
                    },
                    actor,
                )
                .await?;
                workflow_launch_application(
                    workflow_hub(&root)?,
                    &root,
                    WorkflowLaunchApplicationRequest {
                        dispatch,
                        model: input.model,
                        tool: input.tool,
                        phase_timeout_secs: input.phase_timeout_secs,
                        idempotency_key: input.idempotency_key,
                    },
                    actor,
                )
                .await
            })
            .await)
    }

    pub(super) async fn workflow_execute_inproc(
        &self,
        input: super::WorkflowExecuteInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.execute", project_root, async |root, actor| {
                let workflow_input = workflow_input(input.input, input.input_json)?;
                let workflow_ref = input.workflow_ref;
                let dispatch = workflow_dispatch_application(
                    &root,
                    WorkflowDispatchApplicationRequest {
                        title: None,
                        subject_id: Some(input.subject_id),
                        description: None,
                        workflow_ref: workflow_ref.clone(),
                        input: workflow_input.clone(),
                        vars: input.vars.clone(),
                    },
                    actor,
                )
                .await?;
                workflow_execute_application(
                    WorkflowExecuteArgs {
                        workflow_id: None,
                        title: None,
                        subject_dispatch: Some(dispatch),
                        description: None,
                        workflow_ref,
                        phase: input.phase,
                        model: input.model,
                        tool: input.tool,
                        phase_timeout_secs: input.phase_timeout_secs,
                        input: workflow_input,
                        vars: input.vars,
                        actor: actor.cloned(),
                    },
                    workflow_hub(&root)?,
                    &root,
                )
                .await
            })
            .await)
    }

    pub(super) async fn workflow_run_multiple_inproc(
        &self,
        input: super::WorkflowRunMultipleInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let tool_name = "animus.workflow.run-multiple";
        if let Err(message) = super::validate_workflow_run_multiple_input(tool_name, &input.runs) {
            let error = crate::invalid_input_error(message);
            return Ok(CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &error)));
        }
        self.audit_actor_tool_decision(tool_name, true, "forward");
        let project_root = resolve_project_root(&self.default_project_root, input.project_root);
        let requested = input.runs.len();
        let mut results = Vec::with_capacity(requested);
        let mut stopped = false;
        for (index, item) in input.runs.into_iter().enumerate() {
            if stopped {
                results.push(json!({
                    "index": index,
                    "status": "skipped",
                    "target_id": item.subject_id,
                    "command": "workflow.run",
                    "result": null,
                    "error": null,
                    "exit_code": null,
                    "reason": "stopped after earlier failure",
                }));
                continue;
            }
            let target_id = item.subject_id.clone();
            let outcome = async {
                let dispatch = workflow_dispatch_application(
                    &project_root,
                    WorkflowDispatchApplicationRequest {
                        title: None,
                        subject_id: Some(item.subject_id),
                        description: None,
                        workflow_ref: item.workflow_ref,
                        input: workflow_input(item.input, item.input_json)?,
                        vars: item.vars,
                    },
                    self.pinned_actor(),
                )
                .await?;
                workflow_launch_application(
                    workflow_hub(&project_root)?,
                    &project_root,
                    WorkflowLaunchApplicationRequest {
                        dispatch,
                        model: item.model,
                        tool: item.tool,
                        phase_timeout_secs: item.phase_timeout_secs,
                        idempotency_key: item.idempotency_key,
                    },
                    self.pinned_actor(),
                )
                .await
            }
            .await;
            match outcome {
                Ok(result) => results.push(json!({
                    "index": index,
                    "status": "success",
                    "target_id": target_id,
                    "command": "workflow.run",
                    "result": result,
                    "exit_code": 0,
                })),
                Err(error) => {
                    let mut payload = build_inproc_tool_error_payload(tool_name, &error);
                    payload.as_object_mut().map(|map| map.remove("tool"));
                    let exit_code = payload.get("exit_code").cloned().unwrap_or(Value::Null);
                    results.push(json!({
                        "index": index,
                        "status": "failed",
                        "target_id": target_id,
                        "command": "workflow.run",
                        "result": null,
                        "error": payload,
                        "exit_code": exit_code,
                    }));
                    stopped = input.on_error == super::OnError::Stop;
                }
            }
        }
        let executed = results.iter().filter(|item| item["status"] != "skipped").count();
        let succeeded = results.iter().filter(|item| item["status"] == "success").count();
        let failed = results.iter().filter(|item| item["status"] == "failed").count();
        let skipped = results.iter().filter(|item| item["status"] == "skipped").count();
        Ok(CallToolResult::structured(json!({
            "schema": super::BATCH_RESULT_SCHEMA,
            "tool": tool_name,
            "on_error": input.on_error.as_str(),
            "summary": {
                "requested": requested,
                "executed": executed,
                "succeeded": succeeded,
                "failed": failed,
                "skipped": skipped,
                "completed": failed == 0,
            },
            "results": results,
        })))
    }

    pub(super) async fn workflow_get_inproc(&self, input: super::IdInput) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.get", project_root, async |root, actor| {
                workflow_get_application(workflow_hub(&root)?, &root, &input.id, actor).await
            })
            .await)
    }

    pub(super) async fn workflow_pause_inproc(
        &self,
        input: super::WorkflowDestructiveInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.pause", project_root, async |root, actor| {
                workflow_pause_application(
                    workflow_hub(&root)?,
                    &root,
                    WorkflowControlApplicationRequest { id: input.id, confirm: input.confirm, dry_run: input.dry_run },
                    actor,
                )
                .await
            })
            .await)
    }

    pub(super) async fn workflow_resume_inproc(
        &self,
        input: super::IdInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.resume", project_root, async |root, actor| {
                workflow_resume_application(workflow_hub(&root)?, &root, &input.id, false, actor).await
            })
            .await)
    }

    pub(super) async fn workflow_cancel_inproc(
        &self,
        input: super::WorkflowDestructiveInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.cancel", project_root, async |root, actor| {
                workflow_cancel_application(
                    workflow_hub(&root)?,
                    &root,
                    WorkflowControlApplicationRequest { id: input.id, confirm: input.confirm, dry_run: input.dry_run },
                    actor,
                )
                .await
            })
            .await)
    }

    pub(super) async fn workflow_phase_approve_inproc(
        &self,
        input: super::WorkflowPhaseApproveInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.phase.approve", project_root, async |root, actor| {
                workflow_phase_approve_application(
                    workflow_hub(&root)?,
                    &root,
                    &input.workflow_id,
                    &input.phase_id,
                    input.feedback.as_deref().unwrap_or_default(),
                    actor,
                )
                .await
            })
            .await)
    }

    pub(super) async fn workflow_phase_reject_inproc(
        &self,
        input: super::WorkflowPhaseRejectInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.phase.reject", project_root, async |root, actor| {
                workflow_phase_reject_application(
                    workflow_hub(&root)?,
                    &root,
                    &input.workflow_id,
                    &input.phase_id,
                    &input.reason,
                    actor,
                )
                .await
            })
            .await)
    }

    pub(super) async fn workflow_decisions_inproc(
        &self,
        input: super::IdListInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let guard = ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens };
        Ok(self
            .run_workflow_application("animus.workflow.decisions", project_root, async |root, actor| {
                let items = workflow_decisions_application(workflow_hub(&root)?, &root, &input.id, actor).await?;
                build_guarded_list_result("animus.workflow.decisions", items, guard)
            })
            .await)
    }

    pub(super) async fn workflow_checkpoints_list_inproc(
        &self,
        input: super::IdListInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let guard = ListGuardInput { limit: input.limit, offset: input.offset, max_tokens: input.max_tokens };
        Ok(self
            .run_workflow_application("animus.workflow.checkpoints.list", project_root, async |root, actor| {
                let items =
                    workflow_checkpoints_list_application(workflow_hub(&root)?, &root, &input.id, actor).await?;
                build_guarded_list_result("animus.workflow.checkpoints.list", items, guard)
            })
            .await)
    }

    pub(super) async fn workflow_phases_list_inproc(
        &self,
        project_root: Option<String>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self
            .run_workflow_application("animus.workflow.phases.list", project_root, async |root, _| {
                workflow_phases_list_application(&root)
            })
            .await)
    }

    pub(super) async fn workflow_phase_get_inproc(
        &self,
        input: super::WorkflowPhaseGetInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.phases.get", project_root, async |root, _| {
                workflow_phase_get_application(&root, &input.phase)
            })
            .await)
    }

    pub(super) async fn workflow_definitions_list_inproc(
        &self,
        project_root: Option<String>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self
            .run_workflow_application("animus.workflow.definitions.list", project_root, async |root, _| {
                workflow_definitions_list_application(&root)
            })
            .await)
    }

    pub(super) async fn workflow_config_get_inproc(
        &self,
        project_root: Option<String>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self
            .run_workflow_application("animus.workflow.config.get", project_root, async |root, actor| {
                workflow_config_get_application(&root, actor)
            })
            .await)
    }

    pub(super) async fn workflow_config_validate_inproc(
        &self,
        project_root: Option<String>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self
            .run_workflow_application("animus.workflow.config.validate", project_root, async |root, actor| {
                workflow_config_validate_application(&root, actor)
            })
            .await)
    }

    pub(super) async fn workflow_config_set_inproc(
        &self,
        input: super::WorkflowConfigSetInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.config.set", project_root, async |root, _| {
                let config = match (input.config, input.file) {
                    (Some(value), None) => serde_json::from_value(value)
                        .map_err(|error| crate::invalid_input_error(format!("config: {error}")))?,
                    (None, Some(file)) => workflow_config_source_application(Some(&file))?,
                    (Some(_), Some(_)) => {
                        return Err(crate::invalid_input_error("config and file are mutually exclusive"))
                    }
                    (None, None) => return Err(crate::invalid_input_error("one of config or file is required")),
                };
                workflow_config_set_application(&root, config)
            })
            .await)
    }

    pub(super) async fn workflow_config_agent_set_inproc(
        &self,
        input: super::WorkflowConfigAgentSetInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.config.agent-set", project_root, async |root, _| {
                let profile = structured_or_compat_json(input.profile, input.input_json, "profile")?;
                workflow_config_agent_set_application(&root, &input.id, profile)
            })
            .await)
    }

    pub(super) async fn workflow_config_agent_remove_inproc(
        &self,
        input: super::WorkflowConfigEntityRemoveInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.config.agent-remove", project_root, async |root, _| {
                workflow_config_agent_remove_application(&root, &input.id)
            })
            .await)
    }

    pub(super) async fn workflow_config_workflow_set_inproc(
        &self,
        input: super::WorkflowConfigWorkflowSetInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.config.workflow-set", project_root, async |root, _| {
                let workflow = structured_or_compat_json(input.workflow, input.input_json, "workflow")?;
                workflow_config_workflow_set_application(&root, workflow)
            })
            .await)
    }

    pub(super) async fn workflow_config_workflow_remove_inproc(
        &self,
        input: super::WorkflowConfigEntityRemoveInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.config.workflow-remove", project_root, async |root, _| {
                workflow_config_workflow_remove_application(&root, &input.id)
            })
            .await)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn actor(user_id: &str, tenant_id: &str) -> animus_actor::Actor {
        animus_actor::Actor { user_id: user_id.to_string(), claims: Vec::new(), tenant_id: Some(tenant_id.to_string()) }
    }

    async fn actor_workflow(
        root: &tempfile::TempDir,
        owner: &animus_actor::Actor,
    ) -> protocol::orchestrator::OrchestratorWorkflow {
        let root = root.path().to_string_lossy();
        let hub = FileServiceHub::new(root.as_ref()).expect("file service hub");
        hub.workflows()
            .run(
                orchestrator_core::WorkflowRunInput::for_task(
                    "TASK-actor-control".to_string(),
                    Some("standard".to_string()),
                ),
                Some(owner),
            )
            .await
            .expect("actor workflow")
    }

    #[tokio::test]
    async fn workflow_pause_dry_run_is_owner_scoped_and_does_not_mutate() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());
        let alice = actor("alice", "workspace-a");
        let workflow = actor_workflow(&root, &alice).await;
        let server = super::super::new_ao_mcp_server_with_options(
            root.path().to_string_lossy().as_ref(),
            false,
            None,
            None,
            Some(alice),
        );
        let result = server
            .workflow_pause_inproc(super::super::WorkflowDestructiveInput {
                id: workflow.id.clone(),
                confirm: None,
                dry_run: true,
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_ne!(result.is_error, Some(true));
        let payload = result.structured_content.expect("structured payload");
        assert_eq!(payload.pointer("/result/dry_run").and_then(Value::as_bool), Some(true), "{payload}");
        let hub = FileServiceHub::new(root.path().to_string_lossy().as_ref()).expect("file service hub");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow reload");
        assert_eq!(reloaded.status, orchestrator_core::WorkflowStatus::Running);
    }

    #[tokio::test]
    async fn workflow_pause_mutates_an_owner_scoped_workflow_after_confirmation() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());
        let alice = actor("alice", "workspace-a");
        let workflow = actor_workflow(&root, &alice).await;
        let server = super::super::new_ao_mcp_server_with_options(
            root.path().to_string_lossy().as_ref(),
            false,
            None,
            None,
            Some(alice),
        );
        let result = server
            .workflow_pause_inproc(super::super::WorkflowDestructiveInput {
                id: workflow.id.clone(),
                confirm: Some(workflow.id.clone()),
                dry_run: false,
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_ne!(result.is_error, Some(true));
        let payload = result.structured_content.expect("structured payload");
        assert_eq!(payload.pointer("/result/status").and_then(Value::as_str), Some("paused"), "{payload}");
        assert_eq!(payload.pointer("/result/actor/user_id").and_then(Value::as_str), Some("alice"), "{payload}");
        let hub = FileServiceHub::new(root.path().to_string_lossy().as_ref()).expect("file service hub");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow reload");
        assert_eq!(reloaded.status, orchestrator_core::WorkflowStatus::Paused);
    }

    #[tokio::test]
    async fn workflow_controls_conceal_cross_actor_workflows_before_mutation() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());
        let workflow = actor_workflow(&root, &actor("alice", "workspace-a")).await;
        let server = super::super::new_ao_mcp_server_with_options(
            root.path().to_string_lossy().as_ref(),
            false,
            None,
            None,
            Some(actor("bob", "workspace-a")),
        );
        let result = server
            .workflow_cancel_inproc(super::super::WorkflowDestructiveInput {
                id: workflow.id.clone(),
                confirm: Some(workflow.id.clone()),
                dry_run: false,
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"), "{payload}");
        let hub = FileServiceHub::new(root.path().to_string_lossy().as_ref()).expect("file service hub");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow reload");
        assert_eq!(reloaded.status, orchestrator_core::WorkflowStatus::Running);
    }

    #[tokio::test]
    async fn manual_phase_controls_conceal_cross_actor_workflows_before_phase_validation() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());
        let workflow = actor_workflow(&root, &actor("alice", "workspace-a")).await;
        let server = super::super::new_ao_mcp_server_with_options(
            root.path().to_string_lossy().as_ref(),
            false,
            None,
            None,
            Some(actor("alice", "workspace-b")),
        );
        let result = server
            .workflow_phase_reject_inproc(super::super::WorkflowPhaseRejectInput {
                workflow_id: workflow.id,
                phase_id: "manual-review".to_string(),
                reason: "not mine".to_string(),
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"), "{payload}");
    }

    fn workflow_run_input() -> super::super::WorkflowRunInput {
        super::super::WorkflowRunInput {
            title: Some("Typed launch".to_string()),
            subject_id: None,
            description: None,
            workflow_ref: None,
            input_json: None,
            input: None,
            vars: Default::default(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            idempotency_key: None,
            project_root: None,
        }
    }

    fn bulk_workflow_run(subject_id: &str) -> super::super::BulkWorkflowRunItem {
        super::super::BulkWorkflowRunItem { subject_id: subject_id.to_string(), ..Default::default() }
    }

    #[test]
    fn workflow_input_preserves_nested_values_and_supports_legacy_json() {
        let nested = json!({"request": {"items": [1, true, {"name": "alpha"}]}});
        assert_eq!(workflow_input(Some(nested.clone()), None).expect("typed input"), Some(nested.clone()));
        assert_eq!(workflow_input(None, Some(nested.to_string())).expect("legacy JSON input"), Some(nested));
    }

    #[test]
    fn workflow_input_rejects_conflicting_and_malformed_encodings() {
        let conflict = workflow_input(Some(json!({"typed": true})), Some("{}".to_string())).unwrap_err();
        assert_eq!(crate::classify_cli_error_kind(&conflict).code(), "invalid_input");
        let malformed = workflow_input(None, Some("{".to_string())).unwrap_err();
        assert_eq!(crate::classify_cli_error_kind(&malformed).code(), "invalid_input");
    }

    #[tokio::test]
    async fn workflow_run_input_conflict_is_a_typed_in_process_error() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let mut input = workflow_run_input();
        input.input = Some(json!({"typed": true}));
        input.input_json = Some("{}".to_string());
        let result = server.workflow_run_inproc(input).await.expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");
    }

    #[tokio::test]
    async fn workflow_execute_input_conflict_is_a_typed_in_process_error() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server
            .workflow_execute_inproc(super::super::WorkflowExecuteInput {
                subject_id: "TASK-1".to_string(),
                workflow_ref: None,
                phase: None,
                model: None,
                tool: None,
                phase_timeout_secs: None,
                input_json: Some("{}".to_string()),
                input: Some(json!({"typed": true})),
                vars: Default::default(),
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");
    }

    #[tokio::test]
    async fn workflow_run_multiple_stop_skips_after_a_typed_item_failure() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let mut first = bulk_workflow_run("TASK-1");
        first.input = Some(json!({"typed": true}));
        first.input_json = Some("{}".to_string());
        let result = server
            .workflow_run_multiple_inproc(super::super::WorkflowRunMultipleInput {
                runs: vec![first, bulk_workflow_run("TASK-2")],
                on_error: super::super::OnError::Stop,
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        let payload = result.structured_content.expect("structured batch payload");
        assert_eq!(payload.pointer("/summary/failed").and_then(Value::as_u64), Some(1), "{payload}");
        assert_eq!(payload.pointer("/summary/skipped").and_then(Value::as_u64), Some(1), "{payload}");
        assert_eq!(payload.pointer("/results/0/error/error/code").and_then(Value::as_str), Some("invalid_input"));
        assert_eq!(payload.pointer("/results/1/status").and_then(Value::as_str), Some("skipped"));
    }

    #[tokio::test]
    async fn workflow_run_multiple_continue_isolates_typed_item_failures() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let mut first = bulk_workflow_run("TASK-1");
        first.input = Some(json!({"typed": true}));
        first.input_json = Some("{}".to_string());
        let mut second = bulk_workflow_run("TASK-2");
        second.input_json = Some("{".to_string());
        let result = server
            .workflow_run_multiple_inproc(super::super::WorkflowRunMultipleInput {
                runs: vec![first, second],
                on_error: super::super::OnError::Continue,
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        let payload = result.structured_content.expect("structured batch payload");
        assert_eq!(payload.pointer("/summary/executed").and_then(Value::as_u64), Some(2), "{payload}");
        assert_eq!(payload.pointer("/summary/failed").and_then(Value::as_u64), Some(2), "{payload}");
        assert_eq!(payload.pointer("/summary/skipped").and_then(Value::as_u64), Some(0), "{payload}");
        assert_eq!(payload.pointer("/results/0/error/error/code").and_then(Value::as_str), Some("invalid_input"));
        assert_eq!(payload.pointer("/results/1/error/error/code").and_then(Value::as_str), Some("invalid_input"));
    }

    #[tokio::test]
    async fn workflow_list_validation_is_a_typed_in_process_error() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server
            .workflow_list_inproc(super::super::WorkflowListInput {
                project_root: None,
                status: Some("not-a-workflow-status".to_string()),
                workflow_ref: None,
                subject_id: None,
                phase_id: None,
                search: None,
                sort: None,
                limit: None,
                offset: None,
                max_tokens: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "payload: {payload}");
    }

    #[tokio::test]
    async fn workflow_list_returns_guarded_items_without_cli_envelope_drift() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(root.path().to_string_lossy().as_ref()));
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server
            .workflow_list_inproc(super::super::WorkflowListInput {
                project_root: None,
                status: Some("running".to_string()),
                workflow_ref: None,
                subject_id: None,
                phase_id: None,
                search: None,
                sort: None,
                limit: Some(5),
                offset: Some(0),
                max_tokens: None,
            })
            .await
            .expect("tool transport succeeds");

        let payload = result.structured_content.expect("structured payload");
        assert_ne!(result.is_error, Some(true), "payload: {payload}");
        assert_eq!(payload.get("tool").and_then(Value::as_str), Some("animus.workflow.list"));
        assert_eq!(
            payload.pointer("/result/tool").and_then(Value::as_str),
            Some("animus.workflow.list"),
            "payload: {payload}",
        );
        assert_eq!(payload.pointer("/result/items").and_then(Value::as_array).map(Vec::len), Some(0));
        assert_eq!(payload.pointer("/result/pagination/total").and_then(Value::as_u64), Some(0));
    }

    #[tokio::test]
    async fn workflow_config_get_returns_structured_data_without_cli_exec() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server.workflow_config_get_inproc(None).await.expect("tool transport succeeds");

        assert_ne!(result.is_error, Some(true));
        let payload = result.structured_content.expect("structured payload");
        assert_eq!(payload.get("tool").and_then(Value::as_str), Some("animus.workflow.config.get"));
        assert!(payload.pointer("/result/workflow_config").is_some());
    }

    #[test]
    fn structured_agent_profile_and_compat_json_decode_to_the_same_typed_model() {
        let value = json!({
            "description": "Reviews changes",
            "system_prompt": "Review the implementation",
            "extra_config": {"reasoning_effort": "high"}
        });
        let structured: orchestrator_config::AgentProfileOverlay =
            structured_or_compat_json(Some(value.clone()), None, "profile").expect("structured profile");
        let compatibility: orchestrator_config::AgentProfileOverlay =
            structured_or_compat_json(None, Some(value.to_string()), "profile").expect("compatibility profile");

        assert_eq!(serde_json::to_value(structured).unwrap(), serde_json::to_value(compatibility).unwrap());
    }

    #[tokio::test]
    async fn workflow_config_set_rejects_conflicting_sources_before_file_access() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server
            .workflow_config_set_inproc(super::super::WorkflowConfigSetInput {
                config: Some(json!({})),
                file: Some("/path/that/must/not/be/read.json".to_string()),
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");
        assert!(
            payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("mutually exclusive")),
            "{payload}"
        );
    }

    #[tokio::test]
    async fn workflow_config_agent_set_rejects_structured_and_compat_payloads() {
        let _env_lock = lock_env();
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server
            .workflow_config_agent_set_inproc(super::super::WorkflowConfigAgentSetInput {
                id: "reviewer".to_string(),
                profile: Some(json!({"description": "review", "system_prompt": "review"})),
                input_json: Some("{}".to_string()),
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");
    }
}
