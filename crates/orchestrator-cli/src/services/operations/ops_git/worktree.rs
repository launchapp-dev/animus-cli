use super::*;
use crate::{dry_run_envelope, not_found_error};
use anyhow::{anyhow, Context, Result};
use orchestrator_core::{FileServiceHub, ServiceHub, TaskStatus};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::process::Output;

use super::model::GitSyncStatusCli;
use super::store::{
    ensure_confirmation, git_confirmation_next_step, load_worktrees, resolve_repo_path,
    resolve_worktree_path, run_git,
};

#[derive(Debug, Clone)]
struct TaskPruneMeta {
    id: String,
    status: TaskStatus,
    worktree_path: Option<String>,
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

const PRUNE_WORKTREES_CONFIRMATION_OPERATION: &str = "prune_worktrees";

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
    if let Some(branch_name) = worktree_branch
        .map(normalize_branch)
        .filter(|value| !value.is_empty())
    {
        return Some(branch_name);
    }

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

fn managed_worktrees_root(project_root: &str) -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow!("failed to resolve home directory for managed worktree pruning"))?;
    let project_path = Path::new(project_root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(project_root));
    Ok(home
        .join(".ao")
        .join(protocol::repository_scope_for_path(&project_path))
        .join("worktrees"))
}

fn path_is_within_root(path: &str, root: &Path) -> bool {
    let path_normalized = PathBuf::from(normalize_path_for_match(path));
    let root_normalized = PathBuf::from(normalize_path_for_match(&root.to_string_lossy()));
    path_normalized == root_normalized || path_normalized.starts_with(&root_normalized)
}

fn is_remote_branch_protected(branch_name: &str) -> bool {
    let normalized = normalize_branch(branch_name).to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "main" | "master" | "develop" | "dev" | "trunk" | "stable"
    ) || normalized.starts_with("release/")
        || normalized.starts_with("hotfix/")
}

fn is_task_branch(branch_name: &str) -> bool {
    let normalized = normalize_branch(branch_name).to_ascii_lowercase();
    normalized.starts_with("ao/task-") || normalized.starts_with("task-")
}

async fn clear_task_worktree_path_if_matches(
    task_service: &std::sync::Arc<dyn orchestrator_core::TaskServiceApi>,
    task_id: &str,
    removed_path: &str,
) -> Result<bool> {
    let task = task_service
        .get(task_id)
        .await
        .with_context(|| format!("failed to load task {} for prune metadata cleanup", task_id))?;

    let Some(current_path) = task.worktree_path.as_deref() else {
        return Ok(false);
    };
    if normalize_path_for_match(current_path) != normalize_path_for_match(removed_path) {
        return Ok(false);
    }

    let mut updated = task;
    updated.worktree_path = None;
    updated.metadata.updated_by = protocol::ACTOR_CLI.to_string();
    task_service
        .replace(updated)
        .await
        .with_context(|| format!("failed to clear task worktree path for {}", task_id))?;
    Ok(true)
}

