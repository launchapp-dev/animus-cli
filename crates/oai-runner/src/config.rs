use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ResolvedConfig {
    pub api_base: String,
    pub api_key: String,
    pub model_id: String,
}

pub fn resolve_config(model: &str, api_base: Option<String>, api_key: Option<String>) -> Result<ResolvedConfig> {
    let normalized = model.to_ascii_lowercase();

    let api_base = match api_base {
        Some(base) => base,
        None => infer_api_base(&normalized)?,
    };

    let api_key = match api_key {
        Some(key) => key,
        None => resolve_api_key(&normalized, &api_base)?,
    };

    let model_id = strip_provider_prefix(model);

    Ok(ResolvedConfig { api_base, api_key, model_id })
}

fn infer_api_base(normalized_model: &str) -> Result<String> {
    if normalized_model.starts_with("minimax/") || normalized_model.contains("minimax") {
        return Ok("https://api.minimax.io/v1".to_string());
    }
    if normalized_model.starts_with("zai") || normalized_model.starts_with("glm") || normalized_model.contains("glm") {
        return Ok("https://api.z.ai/api/coding/paas/v4".to_string());
    }
    if normalized_model.starts_with("deepseek/") || normalized_model.contains("deepseek") {
        return Ok("https://api.deepseek.com/v1".to_string());
    }
    if normalized_model.starts_with("openrouter/") {
        return Ok("https://openrouter.ai/api/v1".to_string());
    }
    if normalized_model.starts_with("groq/") || normalized_model.contains("groq") {
        return Ok("https://api.groq.com/openai/v1".to_string());
    }
    if normalized_model.starts_with("together/") || normalized_model.contains("together") {
        return Ok("https://api.together.xyz/v1".to_string());
    }
    if normalized_model.starts_with("fireworks/") || normalized_model.contains("fireworks") {
        return Ok("https://api.fireworks.ai/inference/v1".to_string());
    }
    if normalized_model.starts_with("mistral/") || normalized_model.contains("mistral") {
        return Ok("https://api.mistral.ai/v1".to_string());
    }
    if let Ok(base) = std::env::var("OPENAI_API_BASE") {
        return Ok(base);
    }
    if let Ok(base) = std::env::var("OPENAI_BASE_URL") {
        return Ok(base);
    }
    Ok("https://api.openai.com/v1".to_string())
}

fn resolve_api_key(normalized_model: &str, api_base: &str) -> Result<String> {
    if let Some(key) = protocol::credentials::Credentials::load_global().resolve(normalized_model, api_base) {
        return Ok(key);
    }

    if let Some(key) = try_opencode_auth_json(normalized_model, api_base) {
        return Ok(key);
    }

    for env_var in ["OPENAI_API_KEY", "GROQ_API_KEY", "TOGETHER_API_KEY", "FIREWORKS_API_KEY", "MISTRAL_API_KEY"] {
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                let prefix = env_var.split('_').next().unwrap_or("").to_ascii_lowercase();
                if prefix.is_empty() || normalized_model.contains(&prefix) || env_var == "OPENAI_API_KEY" {
                    return Ok(key);
                }
            }
        }
    }

    bail!(
        "No API key found for model '{}'. Set OPENAI_API_KEY, a provider-specific key (GROQ_API_KEY, etc.), or use `ao setup credentials`.",
        normalized_model
    )
}

#[derive(Deserialize)]
struct ProviderAuth {
    #[serde(default, alias = "apiKey")]
    key: Option<String>,
}

fn try_opencode_auth_json(normalized_model: &str, api_base: &str) -> Option<String> {
    let home = dirs_path()?;
    let auth_path = home.join(".local/share/opencode/auth.json");
    let content = std::fs::read_to_string(auth_path).ok()?;
    let auth: HashMap<String, ProviderAuth> = serde_json::from_str(&content).ok()?;

    for (provider_name, provider) in &auth {
        let matches = api_base.contains(provider_name.as_str())
            || provider_name.contains(api_base)
            || normalized_model.starts_with(provider_name.as_str())
            || normalized_model.contains(provider_name.as_str());
        if matches {
            if let Some(key) = &provider.key {
                if !key.is_empty() {
                    return Some(key.clone());
                }
            }
        }
    }

    None
}

fn strip_provider_prefix(model: &str) -> String {
    let prefixes = [
        "minimax/",
        "zai-coding-plan/",
        "zai/",
        "deepseek/",
        "openrouter/",
        "groq/",
        "together/",
        "fireworks/",
        "mistral/",
    ];
    for prefix in prefixes {
        if let Some(stripped) = model.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    let lower = model.to_ascii_lowercase();
    for prefix in prefixes {
        if let Some(_rest) = lower.strip_prefix(prefix) {
            return model[prefix.len()..].to_string();
        }
    }
    model.to_string()
}

fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_api_base_for_minimax() {
        let result = infer_api_base("minimax/minimax-m2.1").unwrap();
        assert_eq!(result, "https://api.minimax.io/v1");
    }

    #[test]
    fn infer_api_base_for_glm() {
        let result = infer_api_base("zai-coding-plan/glm-5").unwrap();
        assert_eq!(result, "https://api.z.ai/api/coding/paas/v4");
    }

    #[test]
    fn infer_api_base_for_deepseek() {
        let result = infer_api_base("deepseek/deepseek-chat").unwrap();
        assert_eq!(result, "https://api.deepseek.com/v1");
    }

    #[test]
    fn infer_api_base_for_openrouter() {
        let result = infer_api_base("openrouter/anthropic/claude").unwrap();
        assert_eq!(result, "https://openrouter.ai/api/v1");
    }

    #[test]
    fn infer_api_base_for_groq() {
        let result = infer_api_base("groq/llama-3.3-70b").unwrap();
        assert_eq!(result, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn infer_api_base_for_together() {
        let result = infer_api_base("together/meta-llama/llama-3-70b").unwrap();
        assert_eq!(result, "https://api.together.xyz/v1");
    }

    #[test]
    fn infer_api_base_for_fireworks() {
        let result = infer_api_base("fireworks/llama-v3p3-70b-instruct").unwrap();
        assert_eq!(result, "https://api.fireworks.ai/inference/v1");
    }

    #[test]
    fn infer_api_base_for_mistral() {
        let result = infer_api_base("mistral/mistral-large-latest").unwrap();
        assert_eq!(result, "https://api.mistral.ai/v1");
    }

    #[test]
    fn infer_api_base_falls_back_to_openai_for_unknown_model() {
        std::env::remove_var("OPENAI_API_BASE");
        std::env::remove_var("OPENAI_BASE_URL");
        let result = infer_api_base("unknown-model").unwrap();
        assert_eq!(result, "https://api.openai.com/v1");
    }

    #[test]
    fn resolve_config_uses_explicit_overrides() {
        let config = resolve_config(
            "minimax/MiniMax-M2.1",
            Some("https://custom.api.com".to_string()),
            Some("sk-test-key".to_string()),
        )
        .unwrap();
        assert_eq!(config.api_base, "https://custom.api.com");
        assert_eq!(config.api_key, "sk-test-key");
        assert_eq!(config.model_id, "MiniMax-M2.1");
    }
}
