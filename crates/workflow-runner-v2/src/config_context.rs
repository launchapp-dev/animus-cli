use std::path::Path;

use orchestrator_config::agent_runtime_config::{
    PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode, PhaseOutputContract,
};
use orchestrator_core::AgentRuntimeConfig;
use protocol::PhaseCapabilities;
use serde_json::Value;

pub struct RuntimeConfigContext {
    pub agent_runtime_config: AgentRuntimeConfig,
    pub workflow_config: orchestrator_core::LoadedWorkflowConfig,
}

impl RuntimeConfigContext {
    pub fn load(project_root: &str) -> Self {
        let agent_runtime_config = orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root));
        let workflow_config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
        Self { agent_runtime_config, workflow_config }
    }

    pub fn phase_execution(&self, phase_id: &str) -> Option<&PhaseExecutionDefinition> {
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .or_else(|| self.agent_runtime_config.phase_execution(phase_id))
    }

    pub fn phase_mode(&self, phase_id: &str) -> PhaseExecutionMode {
        self.phase_execution(phase_id).map(|def| def.mode.clone()).unwrap_or(PhaseExecutionMode::Agent)
    }

    pub fn phase_agent_id(&self, phase_id: &str) -> Option<String> {
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.agent_id.clone())
            .or_else(|| self.agent_runtime_config.phase_agent_id(phase_id).map(ToOwned::to_owned))
    }

    pub fn phase_system_prompt(&self, phase_id: &str) -> Option<String> {
        self.agent_runtime_config.phase_system_prompt(phase_id).map(ToOwned::to_owned)
    }

    pub fn phase_directive(&self, phase_id: &str) -> String {
        self.agent_runtime_config
            .phase_directive(phase_id)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Execute the current workflow phase with production-quality output.".to_string())
    }

    pub fn phase_capabilities(&self, phase_id: &str) -> PhaseCapabilities {
        self.agent_runtime_config.phase_capabilities(phase_id)
    }

    pub fn phase_output_contract(&self, phase_id: &str) -> Option<&PhaseOutputContract> {
        self.agent_runtime_config.phase_output_contract(phase_id)
    }

    pub fn phase_mcp_servers(&self, phase_id: &str) -> Vec<String> {
        self.workflow_config
            .config
            .phase_mcp_bindings
            .get(phase_id)
            .map(|binding| binding.servers.clone())
            .unwrap_or_default()
    }

    pub fn phase_output_json_schema(&self, phase_id: &str) -> Option<&Value> {
        self.agent_runtime_config.phase_output_json_schema(phase_id)
    }

    pub fn phase_decision_contract(&self, phase_id: &str) -> Option<&PhaseDecisionContract> {
        self.agent_runtime_config.phase_decision_contract(phase_id)
    }

    pub fn phase_tool_override(&self, phase_id: &str) -> Option<String> {
        self.agent_runtime_config.phase_tool_override(phase_id).map(ToOwned::to_owned)
    }

    pub fn phase_model_override(&self, phase_id: &str) -> Option<String> {
        self.agent_runtime_config.phase_model_override(phase_id).map(ToOwned::to_owned)
    }

    pub fn phase_fallback_models(&self, phase_id: &str) -> Vec<String> {
        self.agent_runtime_config.phase_fallback_models(phase_id)
    }

    pub fn phase_fallback_tools(&self, phase_id: &str) -> Vec<String> {
        self.agent_runtime_config.phase_fallback_tools(phase_id)
    }

    pub fn phase_command(&self, phase_id: &str) -> Option<&PhaseCommandDefinition> {
        self.phase_execution(phase_id).and_then(|def| def.command.as_ref())
    }

    /// Resolved provider permission/approval mode for a phase, consulting
    /// the workflow YAML overlay first: the YAML phase
    /// `runtime.permission_mode` wins over the agent-runtime phase
    /// `runtime.permission_mode`, which wins over the phase agent
    /// profile's `permission_mode` (agent id resolved YAML-first, profile
    /// looked up in the YAML `agents:` overlay first).
    // TODO(codex-p2): workflow validation accepts case-insensitive phase
    // references, but every accessor in this module (phase_execution,
    // phase_agent_id, and this one) does exact-match `get(phase_id)`
    // lookups, so a differently-cased reference falls through to the
    // agent-runtime defaults. Fixing only this accessor would make the
    // module's semantics inconsistent; normalize the lookup for ALL
    // accessors in one pass in a follow-up.
    pub fn phase_permission_mode(&self, phase_id: &str) -> Option<String> {
        if let Some(value) = self
            .workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.runtime.as_ref())
            .and_then(|runtime| runtime.permission_mode.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
        if let Some(value) = self
            .agent_runtime_config
            .phase_execution(phase_id)
            .and_then(|def| def.runtime.as_ref())
            .and_then(|runtime| runtime.permission_mode.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
        let agent_id = self.phase_agent_id(phase_id)?;
        self.workflow_config
            .config
            .agent_profiles
            .get(&agent_id)
            .and_then(|profile| profile.permission_mode.as_deref())
            .or_else(|| {
                self.agent_runtime_config
                    .agent_profile(&agent_id)
                    .and_then(|profile| profile.permission_mode.as_deref())
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    /// True when the phase's agent profile (agent id resolved YAML-first,
    /// profile looked up in the YAML `agents:` overlay first) carries an
    /// `approval_policy`.
    pub fn phase_has_approval_policy(&self, phase_id: &str) -> bool {
        let Some(agent_id) = self.phase_agent_id(phase_id) else {
            return false;
        };
        self.workflow_config
            .config
            .agent_profiles
            .get(&agent_id)
            .and_then(|profile| profile.approval_policy.as_ref())
            .or_else(|| {
                self.agent_runtime_config.agent_profile(&agent_id).and_then(|profile| profile.approval_policy.as_ref())
            })
            .is_some()
    }
}
