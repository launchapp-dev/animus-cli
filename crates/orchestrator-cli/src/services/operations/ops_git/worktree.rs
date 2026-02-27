use super::*;
use crate::not_found_error;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::process::Output;

use super::model::GitSyncStatusCli;
use super::store::{
    ensure_confirmation, git_confirmation_next_step, load_worktrees, resolve_repo_path,
    resolve_worktree_path, run_git,
};

#[derive(Debug, Clone)]
struct TaskPruneMeta {
    id: String,
    status: String,
    worktree_path: Option<String>,
    branch_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskPruneRecord {
    id: String,
    status: String,
    #[serde(default)]
    worktree_path: Option<String>,
    #[serde(default)]
    branch_name: Option<String>,
}

#[derive(Debug, Clone)]
struct PruneCandidate {
    worktree_name: String,
    path: String,
    branch: Option<String>,
    task_id: String,
    task_status: String,
    remote_branch: Option<String>,
}

fn normalize_task_status(status: &str) -> String {
    status.trim().to_ascii_lowercase().replace('_', "-")
}

fn is_terminal_task_status(status: &str) -> bool {
    matches!(normalize_task_status(status).as_str(), "done" | "cancelled")
}

fn normalize_branch(branch: &str) -> String {
    branch.trim().trim_start_matches("refs/heads/").to_string()
}

fn normalize_path_for_match(path: &str) -> String {
    let candidate = PathBuf::from(path.trim());
    if let Ok(canonical) = candidate.canonicalize() {
        return canonical.to_string_lossy().to_string();
    }
    candidate.to_string_lossy().to_string()
}

fn task_id_from_sanitized_token(token: &str) -> Option<String> {
    let trimmed = token.trim();
    let suffix = trimmed.strip_prefix("task-")?;
    if suffix.is_empty() {
        return None;
    }
    Some(format!("TASK-{}", suffix.to_ascii_uppercase()))
}

fn infer_task_id(branch: Option<&str>, worktree_name: &str) -> Option<String> {
    if let Some(branch_name) = branch {
        let normalized = normalize_branch(branch_name);
        if let Some(rest) = normalized.strip_prefix("ao/") {
            if let Some(task_id) = task_id_from_sanitized_token(rest) {
                return Some(task_id);
            }
        }
        if let Some(task_id) = task_id_from_sanitized_token(&normalized) {
            return Some(task_id);
        }
    }

    let name = worktree_name.trim();
    if let Some(rest) = name.strip_prefix("task-") {
        return task_id_from_sanitized_token(rest);
    }
    task_id_from_sanitized_token(name)
}

fn branch_for_remote_delete(
    task: Option<&TaskPruneMeta>,
    worktree_branch: Option<&str>,
) -> Option<String> {
    if let Some(branch_name) = task
        .and_then(|record| record.branch_name.as_deref())
        .map(normalize_branch)
        .filter(|value| !value.is_empty())
    {
        return Some(branch_name);
    }

    worktree_branch
        .map(normalize_branch)
        .filter(|value| !value.is_empty())
}

fn summarize_output(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    "command returned non-zero exit code without output".to_string()
}

fn load_tasks_for_prune(project_root: &str) -> Result<Vec<TaskPruneMeta>> {
    let tasks_root = Path::new(project_root).join(".ao").join("tasks");
    if !tasks_root.exists() {
        return Ok(Vec::new());
    }

    let mut task_files = Vec::new();
    for entry in fs::read_dir(&tasks_root)
        .with_context(|| format!("failed to read task directory {}", tasks_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if file_name.starts_with("TASK-") && file_name.ends_with(".json") {
            task_files.push(path);
        }
    }
    task_files.sort();

    let mut tasks = Vec::new();
    for path in task_files {
        let payload = fs::read_to_string(&path)
            .with_context(|| format!("failed to read task file {}", path.display()))?;
        let record: TaskPruneRecord = serde_json::from_str(&payload)
            .with_context(|| format!("invalid task JSON {}", path.display()))?;
        tasks.push(TaskPruneMeta {
            id: record.id,
            status: normalize_task_status(&record.status),
            worktree_path: record.worktree_path,
            branch_name: record.branch_name.map(|branch| normalize_branch(&branch)),
        });
    }

    Ok(tasks)
}

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
                command.arg(&args.worktree_path);
            } else {
                command.arg(&args.worktree_path).arg(&args.branch);
            }
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
                .ok_or_else(|| {
                    not_found_error(format!("worktree not found: {}", args.worktree_name))
                })?;
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
        GitWorktreeCommand::Prune(args) => {
            let repo_path = resolve_repo_path(project_root, &args.repo)?;
            let repo_path_display = repo_path.display().to_string();
            let repo_path_normalized = normalize_path_for_match(&repo_path_display);
            let worktrees = load_worktrees(&repo_path)?;
            let tasks = load_tasks_for_prune(project_root)?;

            let mut tasks_by_id: HashMap<String, TaskPruneMeta> = HashMap::new();
            let mut tasks_by_branch: HashMap<String, TaskPruneMeta> = HashMap::new();
            let mut tasks_by_worktree_path: HashMap<String, TaskPruneMeta> = HashMap::new();
            for task in tasks {
                tasks_by_id.insert(task.id.to_ascii_uppercase(), task.clone());
                if let Some(branch_name) = task
                    .branch_name
                    .as_deref()
                    .map(normalize_branch)
                    .filter(|value| !value.is_empty())
                {
                    tasks_by_branch.insert(branch_name.to_ascii_lowercase(), task.clone());
                }
                if let Some(path) = task
                    .worktree_path
                    .as_deref()
                    .map(normalize_path_for_match)
                    .filter(|value| !value.is_empty())
                {
                    tasks_by_worktree_path.insert(path, task);
                }
            }

            let mut worktree_reports = Vec::new();
            let mut candidates = Vec::new();
            for entry in worktrees {
                let normalized_path = normalize_path_for_match(&entry.path);
                let branch_normalized = entry.branch.as_deref().map(normalize_branch);
                let inferred_task_id = infer_task_id(entry.branch.as_deref(), &entry.worktree_name);

                let matched_task = tasks_by_worktree_path
                    .get(&normalized_path)
                    .or_else(|| {
                        branch_normalized
                            .as_deref()
                            .and_then(|branch| tasks_by_branch.get(&branch.to_ascii_lowercase()))
                    })
                    .or_else(|| {
                        inferred_task_id
                            .as_deref()
                            .and_then(|task_id| tasks_by_id.get(&task_id.to_ascii_uppercase()))
                    });

                let matched_task = matched_task.cloned();
                let primary_repo_worktree = normalized_path == repo_path_normalized;
                let task_id = matched_task
                    .as_ref()
                    .map(|task| task.id.clone())
                    .or_else(|| inferred_task_id.clone());
                let task_status = matched_task.as_ref().map(|task| task.status.clone());
                let terminal_task = task_status
                    .as_deref()
                    .map(is_terminal_task_status)
                    .unwrap_or(false);
                let is_candidate = terminal_task && !primary_repo_worktree;
                let remote_branch =
                    branch_for_remote_delete(matched_task.as_ref(), entry.branch.as_deref());

                if is_candidate {
                    candidates.push(PruneCandidate {
                        worktree_name: entry.worktree_name.clone(),
                        path: entry.path.clone(),
                        branch: entry.branch.as_deref().map(normalize_branch),
                        task_id: task_id.clone().unwrap_or_default(),
                        task_status: task_status.clone().unwrap_or_else(|| "unknown".to_string()),
                        remote_branch: remote_branch.clone(),
                    });
                }

                let reason = if primary_repo_worktree {
                    Some("primary repository worktree".to_string())
                } else if task_id.is_none() {
                    Some("no matching task found".to_string())
                } else if !terminal_task {
                    Some("task is not done/cancelled".to_string())
                } else {
                    Some("task is done/cancelled".to_string())
                };

                worktree_reports.push(json!({
                    "worktree_name": entry.worktree_name,
                    "path": entry.path,
                    "branch": entry.branch,
                    "task_id": task_id,
                    "task_status": task_status,
                    "candidate": is_candidate,
                    "reason": reason,
                    "remote_branch": remote_branch,
                }));
            }

            let candidate_reports: Vec<serde_json::Value> = candidates
                .iter()
                .map(|candidate| {
                    let mut planned_effects = vec![
                        "remove git worktree registration".to_string(),
                        "remove worktree directory".to_string(),
                    ];
                    if args.delete_remote_branch {
                        if candidate.remote_branch.is_some() {
                            planned_effects
                                .push(format!("delete remote branch on {}", args.remote));
                        } else {
                            planned_effects.push(
                                "skip remote branch deletion (branch metadata unavailable)"
                                    .to_string(),
                            );
                        }
                    }

                    json!({
                        "worktree_name": candidate.worktree_name,
                        "path": candidate.path,
                        "branch": candidate.branch,
                        "task_id": candidate.task_id,
                        "task_status": candidate.task_status,
                        "remote_branch": candidate.remote_branch,
                        "planned_effects": planned_effects,
                    })
                })
                .collect();

            if args.dry_run {
                return print_value(
                    json!({
                        "operation": "git.worktree.prune",
                        "repo": args.repo,
                        "repo_path": repo_path_display,
                        "dry_run": true,
                        "delete_remote_branch": args.delete_remote_branch,
                        "remote": args.remote,
                        "total_worktrees": worktree_reports.len(),
                        "candidate_count": candidate_reports.len(),
                        "worktrees": worktree_reports,
                        "candidates": candidate_reports,
                        "pruned_count": 0,
                        "remote_deleted_count": 0,
                        "errors": [],
                    }),
                    json,
                );
            }

            let mut results = Vec::new();
            let mut errors = Vec::new();
            let mut pruned_count = 0usize;
            let mut remote_deleted_count = 0usize;
            for candidate in candidates {
                let remove_output = ProcessCommand::new("git")
                    .arg("-C")
                    .arg(&repo_path)
                    .args(["worktree", "remove", "--force", candidate.path.as_str()])
                    .output()
                    .with_context(|| {
                        format!("failed to remove worktree {}", candidate.worktree_name)
                    })?;

                let mut removed = remove_output.status.success();
                let mut remove_error = None;
                if !removed {
                    remove_error = Some(summarize_output(&remove_output));
                    let worktree_path = Path::new(&candidate.path);
                    if worktree_path.exists() {
                        let _ = fs::remove_dir_all(worktree_path);
                    }
                    let _ = ProcessCommand::new("git")
                        .arg("-C")
                        .arg(&repo_path)
                        .args(["worktree", "prune"])
                        .output();

                    let candidate_path_normalized = normalize_path_for_match(&candidate.path);
                    let still_present = load_worktrees(&repo_path)?.into_iter().any(|entry| {
                        normalize_path_for_match(&entry.path) == candidate_path_normalized
                    });
                    if !still_present {
                        removed = true;
                        remove_error = None;
                    }
                }

                if removed {
                    pruned_count = pruned_count.saturating_add(1);
                } else if let Some(error) = remove_error.as_ref() {
                    errors.push(json!({
                        "worktree_name": candidate.worktree_name,
                        "path": candidate.path,
                        "task_id": candidate.task_id,
                        "stage": "remove_worktree",
                        "message": error,
                    }));
                }

                let mut remote_deleted = None;
                let mut remote_error = None;
                if args.delete_remote_branch {
                    if let Some(branch_name) = candidate.remote_branch.as_deref() {
                        if !matches!(branch_name, "main" | "master") {
                            let remote_output = ProcessCommand::new("git")
                                .arg("-C")
                                .arg(&repo_path)
                                .args(["push", args.remote.as_str(), "--delete", branch_name])
                                .output()
                                .with_context(|| {
                                    format!("failed to delete remote branch {}", branch_name)
                                })?;
                            if remote_output.status.success() {
                                remote_deleted = Some(true);
                                remote_deleted_count = remote_deleted_count.saturating_add(1);
                            } else {
                                remote_deleted = Some(false);
                                remote_error = Some(summarize_output(&remote_output));
                                errors.push(json!({
                                    "worktree_name": candidate.worktree_name,
                                    "path": candidate.path,
                                    "task_id": candidate.task_id,
                                    "stage": "delete_remote_branch",
                                    "branch": branch_name,
                                    "message": remote_error.clone(),
                                }));
                            }
                        } else {
                            remote_deleted = Some(false);
                            remote_error = Some("protected branch not deleted".to_string());
                        }
                    } else {
                        remote_deleted = Some(false);
                        remote_error = Some("branch metadata unavailable".to_string());
                    }
                }

                results.push(json!({
                    "worktree_name": candidate.worktree_name,
                    "path": candidate.path,
                    "branch": candidate.branch,
                    "task_id": candidate.task_id,
                    "task_status": candidate.task_status,
                    "removed": removed,
                    "remove_error": remove_error,
                    "remote_branch": candidate.remote_branch,
                    "remote_branch_deleted": remote_deleted,
                    "remote_error": remote_error,
                }));
            }

            let prune_metadata_output = ProcessCommand::new("git")
                .arg("-C")
                .arg(&repo_path)
                .args(["worktree", "prune"])
                .output()
                .with_context(|| {
                    format!("failed to prune worktree metadata in {}", repo_path_display)
                })?;
            if !prune_metadata_output.status.success() {
                errors.push(json!({
                    "stage": "worktree_prune",
                    "message": summarize_output(&prune_metadata_output),
                }));
            }

            print_value(
                json!({
                    "operation": "git.worktree.prune",
                    "repo": args.repo,
                    "repo_path": repo_path_display,
                    "dry_run": false,
                    "delete_remote_branch": args.delete_remote_branch,
                    "remote": args.remote,
                    "total_worktrees": worktree_reports.len(),
                    "candidate_count": candidate_reports.len(),
                    "pruned_count": pruned_count,
                    "remote_deleted_count": remote_deleted_count,
                    "worktrees": worktree_reports,
                    "candidates": candidate_reports,
                    "results": results,
                    "errors": errors,
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
