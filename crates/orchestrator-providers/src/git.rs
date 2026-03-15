use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MergeResult {
    pub merged: bool,
    pub conflicted_files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CreatePrInput {
    pub cwd: String,
    pub base_branch: String,
    pub head_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Debug, Clone)]
pub struct PullRequestInfo {
    pub id: Option<String>,
    pub number: Option<u64>,
    pub url: Option<String>,
}

#[async_trait]
pub trait GitProvider: Send + Sync {
    async fn create_worktree(
        &self,
        project_root: &str,
        worktree_path: &str,
        branch_name: &str,
        base_ref: Option<&str>,
    ) -> Result<WorktreeInfo>;

    async fn remove_worktree(&self, project_root: &str, worktree_path: &str) -> Result<()>;

    async fn push_branch(&self, cwd: &str, remote: &str, branch: &str) -> Result<()>;

    async fn is_branch_merged(&self, project_root: &str, branch_name: &str) -> Result<Option<bool>>;

    async fn merge_branch(
        &self,
        cwd: &str,
        source_branch: &str,
        target_branch: &str,
        no_fast_forward: bool,
    ) -> Result<MergeResult>;

    async fn create_pull_request(&self, input: CreatePrInput) -> Result<PullRequestInfo>;

    async fn enable_auto_merge(&self, cwd: &str, head_branch: &str) -> Result<()>;
}

#[derive(Default)]
pub struct GitHubProvider;

#[derive(Debug, Clone)]
pub struct BuiltinGitProvider {
    project_root: PathBuf,
}

impl BuiltinGitProvider {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self { project_root: project_root.into() }
    }

    async fn run_git(&self, args: &[String], cwd: Option<&str>) -> Result<std::process::Output> {
        let cwd = cwd.map_or(self.project_root.as_path(), Path::new);
        let output = Command::new("git").args(args).current_dir(cwd).output().await?;
        Ok(output)
    }

    async fn run_gh(&self, args: &[String], cwd: Option<&str>) -> Result<std::process::Output> {
        let cwd = cwd.map_or(self.project_root.as_path(), Path::new);
        let output = Command::new("gh").args(args).current_dir(cwd).output().await?;
        Ok(output)
    }

    fn command_failed(output: &std::process::Output, command: &str, args: &[String]) -> anyhow::Error {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::anyhow!(
            "{command} {} failed (exit {}): stdout: {stdout}, stderr: {stderr}",
            args.join(" "),
            output.status.code().map(|code| code.to_string()).unwrap_or_else(|| "unknown".to_string())
        )
    }
}

#[async_trait]
impl GitProvider for GitHubProvider {
    async fn create_worktree(
        &self,
        _project_root: &str,
        _worktree_path: &str,
        _branch_name: &str,
        _base_ref: Option<&str>,
    ) -> Result<WorktreeInfo> {
        todo!()
    }

    async fn remove_worktree(&self, _project_root: &str, _worktree_path: &str) -> Result<()> {
        todo!()
    }

    async fn push_branch(&self, _cwd: &str, _remote: &str, _branch: &str) -> Result<()> {
        todo!()
    }

    async fn is_branch_merged(&self, _project_root: &str, _branch_name: &str) -> Result<Option<bool>> {
        todo!()
    }

    async fn merge_branch(
        &self,
        _cwd: &str,
        _source_branch: &str,
        _target_branch: &str,
        _no_fast_forward: bool,
    ) -> Result<MergeResult> {
        todo!()
    }

    async fn create_pull_request(&self, _input: CreatePrInput) -> Result<PullRequestInfo> {
        todo!()
    }

    async fn enable_auto_merge(&self, _cwd: &str, _head_branch: &str) -> Result<()> {
        todo!()
    }
}

#[async_trait]
impl GitProvider for BuiltinGitProvider {
    async fn create_worktree(
        &self,
        _project_root: &str,
        worktree_path: &str,
        branch_name: &str,
        _base_ref: Option<&str>,
    ) -> Result<WorktreeInfo> {
        let args = vec![
            "worktree".to_string(),
            "add".to_string(),
            worktree_path.to_string(),
            "-b".to_string(),
            branch_name.to_string(),
            "HEAD".to_string(),
        ];
        let output = self.run_git(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "git", &args));
        }

        Ok(WorktreeInfo {
            name: branch_name.to_string(),
            path: worktree_path.to_string(),
            branch: Some(branch_name.to_string()),
        })
    }

    async fn remove_worktree(&self, _project_root: &str, worktree_path: &str) -> Result<()> {
        let args = vec!["worktree".to_string(), "remove".to_string(), worktree_path.to_string()];
        let output = self.run_git(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "git", &args));
        }

        Ok(())
    }

    async fn push_branch(&self, _cwd: &str, _remote: &str, branch: &str) -> Result<()> {
        let args = vec!["push".to_string(), "-u".to_string(), "origin".to_string(), branch.to_string()];
        let output = self.run_git(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "git", &args));
        }

        Ok(())
    }

    async fn is_branch_merged(&self, _project_root: &str, branch_name: &str) -> Result<Option<bool>> {
        let args = vec!["branch".to_string(), "--merged".to_string(), "main".to_string()];
        let output = self.run_git(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "git", &args));
        }

        let merged = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .any(|line| line == branch_name);

        Ok(Some(merged))
    }

    async fn merge_branch(
        &self,
        _cwd: &str,
        source_branch: &str,
        _target_branch: &str,
        _no_fast_forward: bool,
    ) -> Result<MergeResult> {
        let args = vec!["merge".to_string(), source_branch.to_string(), "--no-edit".to_string()];
        let output = self.run_git(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "git", &args));
        }

        Ok(MergeResult { merged: true, conflicted_files: vec![] })
    }

    async fn create_pull_request(&self, input: CreatePrInput) -> Result<PullRequestInfo> {
        let mut args = vec![
            "pr".to_string(),
            "create".to_string(),
            "--title".to_string(),
            input.title,
            "--body".to_string(),
            input.body,
            "--head".to_string(),
            input.head_branch,
        ];

        if input.draft {
            args.push("--draft".to_string());
        }

        let output = self.run_gh(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "gh", &args));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PullRequestInfo { id: None, number: None, url: if stdout.is_empty() { None } else { Some(stdout) } })
    }

    async fn enable_auto_merge(&self, _cwd: &str, head_branch: &str) -> Result<()> {
        let args = vec![
            "pr".to_string(),
            "merge".to_string(),
            head_branch.to_string(),
            "--auto".to_string(),
            "--merge".to_string(),
        ];
        let output = self.run_gh(&args, None).await?;

        if !output.status.success() {
            return Err(Self::command_failed(&output, "gh", &args));
        }

        Ok(())
    }
}
