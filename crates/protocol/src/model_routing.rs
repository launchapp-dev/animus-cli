#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRoutingComplexity {
    Low,
    Medium,
    High,
}

impl ModelRoutingComplexity {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" | "simple" => Some(Self::Low),
            "medium" | "moderate" => Some(Self::Medium),
            "high" | "complex" => Some(Self::High),
            _ => None,
        }
    }
}

pub fn normalize_tool_id(tool_id: &str) -> String {
    match tool_id.trim().to_ascii_lowercase().as_str() {
        "glm" | "minimax" | "open-code" => "opencode".to_string(),
        other => other.to_string(),
    }
}

pub fn canonical_model_id(model_id: &str) -> String {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match trimmed.to_ascii_lowercase().as_str() {
        "sonnet" | "claude-sonnet" | "claude-sonnet-latest" | "claude-sonnet-4" => {
            "claude-sonnet-4-6".to_string()
        }
        "claude-sonnet-4.5" | "claude-sonnet-4-5" | "claude-4.5-sonnet" | "claude-4-5-sonnet" => {
            "claude-sonnet-4-5".to_string()
        }
        "claude-sonnet-4.6" | "claude-sonnet-4-6" | "claude-4.6-sonnet" | "claude-4-6-sonnet" => {
            "claude-sonnet-4-6".to_string()
        }
        "opus" | "claude-opus" | "claude-opus-latest" | "claude-opus-4" => {
            "claude-opus-4-6".to_string()
        }
        "claude-opus-4.1" | "claude-opus-4-1" | "claude-4.1-opus" | "claude-4-1-opus" => {
            "claude-opus-4-1".to_string()
        }
        "claude-opus-4.6" | "claude-opus-4-6" | "claude-4.6-opus" | "claude-4-6-opus" => {
            "claude-opus-4-6".to_string()
        }
        "claude-opus-4.5" | "claude-opus-4-5" | "claude-4.5-opus" | "claude-4-5-opus" => {
            "claude-opus-4-5".to_string()
        }
        "gpt-5.3-codex" | "gpt-5-3-codex" | "gpt5.3-codex" | "gpt5-3-codex" | "gpt_5.3_codex"
        | "gpt_5_3_codex" => "gpt-5.3-codex".to_string(),
        "gpt-5.3-codex-spark"
        | "gpt-5-3-codex-spark"
        | "gpt5.3-codex-spark"
        | "gpt5-3-codex-spark"
        | "gpt_5.3_codex_spark"
        | "gpt_5_3_codex_spark"
        | "codex-spark" => "gpt-5.3-codex-spark".to_string(),
        "gemini" | "gemini-pro" | "gemini-2.5" | "gemini-2.5-pro-latest" | "gemini-pro-2.5" => {
            "gemini-2.5-pro".to_string()
        }
        "gemini-2.5-flash-latest" | "gemini-flash-2.5" => "gemini-2.5-flash".to_string(),
        "gemini-3" | "gemini-3.0-pro" | "gemini-3-pro-latest" | "gemini-pro-3" => {
            "gemini-3-pro".to_string()
        }
        "glm-5" | "glm5" | "zai/glm-5" | "z-ai/glm-5" | "zai-coding-plan-glm-5" => {
            "zai-coding-plan/glm-5".to_string()
        }
        "minimax-m2.5"
        | "minimax-m2-5"
        | "minimax/m2.5"
        | "minimax/m2-5"
        | "minimax/minimax-m2.5" => "minimax/MiniMax-M2.5".to_string(),
        _ => trimmed.to_string(),
    }
}

pub fn tool_for_model_id(model_id: &str) -> &'static str {
    let normalized = canonical_model_id(model_id).to_ascii_lowercase();

    if normalized.is_empty() {
        return "codex";
    }

    if normalized.starts_with("gemini") || normalized.contains("gemini") {
        return "gemini";
    }

    if normalized.starts_with("claude") || normalized.contains("claude") {
        return "claude";
    }

    if normalized.starts_with("opencode")
        || normalized.starts_with("glm")
        || normalized.starts_with("minimax")
        || normalized.starts_with("qwen")
        || normalized.starts_with("deepseek")
        || normalized.contains("glm")
        || normalized.contains("minimax")
        || normalized.contains("deepseek")
    {
        return "opencode";
    }

    "codex"
}

