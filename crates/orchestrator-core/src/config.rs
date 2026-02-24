use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub project_root: Option<String>,
    pub log_dir: Option<String>,
    pub max_agents: Option<usize>,
    pub headless: bool,
    pub runner_endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectRootSource {
    CliArg,
    EnvVar,
    Registry,
    CurrentDir,
}

pub fn resolve_project_root(config: &RuntimeConfig) -> (String, ProjectRootSource) {
    if let Some(root) = config
        .project_root
        .as_deref()
        .map(str::trim)
        .filter(|root| !root.is_empty())
    {
        return (root.to_string(), ProjectRootSource::CliArg);
    }

    if let Ok(root) = std::env::var("PROJECT_ROOT") {
        let root = root.trim();
        if !root.is_empty() {
            return (root.to_string(), ProjectRootSource::EnvVar);
        }
    }

    if let Some(root) = resolve_project_root_from_registry() {
        return (root, ProjectRootSource::Registry);
    }

    let cwd = std::env::current_dir()
        .expect("Failed to get current directory")
        .to_string_lossy()
        .to_string();

    (cwd, ProjectRootSource::CurrentDir)
}

// Mirrors existing precedence without importing Tauri internals.
fn resolve_project_root_from_registry() -> Option<String> {
    let home = dirs::home_dir()?;
    let candidate = home
        .join(".config")
        .join("agent-orchestrator")
        .join("last-project-root");
    let content = std::fs::read_to_string(candidate).ok()?;
    let root = content.trim();
    if root.is_empty() {
        None
    } else {
        Some(root.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_project_root_wins() {
        let config = RuntimeConfig {
            project_root: Some("/tmp/custom".to_string()),
            ..RuntimeConfig::default()
        };

        let (root, source) = resolve_project_root(&config);
        assert_eq!(root, "/tmp/custom");
        assert_eq!(source, ProjectRootSource::CliArg);
    }
}
