use std::path::Path;

use orchestrator_config::{
    apply_skill_for_tool, merge_skill_applications, skill_resolution::resolve_skills_for_project,
    SkillApplicationResult,
};
use serde_json::Value;

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

pub fn resolve_and_apply_phase_skills(
    ctx: &RuntimeConfigContext,
    project_root: &str,
    phase_id: &str,
    tool_id: &str,
) -> Option<SkillApplicationResult> {
    let skills = collect_phase_skills(ctx, phase_id);
    if skills.is_empty() {
        return None;
    }

    let resolved = resolve_skills_for_project(&skills, Path::new(project_root)).ok()?;
    if resolved.is_empty() {
        return None;
    }

    let applications: Vec<SkillApplicationResult> =
        resolved.iter().map(|r| apply_skill_for_tool(&r.definition, tool_id)).collect();

    Some(merge_skill_applications(&applications))
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
