use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectMcpServerEntry {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub assign_to: Vec<String>,
    /// Transport type: "stdio" (default) or "http".
    #[serde(default)]
    pub transport: Option<String>,
    /// HTTP endpoint URL. Required when transport is "http".
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeProfileEntry {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub agent_runner_token: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, ProjectMcpServerEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub claude_profiles: BTreeMap<String, ClaudeProfileEntry>,
    /// Default subject kind for CLI commands that take `--kind`. When set,
    /// `animus subject list` (and siblings) may be invoked without `--kind`
    /// and the configured kind is used. Set this so the common case
    /// (single-backend projects) doesn't need to pass `--kind task` every
    /// invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_subject_kind: Option<String>,
    /// Opt-in anonymous usage metrics. `enabled = None` means the
    /// first-run prompt has not been shown yet; `Some(false)` means the
    /// user (or env override) declined; `Some(true)` means opted in.
    /// See `docs/reference/configuration.md` for the privacy invariants.
    #[serde(default, skip_serializing_if = "MetricsConfig::is_default")]
    pub metrics: MetricsConfig,
    /// Self-update behaviour. Absent in older config files; the `Default`
    /// posture matches a fresh install (mode=notify, interval=P1D,
    /// channel=stable). See `crates/orchestrator-cli/src/services/self_update`.
    #[serde(default, skip_serializing_if = "AutoUpdateConfig::is_default")]
    pub auto_update: AutoUpdateConfig,
}

/// Self-update operating mode. Operators can change this via
/// `.animus/config.json`, the `ANIMUS_AUTO_UPDATE_MODE` env var, or
/// short-circuit the whole machinery via `ANIMUS_AUTO_UPDATE_DISABLE=1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AutoUpdateMode {
    Off,
    #[default]
    Notify,
    Prompt,
    Auto,
}

impl AutoUpdateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Notify => "notify",
            Self::Prompt => "prompt",
            Self::Auto => "auto",
        }
    }

    pub fn parse_lossy(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" => Some(Self::Off),
            "notify" | "default" => Some(Self::Notify),
            "prompt" | "ask" => Some(Self::Prompt),
            "auto" | "apply" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AutoUpdateChannel {
    #[default]
    Stable,
    Prerelease,
}

impl AutoUpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoUpdateConfig {
    #[serde(default)]
    pub mode: AutoUpdateMode,
    #[serde(default = "default_check_interval")]
    pub check_interval: String,
    #[serde(default)]
    pub channel: AutoUpdateChannel,
}

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self {
            mode: AutoUpdateMode::default(),
            check_interval: default_check_interval(),
            channel: AutoUpdateChannel::default(),
        }
    }
}

impl AutoUpdateConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

fn default_check_interval() -> String {
    "P1D".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default = "default_metrics_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_metrics_batch_interval")]
    pub batch_interval: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_id: Option<String>,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            endpoint: default_metrics_endpoint(),
            batch_interval: default_metrics_batch_interval(),
            install_id: None,
        }
    }
}

impl MetricsConfig {
    pub fn is_default(&self) -> bool {
        self.enabled.is_none()
            && self.install_id.is_none()
            && self.endpoint == default_metrics_endpoint()
            && self.batch_interval == default_metrics_batch_interval()
    }
}

fn default_metrics_endpoint() -> String {
    "https://metrics.animus.dev/v1/events".to_string()
}

fn default_metrics_batch_interval() -> String {
    "P1D".to_string()
}

