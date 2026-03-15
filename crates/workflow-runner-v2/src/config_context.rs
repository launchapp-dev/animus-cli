use std::path::Path;

use orchestrator_config::agent_runtime_config::{
    PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode, PhaseOutputContract,
};
use orchestrator_core::AgentRuntimeConfig;
use protocol::PhaseCapabilities;
use serde_json::Value;

use crate::runtime_support::{load_workflow_runtime_config, WorkflowRuntimeConfigLite};

pub struct RuntimeConfigContext {
    pub agent_runtime_config: AgentRuntimeConfig,
    pub workflow_config: orchestrator_core::LoadedWorkflowConfig,
    pub workflow_runtime_config: WorkflowRuntimeConfigLite,
}

impl RuntimeConfigContext {
    pub fn load(project_root: &str) -> Self {
        let agent_runtime_config = orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root));
        let workflow_config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
        let workflow_runtime_config = load_workflow_runtime_config(project_root);
        Self { agent_runtime_config, workflow_config, workflow_runtime_config }
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

    pub fn phase_command(&self, phase_id: &str) -> Option<&PhaseCommandDefinition> {
        self.phase_execution(phase_id).and_then(|def| def.command.as_ref())
    }
}
