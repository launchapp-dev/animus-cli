use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::{build_guarded_list_result, AoMcpServer, ListGuardInput};
use crate::services::operations::{
    workflow_checkpoints_list_application, workflow_config_get_application, workflow_config_validate_application,
    workflow_decisions_application, workflow_definitions_list_application, workflow_get_application,
    workflow_list_application, workflow_phase_get_application, workflow_phases_list_application,
    WorkflowListApplicationRequest,
};
use anyhow::Result;
use orchestrator_core::{services::ServiceHub, FileServiceHub};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use std::sync::Arc;

fn workflow_hub(project_root: &str) -> Result<Arc<dyn ServiceHub>> {
    Ok(Arc::new(FileServiceHub::new(project_root)?))
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

    pub(super) async fn workflow_get_inproc(&self, input: super::IdInput) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_workflow_application("animus.workflow.get", project_root, async |root, actor| {
                workflow_get_application(workflow_hub(&root)?, &root, &input.id, actor).await
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workflow_list_validation_is_a_typed_in_process_error() {
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
    async fn workflow_config_get_returns_structured_data_without_cli_exec() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server.workflow_config_get_inproc(None).await.expect("tool transport succeeds");

        assert_ne!(result.is_error, Some(true));
        let payload = result.structured_content.expect("structured payload");
        assert_eq!(payload.get("tool").and_then(Value::as_str), Some("animus.workflow.config.get"));
        assert!(payload.pointer("/result/workflow_config").is_some());
    }
}
