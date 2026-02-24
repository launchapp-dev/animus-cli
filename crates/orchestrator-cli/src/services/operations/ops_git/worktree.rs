use super::*;
use anyhow::{anyhow, Result};

use super::model::GitSyncStatusCli;
use super::store::{
    ensure_force_confirmation, load_worktrees, resolve_repo_path, resolve_worktree_path, run_git,
};

pub(super) fn handle_git_worktree(
    command: GitWorktreeCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        GitWorktreeCommand::Create(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let mut command = ProcessCommand::new("git");
            command.arg("-C").arg(&repo_path).arg("worktree").arg("add");
            if args.create_branch {
                command.arg("-b").arg(&args.branch);
            }
            command.arg(&args.worktree_path).arg(&args.branch);
            let output = command.output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "git worktree add failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            print_value(
                serde_json::json!({
                    "repo": args.repo,
                    "worktree_name": args.worktree_name,
                    "worktree_path": args.worktree_path,
                    "branch": args.branch,
                }),
                json,
            )
        }
        GitWorktreeCommand::List(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            print_value(load_worktrees(&repo_path)?, json)
        }
        GitWorktreeCommand::Get(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let worktree = load_worktrees(&repo_path)?
                .into_iter()
                .find(|entry| entry.worktree_name == args.worktree_name)
                .ok_or_else(|| anyhow!("worktree not found: {}", args.worktree_name))?;
            print_value(worktree, json)
        }
        GitWorktreeCommand::Remove(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            if args.force {
                ensure_force_confirmation(project_root, args.confirmation_id.as_deref())?;
            }
            let mut cmd = vec!["worktree", "remove", args.worktree_name.as_str()];
            if args.force {
                cmd.push("--force");
            }
            let output = run_git(&repo_path, &cmd)?;
            print_value(
                serde_json::json!({
                    "repo": args.repo,
                    "worktree_name": args.worktree_name,
                    "force": args.force,
                    "output": output,
                }),
                json,
            )
        }
        GitWorktreeCommand::Pull(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let worktree_path = resolve_worktree_path(&repo_path, &args.worktree_name)?;
            let output = run_git(&worktree_path, &["pull", args.remote.as_str()])?;
            print_value(
                serde_json::json!({
                    "repo": args.repo,
                    "worktree_name": args.worktree_name,
                    "remote": args.remote,
                    "output": output,
                }),
                json,
            )
        }
        GitWorktreeCommand::Push(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let worktree_path = resolve_worktree_path(&repo_path, &args.worktree_name)?;
            if args.force {
                ensure_force_confirmation(project_root, args.confirmation_id.as_deref())?;
            }
            let branch = run_git(&worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
            let mut cmd = vec!["push", args.remote.as_str(), branch.trim()];
            if args.force {
                cmd.push("--force");
            }
            let output = run_git(&worktree_path, &cmd)?;
            print_value(
                serde_json::json!({
                    "repo": args.repo,
                    "worktree_name": args.worktree_name,
                    "remote": args.remote,
                    "force": args.force,
                    "output": output,
                }),
                json,
            )
        }
        GitWorktreeCommand::Sync(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let worktree_path = resolve_worktree_path(&repo_path, &args.worktree_name)?;
            let pull_output = run_git(&worktree_path, &["pull", args.remote.as_str()])?;
            let branch = run_git(&worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
            let push_output = run_git(
                &worktree_path,
                &["push", args.remote.as_str(), branch.trim()],
            )?;
            print_value(
                serde_json::json!({
                    "repo": args.repo,
                    "worktree_name": args.worktree_name,
                    "remote": args.remote,
                    "pull_output": pull_output,
                    "push_output": push_output,
                }),
                json,
            )
        }
        GitWorktreeCommand::SyncStatus(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let worktree_path = resolve_worktree_path(&repo_path, &args.worktree_name)?;
            let status = run_git(&worktree_path, &["status", "--porcelain", "-b"])?;
            let mut lines = status.lines();
            let branch_line = lines.next().unwrap_or_default().to_string();
            let clean = lines.clone().all(|line| line.trim().is_empty());
            let sync = GitSyncStatusCli {
                worktree_name: args.worktree_name,
                clean,
                branch: Some(branch_line.clone()),
                ahead_behind: branch_line
                    .split('[')
                    .nth(1)
                    .map(|value| value.trim_end_matches(']').to_string()),
            };
            print_value(sync, json)
        }
    }
}
