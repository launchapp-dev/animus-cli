use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::AoMcpServer;
use crate::services::operations::{
    output_artifacts_application, output_jsonl_application, output_monitor_application,
    output_phase_outputs_application, output_read_application,
};
use anyhow::Result;
use rmcp::model::CallToolResult;
use serde::Serialize;
use serde_json::json;

impl AoMcpServer {
    fn run_output_application<T, F>(
        &self,
        tool_name: &str,
        project_root: Option<String>,
        actor_bound: bool,
        call: F,
    ) -> CallToolResult
    where
        T: Serialize,
        F: FnOnce(&str, Option<&animus_actor::Actor>) -> Result<T>,
    {
        self.audit_actor_tool_decision(tool_name, actor_bound, if actor_bound { "forward" } else { "management-only" });
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match call(&project_root, self.pinned_actor()) {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(error) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &error)),
        }
    }

    pub(super) fn output_run_inproc(&self, input: super::RunIdInput) -> CallToolResult {
        self.run_output_application("animus.output.run", input.project_root, true, |root, actor| {
            output_read_application(root, Some(&input.run_id), None, None, actor)
        })
    }

    pub(super) fn output_phase_outputs_inproc(&self, input: super::OutputPhaseOutputsInput) -> CallToolResult {
        self.run_output_application("animus.output.phase-outputs", input.project_root, true, |root, actor| {
            output_phase_outputs_application(root, &input.workflow_id, input.phase_id.as_deref(), actor)
        })
    }

    pub(super) fn output_monitor_inproc(&self, input: super::OutputMonitorInput) -> CallToolResult {
        self.run_output_application("animus.output.monitor", input.project_root, false, |root, _actor| {
            output_monitor_application(root, &input.run_id, input.task_id.as_deref(), input.phase_id.as_deref())
        })
    }

    pub(super) fn output_jsonl_inproc(&self, input: super::OutputJsonlInput) -> CallToolResult {
        self.run_output_application("animus.output.jsonl", input.project_root, false, |root, _actor| {
            output_jsonl_application(root, &input.run_id, input.entries)
        })
    }

    pub(super) fn output_artifacts_inproc(&self, input: super::ExecutionIdInput) -> CallToolResult {
        self.run_output_application("animus.output.artifacts", input.project_root, false, |root, _actor| {
            output_artifacts_application(root, &input.execution_id)
        })
    }
}

#[cfg(test)]
mod tests {
    use protocol::test_utils::EnvVarGuard;
    use protocol::RunId;
    use serde_json::Value;

