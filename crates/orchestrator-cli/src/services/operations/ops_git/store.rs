use super::*;
use anyhow::{anyhow, Context, Result};

use super::model::{GitConfirmationStoreCli, GitRepoRegistry, GitWorktreeInfoCli};

fn git_repo_registry_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("git-repos.json")
}

fn git_confirmations_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("git-confirmations.json")
}

pub(super) fn load_git_repo_registry(project_root: &str) -> Result<GitRepoRegistry> {
    read_json_or_default(&git_repo_registry_path(project_root))
}

pub(super) fn save_git_repo_registry(project_root: &str, registry: &GitRepoRegistry) -> Result<()> {
    write_json_pretty(&git_repo_registry_path(project_root), registry)
}

pub(super) fn load_git_confirmations(project_root: &str) -> Result<GitConfirmationStoreCli> {
    read_json_or_default(&git_confirmations_path(project_root))
}

pub(super) fn save_git_confirmations(
    project_root: &str,
    store: &GitConfirmationStoreCli,
) -> Result<()> {
    write_json_pretty(&git_confirmations_path(project_root), store)
}

pub(super) fn repos_root(project_root: &str) -> PathBuf {
    Path::new(project_root).join(".ao").join("repos")
}

pub(super) fn run_git(repo_path: &Path, args: &[&str]) -> Result<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git command in {}", repo_path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("git command failed: {}", stderr);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(super) fn resolve_repo_path(project_root: &str, repo_name: &str) -> Result<PathBuf> {
    if repo_name == "." || repo_name == "current" {
        return Ok(PathBuf::from(project_root));
    }

    let repo_path_candidate = PathBuf::from(repo_name);
    if repo_path_candidate.exists() {
        return Ok(repo_path_candidate);
    }

    let registry = load_git_repo_registry(project_root)?;
    if let Some(repo) = registry.repos.iter().find(|repo| repo.name == repo_name) {
        return Ok(PathBuf::from(&repo.path));
    }

    let repo_path = repos_root(project_root).join(repo_name);
    if repo_path.exists() {
        return Ok(repo_path);
    }

    anyhow::bail!("repository not found: {repo_name}")
}

fn parse_worktree_list_output(output: &str) -> Vec<GitWorktreeInfoCli> {
    let mut worktrees = Vec::new();
    let mut current: Option<GitWorktreeInfoCli> = None;

    for line in output.lines() {
        if line.trim().is_empty() {
            if let Some(record) = current.take() {
                worktrees.push(record);
            }
            continue;
        }

        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(record) = current.take() {
                worktrees.push(record);
            }
            let path = path.trim().to_string();
            let worktree_name = PathBuf::from(&path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("worktree")
                .to_string();
            current = Some(GitWorktreeInfoCli {
                worktree_name,
                path,
                head: None,
                branch: None,
            });
            continue;
        }

        if let Some(head) = line.strip_prefix("HEAD ") {
            if let Some(record) = current.as_mut() {
                record.head = Some(head.trim().to_string());
            }
            continue;
        }

        if let Some(branch) = line.strip_prefix("branch ") {
            if let Some(record) = current.as_mut() {
                record.branch = Some(branch.trim().trim_start_matches("refs/heads/").to_string());
            }
        }
    }

    if let Some(record) = current.take() {
        worktrees.push(record);
    }
    worktrees
}

pub(super) fn load_worktrees(repo_path: &Path) -> Result<Vec<GitWorktreeInfoCli>> {
    let output = run_git(repo_path, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list_output(&output))
}

pub(super) fn resolve_worktree_path(repo_path: &Path, worktree_name: &str) -> Result<PathBuf> {
    let worktrees = load_worktrees(repo_path)?;
    let worktree = worktrees
        .into_iter()
        .find(|entry| entry.worktree_name == worktree_name || entry.path.ends_with(worktree_name))
        .ok_or_else(|| anyhow!("worktree not found: {worktree_name}"))?;
    Ok(PathBuf::from(worktree.path))
}

pub(super) fn git_confirmation_next_step(operation_type: &str, repo_name: &str) -> String {
    format!(
        "request and approve a git confirmation for '{}' on '{}', then rerun with --confirmation-id <id>",
        operation_type, repo_name
    )
}

fn git_confirmation_required_message(operation_type: &str, repo_name: &str) -> String {
    format!(
        "CONFIRMATION_REQUIRED: {}; use --dry-run to preview changes",
        git_confirmation_next_step(operation_type, repo_name)
    )
}

pub(super) fn ensure_confirmation(
    project_root: &str,
    confirmation_id: Option<&str>,
    operation_type: &str,
    repo_name: &str,
) -> Result<()> {
    let confirmation_id = confirmation_id
        .ok_or_else(|| anyhow!(git_confirmation_required_message(operation_type, repo_name)))?;
    let store = load_git_confirmations(project_root)?;
    let request = store
        .requests
        .iter()
        .find(|request| request.id == confirmation_id)
        .ok_or_else(|| anyhow!("confirmation request not found: {confirmation_id}"))?;
    if request.blocked {
        anyhow::bail!("operation blocked by policy: {}", request.reason);
    }
    if !request.required {
        anyhow::bail!(
            "confirmation_id '{}' is not marked as required for destructive operations",
            confirmation_id
        );
    }
    if !request.operation_type.eq_ignore_ascii_case(operation_type) {
        anyhow::bail!(
            "confirmation '{}' operation mismatch: expected '{}', found '{}'",
            confirmation_id,
            operation_type,
            request.operation_type
        );
    }
    if !request.repo_name.eq_ignore_ascii_case(repo_name) {
        anyhow::bail!(
            "confirmation '{}' repo mismatch: expected '{}', found '{}'",
            confirmation_id,
            repo_name,
            request.repo_name
        );
    }
    if request.approved != Some(true) {
        anyhow::bail!("operation not approved for confirmation_id: {confirmation_id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{git_confirmation_next_step, git_confirmation_required_message};

    #[test]
    fn git_confirmation_messages_use_canonical_token_order() {
        let next_step = git_confirmation_next_step("force_push", "repo-a");
        assert_eq!(
            next_step,
            "request and approve a git confirmation for 'force_push' on 'repo-a', then rerun with --confirmation-id <id>"
        );

        let required = git_confirmation_required_message("force_push", "repo-a");
        assert_eq!(
            required,
            "CONFIRMATION_REQUIRED: request and approve a git confirmation for 'force_push' on 'repo-a', then rerun with --confirmation-id <id>; use --dry-run to preview changes"
        );
    }
}
