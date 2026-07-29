use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::AoMcpServer;
use crate::services::runtime::{
    agent_get_application, agent_list_application, agent_memory_append_application, agent_memory_clear_application,
    agent_memory_get_application, agent_message_list_application, agent_message_send_application,
};
use anyhow::Result;
use rmcp::model::CallToolResult;
use serde_json::{json, Value};

impl AoMcpServer {
    fn run_agent_application<F>(
        &self,
        tool_name: &str,
        project_root: Option<String>,
        actor_bound: bool,
        call: F,
    ) -> CallToolResult
    where
        F: FnOnce(&str, Option<&animus_actor::Actor>) -> Result<Value>,
    {
        self.audit_actor_tool_decision(tool_name, actor_bound, if actor_bound { "forward" } else { "management-only" });
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match call(&project_root, self.pinned_actor()) {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(error) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &error)),
        }
    }

    pub(super) fn agent_list_inproc(&self, project_root: Option<String>) -> CallToolResult {
        self.run_agent_application("animus.agent.list", project_root, true, agent_list_application)
    }

    pub(super) fn agent_get_inproc(&self, input: super::AgentProfileInput) -> CallToolResult {
        self.run_agent_application("animus.agent.get", input.project_root, true, |root, actor| {
            agent_get_application(root, &input.id, actor)
        })
    }

    pub(super) fn agent_memory_get_inproc(&self, input: super::AgentMemoryGetInput) -> CallToolResult {
        self.run_agent_application("animus.agent.memory.get", input.project_root, false, |root, _actor| {
            agent_memory_get_application(root, &input.agent)
        })
    }

    pub(super) fn agent_memory_append_inproc(&self, input: super::AgentMemoryAppendInput) -> CallToolResult {
        self.run_agent_application("animus.agent.memory.append", input.project_root, false, |root, _actor| {
            agent_memory_append_application(root, &input.agent, &input.text, input.source.as_deref())
        })
    }

    pub(super) fn agent_memory_clear_inproc(&self, input: super::AgentMemoryGetInput) -> CallToolResult {
        self.run_agent_application("animus.agent.memory.clear", input.project_root, false, |root, _actor| {
            agent_memory_clear_application(root, &input.agent)
        })
    }

    pub(super) fn agent_message_send_inproc(&self, input: super::AgentMessageSendInput) -> CallToolResult {
        self.run_agent_application("animus.agent.message.send", input.project_root, false, |root, _actor| {
            agent_message_send_application(
                root,
                &input.channel,
                &input.from,
                input.to.as_deref(),
                &input.text,
                input.workflow_id.as_deref(),
                input.phase_id.as_deref(),
            )
        })
    }

    pub(super) fn agent_message_list_inproc(&self, input: super::AgentMessageListInput) -> CallToolResult {
        self.run_agent_application("animus.agent.message.list", input.project_root, false, |root, _actor| {
            agent_message_list_application(root, input.channel.as_deref(), input.agent.as_deref(), input.limit)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_project(
    ) -> (tempfile::TempDir, orchestrator_config::workflow_config::config_source_client::test_seam::TestBaseGuard) {
        let root = tempfile::tempdir().expect("project root");
        let animus = root.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("animus dir");
        std::fs::write(
            animus.join("workflows.yaml"),
            "tools_allowlist:\n  - cargo\nphases:\n  work:\n    mode: agent\n    agent_id: rafael\nagents:\n  rafael:\n    description: Workspace agent\n    system_prompt: Help the team\n    skills: []\n    memory:\n      enabled: true\n      max_entries: 2\nworkflows:\n  - id: standard\n    name: Standard\n    phases:\n      - work\n",
        )
        .expect("workflow config");
        let guard =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());
        (root, guard)
    }

    #[test]
    fn agent_list_and_get_return_typed_in_process_profiles() {
        let (root, _guard) = configured_project();
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());

        let list = server.agent_list_inproc(None);
        assert_ne!(list.is_error, Some(true));
        let payload = list.structured_content.expect("list payload");
        assert_eq!(payload.pointer("/result/agents/0/id").and_then(Value::as_str), Some("rafael"), "{payload}");

        let get =
            server.agent_get_inproc(super::super::AgentProfileInput { id: "rafael".to_string(), project_root: None });
        assert_ne!(get.is_error, Some(true));
        let payload = get.structured_content.expect("get payload");
        assert_eq!(payload.pointer("/result/id").and_then(Value::as_str), Some("rafael"), "{payload}");
    }

    #[test]
    fn agent_memory_mutations_share_the_typed_application_store() {
        let (root, _guard) = configured_project();
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());

        let append = server.agent_memory_append_inproc(super::super::AgentMemoryAppendInput {
            agent: "rafael".to_string(),
            text: "Arena is the shared workplace".to_string(),
            source: Some("test".to_string()),
            project_root: None,
        });
        let append_is_error = append.is_error;
        let append_payload = append.structured_content.expect("append payload");
        assert_ne!(append_is_error, Some(true), "{append_payload}");

        let get = server.agent_memory_get_inproc(super::super::AgentMemoryGetInput {
            agent: "rafael".to_string(),
            project_root: None,
        });
        let payload = get.structured_content.expect("memory payload");
        assert_eq!(
            payload.pointer("/result/entries/0/text").and_then(Value::as_str),
            Some("Arena is the shared workplace"),
            "{payload}"
        );
    }

    #[test]
    fn agent_message_validation_returns_a_typed_not_found_error() {
        let (root, _guard) = configured_project();
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server.agent_message_send_inproc(super::super::AgentMessageSendInput {
            channel: "missing".to_string(),
            from: "rafael".to_string(),
            to: None,
            text: "hello".to_string(),
            workflow_id: None,
            phase_id: None,
            project_root: None,
        });

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"), "{payload}");
    }

    #[test]
    fn actor_bound_agent_rosters_are_loaded_from_each_users_partition() {
        use orchestrator_config::workflow_config::config_source_client::test_seam;

        let root = tempfile::tempdir().expect("project root");
        let animus = root.path().join(".animus");
        std::fs::create_dir_all(&animus).expect("animus dir");
        let config_for = |agent: &str| {
            format!(
                "tools_allowlist:\n  - cargo\nphases:\n  work:\n    mode: agent\n    agent_id: {agent}\nagents:\n  {agent}:\n    description: Workspace agent\n    system_prompt: Help the team\n    skills: []\nworkflows:\n  - id: standard\n    name: Standard\n    phases:\n      - work\n"
            )
        };

        std::fs::write(animus.join("workflows.yaml"), config_for("rafael")).expect("alice workflow config");
        let alice_base = orchestrator_core::compile_yaml_workflow_files(root.path())
            .expect("compile alice config")
            .expect("alice config present");
        std::fs::write(animus.join("workflows.yaml"), config_for("maria")).expect("bob workflow config");
        let bob_base = orchestrator_core::compile_yaml_workflow_files(root.path())
            .expect("compile bob config")
            .expect("bob config present");

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
        let _alice_guard = test_seam::install_for_actor(root.path(), &alice, alice_base);
        let _bob_guard = test_seam::install_for_actor(root.path(), &bob, bob_base);
        let project_root = root.path().to_string_lossy();
        let alice_server =
            super::super::new_ao_mcp_server_with_options(project_root.as_ref(), false, None, None, Some(alice));
        let bob_server =
            super::super::new_ao_mcp_server_with_options(project_root.as_ref(), false, None, None, Some(bob));

        let alice_payload = alice_server.agent_list_inproc(None).structured_content.expect("alice roster");
        let bob_payload = bob_server.agent_list_inproc(None).structured_content.expect("bob roster");
        assert_eq!(alice_payload.pointer("/result/agents/0/id").and_then(Value::as_str), Some("rafael"));
        assert_eq!(bob_payload.pointer("/result/agents/0/id").and_then(Value::as_str), Some("maria"));
    }
}
