use super::*;
use anyhow::{anyhow, Result};

use super::model::GitSyncStatusCli;
use super::store::{
    ensure_confirmation, git_confirmation_next_step, load_worktrees, resolve_repo_path,
    resolve_worktree_path, run_git,
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
            let worktree_path = resolve_worktree_path(&repo_path, &args.worktree_name)?;
            let mut cmd = vec!["worktree", "remove", args.worktree_name.as_str()];
            if args.force {
                cmd.push("--force");
            }
            if args.dry_run {
                let repo = args.repo.clone();
                let worktree_name = args.worktree_name.clone();
                return print_value(
                    serde_json::json!({
                        "operation": "git.worktree.remove",
                        "target": {
                            "repo": repo.clone(),
                            "worktree_name": worktree_name.clone(),
                        },
                        "action": "git.worktree.remove",
                        "dry_run": true,
                        "destructive": true,
                        "planned_effects": [
                            "remove git worktree from repository",
                        ],
                        "next_step": git_confirmation_next_step("remove_worktree", &repo),
                        "repo": repo,
                        "repo_path": repo_path.display().to_string(),
                        "worktree_name": worktree_name,
                        "worktree_path": worktree_path.display().to_string(),
                        "force": args.force,
                        "requires_confirmation": true,
                        "command": cmd,
                    }),
                    json,
                );
            }
            ensure_confirmation(
                project_root,
                args.confirmation_id.as_deref(),
                "remove_worktree",
                &args.repo,
            )?;
            let output = run_git(&repo_path, &cmd)?;
            print_value(
                serde_json::json!({
                    "repo": args.repo,
                    "worktree_name": args.worktree_name,
                    "worktree_path": worktree_path.display().to_string(),
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
            let branch = run_git(&worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
            let mut cmd = vec!["push", args.remote.as_str(), branch.trim()];
            if args.force {
                cmd.push("--force");
            }
            if args.dry_run {
                let repo = args.repo.clone();
                let worktree_name = args.worktree_name.clone();
                let remote = args.remote.clone();
                let branch_name = branch.trim().to_string();
                let next_step = if args.force {
                    git_confirmation_next_step("force_push", &repo)
                } else {
                    "rerun without --dry-run to execute git worktree push".to_string()
                };
                return print_value(
                    serde_json::json!({
                        "operation": "git.worktree.push",
                        "target": {
                            "repo": repo.clone(),
                            "worktree_name": worktree_name.clone(),
                            "remote": remote.clone(),
                            "branch": branch_name.clone(),
                        },
                        "action": "git.worktree.push",
                        "dry_run": true,
                        "destructive": args.force,
                        "planned_effects": [
                            "push worktree branch updates to remote",
                        ],
                        "next_step": next_step,
                        "repo": repo,
                        "worktree_name": worktree_name,
                        "worktree_path": worktree_path.display().to_string(),
                        "remote": remote,
                        "branch": branch_name,
                        "force": args.force,
                        "requires_confirmation": args.force,
                        "command": cmd,
                    }),
                    json,
                );
            }
            if args.force {
                ensure_confirmation(
                    project_root,
                    args.confirmation_id.as_deref(),
                    "force_push",
                    &args.repo,
                )?;
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
