use std::path::Path;

use anyhow::Result;
use orchestrator_config::{
    apply_skill_for_execution, merge_skill_applications, parse_skill_capability_key, preview_skill_application,
    skill_resolution::{resolve_skills_for_project, ResolvedSkill},
    SkillApplicationResult, SkillCapabilityKey,
};
use protocol::PhaseCapabilities;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::config_context::RuntimeConfigContext;

fn collect_phase_skills(ctx: &RuntimeConfigContext, phase_id: &str) -> Vec<String> {
    let wf_phase_skills =
        ctx.workflow_config.config.phase_definitions.get(phase_id).map(|def| &def.skills).filter(|s| !s.is_empty());
    if let Some(skills) = wf_phase_skills {
        return skills.clone();
    }

    if let Some(phase_skills) =
        ctx.agent_runtime_config.phase_execution(phase_id).map(|def| &def.skills).filter(|s| !s.is_empty())
    {
        return phase_skills.clone();
    }

    let agent_id = ctx.phase_agent_id(phase_id);
    if let Some(id) = agent_id.as_deref() {
        let wf_profile_skills =
            ctx.workflow_config.config.agent_profiles.get(id).map(|p| &p.skills).filter(|s| !s.is_empty());
        if let Some(skills) = wf_profile_skills {
            return skills.clone();
        }

        if let Some(profile_skills) =
            ctx.agent_runtime_config.agent_profile(id).map(|p| &p.skills).filter(|s| !s.is_empty())
        {
            return profile_skills.clone();
        }
    }

    Vec::new()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolvedPhaseSkillSet {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_skills: Vec<ResolvedSkill>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppliedPhaseSkills {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_skills: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_skills: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "SkillApplicationResult::is_empty")]
    pub application: SkillApplicationResult,
}

pub fn resolve_phase_skills(
    ctx: &RuntimeConfigContext,
    project_root: &Path,
    phase_id: &str,
) -> Result<ResolvedPhaseSkillSet> {
    let skills = collect_phase_skills(ctx, phase_id);
    if skills.is_empty() {
        return Ok(ResolvedPhaseSkillSet::default());
    }

    let resolved = resolve_skills_for_project(&skills, project_root)?;

    Ok(ResolvedPhaseSkillSet { requested_skills: skills, resolved_skills: resolved })
}

pub fn preview_phase_capabilities(base: &PhaseCapabilities, resolved: &ResolvedPhaseSkillSet) -> PhaseCapabilities {
    let preview_results = resolved
        .resolved_skills
        .iter()
        .filter_map(|skill| preview_skill_application(&skill.definition))
        .collect::<Vec<_>>();
    if preview_results.is_empty() {
        return base.clone();
    }

    let preview = merge_skill_applications(&preview_results);
    apply_skill_capability_overrides(base, &preview.capabilities)
}

pub fn apply_phase_skills(resolved: &ResolvedPhaseSkillSet, tool_id: &str, model_id: &str) -> AppliedPhaseSkills {
    let mut applied_skills = Vec::new();
    let mut applications = Vec::new();

    for skill in &resolved.resolved_skills {
        if let Some(application) = apply_skill_for_execution(&skill.definition, tool_id, Some(model_id)) {
            applied_skills.push(skill.clone());
            applications.push(application);
        }
    }

    AppliedPhaseSkills {
        requested_skills: resolved.requested_skills.clone(),
        resolved_skills: resolved.resolved_skills.clone(),
        applied_skills,
        application: merge_skill_applications(&applications),
    }
}

pub fn apply_skill_capability_overrides(
    base: &PhaseCapabilities,
    overrides: &std::collections::BTreeMap<String, bool>,
) -> PhaseCapabilities {
    let mut caps = base.clone();
    for (name, enabled) in overrides {
        match parse_skill_capability_key(name) {
            Some(SkillCapabilityKey::WritesFiles) => caps.writes_files = *enabled,
            Some(SkillCapabilityKey::MutatesState) => caps.mutates_state = *enabled,
            Some(SkillCapabilityKey::RequiresCommit) => caps.requires_commit = *enabled,
            Some(SkillCapabilityKey::EnforceProductChanges) => caps.enforce_product_changes = *enabled,
            Some(SkillCapabilityKey::IsResearch) => caps.is_research = *enabled,
            Some(SkillCapabilityKey::IsUiUx) => caps.is_ui_ux = *enabled,
            Some(SkillCapabilityKey::IsReview) => caps.is_review = *enabled,
            Some(SkillCapabilityKey::IsTesting) => caps.is_testing = *enabled,
            Some(SkillCapabilityKey::IsRequirements) => caps.is_requirements = *enabled,
            None => {
                warn!(capability = %name, "Ignoring unknown skill capability override");
            }
        }
    }
    caps
}

