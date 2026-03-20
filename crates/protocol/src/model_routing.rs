use std::collections::HashMap;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct McpRuntimeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdio_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdio_args_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_draft: Option<String>,
}

impl McpRuntimeConfig {
    pub fn is_http_transport(&self) -> bool {
        self.transport.as_deref().map(|v| v.trim().to_ascii_lowercase()) == Some("http".to_string())
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseRoutingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_ux_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_ux_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_edit_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_edit_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_fallback_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research_fallback_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui_ux_fallback_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_phase: HashMap<String, PhaseOverride>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
}

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
        "open-code" => "opencode".to_string(),
        "glm" | "minimax" | "oai" | "ao-oai-runner" | "groq" | "together" | "fireworks" | "mistral" => {
            "oai-runner".to_string()
        }
        other => other.to_string(),
    }
}

pub fn canonical_model_id(model_id: &str) -> String {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match trimmed.to_ascii_lowercase().as_str() {
        "sonnet" | "claude-sonnet" | "claude-sonnet-latest" | "claude-sonnet-4" => "claude-sonnet-4-6".to_string(),
        "claude-sonnet-4.5" | "claude-sonnet-4-5" | "claude-4.5-sonnet" | "claude-4-5-sonnet" => {
            "claude-sonnet-4-5".to_string()
        }
        "claude-sonnet-4.6" | "claude-sonnet-4-6" | "claude-4.6-sonnet" | "claude-4-6-sonnet" => {
            "claude-sonnet-4-6".to_string()
        }
        "opus" | "claude-opus" | "claude-opus-latest" | "claude-opus-4" => "claude-opus-4-6".to_string(),
        "claude-opus-4.1" | "claude-opus-4-1" | "claude-4.1-opus" | "claude-4-1-opus" => "claude-opus-4-1".to_string(),
        "claude-opus-4.6" | "claude-opus-4-6" | "claude-4.6-opus" | "claude-4-6-opus" => "claude-opus-4-6".to_string(),
        "claude-opus-4.5" | "claude-opus-4-5" | "claude-4.5-opus" | "claude-4-5-opus" => "claude-opus-4-5".to_string(),
        "gpt-5.3-codex" | "gpt-5-3-codex" | "gpt5.3-codex" | "gpt5-3-codex" | "gpt_5.3_codex" | "gpt_5_3_codex" => {
            "gpt-5.3-codex".to_string()
        }
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
        "gemini-3" | "gemini-3.0-pro" | "gemini-3-pro-latest" | "gemini-pro-3" => "gemini-3-pro".to_string(),
        "glm-5" | "glm5" | "zai/glm-5" | "z-ai/glm-5" | "zai-coding-plan-glm-5" | "zai-coding-plan/glm-5" => {
            "zai-coding-plan/glm-5".to_string()
        }
        "minimax-m2.5"
        | "minimax-m2-5"
        | "minimax/m2.5"
        | "minimax/m2-5"
        | "minimax/minimax-m2.5"
        | "minimax/MiniMax-M2.7" => "minimax/MiniMax-M2.7".to_string(),
        "minimax-m2.1"
        | "minimax-m2-1"
        | "minimax/m2.1"
        | "minimax/m2-1"
        | "minimax/minimax-m2.1"
        | "minimax/MiniMax-M2.1" => "minimax/MiniMax-M2.1".to_string(),
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

    if normalized.starts_with("glm")
        || normalized.starts_with("minimax")
        || normalized.starts_with("zai")
        || normalized.contains("glm")
        || normalized.contains("minimax")
        || normalized.starts_with("openrouter/")
        || normalized.starts_with("groq/")
        || normalized.starts_with("together/")
        || normalized.starts_with("fireworks/")
        || normalized.starts_with("mistral/")
    {
        return "oai-runner";
    }

    if normalized.starts_with("opencode")
        || normalized.starts_with("qwen")
        || normalized.starts_with("deepseek")
        || normalized.contains("deepseek")
    {
        return "opencode";
    }

    "codex"
}

pub fn tool_supports_repository_writes(tool_id: &str) -> bool {
    matches!(normalize_tool_id(tool_id).as_str(), "codex" | "claude" | "gemini" | "opencode" | "oai-runner")
}

pub fn required_api_keys_for_tool(_tool_id: &str) -> &'static [&'static str] {
    &[]
}

