//! DEPRECATED: Will be replaced by GitProvider trait. See providers/git.rs
use super::*;

pub(crate) use ::workflow_runner_v2::MergeConflictContext;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PostMergeOutcome {
    Skipped,
    Completed,
    Conflict { context: MergeConflictContext },
}

pub fn push_branch(cwd: &str, remote: &str, branch: &str) -> Result<()> {
    run_external_command(cwd, "git", &["push", remote, branch], "push source branch")
}

pub fn create_pull_request(
    cwd: &str,
    base_branch: &str,
    head_branch: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<()> {
    let gh_available = ProcessCommand::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !gh_available {
        anyhow::bail!("gh CLI is not installed");
    }

    let mut command = ProcessCommand::new("gh");
    command
        .current_dir(cwd)
        .args([
            "pr",
            "create",
            "--base",
            base_branch,
            "--head",
            head_branch,
            "--title",
            title,
            "--body",
            body,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if draft {
        command.arg("--draft");
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run gh pr create in {}", cwd))?;
    if output.status.success() {
        return Ok(());
    }

    let summary = summarize_command_output(&output.stdout, &output.stderr);
    let summary_lower = summary.to_ascii_lowercase();
    if summary_lower.contains("already exists") {
        return Ok(());
    }
    anyhow::bail!("gh pr create failed: {summary}")
}


