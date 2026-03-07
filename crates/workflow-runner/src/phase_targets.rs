use std::collections::HashSet;
use std::path::Path;

use orchestrator_core;
use protocol::{
    canonical_model_id, default_fallback_models_for_phase, default_model_specs,
    default_primary_model_for_phase, normalize_tool_id, tool_for_model_id,
    tool_supports_repository_writes, ModelRoutingComplexity, PhaseCapabilities,
};

pub struct PhaseTargetPlanner;

impl PhaseTargetPlanner {
    pub fn tool_for_model_id(model_id: &str) -> &'static str {
        tool_for_model_id(model_id)
    }

    pub fn resolve_phase_execution_target(
        phase_id: &str,
        model_override: Option<&str>,
        tool_override: Option<&str>,
        complexity: Option<ModelRoutingComplexity>,
        caps: &PhaseCapabilities,
    ) -> (String, String) {
        let resolved_complexity = complexity.or_else(|| phase_complexity_from_env(phase_id));
        let model_id = model_override
            .map(canonical_model_id)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| phase_model_id(phase_id, resolved_complexity, caps));
        let tool_id = tool_override
            .map(normalize_tool_id)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| phase_tool_id(phase_id, &model_id, caps));
        enforce_write_capable_phase_target(tool_id, model_id, caps.writes_files)
    }

    pub fn build_phase_execution_targets(
        phase_id: &str,
        model_override: Option<&str>,
        tool_override: Option<&str>,
        configured_fallback_models: &[String],
        complexity: Option<ModelRoutingComplexity>,
        project_root: Option<&str>,
        caps: &PhaseCapabilities,
    ) -> Vec<(String, String)> {
        let resolved_complexity = complexity.or_else(|| phase_complexity_from_env(phase_id));
        let (primary_tool, primary_model) = Self::resolve_phase_execution_target(
            phase_id,
            model_override,
            tool_override,
            resolved_complexity,
            caps,
        );

        let mut candidate_models = Vec::new();
        candidate_models.push(primary_model.clone());
        candidate_models.extend(
            configured_fallback_models
                .iter()
                .map(String::as_str)
                .map(canonical_model_id)
                .filter(|value| !value.is_empty()),
        );
        candidate_models.extend(phase_fallback_models_from_env(phase_id, caps));
        candidate_models.extend(
            default_fallback_models_for_phase(resolved_complexity, caps)
                .into_iter()
                .map(canonical_model_id)
                .filter(|value| !value.is_empty()),
        );

        let mut targets = Vec::new();
        let mut seen_models = HashSet::new();
        for candidate_model in candidate_models {
            let model_key = candidate_model.to_ascii_lowercase();
            if !seen_models.insert(model_key) {
                continue;
            }

            if let Some(root) = project_root {
                if orchestrator_core::is_model_suppressed_for_phase(
                    Path::new(root),
                    &candidate_model,
                    phase_id,
                ) {
                    continue;
                }
            }

            let (tool_id, model_id) = if candidate_model.eq_ignore_ascii_case(&primary_model) {
                (primary_tool.clone(), primary_model.clone())
            } else {
                enforce_write_capable_phase_target(
                    Self::tool_for_model_id(&candidate_model).to_string(),
                    candidate_model,
                    caps.writes_files,
                )
            };
            targets.push((tool_id, model_id));
        }

        if targets.is_empty() {
            targets.push((primary_tool, primary_model));
        }

        targets
    }
}

fn phase_model_id(
    phase_id: &str,
    complexity: Option<ModelRoutingComplexity>,
    caps: &PhaseCapabilities,
) -> String {
    let phase_key = phase_id
        .trim()
        .to_ascii_uppercase()
        .replace('-', "_")
        .replace(' ', "_");
    let phase_override_key = format!("AO_PHASE_MODEL_{phase_key}");

    if let Ok(value) = std::env::var(&phase_override_key) {
        let model = canonical_model_id(&value);
        if !model.is_empty() {
            return model;
        }
    }

    if caps.is_ui_ux {
        if let Ok(value) = std::env::var("AO_PHASE_MODEL_UI_UX") {
            let model = canonical_model_id(&value);
            if !model.is_empty() {
                return model;
            }
        }
    }

    if caps.is_research {
        if let Ok(value) = std::env::var("AO_PHASE_MODEL_RESEARCH") {
            let model = canonical_model_id(&value);
            if !model.is_empty() {
                return model;
            }
        }
    }

    if let Ok(value) = std::env::var("AO_PHASE_MODEL") {
        let model = canonical_model_id(&value);
        if !model.is_empty() {
            return model;
        }
    }

    default_primary_model_for_phase(complexity, caps).to_string()
}