pub fn default_model_specs() -> Vec<(String, String)> {
    vec![
        ("claude-sonnet-4-6".to_string(), "claude".to_string()),
        ("claude-opus-4-6".to_string(), "claude".to_string()),
        ("gpt-5.4".to_string(), "codex".to_string()),
        ("gpt-5.3-codex-spark".to_string(), "codex".to_string()),
        ("gpt-5".to_string(), "codex".to_string()),
        ("gemini-2.5-pro".to_string(), "gemini".to_string()),
        ("gemini-2.5-flash".to_string(), "gemini".to_string()),
        ("gemini-3-pro".to_string(), "gemini".to_string()),
        ("gemini-3.1-pro-preview".to_string(), "gemini".to_string()),
        ("minimax/MiniMax-M2.7".to_string(), "oai-runner".to_string()),
        ("zai-coding-plan/glm-5".to_string(), "oai-runner".to_string()),
    ]
}

pub fn default_model_for_tool(tool_id: &str) -> Option<&'static str> {
    match normalize_tool_id(tool_id).as_str() {
        "claude" => Some("claude-sonnet-4-6"),
        "codex" | "openai" => Some("gpt-5.4"),
        "gemini" => Some("gemini-2.5-pro"),
        "opencode" => Some("zai-coding-plan/glm-5"),
        "oai-runner" => Some("minimax/MiniMax-M2.7"),
        _ => None,
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseCapabilities {
    #[serde(default)]
    pub writes_files: bool,
    #[serde(default)]
    pub mutates_state: bool,
    #[serde(default)]
    pub requires_commit: bool,
    #[serde(default)]
    pub enforce_product_changes: bool,
    #[serde(default)]
    pub is_research: bool,
    #[serde(default)]
    pub is_ui_ux: bool,
    #[serde(default)]
    pub is_review: bool,
    #[serde(default)]
    pub is_testing: bool,
    #[serde(default)]
    pub is_requirements: bool,
}

impl PhaseCapabilities {
    pub fn defaults_for_phase(phase_id: &str) -> Self {
        match phase_id {
            "implementation" => {
                Self { writes_files: true, requires_commit: true, enforce_product_changes: true, ..Default::default() }
            }
            "wireframe" | "design" => Self { writes_files: true, is_ui_ux: true, ..Default::default() },
            "ux-research" | "mockup-review" | "ui-design" | "ux-design" => {
                Self { is_ui_ux: true, ..Default::default() }
            }
            "design-review" => Self { is_ui_ux: true, is_review: true, ..Default::default() },
            "research" => Self { is_research: true, ..Default::default() },
            "code-review" | "review" | "architecture" => Self { is_review: true, ..Default::default() },
            "requirements" => Self { is_requirements: true, ..Default::default() },
            "testing" | "test" | "qa" => Self { is_testing: true, ..Default::default() },
            _ => Self::default(),
        }
    }

    pub fn merge_with_defaults(self, phase_id: &str) -> Self {
        let defaults = Self::defaults_for_phase(phase_id);
        Self {
            writes_files: self.writes_files || defaults.writes_files,
            mutates_state: self.mutates_state || defaults.mutates_state,
            requires_commit: self.requires_commit || defaults.requires_commit,
            enforce_product_changes: self.enforce_product_changes || defaults.enforce_product_changes,
            is_research: self.is_research || defaults.is_research,
            is_ui_ux: self.is_ui_ux || defaults.is_ui_ux,
            is_review: self.is_review || defaults.is_review,
            is_testing: self.is_testing || defaults.is_testing,
            is_requirements: self.is_requirements || defaults.is_requirements,
        }
    }

    pub fn is_strictly_read_only(&self) -> bool {
        !self.writes_files && !self.mutates_state
    }
}

pub fn default_primary_model_for_phase(
    complexity: Option<ModelRoutingComplexity>,
    caps: &PhaseCapabilities,
) -> &'static str {
    if caps.is_ui_ux || caps.is_research {
        return "gemini-3.1-pro-preview";
    }

    if caps.is_review {
        return match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
            ModelRoutingComplexity::High => "claude-opus-4-6",
            ModelRoutingComplexity::Low | ModelRoutingComplexity::Medium => "claude-sonnet-4-6",
        };
    }

    if caps.is_requirements {
        return match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
            ModelRoutingComplexity::Low => "minimax/MiniMax-M2.7",
            ModelRoutingComplexity::Medium | ModelRoutingComplexity::High => "claude-sonnet-4-6",
        };
    }

    if caps.is_testing {
        return match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
            ModelRoutingComplexity::Low => "minimax/MiniMax-M2.7",
            ModelRoutingComplexity::Medium | ModelRoutingComplexity::High => "claude-sonnet-4-6",
        };
    }

    match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
        ModelRoutingComplexity::Low => "zai-coding-plan/glm-5",
        ModelRoutingComplexity::Medium | ModelRoutingComplexity::High => "claude-sonnet-4-6",
    }
}