    #[test]
    fn output_read_is_in_process_and_conceals_cross_actor_runs() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp home");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let alice = animus_actor::Actor {
            user_id: "alice".to_string(),
            claims: Vec::new(),
            tenant_id: Some("workspace-a".to_string()),
        };
        let bob = animus_actor::Actor {
            user_id: "bob".to_string(),
            claims: Vec::new(),
            tenant_id: Some("workspace-a".to_string()),
        };
        let workflow_id = "wf-owned";
        let manager = orchestrator_core::WorkflowStateManager::new(&project_root);
        let workflow = protocol::orchestrator::OrchestratorWorkflow {
            id: workflow_id.to_string(),
            execution_fence: None,
            task_id: "TASK-971".to_string(),
            workflow_ref: Some("standard".to_string()),
            subject: None,
            input: None,
            vars: std::collections::HashMap::new(),
            status: protocol::orchestrator::WorkflowStatus::Running,
            current_phase_index: 0,
            phases: Vec::new(),
            machine_state: Default::default(),
            current_phase: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            failure_reason: None,
            checkpoint_metadata: Default::default(),
            rework_counts: std::collections::HashMap::new(),
            total_reworks: 0,
            decision_history: Vec::new(),
        };
        manager.save(&workflow).expect("persist workflow");
        manager.set_workflow_actor(workflow_id, Some(&alice)).expect("persist workflow owner");
        let run_dir = crate::run_dir(project_root.to_string_lossy().as_ref(), &RunId(workflow_id.to_string()), None);
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(run_dir.join("events.jsonl"), "{\"type\":\"assistant\",\"text\":\"ready\"}\n").expect("events");
        let phase_dir =
            animus_runtime_shared::phase_output::phase_output_dir(project_root.to_string_lossy().as_ref(), workflow_id);
        std::fs::create_dir_all(&phase_dir).expect("phase output dir");
        std::fs::write(
            phase_dir.join("build.json"),
            "{\"phase_id\":\"build\",\"completed_at\":\"2026-07-29T00:00:00Z\",\"verdict\":\"advance\"}",
        )
        .expect("phase output");

        let root = project_root.to_string_lossy();
        let alice_server = super::super::new_ao_mcp_server_with_options(root.as_ref(), false, None, None, Some(alice));
        let bob_server = super::super::new_ao_mcp_server_with_options(root.as_ref(), false, None, None, Some(bob));
        let input = || super::super::RunIdInput { run_id: workflow_id.to_string(), project_root: None };

        let allowed = alice_server.output_run_inproc(input());
        let allowed_is_error = allowed.is_error;
        let payload = allowed.structured_content.expect("owned output");
        assert_ne!(allowed_is_error, Some(true), "{payload}");
        assert_eq!(payload.pointer("/result/0/text").and_then(Value::as_str), Some("ready"), "{payload}");

        let denied = bob_server.output_run_inproc(input());
        assert_eq!(denied.is_error, Some(true));
        let payload = denied.structured_content.expect("concealed output");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"), "{payload}");

        let phase_input = || super::super::OutputPhaseOutputsInput {
            workflow_id: workflow_id.to_string(),
            phase_id: Some("build".to_string()),
            project_root: None,
        };
        let allowed = alice_server.output_phase_outputs_inproc(phase_input());
        let allowed_is_error = allowed.is_error;
        let payload = allowed.structured_content.expect("owned phase output");
        assert_ne!(allowed_is_error, Some(true), "{payload}");
        assert_eq!(payload.pointer("/result/outputs/0/verdict").and_then(Value::as_str), Some("advance"));

        let denied = bob_server.output_phase_outputs_inproc(phase_input());
        assert_eq!(denied.is_error, Some(true));
        let payload = denied.structured_content.expect("concealed phase output");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"), "{payload}");
    }

    #[test]
    fn jsonl_and_monitor_share_typed_run_entries_without_a_child_cli() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp home");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let run_id = "run-output";
        let run_dir = crate::run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None);
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("stdout.jsonl"),
            "{\"task_id\":\"TASK-971\",\"phase_id\":\"build\",\"text\":\"keep\"}\n{\"task_id\":\"OTHER\",\"phase_id\":\"build\",\"text\":\"drop\"}\n",
        )
        .expect("stdout events");
        let server = super::super::new_ao_mcp_server(project_root.to_string_lossy().as_ref());

        let jsonl = server.output_jsonl_inproc(super::super::OutputJsonlInput {
            run_id: run_id.to_string(),
            entries: true,
            project_root: None,
        });
        let payload = jsonl.structured_content.expect("jsonl output");
        assert_eq!(payload.pointer("/result/0/source_file").and_then(Value::as_str), Some("stdout.jsonl"));

        let monitor = server.output_monitor_inproc(super::super::OutputMonitorInput {
            run_id: run_id.to_string(),
            task_id: Some("TASK-971".to_string()),
            phase_id: Some("build".to_string()),
            project_root: None,
        });
        let payload = monitor.structured_content.expect("monitor output");
        assert_eq!(payload.pointer("/result/0/text").and_then(Value::as_str), Some("keep"));
        assert_eq!(payload.pointer("/result").and_then(Value::as_array).map(Vec::len), Some(1));
    }
}