pub fn tool_supports_repository_writes(tool_id: &str) -> bool {
    matches!(
        normalize_tool_id(tool_id).as_str(),
        "codex" | "claude" | "opencode"
    )
}

pub fn required_api_keys_for_tool(tool_id: &str) -> &'static [&'static str] {
    match normalize_tool_id(tool_id).as_str() {
        "claude" => &["ANTHROPIC_API_KEY"],
        "codex" | "openai" => &["OPENAI_API_KEY"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "opencode" => &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"],
        _ => &[],
    }
}

pub fn default_model_specs() -> Vec<(String, String)> {
    vec![
        ("claude-sonnet-4-6".to_string(), "claude".to_string()),
        ("claude-opus-4-6".to_string(), "claude".to_string()),
        ("gpt-5.3-codex".to_string(), "codex".to_string()),
        ("gpt-5.3-codex-spark".to_string(), "codex".to_string()),
        ("gpt-5".to_string(), "codex".to_string()),
        ("gemini-2.5-pro".to_string(), "gemini".to_string()),
        ("gemini-2.5-flash".to_string(), "gemini".to_string()),
        ("gemini-3-pro".to_string(), "gemini".to_string()),
        ("gemini-3.1-pro-preview".to_string(), "gemini".to_string()),
        ("zai-coding-plan/glm-5".to_string(), "opencode".to_string()),
        ("minimax/MiniMax-M2.5".to_string(), "opencode".to_string()),
    ]
}

pub fn default_model_for_tool(tool_id: &str) -> Option<&'static str> {
    match normalize_tool_id(tool_id).as_str() {
        "claude" => Some("claude-sonnet-4-6"),
        "codex" | "openai" => Some("gpt-5.3-codex"),
        "gemini" => Some("gemini-2.5-pro"),
        "opencode" => Some("zai-coding-plan/glm-5"),
        _ => None,
    }
}

fn is_ui_ux_phase(phase_id: &str) -> bool {
    matches!(
        phase_id,
        "ux-research" | "wireframe" | "mockup-review" | "design" | "ui-design" | "ux-design"
    )
}

fn is_research_phase(phase_id: &str) -> bool {
    phase_id == "research"
}

fn is_review_phase(phase_id: &str) -> bool {
    matches!(
        phase_id,
        "code-review" | "review" | "architecture" | "design-review"
    )
}

pub fn default_primary_model_for_phase(
    phase_id: &str,
    complexity: Option<ModelRoutingComplexity>,
) -> &'static str {
    if is_ui_ux_phase(phase_id) || is_research_phase(phase_id) {
        return "gemini-3.1-pro-preview";
    }

    if is_review_phase(phase_id) {
        return match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
            ModelRoutingComplexity::High => "claude-opus-4-6",
            ModelRoutingComplexity::Low | ModelRoutingComplexity::Medium => "claude-sonnet-4-6",
        };
    }

    "claude-sonnet-4-6"
}