fn phase_tool_id(phase_id: &str, model_id: &str, caps: &PhaseCapabilities) -> String {
    let phase_key = phase_id
        .trim()
        .to_ascii_uppercase()
        .replace('-', "_")
        .replace(' ', "_");
    let phase_override_key = format!("AO_PHASE_TOOL_{phase_key}");

    if let Ok(value) = std::env::var(&phase_override_key) {
        let tool = normalize_tool_id(&value);
        if !tool.is_empty() {
            return tool;
        }
    }

    if caps.is_ui_ux {
        if let Ok(value) = std::env::var("AO_PHASE_TOOL_UI_UX") {
            let tool = normalize_tool_id(&value);
            if !tool.is_empty() {
                return tool;
            }
        }
    }

    if caps.is_research {
        if let Ok(value) = std::env::var("AO_PHASE_TOOL_RESEARCH") {
            let tool = normalize_tool_id(&value);
            if !tool.is_empty() {
                return tool;
            }
        }
    }

    if let Ok(value) = std::env::var("AO_PHASE_TOOL") {
        let tool = normalize_tool_id(&value);
        if !tool.is_empty() {
            return tool;
        }
    }

    PhaseTargetPlanner::tool_for_model_id(model_id).to_string()
}

fn enforce_write_capable_phase_target(
    tool_id: String,
    model_id: String,
    phase_writes_files: bool,
) -> (String, String) {
    let normalized_tool_id = normalize_tool_id(&tool_id);
    if !phase_writes_files {
        return (normalized_tool_id, model_id);
    }
    if !protocol::parse_env_bool("AO_ALLOW_NON_EDITING_PHASE_TOOL")
        && !tool_supports_repository_writes(&normalized_tool_id)
    {
        let fallback_model = std::env::var("AO_PHASE_MODEL_FILE_EDIT")
            .ok()
            .map(|value| canonical_model_id(&value))
            .filter(|value| !value.is_empty());
        let fallback_tool = std::env::var("AO_PHASE_TOOL_FILE_EDIT")
            .ok()
            .map(|value| normalize_tool_id(&value))
            .filter(|value| !value.is_empty());
        if let (Some(m), Some(t)) = (&fallback_model, &fallback_tool) {
            return (t.clone(), m.clone());
        }
        if let Some((m, t)) = default_model_specs()
            .into_iter()
            .find(|(_, t)| tool_supports_repository_writes(t))
        {
            return (
                fallback_tool.unwrap_or(t),
                fallback_model.unwrap_or(m),
            );
        }
        return (normalized_tool_id, model_id);
    }
    (normalized_tool_id, model_id)
}

fn parse_model_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(canonical_model_id)
        .filter(|value| !value.is_empty())
        .collect()
}

fn env_phase_key(phase_id: &str) -> String {
    phase_id
        .trim()
        .to_ascii_uppercase()
        .replace('-', "_")
        .replace(' ', "_")
}

fn phase_complexity_from_env(phase_id: &str) -> Option<ModelRoutingComplexity> {
    let phase_key = env_phase_key(phase_id);
    let phase_specific = format!("AO_PHASE_COMPLEXITY_{phase_key}");
    if let Ok(value) = std::env::var(&phase_specific) {
        if let Some(parsed) = ModelRoutingComplexity::parse(&value) {
            return Some(parsed);
        }
    }

    std::env::var("AO_PHASE_COMPLEXITY")
        .ok()
        .and_then(|value| ModelRoutingComplexity::parse(&value))
}

fn phase_fallback_models_from_env(phase_id: &str, caps: &PhaseCapabilities) -> Vec<String> {
    let phase_key = env_phase_key(phase_id);
    let phase_specific = format!("AO_PHASE_FALLBACK_MODELS_{phase_key}");
    if let Ok(value) = std::env::var(&phase_specific) {
        let parsed = parse_model_list(&value);
        if !parsed.is_empty() {
            return parsed;
        }
    }

    if caps.is_ui_ux {
        if let Ok(value) = std::env::var("AO_PHASE_FALLBACK_MODELS_UI_UX") {
            let parsed = parse_model_list(&value);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }

    if caps.is_research {
        if let Ok(value) = std::env::var("AO_PHASE_FALLBACK_MODELS_RESEARCH") {
            let parsed = parse_model_list(&value);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }

    std::env::var("AO_PHASE_FALLBACK_MODELS")
        .ok()
        .map(|value| parse_model_list(&value))
        .unwrap_or_default()
}
