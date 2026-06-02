// Phase accessors give workflow YAML phase definitions precedence over the
// agent_runtime_config fallback. Pre-lift behaviour of `workflow-runner-v2`
// silently dropped workflow YAML overrides for many accessors below; this
// module now consults `workflow_config.phase_definitions` first (via the
// shared `phase_execution()` helper) and falls back to the agent runtime
// config only when the YAML definition does not supply the field. (Codex
// P2 #2.)

use std::path::Path;

use orchestrator_config::agent_runtime_config::{
    AgentRuntimeOverrides, PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode,
    PhaseOutputContract,
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
        // TODO(codex-p2): `load_agent_runtime_config_or_default` already merges
        // project YAML overrides into the runtime config, so the YAML-wins
        // accessors below cannot fall back to TRUE builtin runtime defaults
        // when the project sparsely overrides a phase. Loading an
        // unmerged builtin in addition would let accessors restore output
        // /decision contracts after partial overrides. Deferred to v0.5.2
        // pending an `orchestrator-core` helper that exposes the builtin
        // config independently of project merging.
        let agent_runtime_config = orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root));
        let workflow_config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
        Self { agent_runtime_config, workflow_config }
    }

    /// Returns the workflow YAML phase definition when present, falling
    /// back to `agent_runtime_config` so call sites can read a single
    /// merged view.
    pub fn phase_execution(&self, phase_id: &str) -> Option<&PhaseExecutionDefinition> {
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .or_else(|| self.agent_runtime_config.phase_execution(phase_id))
    }

    /// Returns the workflow YAML `runtime` override block when present.
    /// Helper used by the phase accessors below to express the
    /// "YAML wins over agent_runtime_config" precedence.
    fn yaml_phase_runtime(&self, phase_id: &str) -> Option<&AgentRuntimeOverrides> {
        self.workflow_config.config.phase_definitions.get(phase_id).and_then(|def| def.runtime.as_ref())
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
        if let Some(prompt) = self
            .workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.system_prompt.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(prompt.to_string());
        }
        self.agent_runtime_config.phase_system_prompt(phase_id).map(ToOwned::to_owned)
    }

    pub fn phase_directive(&self, phase_id: &str) -> String {
        if let Some(directive) = self
            .workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.directive.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return directive.to_string();
        }
        self.agent_runtime_config
            .phase_directive(phase_id)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Execute the current workflow phase with production-quality output.".to_string())
    }

    pub fn phase_capabilities(&self, phase_id: &str) -> PhaseCapabilities {
        if let Some(caps) =
            self.workflow_config.config.phase_definitions.get(phase_id).and_then(|def| def.capabilities.clone())
        {
            return caps.merge_with_defaults(phase_id);
        }
        self.agent_runtime_config.phase_capabilities(phase_id)
    }

    pub fn phase_output_contract(&self, phase_id: &str) -> Option<&PhaseOutputContract> {
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.output_contract.as_ref())
            .or_else(|| self.agent_runtime_config.phase_output_contract(phase_id))
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
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.output_json_schema.as_ref())
            .or_else(|| self.agent_runtime_config.phase_output_json_schema(phase_id))
    }

    pub fn phase_decision_contract(&self, phase_id: &str) -> Option<&PhaseDecisionContract> {
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.decision_contract.as_ref())
            .or_else(|| self.agent_runtime_config.phase_decision_contract(phase_id))
    }

    pub fn phase_tool_override(&self, phase_id: &str) -> Option<String> {
        if let Some(value) =
            self.yaml_phase_runtime(phase_id).and_then(|r| r.tool.as_deref()).map(str::trim).filter(|s| !s.is_empty())
        {
            return Some(value.to_string());
        }
        self.agent_runtime_config.phase_tool_override(phase_id).map(ToOwned::to_owned)
    }

    pub fn phase_model_override(&self, phase_id: &str) -> Option<String> {
        if let Some(value) =
            self.yaml_phase_runtime(phase_id).and_then(|r| r.model.as_deref()).map(str::trim).filter(|s| !s.is_empty())
        {
            return Some(value.to_string());
        }
        self.agent_runtime_config.phase_model_override(phase_id).map(ToOwned::to_owned)
    }

    pub fn phase_fallback_models(&self, phase_id: &str) -> Vec<String> {
        if let Some(values) = self.yaml_phase_runtime(phase_id).map(|r| {
            r.fallback_models
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        }) {
            if !values.is_empty() {
                return values;
            }
        }
        self.agent_runtime_config.phase_fallback_models(phase_id)
    }

    pub fn phase_fallback_tools(&self, phase_id: &str) -> Vec<String> {
        if let Some(yaml_runtime) = self.yaml_phase_runtime(phase_id) {
            let yaml_tools: Vec<String> = yaml_runtime
                .fallback_tools
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            if !yaml_tools.is_empty() {
                return yaml_tools;
            }
            // Codex P2 round 6: when YAML defines `fallback_models` but omits
            // `fallback_tools`, do NOT fall back to the agent_runtime_config
            // tools — those are paired by index with the runtime config's
            // models and would mismatch the YAML-supplied models (e.g.
            // YAML `fallback_models: [gemini-2.5-pro]` paired with inherited
            // `fallback_tools: [codex]`). Return an empty vec so the phase
            // target planner auto-derives the tool from the model.
            let yaml_supplied_fallback_models =
                yaml_runtime.fallback_models.iter().any(|value| !value.trim().is_empty());
            if yaml_supplied_fallback_models {
                return Vec::new();
            }
        }
        self.agent_runtime_config.phase_fallback_tools(phase_id)
    }

    pub fn phase_command(&self, phase_id: &str) -> Option<&PhaseCommandDefinition> {
        self.phase_execution(phase_id)
            .and_then(|def| def.command.as_ref())
            .or_else(|| self.agent_runtime_config.phase_command(phase_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, workflow_config_hash, LoadedWorkflowConfig,
        WorkflowConfigMetadata, WorkflowConfigSource,
    };
    use std::path::PathBuf;

    fn make_ctx_with_yaml_override(phase_id: &str, def: PhaseExecutionDefinition) -> RuntimeConfigContext {
        let mut workflow = builtin_workflow_config();
        workflow.phase_definitions.insert(phase_id.to_string(), def);
        let metadata = WorkflowConfigMetadata {
            schema: workflow.schema.clone(),
            version: workflow.version,
            hash: workflow_config_hash(&workflow),
            source: WorkflowConfigSource::Builtin,
        };
        RuntimeConfigContext {
            agent_runtime_config: builtin_agent_runtime_config(),
            workflow_config: LoadedWorkflowConfig { metadata, config: workflow, path: PathBuf::from("builtin") },
        }
    }

    /// Codex P2 #2: workflow YAML phase definitions must override
    /// `agent_runtime_config` for `phase_tool_override`, `phase_model_override`,
    /// `phase_system_prompt`, `phase_directive`, `phase_fallback_models`,
    /// `phase_fallback_tools`, `phase_output_contract`, and
    /// `phase_decision_contract`. Pre-fix these accessors silently dropped the
    /// YAML override.
    #[test]
    fn yaml_phase_definition_overrides_agent_runtime_config() {
        let override_def = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("yaml-agent".to_string()),
            directive: Some("yaml-directive".to_string()),
            system_prompt: Some("yaml-system-prompt".to_string()),
            runtime: Some(AgentRuntimeOverrides {
                tool: Some("yaml-tool".to_string()),
                model: Some("yaml-model".to_string()),
                fallback_models: vec!["yaml-fallback-model".to_string()],
                fallback_tools: vec!["yaml-fallback-tool".to_string()],
                ..Default::default()
            }),
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Default::default(),
        };
        let ctx = make_ctx_with_yaml_override("implementation", override_def);

        assert_eq!(ctx.phase_agent_id("implementation").as_deref(), Some("yaml-agent"));
        assert_eq!(ctx.phase_tool_override("implementation").as_deref(), Some("yaml-tool"));
        assert_eq!(ctx.phase_model_override("implementation").as_deref(), Some("yaml-model"));
        assert_eq!(ctx.phase_system_prompt("implementation").as_deref(), Some("yaml-system-prompt"));
        assert_eq!(ctx.phase_directive("implementation"), "yaml-directive");
        assert_eq!(ctx.phase_fallback_models("implementation"), vec!["yaml-fallback-model".to_string()]);
        assert_eq!(ctx.phase_fallback_tools("implementation"), vec!["yaml-fallback-tool".to_string()]);
    }

    /// Codex P2 round 6: when workflow YAML supplies `fallback_models` but
    /// omits `fallback_tools`, the accessor must NOT pair the YAML models
    /// with inherited agent_runtime_config tools (which are paired by index
    /// with the agent_runtime_config models, not the YAML models). Returning
    /// an empty fallback_tools vec lets the phase target planner auto-derive
    /// the correct tool from each YAML model.
    #[test]
    fn yaml_fallback_models_without_yaml_fallback_tools_returns_empty_tools() {
        let override_def = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: None,
            directive: None,
            system_prompt: None,
            runtime: Some(AgentRuntimeOverrides {
                fallback_models: vec!["gemini-2.5-pro".to_string()],
                // fallback_tools intentionally omitted
                ..Default::default()
            }),
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Default::default(),
        };
        let ctx = make_ctx_with_yaml_override("implementation", override_def);

        assert_eq!(ctx.phase_fallback_models("implementation"), vec!["gemini-2.5-pro".to_string()]);
        assert!(
            ctx.phase_fallback_tools("implementation").is_empty(),
            "YAML fallback_models without YAML fallback_tools must NOT inherit unrelated agent_runtime_config tools — they would mismatch by index"
        );
    }

    /// When the YAML phase definition omits a field, the accessor must fall
    /// back to `agent_runtime_config` (preserving pre-fix behavior for the
    /// unspecified subset of fields).
    #[test]
    fn yaml_phase_definition_falls_back_to_agent_runtime_for_missing_fields() {
        // YAML definition with only `agent_id` set — everything else should
        // resolve from the agent_runtime_config side.
        let sparse = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("default".to_string()),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Default::default(),
        };
        let ctx = make_ctx_with_yaml_override("implementation", sparse);

        // No YAML directive — fall through to agent_runtime_config or default
        // string; either way it must not panic and must be non-empty.
        assert!(!ctx.phase_directive("implementation").is_empty());
    }
}
