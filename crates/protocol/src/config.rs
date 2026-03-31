use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ProjectMcpServerTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        auth_token: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMcpServerEntry {
    #[serde(flatten)]
    pub transport: ProjectMcpServerTransport,
    #[serde(default)]
    pub assign_to: Vec<String>,
}

impl ProjectMcpServerEntry {
    pub fn is_assigned_to(&self, agent_id: Option<&str>) -> bool {
        self.assign_to.is_empty()
            || agent_id
                .is_some_and(|id| self.assign_to.iter().any(|assigned| assigned.eq_ignore_ascii_case(id)))
    }
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
}

impl Config {
    pub fn global_config_dir() -> PathBuf {
        if let Some(override_path) = config_dir_override() {
            return override_path;
        }

        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".ao")
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
        Ok(project_path.join(".ao").join("config.json"))
    }

    fn load_or_initialize(config_path: &Path) -> Result<Self> {
        if config_path.exists() {
            let content = fs::read_to_string(config_path)?;
            return serde_json::from_str(&content).context("Failed to parse config file");
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let default_config =
            Self { agent_runner_token: None, mcp_servers: BTreeMap::new(), claude_profiles: BTreeMap::new() };
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
    std::env::var("AO_CONFIG_DIR")
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
    let mut prefixes = vec!["ao.".to_string(), "mcp__ao__".to_string(), "mcp.ao.".to_string()];

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
        assert!(matches!(
            entry.transport,
            ProjectMcpServerTransport::Stdio { ref command, ref args, ref env }
                if command == "/usr/local/bin/db-mcp"
                    && args == &vec!["--port".to_string(), "5432".to_string()]
                    && env.get("DB_HOST").map(String::as_str) == Some("localhost")
        ));
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
    fn config_with_remote_mcp_server_roundtrips() {
        let json = r#"{
            "agent_runner_token": null,
            "mcp_servers": {
                "docs": {
                    "url": "https://docs.example/mcp",
                    "auth_token": "Bearer docs",
                    "assign_to": ["researcher"]
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        let entry = &config.mcp_servers["docs"];
        assert!(matches!(
            entry.transport,
            ProjectMcpServerTransport::StreamableHttp { ref url, ref auth_token }
                if url == "https://docs.example/mcp" && auth_token.as_deref() == Some("Bearer docs")
        ));
        assert_eq!(entry.assign_to, vec!["researcher"]);

        let serialized = serde_json::to_string(&config).unwrap();
        let roundtripped: Config = serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            roundtripped.mcp_servers["docs"].transport,
            ProjectMcpServerTransport::StreamableHttp { ref url, ref auth_token }
                if url == "https://docs.example/mcp" && auth_token.as_deref() == Some("Bearer docs")
        ));
    }

    #[test]
    fn project_mcp_server_assignment_matching_is_case_insensitive() {
        let entry = ProjectMcpServerEntry {
            transport: ProjectMcpServerTransport::StreamableHttp {
                url: "https://docs.example/mcp".to_string(),
                auth_token: None,
            },
            assign_to: vec!["SWE".to_string()],
        };

        assert!(entry.is_assigned_to(Some("swe")));
        assert!(!entry.is_assigned_to(Some("research")));
    }

    #[test]
    fn config_serialization_omits_empty_mcp_servers() {
        let config =
            Config { agent_runner_token: None, mcp_servers: BTreeMap::new(), claude_profiles: BTreeMap::new() };
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(!json.contains("mcp_servers"));
        assert!(!json.contains("claude_profiles"));
    }
}
