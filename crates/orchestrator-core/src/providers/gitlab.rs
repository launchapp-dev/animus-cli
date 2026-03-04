use std::env;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;

use crate::providers::git::{
    BuiltinGitProvider, CreatePrInput, GitProvider, MergeResult, PullRequestInfo,
};

#[cfg(feature = "gitlab")]
#[derive(Debug, Clone)]
pub struct GitLabConfig {
    pub base_url: String,
    pub project_id: String,
    pub token_env: String,
}

#[cfg(feature = "gitlab")]
#[derive(Debug, Clone)]
pub struct GitLabGitProvider {
    pub config: GitLabConfig,
    pub client: Client,
    pub project_root: PathBuf,
}

#[cfg(feature = "gitlab")]
impl GitLabGitProvider {
    pub fn new(config: GitLabConfig, client: Client, project_root: impl Into<PathBuf>) -> Self {
        Self {
            config,
            client,
            project_root: project_root.into(),
        }
    }

    fn api_base(&self) -> String {
        format!(
            "{}/api/v4/projects/{}",
            self.config.base_url.trim_end_matches('/'),
            self.config.project_id.replace('/', "%2F"),
        )
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/{}", self.api_base(), path.trim_start_matches('/'))
    }

    fn auth_token(&self) -> Result<String> {
        env::var(&self.config.token_env).with_context(|| {
            format!(
                "GitLab token environment variable is missing: {}",
                self.config.token_env
            )
        })
    }

    async fn ensure_success(
        &self,
        response: reqwest::Response,
        action: &str,
    ) -> Result<reqwest::Response> {
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        Err(anyhow!("GitLab {action} failed ({status}): {body}"))
    }

    async fn merge_request_iid_for_branch(&self, head_branch: &str) -> Result<u64> {
        let token = self.auth_token()?;
        let response = self
            .client
            .get(self.api_url("merge_requests"))
            .header("Private-Token", token)
            .query(&[
                ("state", "opened"),
                ("source_branch", head_branch),
                ("per_page", "100"),
            ])
            .send()
            .await
            .context("failed to fetch merge requests from GitLab")?;
        let response = self
            .ensure_success(response, "fetch merge request")
            .await?;

        let requests: Vec<Value> = response
            .json()
            .await
            .context("failed to parse GitLab merge request list")?;
        let maybe_iid = requests.into_iter().find_map(|request| {
            if request.get("source_branch").and_then(Value::as_str) != Some(head_branch) {
                return None;
            }
            request.get("iid").and_then(Self::as_u64)
        });

        maybe_iid.ok_or_else(|| anyhow!("merge request for branch {head_branch} was not found"))
    }

    fn as_string(value: &Value) -> Option<String> {
        value
            .as_str()
            .map(ToString::to_string)
            .or_else(|| value.as_u64().map(|value| value.to_string()))
    }

    fn as_u64(value: &Value) -> Option<u64> {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
    }
}

#[cfg(feature = "gitlab")]
#[async_trait::async_trait]
impl GitProvider for GitLabGitProvider {
    async fn create_worktree(
        &self,
        project_root: &str,
        worktree_path: &str,
        branch_name: &str,
        base_ref: Option<&str>,
    ) -> Result<crate::providers::WorktreeInfo> {
        let builtin = BuiltinGitProvider::new(project_root);
        builtin
            .create_worktree(project_root, worktree_path, branch_name, base_ref)
            .await
    }

    async fn remove_worktree(&self, project_root: &str, worktree_path: &str) -> Result<()> {
        let builtin = BuiltinGitProvider::new(project_root);
        builtin.remove_worktree(project_root, worktree_path).await
    }

    async fn push_branch(&self, cwd: &str, remote: &str, branch: &str) -> Result<()> {
        let _ = remote;
        let builtin = BuiltinGitProvider::new(cwd);
        builtin.push_branch(cwd, remote, branch).await
    }

    async fn is_branch_merged(
        &self,
        project_root: &str,
        branch_name: &str,
    ) -> Result<Option<bool>> {
        let builtin = BuiltinGitProvider::new(project_root);
        builtin
            .is_branch_merged(project_root, branch_name)
            .await
    }

    async fn merge_branch(
        &self,
        cwd: &str,
        source_branch: &str,
        target_branch: &str,
        no_fast_forward: bool,
    ) -> Result<MergeResult> {
        let _ = (target_branch, no_fast_forward);
        let builtin = BuiltinGitProvider::new(cwd);
        builtin
            .merge_branch(cwd, source_branch, target_branch, no_fast_forward)
            .await
    }

    async fn create_pull_request(&self, input: CreatePrInput) -> Result<PullRequestInfo> {
        let token = self.auth_token()?;
        let title = if input.draft {
            format!("Draft: {}", input.title)
        } else {
            input.title
        };
        let response = self
            .client
            .post(self.api_url("merge_requests"))
            .header("Private-Token", token)
            .json(&serde_json::json!({
                "source_branch": input.head_branch,
                "target_branch": input.base_branch,
                "title": title,
                "description": input.body,
            }))
            .send()
            .await
            .context("failed to create GitLab merge request")?;
        let response = self
            .ensure_success(response, "create merge request")
            .await?;
        let payload: Value = response
            .json()
            .await
            .context("failed to parse GitLab merge request response")?;

        Ok(PullRequestInfo {
            id: payload.get("id").and_then(Self::as_string),
            number: payload.get("iid").and_then(Self::as_u64),
            url: payload
                .get("web_url")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        })
    }

    async fn enable_auto_merge(&self, _cwd: &str, head_branch: &str) -> Result<()> {
        let _ = _cwd;
        let iid = self.merge_request_iid_for_branch(head_branch).await?;
        let token = self.auth_token()?;
        let response = self
            .client
            .put(self.api_url(&format!("merge_requests/{iid}/merge")))
            .header("Private-Token", token)
            .query(&[("merge_when_pipeline_succeeds", "true")])
            .send()
            .await
            .context("failed to enable auto-merge in GitLab")?;
        let _ = self.ensure_success(response, "enable auto-merge").await?;

        Ok(())
    }
}