pub(super) async fn handle_git_worktree(
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
                    dry_run_envelope(
                        "git.worktree.remove",
                        serde_json::json!({ "repo": repo, "worktree": worktree_name }),
                        "git.worktree.remove",
                        vec!["remove git worktree from repository".to_string()],
                        &git_confirmation_next_step("remove_worktree", &repo),
                    ),
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
            let managed_root = managed_worktrees_root(project_root)?;
            let managed_root_display = managed_root.display().to_string();

            let mut worktrees = load_worktrees(&repo_path)?;
            worktrees.sort_by(|left, right| {
                normalize_path_for_match(&left.path).cmp(&normalize_path_for_match(&right.path))
            });

            let task_service = FileServiceHub::new(project_root)
                .with_context(|| format!("failed to initialize services for {}", project_root))?
                .tasks();
            let tasks = task_service
                .list()
                .await
                .context("failed to load task records for worktree pruning")?;

            let mut tasks_by_id: HashMap<String, TaskPruneMeta> = HashMap::new();
            let mut tasks_by_branch: HashMap<String, TaskPruneMeta> = HashMap::new();
            let mut tasks_by_worktree_path: HashMap<String, TaskPruneMeta> = HashMap::new();
            for task in tasks {
                let task_meta = TaskPruneMeta {
                    id: task.id.clone(),
                    status: task.status,
                    worktree_path: task.worktree_path.clone(),
                    branch_name: task.branch_name.clone(),
                };
                tasks_by_id.insert(task.id.to_ascii_uppercase(), task_meta.clone());
                if let Some(branch_name) = task_meta
                    .branch_name
                    .as_deref()
                    .map(normalize_branch)
                    .filter(|value| !value.is_empty())
                {
                    tasks_by_branch.insert(branch_name.to_ascii_lowercase(), task_meta.clone());
                }
                if let Some(path) = task_meta
                    .worktree_path
                    .as_deref()
                    .map(normalize_path_for_match)
                    .filter(|value| !value.is_empty())
                {
                    tasks_by_worktree_path.insert(path, task_meta);
                }
            }

            let mut worktree_reports = Vec::new();
            let mut candidates = Vec::new();
            for entry in worktrees {
                let normalized_path = normalize_path_for_match(&entry.path);
                let branch_normalized = entry.branch.as_deref().map(normalize_branch);
                let inferred_task_id = infer_task_id(entry.branch.as_deref(), &entry.worktree_name);

                let matched_by_path = tasks_by_worktree_path.get(&normalized_path);
                let matched_by_branch = branch_normalized
                    .as_deref()
                    .and_then(|branch| tasks_by_branch.get(&branch.to_ascii_lowercase()));
                let matched_by_inferred_task_id = inferred_task_id
                    .as_deref()
                    .and_then(|task_id| tasks_by_id.get(&task_id.to_ascii_uppercase()));

                let mut matched_task_ids = BTreeSet::new();
                for task in [
                    matched_by_path,
                    matched_by_branch,
                    matched_by_inferred_task_id,
                ].into_iter().flatten() {
                    matched_task_ids.insert(task.id.clone());
                }

                let ambiguous_task_match = matched_task_ids.len() > 1;
                let matched_task = if ambiguous_task_match {
                    None
                } else {
                    matched_by_path
                        .or(matched_by_branch)
                        .or(matched_by_inferred_task_id)
                        .cloned()
                };
                let primary_repo_worktree = normalized_path == repo_path_normalized;
                let outside_managed_root = !path_is_within_root(&entry.path, &managed_root);
                let task_id = matched_task
                    .as_ref()
                    .map(|task| task.id.clone())
                    .or_else(|| inferred_task_id.clone());
                let task_status = matched_task.as_ref().map(|task| task.status.to_string());
                let terminal_task = matched_task
                    .as_ref()
                    .map(|task| task.status.is_terminal())
                    .unwrap_or(false);
                let is_candidate = terminal_task
                    && !primary_repo_worktree
                    && !outside_managed_root
                    && !ambiguous_task_match;
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
                } else if outside_managed_root {
                    Some("outside managed worktree root".to_string())
                } else if ambiguous_task_match {
                    Some("ambiguous task match".to_string())
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
                let planned_effects = if args.delete_remote_branch {
                    vec![
                        "remove git worktree registrations for terminal task worktrees".to_string(),
                        "remove task worktree directories under managed root".to_string(),
                        format!(
                            "delete eligible task branches from remote '{}'",
                            args.remote
                        ),
                    ]
                } else {
                    vec![
                        "remove git worktree registrations for terminal task worktrees".to_string(),
                        "remove task worktree directories under managed root".to_string(),
                    ]
                };

                let mut envelope = dry_run_envelope(
                    "git.worktree.prune",
                    serde_json::json!({ "repo": args.repo }),
                    "git.worktree.prune",
                    planned_effects,
                    &git_confirmation_next_step(PRUNE_WORKTREES_CONFIRMATION_OPERATION, &args.repo),
                );
                if let Some(obj) = envelope.as_object_mut() {
                    obj.insert(
                        "candidate_count".to_string(),
                        json!(candidate_reports.len()),
                    );
                    obj.insert("candidates".to_string(), json!(candidate_reports));
                }
                return print_value(envelope, json);
            }

            if !candidates.is_empty() {
                ensure_confirmation(
                    project_root,
                    args.confirmation_id.as_deref(),
                    PRUNE_WORKTREES_CONFIRMATION_OPERATION,
                    &args.repo,
                )?;
            }

            let mut results = Vec::new();
            let mut errors = Vec::new();
            let mut pruned_count = 0usize;
            let mut remote_deleted_count = 0usize;
            let mut updated_task_ids = HashSet::new();
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
                let candidate_path_normalized = normalize_path_for_match(&candidate.path);
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

                if removed {
                    let task_id_key = candidate.task_id.to_ascii_uppercase();
                    if !updated_task_ids.contains(&task_id_key) {
                        if let Some(task) = tasks_by_id.get(&task_id_key) {
                            let matches_task_worktree = task
                                .worktree_path
                                .as_deref()
                                .map(normalize_path_for_match)
                                .map(|path| path == candidate_path_normalized)
                                .unwrap_or(false);
                            if matches_task_worktree {
                                match clear_task_worktree_path_if_matches(
                                    &task_service,
                                    &task.id,
                                    &candidate.path,
                                )
                                .await
                                {
                                    Ok(_) => {
                                        updated_task_ids.insert(task_id_key);
                                    }
                                    Err(error) => errors.push(json!({
                                        "worktree_name": candidate.worktree_name,
                                        "path": candidate.path,
                                        "task_id": candidate.task_id,
                                        "stage": "update_task_worktree",
                                        "message": error.to_string(),
                                    })),
                                }
                            }
                        }
                    }
                }

                let mut remote_deleted = None;
                let mut remote_error = None;
                if args.delete_remote_branch {
                    if !removed {
                        remote_deleted = Some(false);
                        remote_error = Some(
                            "local worktree removal failed; skipped remote branch deletion"
                                .to_string(),
                        );
                    } else if let Some(branch_name) = candidate.remote_branch.as_deref() {
                        if is_remote_branch_protected(branch_name) {
                            remote_deleted = Some(false);
                            remote_error = Some("protected branch not deleted".to_string());
                        } else if !is_task_branch(branch_name) {
                            remote_deleted = Some(false);
                            remote_error = Some("branch is not a task branch".to_string());
                        } else {
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
                    "managed_worktrees_root": managed_root_display,
                    "dry_run": false,
                    "delete_remote_branch": args.delete_remote_branch,
                    "remote": args.remote,
                    "total_worktrees": worktree_reports.len(),
                    "candidate_count": candidate_reports.len(),
                    "skipped_count": worktree_reports.len().saturating_sub(candidate_reports.len()),
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
                    dry_run_envelope(
                        "git.worktree.push",
                        serde_json::json!({ "repo": repo, "worktree": worktree_name, "remote": remote, "branch": branch_name }),
                        "git.worktree.push",
                        vec!["push worktree branch updates to remote".to_string()],
                        &next_step,
                    ),
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
