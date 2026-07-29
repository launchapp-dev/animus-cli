use anyhow::Result;
use rmcp::model::CallToolResult;
use serde::Serialize;
use serde_json::json;

use super::daemon_inproc::resolve_project_root;
use super::environment_tools::{
    EnvironmentGetInput, EnvironmentListInput, EnvironmentReapInput, EnvironmentTeardownInput,
};
use super::exec_errors::build_inproc_tool_error_payload;
use super::AoMcpServer;
use crate::services::operations::{
    environment_get_application, environment_list_application, environment_reap_application,
    environment_teardown_application,
};

impl AoMcpServer {
    fn run_environment_application<T, F>(
        &self,
        tool_name: &str,
        project_root: Option<String>,
        call: F,
    ) -> CallToolResult
    where
        T: Serialize,
        F: FnOnce(&str) -> Result<T>,
    {
        self.audit_actor_tool_decision(tool_name, false, "management-only");
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match call(&project_root) {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(error) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &error)),
        }
    }

    pub(super) fn environment_list_inproc(&self, input: EnvironmentListInput) -> CallToolResult {
        self.run_environment_application("animus.environment.list", input.project_root, |root| {
            environment_list_application(root, input.environment.as_deref())
        })
    }

    pub(super) fn environment_get_inproc(&self, input: EnvironmentGetInput) -> CallToolResult {
        self.run_environment_application("animus.environment.get", input.project_root, |root| {
            environment_get_application(root, input.environment.as_deref(), &input.id)
        })
    }

    pub(super) fn environment_teardown_inproc(&self, input: EnvironmentTeardownInput) -> CallToolResult {
        self.run_environment_application("animus.environment.teardown", input.project_root, |root| {
            environment_teardown_application(root, input.environment.as_deref(), &input.id)
        })
    }

    pub(super) fn environment_reap_inproc(&self, input: EnvironmentReapInput) -> CallToolResult {
        self.run_environment_application("animus.environment.reap", input.project_root, |root| {
            environment_reap_application(
                root,
                input.environment.as_deref(),
                input.all,
                input.force,
                input.dry_run,
                input.older_than_secs,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::environment_tools::EnvironmentListInput;
    use super::super::new_ao_mcp_server;

    #[test]
    fn environment_mcp_uses_the_typed_service_and_returns_structured_errors() {
        let temp = tempfile::tempdir().expect("project root");
        let server = new_ao_mcp_server(temp.path().to_string_lossy().as_ref());
        let result = server.environment_list_inproc(EnvironmentListInput { environment: None, project_root: None });

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed environment error");
        assert_eq!(payload.get("tool").and_then(Value::as_str), Some("animus.environment.list"));
        assert!(
            payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("no environment plugin is installed")),
            "{payload}"
        );
    }
}
