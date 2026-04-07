use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct SyncConfig {
    pub server: Option<String>,
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    pub access_token_expires_at: Option<String>,
    pub project_id: Option<String>,
    pub last_synced_at: Option<String>,
}

impl SyncConfig {
    pub fn load_global() -> Self {
        let path = Self::global_path();
        Self::try_load_from(&path).unwrap_or_default()
    }

    pub fn load_for_project(project_root: &str) -> Self {
        let project_path = PathBuf::from(project_root).join(".ao").join("sync.json");
        if let Some(project_config) = Self::try_load_from(&project_path) {
            return project_config.merge_with_global();
        }
        Self::load_global()
    }

    pub fn save_global(&self) -> anyhow::Result<()> {
        let path = Self::global_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn save_for_project(&self, project_root: &str) -> anyhow::Result<()> {
        let path = PathBuf::from(project_root).join(".ao").join("sync.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    fn merge_with_global(self) -> Self {
        let global = Self::load_global();
        SyncConfig {
            server: self.server.or(global.server),
            token: self.token.or(global.token),
            refresh_token: self.refresh_token.or(global.refresh_token),
            access_token_expires_at: self.access_token_expires_at.or(global.access_token_expires_at),
            project_id: self.project_id.or(global.project_id),
            last_synced_at: self.last_synced_at.or(global.last_synced_at),
        }
    }

    fn global_path() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".ao").join("sync.json");
        }
        PathBuf::from(".ao").join("sync.json")
    }

    fn try_load_from(path: &PathBuf) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn is_configured(&self) -> bool {
        self.server.is_some() && self.token.is_some()
    }

    pub fn server_url(&self) -> anyhow::Result<String> {
        self.server.clone().ok_or_else(|| {
            anyhow::anyhow!("Sync server not configured. Run: animus sync setup --server <url> --token <token>")
        })
    }

    pub fn bearer_token(&self) -> anyhow::Result<String> {
        self.token.clone().ok_or_else(|| {
            anyhow::anyhow!("Sync token not configured. Run: animus sync setup --server <url> --token <token>")
        })
    }

    pub fn needs_token_refresh(&self) -> bool {
        if let Some(ref expires_at) = self.access_token_expires_at {
            if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_at) {
                let now = chrono::Utc::now();
                let refresh_threshold = expires.with_timezone(&chrono::Utc) - chrono::Duration::minutes(5);
                return now >= refresh_threshold;
            }
        }
        false
    }

    pub fn can_refresh_token(&self) -> bool {
        self.refresh_token.is_some() && self.server.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_token_refresh_when_expired() {
        let now = chrono::Utc::now();
        let expired_time = now - chrono::Duration::minutes(1);
        let mut config = SyncConfig::default();
        config.access_token_expires_at = Some(expired_time.to_rfc3339());

        assert!(config.needs_token_refresh());
    }

    #[test]
    fn test_needs_token_refresh_within_threshold() {
        let now = chrono::Utc::now();
        let soon_expiring = now + chrono::Duration::minutes(3); // Within 5-minute threshold
        let mut config = SyncConfig::default();
        config.access_token_expires_at = Some(soon_expiring.to_rfc3339());

        assert!(config.needs_token_refresh());
    }

    #[test]
    fn test_no_token_refresh_needed_when_valid() {
        let now = chrono::Utc::now();
        let far_future = now + chrono::Duration::hours(1);
        let mut config = SyncConfig::default();
        config.access_token_expires_at = Some(far_future.to_rfc3339());

        assert!(!config.needs_token_refresh());
    }

    #[test]
    fn test_no_token_refresh_needed_when_no_expiration() {
        let config = SyncConfig::default();
        assert!(!config.needs_token_refresh());
    }

    #[test]
    fn test_can_refresh_token_requires_refresh_token_and_server() {
        let mut config = SyncConfig::default();
        assert!(!config.can_refresh_token());

        config.refresh_token = Some("token".to_string());
        assert!(!config.can_refresh_token());

        config.server = Some("http://example.com".to_string());
        assert!(config.can_refresh_token());
    }

    #[test]
    fn test_sync_config_preserves_token_fields() {
        // Test that token fields are properly serialized and deserialized
        let config = SyncConfig {
            server: Some("http://example.com".to_string()),
            token: Some("access_token".to_string()),
            refresh_token: Some("refresh_token".to_string()),
            access_token_expires_at: Some("2025-12-31T23:59:59Z".to_string()),
            project_id: Some("project-123".to_string()),
            last_synced_at: Some("2025-01-01T00:00:00Z".to_string()),
        };

        // Serialize and deserialize
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SyncConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.server, Some("http://example.com".to_string()));
        assert_eq!(deserialized.token, Some("access_token".to_string()));
        assert_eq!(deserialized.refresh_token, Some("refresh_token".to_string()));
        assert_eq!(deserialized.access_token_expires_at, Some("2025-12-31T23:59:59Z".to_string()));
        assert_eq!(deserialized.project_id, Some("project-123".to_string()));
        assert_eq!(deserialized.last_synced_at, Some("2025-01-01T00:00:00Z".to_string()));
    }
}
