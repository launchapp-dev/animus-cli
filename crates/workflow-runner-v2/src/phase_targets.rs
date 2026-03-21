use std::collections::HashSet;
use std::path::Path;

use orchestrator_core;
use protocol::{
    canonical_model_id, default_fallback_models_for_phase, default_model_specs, default_primary_model_for_phase,
    normalize_tool_id, tool_for_model_id, tool_supports_repository_writes, ModelRoutingComplexity, PhaseCapabilities,
    PhaseRoutingConfig,
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
        routing: &PhaseRoutingConfig,
    ) -> (String, String) {
        let resolved_complexity = complexity.or_else(|| phase_complexity(phase_id, routing));
        let model_id = model_override
            .map(canonical_model_id)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| phase_model_id(phase_id, resolved_complexity, caps, routing));
        let tool_id = tool_override
            .map(normalize_tool_id)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| phase_tool_id(phase_id, &model_id, caps, routing));
        enforce_write_capable_phase_target(tool_id, model_id, caps.writes_files, routing)
    }

    /// Build an ordered list of (tool, model) execution targets for a phase.
    ///
    /// The list starts with the primary target and appends fallback targets.
    /// When `fallback_tools` provides explicit tool overrides for specific
    /// fallback model indices, those are used instead of auto-deriving the
    /// tool from the model ID.
    pub fn build_phase_execution_targets(
        phase_id: &str,
        model_override: Option<&str>,
        tool_override: Option<&str>,
        configured_fallback_models: &[String],
        configured_fallback_tools: &[String],
        complexity: Option<ModelRoutingComplexity>,
        project_root: Option<&str>,
        caps: &PhaseCapabilities,
        routing: &PhaseRoutingConfig,
    ) -> Vec<(String, String)> {
        let resolved_complexity = complexity.or_else(|| phase_complexity(phase_id, routing));
        let (primary_tool, primary_model) = Self::resolve_phase_execution_target(
            phase_id,
            model_override,
            tool_override,
            resolved_complexity,
            caps,
            routing,
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
        candidate_models.extend(phase_fallback_models(phase_id, caps, routing));
        candidate_models.extend(
            default_fallback_models_for_phase(resolved_complexity, caps)
                .into_iter()
                .map(canonical_model_id)
                .filter(|value| !value.is_empty()),
        );

        let mut targets = Vec::new();
        let mut seen_models = HashSet::new();
        let mut configured_fallback_idx = 0usize;
        for candidate_model in candidate_models {
            let model_key = candidate_model.to_ascii_lowercase();
            if !seen_models.insert(model_key) {
                continue;
            }

            if let Some(root) = project_root {
                if orchestrator_core::is_model_suppressed_for_phase(Path::new(root), &candidate_model, phase_id) {
                    continue;
                }
            }

            let (tool_id, model_id) = if candidate_model.eq_ignore_ascii_case(&primary_model) {
                (primary_tool.clone(), primary_model.clone())
            } else {
                // Check if there's an explicit fallback_tool for this configured fallback model
                let explicit_tool = if configured_fallback_idx < configured_fallback_tools.len() {
                    let tool_str = configured_fallback_tools[configured_fallback_idx].trim();
                    if !tool_str.is_empty() {
                        Some(normalize_tool_id(tool_str).to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };
                configured_fallback_idx += 1;

                let auto_tool = explicit_tool.unwrap_or_else(|| Self::tool_for_model_id(&candidate_model).to_string());
                enforce_write_capable_phase_target(auto_tool, candidate_model, caps.writes_files, routing)
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
    routing: &PhaseRoutingConfig,
) -> String {
    let phase_key = env_phase_key(phase_id);
    if let Some(phase_override) = routing.per_phase.get(&phase_key) {
        if let Some(model) = phase_override.model.as_deref().map(canonical_model_id).filter(|v| !v.is_empty()) {
            return model;
        }
    }

    if caps.is_ui_ux {
        if let Some(model) = routing.ui_ux_model.as_deref().map(canonical_model_id).filter(|v| !v.is_empty()) {
            return model;
        }
    }

    if caps.is_research {
        if let Some(model) = routing.research_model.as_deref().map(canonical_model_id).filter(|v| !v.is_empty()) {
            return model;
        }
    }

    if let Some(model) = routing.global_model.as_deref().map(canonical_model_id).filter(|v| !v.is_empty()) {
        return model;
    }

    default_primary_model_for_phase(complexity, caps).to_string()
}

fn phase_tool_id(phase_id: &str, model_id: &str, caps: &PhaseCapabilities, routing: &PhaseRoutingConfig) -> String {
    let phase_key = env_phase_key(phase_id);
    if let Some(phase_override) = routing.per_phase.get(&phase_key) {
        if let Some(tool) = phase_override.tool.as_deref().map(normalize_tool_id).filter(|v| !v.is_empty()) {
            return tool;
        }
    }

    if caps.is_ui_ux {
        if let Some(tool) = routing.ui_ux_tool.as_deref().map(normalize_tool_id).filter(|v| !v.is_empty()) {
            return tool;
        }
    }

    if caps.is_research {
        if let Some(tool) = routing.research_tool.as_deref().map(normalize_tool_id).filter(|v| !v.is_empty()) {
            return tool;
        }
    }

    if let Some(tool) = routing.global_tool.as_deref().map(normalize_tool_id).filter(|v| !v.is_empty()) {
        return tool;
    }

    PhaseTargetPlanner::tool_for_model_id(model_id).to_string()
}

fn enforce_write_capable_phase_target(
    tool_id: String,
    model_id: String,
    phase_writes_files: bool,
    routing: &PhaseRoutingConfig,
) -> (String, String) {
    let normalized_tool_id = normalize_tool_id(&tool_id);
    if !phase_writes_files {
        return (normalized_tool_id, model_id);
    }
    if !protocol::parse_env_bool("AO_ALLOW_NON_EDITING_PHASE_TOOL")
        && !tool_supports_repository_writes(&normalized_tool_id)
    {
        let fallback_model = routing.file_edit_model.as_deref().map(canonical_model_id).filter(|v| !v.is_empty());
        let fallback_tool = routing.file_edit_tool.as_deref().map(normalize_tool_id).filter(|v| !v.is_empty());
        if let (Some(m), Some(t)) = (&fallback_model, &fallback_tool) {
            return (t.clone(), m.clone());
        }
        if let Some((m, t)) = default_model_specs().into_iter().find(|(_, t)| tool_supports_repository_writes(t)) {
            return (fallback_tool.unwrap_or(t), fallback_model.unwrap_or(m));
        }
        return (normalized_tool_id, model_id);
    }
    (normalized_tool_id, model_id)
}

fn env_phase_key(phase_id: &str) -> String {
    phase_id.trim().to_ascii_uppercase().replace(['-', ' '], "_")
}

fn phase_complexity(phase_id: &str, routing: &PhaseRoutingConfig) -> Option<ModelRoutingComplexity> {
    let phase_key = env_phase_key(phase_id);
    if let Some(phase_override) = routing.per_phase.get(&phase_key) {
        if let Some(parsed) = phase_override.complexity.as_deref().and_then(ModelRoutingComplexity::parse) {
            return Some(parsed);
        }
    }
    routing.complexity.as_deref().and_then(ModelRoutingComplexity::parse)
}

fn phase_fallback_models(phase_id: &str, caps: &PhaseCapabilities, routing: &PhaseRoutingConfig) -> Vec<String> {
    let phase_key = env_phase_key(phase_id);
    if let Some(phase_override) = routing.per_phase.get(&phase_key) {
        if !phase_override.fallback_models.is_empty() {
            return phase_override
                .fallback_models
                .iter()
                .map(|s| canonical_model_id(s))
                .filter(|v| !v.is_empty())
                .collect();
        }
    }

    if caps.is_ui_ux && !routing.ui_ux_fallback_models.is_empty() {
        return routing.ui_ux_fallback_models.iter().map(|s| canonical_model_id(s)).filter(|v| !v.is_empty()).collect();
    }

    if caps.is_research && !routing.research_fallback_models.is_empty() {
        return routing
            .research_fallback_models
            .iter()
            .map(|s| canonical_model_id(s))
            .filter(|v| !v.is_empty())
            .collect();
    }

    routing.global_fallback_models.iter().map(|s| canonical_model_id(s)).filter(|v| !v.is_empty()).collect()
}
