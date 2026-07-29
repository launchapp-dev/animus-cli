use super::cost_tools::{BudgetSetInput, CostDecisionsInput};
use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::AoMcpServer;
use crate::services::operations::{budget_get_application, budget_set_application, cost_decisions_application};
use anyhow::Result;
use rmcp::model::CallToolResult;
use serde::Serialize;
use serde_json::json;

impl AoMcpServer {
    fn run_cost_application<T, F>(&self, tool_name: &str, project_root: Option<String>, call: F) -> CallToolResult
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

    pub(super) fn cost_decisions_inproc(&self, input: CostDecisionsInput) -> CallToolResult {
        self.run_cost_application("animus.cost.decisions", input.project_root, |root| {
            cost_decisions_application(root, input.since.as_deref())
        })
    }

    pub(super) fn budget_get_inproc(&self, project_root: Option<String>) -> CallToolResult {
        self.run_cost_application("animus.budget.get", project_root, |root| Ok(budget_get_application(root)))
    }

    pub(super) fn budget_set_inproc(&self, input: BudgetSetInput) -> CallToolResult {
        self.run_cost_application("animus.budget.set", input.project_root, |root| {
            budget_set_application(root, input.max_daily_usd, input.clear)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::cost_tools::{BudgetSetInput, CostDecisionsInput};
    use protocol::test_utils::EnvVarGuard;
    use serde_json::Value;

    #[test]
    fn budget_set_and_get_share_typed_daemon_configuration() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp home");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let server = super::super::new_ao_mcp_server(project_root.to_string_lossy().as_ref());

        let set =
            server.budget_set_inproc(BudgetSetInput { max_daily_usd: Some(18.5), clear: false, project_root: None });
        let set_is_error = set.is_error;
        let payload = set.structured_content.expect("budget set payload");
        assert_ne!(set_is_error, Some(true), "{payload}");
        assert_eq!(payload.pointer("/result/max_daily_usd").and_then(Value::as_f64), Some(18.5));

        let get = server.budget_get_inproc(None);
        let payload = get.structured_content.expect("budget get payload");
        assert_eq!(payload.pointer("/result/daily_cap/max_daily_usd").and_then(Value::as_f64), Some(18.5));

        let clear = server.budget_set_inproc(BudgetSetInput { max_daily_usd: None, clear: true, project_root: None });
        let payload = clear.structured_content.expect("budget clear payload");
        assert_eq!(payload.pointer("/result/max_daily_usd").and_then(Value::as_f64), Some(0.0));
    }

    #[test]
    fn cost_and_budget_validation_return_typed_invalid_input_errors() {
        let temp = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(temp.path().to_string_lossy().as_ref());

        let decisions = server
            .cost_decisions_inproc(CostDecisionsInput { since: Some("yesterday".to_string()), project_root: None });
        assert_eq!(decisions.is_error, Some(true));
        let payload = decisions.structured_content.expect("decisions error");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");

        let budget =
            server.budget_set_inproc(BudgetSetInput { max_daily_usd: Some(10.0), clear: true, project_root: None });
        assert_eq!(budget.is_error, Some(true));
        let payload = budget.structured_content.expect("budget error");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");
    }
}