pub fn inject_skill_overrides(runtime_contract: &mut Value, tool_id: &str, skill_result: &SkillApplicationResult) {
    if !skill_result.extra_args.is_empty() {
        if let Some(args) = runtime_contract.pointer_mut("/cli/launch/args").and_then(Value::as_array_mut) {
            for arg in &skill_result.extra_args {
                if !args.iter().any(|a| a.as_str() == Some(arg)) {
                    args.push(Value::String(arg.clone()));
                }
            }
        }
    }

    if !skill_result.env.is_empty() {
        if runtime_contract.pointer("/cli/launch/env").is_none() {
            if let Some(launch) = runtime_contract.pointer_mut("/cli/launch").and_then(Value::as_object_mut) {
                launch.insert("env".to_string(), Value::Object(serde_json::Map::new()));
            }
        }
        if let Some(env) = runtime_contract.pointer_mut("/cli/launch/env").and_then(Value::as_object_mut) {
            for (key, value) in &skill_result.env {
                env.entry(key.clone()).or_insert(Value::String(value.clone()));
            }
        }
    }

    if !skill_result.codex_config_overrides.is_empty() && tool_id.eq_ignore_ascii_case("codex") {
        if let Some(args) = runtime_contract.pointer_mut("/cli/launch/args").and_then(Value::as_array_mut) {
            for override_val in &skill_result.codex_config_overrides {
                let flag = format!("--config-override={}", override_val);
                if !args.iter().any(|a| a.as_str() == Some(&flag)) {
                    args.push(Value::String(flag));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::build_runtime_contract_with_resume;
    use crate::phase_prompt::{render_phase_prompt_with_ctx_overrides, PhasePromptInputs, PhaseRenderParams};
    use crate::runtime_contract::{inject_named_mcp_servers, set_mcp_tool_policy};

    use orchestrator_config::workflow_config::McpServerDefinition;
    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, workflow_config_hash, write_agent_runtime_config,
        write_workflow_config, LoadedWorkflowConfig, WorkflowConfigMetadata, WorkflowConfigSource,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_installed_skill(temp: &TempDir) {
        let scoped = protocol::scoped_state_root(temp.path()).expect("scoped state root");
        let state_dir = scoped.join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let registry = json!({
            "installed": [{
                "name": "registry-review",
                "version": "1.2.3",
                "source": "acme/registry-review",
                "registry": "catalog",
                "integrity": "sha256:test",
                "artifact": "registry-review-1.2.3.tgz",
                "definition": {
                    "name": "registry-review",
                    "description": "Registry-backed review skill",
                    "activation": {
                        "tools": ["codex"]
                    },
                    "prompt": {
                        "system": "Registry system prompt",
                        "prefix": "Registry prefix",
                        "suffix": "Registry suffix",
                        "directives": ["Validate references"]
                    },
                    "tool_policy": {
                        "allow": ["Read"],
                        "deny": ["Write"]
                    },
                    "model": {
                        "preferred": "gemini-2.5-pro"
                    },
                    "mcp_servers": ["docs"],
                    "capabilities": {
                        "writes_files": false
                    },
                    "extra_args": ["--skill-flag"],
                    "env": {
                        "SKILL_MODE": "review"
                    },
                    "codex_config_overrides": ["profile=review"]
                }
            }]
        });
        std::fs::write(state_dir.join("skills-registry.v1.json"), serde_json::to_vec_pretty(&registry).expect("json"))
            .expect("registry state");
    }

    #[test]
    fn runtime_resolves_installed_registry_skills_into_prompt_and_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow = builtin_workflow_config();
        workflow.mcp_servers.insert(
            "docs".to_string(),
            McpServerDefinition {
                command: "docs-mcp".to_string(),
                args: vec!["--serve".to_string()],
                transport: None,
                url: None,
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::from([("DOCS_TOKEN".to_string(), "abc123".to_string())]),
            },
        );
        write_workflow_config(temp.path(), &workflow).expect("workflow config");
        write_installed_skill(&temp);
        let mut runtime = builtin_agent_runtime_config();
        runtime.phases.get_mut("implementation").expect("implementation phase").skills =
            vec!["registry-review".to_string()];
        write_agent_runtime_config(temp.path(), &runtime).expect("runtime config");

        let project_root = temp.path().to_string_lossy().to_string();
        let ctx = RuntimeConfigContext::load(&project_root);
        let resolved = resolve_phase_skills(&ctx, temp.path(), "implementation").expect("resolve skills");
        assert_eq!(resolved.requested_skills, vec!["registry-review"]);
        assert!(matches!(
            resolved.resolved_skills.first().map(|skill| &skill.source),
            Some(orchestrator_config::skill_scoping::SkillSourceOrigin::Installed { .. })
        ));

        let preview_caps = preview_phase_capabilities(&ctx.phase_capabilities("implementation"), &resolved);
        let applied = apply_phase_skills(&resolved, "codex", "gpt-5.3-codex");
        let effective_caps = apply_skill_capability_overrides(&preview_caps, &applied.application.capabilities);
        assert_eq!(applied.application.model.as_deref(), Some("gemini-2.5-pro"));
        assert!(!effective_caps.writes_files, "skill capability should override implementation write default");

        let rendered = render_phase_prompt_with_ctx_overrides(
            &ctx,
            &PhaseRenderParams {
                project_root: &project_root,
                execution_cwd: &project_root,
                workflow_id: "wf-test",
                subject_id: "TASK-620",
                subject_title: "Runtime skill integration",
                subject_description: "Verify runtime skill resolution",
                phase_id: "implementation",
            },
            PhasePromptInputs::default(),
            Some(effective_caps.clone()),
            Some(&applied.application),
        );
        assert!(
            rendered.system_prompt.as_deref().is_some_and(|value| value.contains("Registry system prompt")),
            "expected skill system prompt in rendered prompt: {rendered:?}"
        );
        assert!(rendered.final_prompt.contains("Registry prefix"));
        assert!(rendered.final_prompt.contains("Skill directives:"));
        assert!(rendered.final_prompt.contains("Validate references"));
        assert!(rendered.final_prompt.contains("Registry suffix"));

        let mut runtime_contract =
            build_runtime_contract_with_resume("codex", "gpt-5.3-codex", &rendered.final_prompt, None)
                .expect("runtime contract");
        set_mcp_tool_policy(
            &mut runtime_contract,
            applied.application.tool_policy.as_ref().expect("skill tool policy"),
        );
        inject_named_mcp_servers(
            &mut runtime_contract,
            &project_root,
            &ctx,
            "implementation",
            &applied.application.mcp_servers,
        )
        .expect("skill mcp servers");
        inject_skill_overrides(&mut runtime_contract, "codex", &applied.application);

        assert_eq!(runtime_contract.pointer("/mcp/tool_policy/allow/0").and_then(Value::as_str), Some("Read"));
        assert_eq!(
            runtime_contract.pointer("/mcp/additional_servers/docs/command").and_then(Value::as_str),
            Some("docs-mcp")
        );
        assert_eq!(
            runtime_contract.pointer("/mcp/additional_servers/docs/env/DOCS_TOKEN").and_then(Value::as_str),
            Some("abc123")
        );
        assert_eq!(runtime_contract.pointer("/cli/launch/env/SKILL_MODE").and_then(Value::as_str), Some("review"));
        let args = runtime_contract.pointer("/cli/launch/args").and_then(Value::as_array).expect("launch args");
        assert!(args.iter().any(|value| value.as_str() == Some("--skill-flag")));
        assert!(args.iter().any(|value| value.as_str() == Some("--config-override=profile=review")));
    }

    #[test]
    fn resolve_phase_skills_reports_missing_skill_in_runtime_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut runtime = builtin_agent_runtime_config();
        runtime.phases.get_mut("implementation").expect("implementation phase").skills =
            vec!["missing-runtime-skill".to_string()];
        let workflow = builtin_workflow_config();
        let ctx = RuntimeConfigContext {
            agent_runtime_config: runtime,
            workflow_config: LoadedWorkflowConfig {
                metadata: WorkflowConfigMetadata {
                    schema: workflow.schema.clone(),
                    version: workflow.version,
                    hash: workflow_config_hash(&workflow),
                    source: WorkflowConfigSource::Builtin,
                },
                config: workflow,
                path: PathBuf::from("builtin"),
            },
        };
        let error =
            resolve_phase_skills(&ctx, temp.path(), "implementation").expect_err("missing skill should fail loudly");
        assert!(error.to_string().contains("missing-runtime-skill"), "error should name missing skill: {error}");
    }

    #[test]
    fn skill_capability_overrides_can_enable_state_mutations() {
        let overrides = BTreeMap::from([("mutates_state".to_string(), true)]);
        let effective = apply_skill_capability_overrides(&PhaseCapabilities::default(), &overrides);
        assert!(effective.mutates_state);
        assert!(!effective.is_strictly_read_only());
    }
}