pub fn default_fallback_models_for_phase(
    complexity: Option<ModelRoutingComplexity>,
    caps: &PhaseCapabilities,
) -> Vec<&'static str> {
    if caps.is_ui_ux || caps.is_research {
        return vec![
            "claude-sonnet-4-6",
            "gemini-2.5-pro",
            "zai-coding-plan/glm-5",
            "minimax/MiniMax-M2.7",
            "gpt-5.3-codex",
        ];
    }

    if caps.is_review {
        return match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
            ModelRoutingComplexity::High => vec![
                "claude-sonnet-4-6",
                "gemini-3.1-pro-preview",
                "zai-coding-plan/glm-5",
                "minimax/MiniMax-M2.7",
                "gpt-5.3-codex",
            ],
            ModelRoutingComplexity::Low | ModelRoutingComplexity::Medium => vec![
                "gemini-3.1-pro-preview",
                "zai-coding-plan/glm-5",
                "minimax/MiniMax-M2.7",
                "gpt-5.3-codex",
                "claude-opus-4-6",
            ],
        };
    }

    match complexity.unwrap_or(ModelRoutingComplexity::Medium) {
        ModelRoutingComplexity::Low => {
            vec!["minimax/MiniMax-M2.7", "claude-sonnet-4-6", "gemini-3.1-pro-preview", "gpt-5.3-codex"]
        }
        ModelRoutingComplexity::Medium => {
            vec!["zai-coding-plan/glm-5", "minimax/MiniMax-M2.7", "gemini-3.1-pro-preview", "gpt-5.3-codex"]
        }
        ModelRoutingComplexity::High => vec![
            "claude-opus-4-6",
            "zai-coding-plan/glm-5",
            "minimax/MiniMax-M2.7",
            "gemini-3.1-pro-preview",
            "gpt-5.3-codex",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
        assert_eq!(canonical_model_id("minimax-m2.1"), "minimax/MiniMax-M2.1");
        assert_eq!(canonical_model_id("minimax-m2.5"), "minimax/MiniMax-M2.7");
    }

    #[test]
    fn tool_routing_detects_claude_opencode_and_gemini_families() {
        assert_eq!(tool_for_model_id("claude-sonnet-4-6"), "claude");
        assert_eq!(tool_for_model_id("claude-opus-4-6"), "claude");
        assert_eq!(tool_for_model_id("openrouter/anthropic/claude-sonnet"), "claude");
        assert_eq!(tool_for_model_id("zai-coding-plan/glm-5"), "oai-runner");
        assert_eq!(tool_for_model_id("minimax/MiniMax-M2.7"), "oai-runner");
        assert_eq!(tool_for_model_id("gemini-2.5-pro"), "gemini");
        assert_eq!(tool_for_model_id("gpt-5.3-codex"), "codex");
    }

    #[test]
    fn complexity_policy_uses_opus_for_high_complexity_review() {
        let caps = PhaseCapabilities::defaults_for_phase("code-review");
        assert_eq!(default_primary_model_for_phase(Some(ModelRoutingComplexity::High), &caps), "claude-opus-4-6");
        assert_eq!(default_primary_model_for_phase(Some(ModelRoutingComplexity::Medium), &caps), "claude-sonnet-4-6");
    }

    #[test]
    fn low_complexity_routes_to_cheaper_models() {
        let impl_caps = PhaseCapabilities::defaults_for_phase("implementation");
        assert_eq!(
            default_primary_model_for_phase(Some(ModelRoutingComplexity::Low), &impl_caps),
            "zai-coding-plan/glm-5"
        );
        let req_caps = PhaseCapabilities::defaults_for_phase("requirements");
        assert_eq!(
            default_primary_model_for_phase(Some(ModelRoutingComplexity::Low), &req_caps),
            "minimax/MiniMax-M2.7"
        );
        let test_caps = PhaseCapabilities::defaults_for_phase("testing");
        assert_eq!(
            default_primary_model_for_phase(Some(ModelRoutingComplexity::Low), &test_caps),
            "minimax/MiniMax-M2.7"
        );
    }

    #[test]
    fn medium_complexity_defaults_to_claude_for_requirements_and_testing() {
        let req_caps = PhaseCapabilities::defaults_for_phase("requirements");
        assert_eq!(
            default_primary_model_for_phase(Some(ModelRoutingComplexity::Medium), &req_caps),
            "claude-sonnet-4-6"
        );
        let test_caps = PhaseCapabilities::defaults_for_phase("testing");
        assert_eq!(
            default_primary_model_for_phase(Some(ModelRoutingComplexity::Medium), &test_caps),
            "claude-sonnet-4-6"
        );
        let impl_caps = PhaseCapabilities::defaults_for_phase("implementation");
        assert_eq!(
            default_primary_model_for_phase(Some(ModelRoutingComplexity::Medium), &impl_caps),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn phase_capabilities_defaults_are_correct() {
        let impl_caps = PhaseCapabilities::defaults_for_phase("implementation");
        assert!(impl_caps.writes_files);
        assert!(impl_caps.requires_commit);
        assert!(impl_caps.enforce_product_changes);
        assert!(impl_caps.is_strictly_read_only() == false);
        assert!(!impl_caps.is_research);

        let research_caps = PhaseCapabilities::defaults_for_phase("research");
        assert!(research_caps.is_research);
        assert!(!research_caps.writes_files);
        assert!(research_caps.is_strictly_read_only());

        let design_caps = PhaseCapabilities::defaults_for_phase("design");
        assert!(design_caps.writes_files);
        assert!(design_caps.is_ui_ux);
        assert!(!design_caps.requires_commit);

        let design_review_caps = PhaseCapabilities::defaults_for_phase("design-review");
        assert!(design_review_caps.is_ui_ux);
        assert!(design_review_caps.is_review);

        let unknown_caps = PhaseCapabilities::defaults_for_phase("custom-phase");
        assert_eq!(unknown_caps, PhaseCapabilities::default());
    }

    #[test]
    fn merge_with_defaults_ors_config_with_phase_defaults() {
        let custom = PhaseCapabilities { writes_files: true, ..Default::default() };
        let merged = custom.merge_with_defaults("research");
        assert!(merged.writes_files);
        assert!(merged.is_research);
    }

    #[test]
    fn mutating_state_capability_prevents_strict_read_only_mode() {
        let caps = PhaseCapabilities { mutates_state: true, ..Default::default() };
        assert!(!caps.is_strictly_read_only());
    }

    #[test]
    fn tool_defaults_are_stable() {
        assert_eq!(default_model_for_tool("claude"), Some("claude-sonnet-4-6"));
        assert_eq!(default_model_for_tool("codex"), Some("gpt-5.4"));
        assert_eq!(default_model_for_tool("gemini"), Some("gemini-2.5-pro"));
        assert_eq!(default_model_for_tool("opencode"), Some("zai-coding-plan/glm-5"));
        assert_eq!(default_model_for_tool("oai-runner"), Some("minimax/MiniMax-M2.7"));
        assert_eq!(default_model_for_tool("unknown"), None);
    }

    #[test]
    fn default_model_specs_start_with_each_tool_default() {
        for tool in ["claude", "codex", "gemini", "oai-runner"] {
            let expected = default_model_for_tool(tool).expect("tool should have default model");
            let first_for_tool = default_model_specs()
                .into_iter()
                .find_map(|(model, tool_id)| (tool_id == tool).then_some(model))
                .expect("tool should exist in default model specs");
            assert_eq!(first_for_tool, expected);
        }
    }

    proptest! {
        #[test]
        fn tool_for_model_id_never_panics(input in "\\PC*") {
            let result = tool_for_model_id(&input);
            prop_assert!(["claude", "codex", "gemini", "opencode", "oai-runner"].contains(&result));
        }

        #[test]
        fn canonical_model_id_is_idempotent(input in "\\PC*") {
            let once = canonical_model_id(&input);
            let twice = canonical_model_id(&once);
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn normalize_tool_id_is_idempotent(input in "\\PC*") {
            let once = normalize_tool_id(&input);
            let twice = normalize_tool_id(&once);
            prop_assert_eq!(once, twice);
        }
    }
}
