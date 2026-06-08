// Phase accessors give workflow YAML phase definitions precedence over the
// agent_runtime_config fallback. Pre-lift behaviour of `workflow-runner-v2`
// silently dropped workflow YAML overrides for many accessors below; this
// module now consults `workflow_config.phase_definitions` first (via the
// shared `phase_execution()` helper) and falls back to the agent runtime
// config only when the YAML definition does not supply the field. (Codex
// P2 #2.)

use std::path::Path;
use std::sync::OnceLock;

use orchestrator_config::agent_runtime_config::{
    AgentRuntimeOverrides, PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode,
    PhaseOutputContract,
};
use orchestrator_core::AgentRuntimeConfig;
use protocol::PhaseCapabilities;
use serde_json::Value;

fn builtin_runtime_config() -> &'static AgentRuntimeConfig {
    static BUILTIN: OnceLock<AgentRuntimeConfig> = OnceLock::new();
    BUILTIN.get_or_init(orchestrator_core::builtin_agent_runtime_config)
}

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
        if let Some(caps) =
            self.agent_runtime_config.phase_execution(phase_id).and_then(|def| def.capabilities.as_ref()).cloned()
        {
            return caps.merge_with_defaults(phase_id);
        }
        builtin_runtime_config().phase_capabilities(phase_id)
    }

    pub fn phase_output_contract(&self, phase_id: &str) -> Option<&PhaseOutputContract> {
        if let Some(value) =
            self.workflow_config.config.phase_definitions.get(phase_id).and_then(|def| def.output_contract.as_ref())
        {
            return Some(value);
        }
        if let Some(value) = self.agent_runtime_config.phase_output_contract(phase_id) {
            return Some(value);
        }
        // Only fall back to the builtin contract when the agent_runtime
        // phase also lacks an explicit `output_json_schema`. Mirror of the
        // `phase_output_json_schema` rule: a schema-only override (custom
        // schema, no contract) must not re-graft the builtin contract.
        if self.agent_runtime_config.phase_output_json_schema(phase_id).is_none() {
            return builtin_runtime_config().phase_output_contract(phase_id);
        }
        None
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
        if let Some(value) =
            self.workflow_config.config.phase_definitions.get(phase_id).and_then(|def| def.output_json_schema.as_ref())
        {
            return Some(value);
        }
        if let Some(value) = self.agent_runtime_config.phase_output_json_schema(phase_id) {
            return Some(value);
        }
        // Only fall back to the builtin schema when the agent_runtime phase
        // is a sparse YAML override that ALSO omitted `output_contract`. If
        // the project supplied a custom contract, do not graft the builtin
        // schema on top — it would re-inject builtin `kind`/required fields
        // that contradict the custom contract.
        if self.agent_runtime_config.phase_output_contract(phase_id).is_none() {
            return builtin_runtime_config().phase_output_json_schema(phase_id);
        }
        None
    }

    pub fn phase_decision_contract(&self, phase_id: &str) -> Option<&PhaseDecisionContract> {
        self.workflow_config
            .config
            .phase_definitions
            .get(phase_id)
            .and_then(|def| def.decision_contract.as_ref())
            .or_else(|| self.agent_runtime_config.phase_decision_contract(phase_id))
            .or_else(|| builtin_runtime_config().phase_decision_contract(phase_id))
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

    /// Resolve the active [`EvalsConfig`] for `phase_id`, preferring the
    /// workflow YAML override but falling back to the runtime config when
    /// the YAML phase block is sparse and omits `evals:`. Mirrors
    /// [`Self::phase_command`] so out-of-tree runners that adopt the eval
    /// gate cannot accidentally drop a runtime-side gate by re-stating
    /// only the agent / directive fields in the project YAML.
    ///
    /// TODO(codex-p2): `RuntimeConfigContext::load` applies
    /// `merge_workflow_runtime_overlay` to the runtime config before this
    /// fallback runs, and that merge currently replaces the whole runtime
    /// phase with the sparse YAML phase. Until the merge is taught to
    /// field-merge `evals` (instead of whole-record replace), the
    /// fallback here only protects the in-process accessor surface; the
    /// canonical fix lives in `agent_runtime_config::merge_workflow_runtime_overlay`.
    pub fn phase_evals(&self, phase_id: &str) -> Option<&orchestrator_config::agent_runtime_config::EvalsConfig> {
        self.phase_execution(phase_id)
            .and_then(|def| def.evals.as_ref())
            .or_else(|| self.agent_runtime_config.phase_evals(phase_id))
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
            worktree: None,
            evals: None,
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
            worktree: None,
            evals: None,
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
            worktree: None,
            evals: None,
        };
        let ctx = make_ctx_with_yaml_override("implementation", sparse);

        // No YAML directive — fall through to agent_runtime_config or default
        // string; either way it must not panic and must be non-empty.
        assert!(!ctx.phase_directive("implementation").is_empty());
    }

    /// v0.5.1 #5b: when project YAML sparsely overrides only some phase
    /// fields, the agent_runtime_config merge replaces the entire phase
    /// definition with the sparse YAML one. Accessors for fields the YAML
    /// did NOT supply (e.g. `output_contract`, `decision_contract`,
    /// `output_json_schema`) must fall back to the unmerged builtin so
    /// real defaults are not silently dropped.
    #[test]
    fn sparse_yaml_override_falls_back_to_builtin_for_unspecified_contracts() {
        let mut agent_runtime_config = builtin_agent_runtime_config();
        let sparse = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("swe".to_string()),
            directive: None,
            system_prompt: None,
            runtime: Some(AgentRuntimeOverrides { model: Some("claude-sonnet-4-6".to_string()), ..Default::default() }),
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
            worktree: None,
            evals: None,
        };
        agent_runtime_config.phases.insert("implementation".to_string(), sparse);
        let workflow = builtin_workflow_config();
        let metadata = WorkflowConfigMetadata {
            schema: workflow.schema.clone(),
            version: workflow.version,
            hash: workflow_config_hash(&workflow),
            source: WorkflowConfigSource::Builtin,
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config,
            workflow_config: LoadedWorkflowConfig { metadata, config: workflow, path: PathBuf::from("builtin") },
        };

        assert_eq!(ctx.phase_model_override("implementation").as_deref(), Some("claude-sonnet-4-6"));
        assert!(
            ctx.phase_output_contract("implementation").is_some(),
            "sparse YAML override must not drop the builtin implementation output_contract"
        );
        assert!(
            ctx.phase_decision_contract("implementation").is_some(),
            "sparse YAML override must not drop the builtin implementation decision_contract"
        );
        assert!(
            ctx.phase_output_json_schema("implementation").is_some(),
            "sparse YAML override must not drop the builtin implementation output_json_schema"
        );
    }

    /// v0.5.1 #5b round-2: when the project supplies a CUSTOM `output_contract`
    /// for a phase but omits `output_json_schema`, the accessor must NOT
    /// graft the builtin schema on top. Doing so would re-inject builtin
    /// `kind`/required fields (e.g. `commit_message`) that contradict the
    /// custom contract and break validation of custom-shaped phase output.
    #[test]
    fn custom_output_contract_does_not_inherit_builtin_json_schema() {
        let mut agent_runtime_config = builtin_agent_runtime_config();
        let custom_contract = PhaseOutputContract {
            kind: "custom_implementation_result".to_string(),
            required_fields: vec!["my_custom_field".to_string()],
            fields: std::collections::BTreeMap::new(),
        };
        let sparse = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("swe".to_string()),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: Some(custom_contract),
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Default::default(),
            worktree: None,
            evals: None,
        };
        agent_runtime_config.phases.insert("implementation".to_string(), sparse);
        let workflow = builtin_workflow_config();
        let metadata = WorkflowConfigMetadata {
            schema: workflow.schema.clone(),
            version: workflow.version,
            hash: workflow_config_hash(&workflow),
            source: WorkflowConfigSource::Builtin,
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config,
            workflow_config: LoadedWorkflowConfig { metadata, config: workflow, path: PathBuf::from("builtin") },
        };

        assert_eq!(
            ctx.phase_output_contract("implementation").map(|c| c.kind.as_str()),
            Some("custom_implementation_result")
        );
        assert!(
            ctx.phase_output_json_schema("implementation").is_none(),
            "when the project supplies a custom output_contract, the builtin json_schema must not leak in"
        );
    }

    /// Mirror of `custom_output_contract_does_not_inherit_builtin_json_schema`:
    /// when the project supplies a custom `output_json_schema` but omits
    /// `output_contract`, the accessor must NOT graft the builtin contract on
    /// top — the schema-only path is a supported configuration shape.
    #[test]
    fn custom_output_json_schema_does_not_inherit_builtin_output_contract() {
        let mut agent_runtime_config = builtin_agent_runtime_config();
        let custom_schema = serde_json::json!({
            "type": "object",
            "required": ["my_custom_field"],
            "properties": { "my_custom_field": { "type": "string" } }
        });
        let sparse = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("swe".to_string()),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: Some(custom_schema),
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Default::default(),
            worktree: None,
            evals: None,
        };
        agent_runtime_config.phases.insert("implementation".to_string(), sparse);
        let workflow = builtin_workflow_config();
        let metadata = WorkflowConfigMetadata {
            schema: workflow.schema.clone(),
            version: workflow.version,
            hash: workflow_config_hash(&workflow),
            source: WorkflowConfigSource::Builtin,
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config,
            workflow_config: LoadedWorkflowConfig { metadata, config: workflow, path: PathBuf::from("builtin") },
        };

        assert!(ctx.phase_output_json_schema("implementation").is_some(), "custom output_json_schema must be returned");
        assert!(
            ctx.phase_output_contract("implementation").is_none(),
            "when the project supplies a custom output_json_schema, the builtin output_contract must not leak in"
        );
    }

    #[test]
    fn phase_evals_falls_back_to_runtime_config_on_sparse_yaml_override() {
        // Codex round-10 P2: a sparse workflow YAML override that omits
        // `evals` must NOT clear an eval gate already configured in the
        // agent runtime config. The merged accessor falls back the same
        // way `phase_command` does.
        use orchestrator_config::agent_runtime_config::{EvalCheck, EvalKind, EvalOnFail, EvalsConfig};

        let mut agent_runtime_config = builtin_agent_runtime_config();
        let evals = EvalsConfig {
            pass_threshold: 1.0,
            on_fail: EvalOnFail::Block,
            max_reworks: 0,
            checks: vec![EvalCheck {
                id: "unit-tests".to_string(),
                kind: EvalKind::Command,
                command: Some("cargo".to_string()),
                args: vec!["test".to_string()],
                working_dir: None,
                timeout_secs: None,
                expected_exit: 0,
                agent: None,
                prompt: None,
            }],
        };
        let runtime_phase = agent_runtime_config.phases.get_mut("implementation").expect("implementation phase exists");
        runtime_phase.evals = Some(evals);

        let sparse = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("swe".to_string()),
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
            worktree: None,
            evals: None,
        };
        let mut workflow = builtin_workflow_config();
        workflow.phase_definitions.insert("implementation".to_string(), sparse);
        let metadata = WorkflowConfigMetadata {
            schema: workflow.schema.clone(),
            version: workflow.version,
            hash: workflow_config_hash(&workflow),
            source: WorkflowConfigSource::Builtin,
        };
        let ctx = RuntimeConfigContext {
            agent_runtime_config,
            workflow_config: LoadedWorkflowConfig { metadata, config: workflow, path: PathBuf::from("builtin") },
        };
        let resolved = ctx.phase_evals("implementation").expect("evals must fall back to runtime config");
        assert_eq!(resolved.checks.len(), 1);
        assert_eq!(resolved.checks[0].id, "unit-tests");
    }
}
