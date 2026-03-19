use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::config::Config;

const DEFAULT_GITHUB_OWNER: &str = "launchpad-ai";
const DEFAULT_GITHUB_REPO: &str = "ao-cli";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    pub github_owner: String,
    pub github_repo: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            github_owner: github_owner_from_env(),
            github_repo: github_repo_from_env(),
        }
    }
}

impl UpdateConfig {
    pub fn update_check_cache_path() -> PathBuf {
        Config::global_config_dir().join("update-check.json")
    }

    pub fn releases_api_url(&self, version: Option<&str>) -> String {
        match version {
            Some(v) => {
                let tag = if v.starts_with('v') { v.to_string() } else { format!("v{v}") };
                format!("https://api.github.com/repos/{}/{}/releases/tags/{tag}", self.github_owner, self.github_repo)
            }
            None => format!(
                "https://api.github.com/repos/{}/{}/releases/latest",
                self.github_owner, self.github_repo
            ),
        }
    }

    pub fn download_base_url(&self, version: &str) -> String {
        let tag = if version.starts_with('v') { version.to_string() } else { format!("v{version}") };
        format!("https://github.com/{}/{}/releases/download/{tag}", self.github_owner, self.github_repo)
    }
}

fn github_owner_from_env() -> String {
    std::env::var("AO_UPDATE_GITHUB_OWNER").unwrap_or_else(|_| DEFAULT_GITHUB_OWNER.to_string())
}

fn github_repo_from_env() -> String {
    std::env::var("AO_UPDATE_GITHUB_REPO").unwrap_or_else(|_| DEFAULT_GITHUB_REPO.to_string())
}