pub fn default_fallback_models_for_phase(
    phase_id: &str,
    complexity: Option<ModelRoutingComplexity>,
) -> Vec<&'static str> {
    if is_ui_ux_phase(phase_id) || is_research_phase(phase_id) {
        return vec![
            "claude-sonnet-4-6",
            "gemini-2.5-pro",
            "zai-coding-plan/glm-5",
            "minimax/MiniMax-M2.5",
            "gpt-5.3-codex",
        ];
    }

    if is_review_phase(phase_id) {
        return match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
            ModelRoutingComplexity::High => vec![
                "claude-sonnet-4-6",
                "gemini-3.1-pro-preview",
                "zai-coding-plan/glm-5",
                "minimax/MiniMax-M2.5",
                "gpt-5.3-codex",
            ],
            ModelRoutingComplexity::Low | ModelRoutingComplexity::Medium => vec![
                "gemini-3.1-pro-preview",
                "zai-coding-plan/glm-5",
                "minimax/MiniMax-M2.5",
                "gpt-5.3-codex",
                "claude-opus-4-6",
            ],
        };
    }

    match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
        ModelRoutingComplexity::Low => vec![
            "zai-coding-plan/glm-5",
            "minimax/MiniMax-M2.5",
            "gemini-3.1-pro-preview",
            "gpt-5.3-codex",
        ],
        ModelRoutingComplexity::Medium => vec![
            "zai-coding-plan/glm-5",
            "minimax/MiniMax-M2.5",
            "gemini-3.1-pro-preview",
            "gpt-5.3-codex",
        ],
        ModelRoutingComplexity::High => vec![
            "claude-opus-4-6",
            "zai-coding-plan/glm-5",
            "minimax/MiniMax-M2.5",
            "gemini-3.1-pro-preview",
            "gpt-5.3-codex",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_model_aliases_normalize_legacy_claude_ids() {
        assert_eq!(canonical_model_id("claude-sonnet-4"), "claude-sonnet-4-6");
        assert_eq!(canonical_model_id("claude-sonnet-4.5"), "claude-sonnet-4-5");
        assert_eq!(canonical_model_id("claude-sonnet-4.6"), "claude-sonnet-4-6");
        assert_eq!(canonical_model_id("claude-4.6-sonnet"), "claude-sonnet-4-6");
        assert_eq!(canonical_model_id("opus"), "claude-opus-4-6");
        assert_eq!(canonical_model_id("claude-opus-4.1"), "claude-opus-4-1");
        assert_eq!(canonical_model_id("claude-4.1-opus"), "claude-opus-4-1");
        assert_eq!(canonical_model_id("claude-opus-4.6"), "claude-opus-4-6");
        assert_eq!(canonical_model_id("claude-4.6-opus"), "claude-opus-4-6");
        assert_eq!(canonical_model_id("GPT-5.3-Codex"), "gpt-5.3-codex");
        assert_eq!(canonical_model_id("codex-spark"), "gpt-5.3-codex-spark");
        assert_eq!(canonical_model_id("gemini-pro"), "gemini-2.5-pro");
        assert_eq!(canonical_model_id("gemini-3.0-pro"), "gemini-3-pro");
        assert_eq!(canonical_model_id("glm-5"), "zai-coding-plan/glm-5");
        assert_eq!(canonical_model_id("minimax-m2.5"), "minimax/MiniMax-M2.5");
    }

    #[test]
    fn tool_routing_detects_claude_opencode_and_gemini_families() {
        assert_eq!(tool_for_model_id("claude-sonnet-4-6"), "claude");
        assert_eq!(tool_for_model_id("claude-opus-4-6"), "claude");
        assert_eq!(
            tool_for_model_id("openrouter/anthropic/claude-sonnet"),
            "claude"
        );
        assert_eq!(tool_for_model_id("zai-coding-plan/glm-5"), "opencode");
        assert_eq!(tool_for_model_id("minimax/MiniMax-M2.5"), "opencode");
        assert_eq!(tool_for_model_id("gemini-2.5-pro"), "gemini");
        assert_eq!(tool_for_model_id("gpt-5.3-codex"), "codex");
    }

    #[test]
    fn complexity_policy_uses_opus_for_high_complexity_review() {
        assert_eq!(
            default_primary_model_for_phase("code-review", Some(ModelRoutingComplexity::High)),
            "claude-opus-4-6"
        );
        assert_eq!(
            default_primary_model_for_phase("code-review", Some(ModelRoutingComplexity::Medium)),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn tool_defaults_are_stable() {
        assert_eq!(default_model_for_tool("claude"), Some("claude-sonnet-4-6"));
        assert_eq!(default_model_for_tool("codex"), Some("gpt-5.3-codex"));
        assert_eq!(default_model_for_tool("gemini"), Some("gemini-2.5-pro"));
        assert_eq!(
            default_model_for_tool("opencode"),
            Some("zai-coding-plan/glm-5")
        );
        assert_eq!(default_model_for_tool("unknown"), None);
    }

    #[test]
    fn default_model_specs_start_with_each_tool_default() {
        for tool in ["claude", "codex", "gemini", "opencode"] {
            let expected = default_model_for_tool(tool).expect("tool should have default model");
            let first_for_tool = default_model_specs()
                .into_iter()
                .find_map(|(model, tool_id)| (tool_id == tool).then_some(model))
                .expect("tool should exist in default model specs");
            assert_eq!(first_for_tool, expected);
        }
    }
}