impl Config {
    pub fn global_config_dir() -> PathBuf {
        if let Some(override_path) = config_dir_override() {
            return override_path;
        }

        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".animus")
    }

    pub fn load_global() -> Result<Self> {
        Self::load_from_dir(&Self::global_config_dir())
    }

    pub fn load_from_dir(config_dir: &Path) -> Result<Self> {
        fs::create_dir_all(config_dir)
            .with_context(|| format!("Failed to create config directory {}", config_dir.display()))?;
        Self::load_or_initialize(&config_dir.join("config.json"))
    }

    pub fn load_or_default(project_root: &str) -> Result<Self> {
        let config_path = Self::config_path(project_root)?;
        Self::load_or_initialize(&config_path)
    }

    pub fn save(&self, project_root: &str) -> Result<()> {
        let config_path = Self::config_path(project_root)?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        fs::write(&config_path, json)?;
        Ok(())
    }

    fn config_path(project_root: &str) -> Result<PathBuf> {
        let project_path = PathBuf::from(project_root).canonicalize().context("Invalid project root")?;
        Ok(project_path.join(".animus").join("config.json"))
    }

    fn load_or_initialize(config_path: &Path) -> Result<Self> {
        if config_path.exists() {
            let content = fs::read_to_string(config_path)?;
            return serde_json::from_str(&content).context("Failed to parse config file");
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let default_config = Self {
            agent_runner_token: None,
            mcp_servers: BTreeMap::new(),
            claude_profiles: BTreeMap::new(),
            default_subject_kind: Some("task".to_string()),
            metrics: MetricsConfig::default(),
            auto_update: AutoUpdateConfig::default(),
        };
        let json = serde_json::to_string_pretty(&default_config)?;
        fs::write(config_path, json)?;
        Ok(default_config)
    }

    pub fn ensure_token_exists(config_dir: &Path) -> Result<()> {
        let config_path = config_dir.join("config.json");
        let mut config = Self::load_from_dir(config_dir)?;
        if config.agent_runner_token.as_deref().is_none_or(|t| t.trim().is_empty()) {
            config.agent_runner_token = Some(Uuid::new_v4().to_string());
            let json = serde_json::to_string_pretty(&config)?;
            fs::write(&config_path, json)
                .with_context(|| format!("Failed to write token to {}", config_path.display()))?;
        }
        Ok(())
    }

    pub fn get_token(&self) -> Result<String> {
        normalize_token("agent_runner_token", self.agent_runner_token.clone().unwrap_or_default())
    }

    pub fn claude_profile(&self, name: &str) -> Option<&ClaudeProfileEntry> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return None;
        }
        self.claude_profiles.get(trimmed)
    }
}

fn normalize_token(source: &str, raw: String) -> Result<String> {
    let token = raw.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("{source} is missing or empty");
    }
    Ok(token)
}

