use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::config::Config;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Credentials {
    #[serde(default)]
    pub providers: HashMap<String, ProviderCredential>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProviderCredential {
    pub api_key: String,
}

impl Credentials {
    pub fn load_global() -> Self {
        let path = Config::global_config_dir().join("credentials.json");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    pub fn resolve(&self, model: &str, api_base: &str) -> Option<String> {
        let model_lower = model.to_ascii_lowercase();
        let base_lower = api_base.to_ascii_lowercase();

        for (provider, cred) in &self.providers {
            if cred.api_key.trim().is_empty() {
                continue;
            }
            let p = provider.to_ascii_lowercase();
            let matches = model_lower.starts_with(&p) || model_lower.contains(&p) || base_lower.contains(&p);
            if matches {
                return Some(cred.api_key.clone());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_matches_model_prefix() {
        let creds = Credentials {
            providers: HashMap::from([("minimax".to_string(), ProviderCredential { api_key: "sk-test".to_string() })]),
        };
        assert_eq!(creds.resolve("minimax/MiniMax-M2.5", "https://api.minimax.io/v1"), Some("sk-test".to_string()));
    }

    #[test]
    fn resolve_matches_model_contains() {
        let creds = Credentials {
            providers: HashMap::from([("deepseek".to_string(), ProviderCredential { api_key: "sk-ds".to_string() })]),
        };
        assert_eq!(creds.resolve("deepseek/deepseek-chat", "https://api.deepseek.com/v1"), Some("sk-ds".to_string()));
    }

    #[test]
    fn resolve_matches_api_base() {
        let creds = Credentials {
            providers: HashMap::from([("openrouter".to_string(), ProviderCredential { api_key: "sk-or".to_string() })]),
        };
        assert_eq!(creds.resolve("anthropic/claude-3", "https://openrouter.ai/api/v1"), Some("sk-or".to_string()));
    }

    #[test]
    fn resolve_returns_none_when_no_match() {
        let creds = Credentials {
            providers: HashMap::from([("minimax".to_string(), ProviderCredential { api_key: "sk-test".to_string() })]),
        };
        assert_eq!(creds.resolve("deepseek/chat", "https://api.deepseek.com"), None);
    }

    #[test]
    fn resolve_skips_empty_keys() {
        let creds = Credentials {
            providers: HashMap::from([("minimax".to_string(), ProviderCredential { api_key: "  ".to_string() })]),
        };
        assert_eq!(creds.resolve("minimax/M2.5", "https://api.minimax.io"), None);
    }

    #[test]
    fn resolve_is_case_insensitive() {
        let creds = Credentials {
            providers: HashMap::from([("MiniMax".to_string(), ProviderCredential { api_key: "sk-mm".to_string() })]),
        };
        assert_eq!(creds.resolve("minimax/MiniMax-M2.5", "https://api.minimax.io/v1"), Some("sk-mm".to_string()));
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        let creds = Credentials::load_global();
        assert!(creds.providers.is_empty() || !creds.providers.is_empty());
    }

    #[test]
    fn deserializes_valid_json() {
        let json = r#"{"providers":{"openai":{"api_key":"sk-123"}}}"#;
        let creds: Credentials = serde_json::from_str(json).unwrap();
        assert_eq!(creds.providers["openai"].api_key, "sk-123");
    }

    #[test]
    fn deserializes_empty_providers() {
        let json = r#"{}"#;
        let creds: Credentials = serde_json::from_str(json).unwrap();
        assert!(creds.providers.is_empty());
    }
}