fn config_dir_override() -> Option<PathBuf> {
    std::env::var("ANIMUS_CONFIG_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Returns the path to the CLI process tracker file.
/// This is used for orphan process detection and cleanup.
pub fn cli_tracker_path() -> PathBuf {
    Config::global_config_dir().join("cli-tracker.json")
}

/// Returns the path to the daemon events log file.
pub fn daemon_events_log_path() -> PathBuf {
    Config::global_config_dir().join("daemon-events.jsonl")
}

/// Returns the default allowed MCP tool prefixes for the given agent ID.
///
/// This constructs the canonical MCP tool prefix whitelist for enforcing
/// MCP-only policy on agent runs. The prefixes cover both direct tool names
/// and MCP-prefixed variants.
pub fn default_allowed_tool_prefixes(agent_id: &str) -> Vec<String> {
    let normalized = agent_id.trim().to_ascii_lowercase();
    let mut prefixes = vec!["animus.".to_string(), "mcp__animus__".to_string(), "mcp.animus.".to_string()];

    if !normalized.is_empty() {
        prefixes.push(format!("{normalized}."));
        prefixes.push(format!("mcp__{normalized}__"));
        prefixes.push(format!("mcp.{normalized}."));

        let snake = normalized.replace('-', "_");
        prefixes.push(format!("{snake}."));
        prefixes.push(format!("mcp__{snake}__"));
        prefixes.push(format!("mcp.{snake}."));
    }

    prefixes.sort();
    prefixes.dedup();
    prefixes
}

/// Parses a boolean environment variable.
///
/// Returns true if the value is not "0", "false", "no", or "off" (case-insensitive).
/// Returns false if not set or matches one of the false values.
pub fn parse_env_bool(key: &str) -> bool {
    parse_env_bool_opt(key).unwrap_or(false)
}

/// Parses a boolean environment variable into an Option.
///
/// Returns Some(true) if the value is not "0", "false", "no", or "off" (case-insensitive).
/// Returns Some(false) if it matches one of the false values.
/// Returns None if not set or empty.
pub fn parse_env_bool_opt(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .map(|value| !matches!(value.as_str(), "0" | "false" | "no" | "off"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_mcp_servers_deserializes() {
        let json = r#"{"agent_runner_token": null}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.mcp_servers.is_empty());
        assert!(config.claude_profiles.is_empty());
    }

    #[test]
    fn config_with_mcp_servers_roundtrips() {
        let json = r#"{
            "agent_runner_token": null,
            "mcp_servers": {
                "my-db": {
                    "command": "/usr/local/bin/db-mcp",
                    "args": ["--port", "5432"],
                    "env": {"DB_HOST": "localhost"},
                    "assign_to": ["swe"]
                }
            },
            "claude_profiles": {
                "work": {
                    "env": {"CLAUDE_CONFIG_DIR": "/Users/test/.claude-work"}
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.mcp_servers.len(), 1);
        let entry = &config.mcp_servers["my-db"];
        assert_eq!(entry.command, "/usr/local/bin/db-mcp");
        assert_eq!(entry.args, vec!["--port", "5432"]);
        assert_eq!(entry.env.get("DB_HOST").map(String::as_str), Some("localhost"));
        assert_eq!(entry.assign_to, vec!["swe"]);
        assert_eq!(
            config.claude_profiles["work"].env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/Users/test/.claude-work")
        );

        let serialized = serde_json::to_string(&config).unwrap();
        let roundtripped: Config = serde_json::from_str(&serialized).unwrap();
        assert_eq!(roundtripped.mcp_servers.len(), 1);
        assert_eq!(roundtripped.claude_profiles.len(), 1);
    }

    #[test]
    fn config_serialization_omits_empty_mcp_servers() {
        let config = Config {
            agent_runner_token: None,
            mcp_servers: BTreeMap::new(),
            claude_profiles: BTreeMap::new(),
            default_subject_kind: None,
            metrics: MetricsConfig::default(),
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(!json.contains("mcp_servers"));
        assert!(!json.contains("claude_profiles"));
        assert!(!json.contains("default_subject_kind"));
        assert!(!json.contains("metrics"));
    }

    #[test]
    fn config_without_metrics_block_defaults_to_unknown_consent() {
        let json = r#"{"agent_runner_token": null}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.metrics.enabled.is_none(), "missing metrics block should mean never-asked");
        assert!(config.metrics.install_id.is_none());
        assert_eq!(config.metrics.endpoint, "https://metrics.animus.dev/v1/events");
        assert_eq!(config.metrics.batch_interval, "P1D");
        assert!(config.metrics.is_default());
    }

    #[test]
    fn config_with_metrics_block_roundtrips() {
        let json = r#"{
            "agent_runner_token": null,
            "metrics": {
                "enabled": true,
                "endpoint": "https://metrics.example.com/v1/events",
                "batch_interval": "PT1H",
                "install_id": "11111111-2222-3333-4444-555555555555"
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.metrics.enabled, Some(true));
        assert_eq!(config.metrics.endpoint, "https://metrics.example.com/v1/events");
        assert_eq!(config.metrics.batch_interval, "PT1H");
        assert_eq!(config.metrics.install_id.as_deref(), Some("11111111-2222-3333-4444-555555555555"));

        let serialized = serde_json::to_string(&config).unwrap();
        let roundtripped: Config = serde_json::from_str(&serialized).unwrap();
        assert_eq!(roundtripped.metrics.enabled, Some(true));
        assert_eq!(roundtripped.metrics.install_id.as_deref(), Some("11111111-2222-3333-4444-555555555555"));
    }
}
