//! DEPRECATED: Will be replaced by GitProvider trait. See providers/git.rs
use super::*;

pub(crate) use ::workflow_runner::executor::MergeConflictContext;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PostMergeOutcome {
    Skipped,
    Completed,
    Conflict { context: MergeConflictContext },
}

fn merge_queue_branch_name(task_id: &str) -> String {
    format!(
        "ao/merge-queue/{}",
        protocol::sanitize_identifier(task_id, "task")
    )
}

fn pull_request_title(task: &orchestrator_core::OrchestratorTask) -> String {
    let title = task.title.trim();
    if title.is_empty() {
        format!("[{}] Automated update", task.id)
    } else {
        format!("[{}] {}", task.id, title)
    }
}

fn pull_request_body(task: &orchestrator_core::OrchestratorTask) -> String {
    let description = task.description.trim();
    if description.is_empty() {
        format!("Automated update for task {}.", task.id)
    } else {
        format!("Automated update for task {}.\n\n{}", task.id, description)
    }
}

pub fn push_branch(cwd: &str, remote: &str, branch: &str) -> Result<()> {
    run_external_command(cwd, "git", &["push", remote, branch], "push source branch")
}

pub fn push_ref(cwd: &str, remote: &str, source_ref: &str, target_ref: &str) -> Result<()> {
    let refspec = format!("{source_ref}:{target_ref}");
    run_external_command(
        cwd,
        "git",
        &["push", remote, refspec.as_str()],
        "push target ref",
    )
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

pub fn enable_pull_request_auto_merge(cwd: &str, head_branch: &str) -> Result<()> {
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

    let output = ProcessCommand::new("gh")
        .current_dir(cwd)
        .args([
            "pr",
            "merge",
            "--auto",
            "--squash",
            "--delete-branch",
            head_branch,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run gh pr merge --auto in {}", cwd))?;
    if output.status.success() {
        return Ok(());
    }

    let summary = summarize_command_output(&output.stdout, &output.stderr);
    let summary_lower = summary.to_ascii_lowercase();
    if summary_lower.contains("already enabled")
        || summary_lower.contains("is already merged")
        || summary_lower.contains("pull request is already merged")
    {
        return Ok(());
    }
    anyhow::bail!("gh pr merge --auto failed: {summary}")
}

fn conflicted_files_in_worktree(cwd: &str) -> Result<Vec<String>> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff", "--name-only", "--diff-filter=U"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to inspect conflicted files in {}", cwd))?;
    if !output.status.success() {
        anyhow::bail!(
            "failed to inspect conflicted files in {}: {}",
            cwd,
            summarize_command_output(&output.stdout, &output.stderr)
        );
    }

    let mut files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

fn merge_head_exists(cwd: &str) -> Result<bool> {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "-q", "--verify", "MERGE_HEAD"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to inspect MERGE_HEAD in {}", cwd))?;
    Ok(status.success())
}

fn head_parent_count(cwd: &str) -> Result<usize> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", "--parents", "-n", "1", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to inspect HEAD commit parents in {}", cwd))?;
    if !output.status.success() {
        anyhow::bail!(
            "failed to inspect HEAD commit parents in {}: {}",
            cwd,
            summarize_command_output(&output.stdout, &output.stderr)
        );
    }

    let parents = String::from_utf8_lossy(&output.stdout);
    let token_count = parents.split_whitespace().count();
    if token_count == 0 {
        anyhow::bail!("failed to parse HEAD commit parents in {}", cwd);
    }
    Ok(token_count.saturating_sub(1))
}

pub fn persist_merge_result_and_push(
    project_root: &str,
    context: &MergeConflictContext,
) -> Result<()> {
    git_status(
        context.merge_worktree_path.as_str(),
        &["branch", "-f", context.merge_queue_branch.as_str(), "HEAD"],
        "persist merge commit ref",
    )?;

    if push_ref(
        context.merge_worktree_path.as_str(),
        context.push_remote.as_str(),
        "HEAD",
        context.target_branch.as_str(),
    )
    .is_err()
    {
        enqueue_git_integration_operation(
            project_root,
            GitIntegrationOperation::PushRef {
                cwd: project_root.to_string(),
                remote: context.push_remote.clone(),
                source_ref: context.merge_queue_branch.clone(),
                target_ref: context.target_branch.clone(),
            },
        )?;
    } else {
        let _ = ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args(["branch", "-D", context.merge_queue_branch.as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

pub async fn post_success_merge_push_and_cleanup(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    cfg: &PostSuccessGitConfig,
) -> Result<PostMergeOutcome> {
    if !is_git_repo(project_root) {
        return Ok(PostMergeOutcome::Skipped);
    }

    let do_pr_flow = cfg.auto_pr_enabled;
    let do_direct_merge = cfg.auto_merge_enabled && !cfg.auto_pr_enabled;
    if !do_pr_flow && !do_direct_merge {
        return Ok(PostMergeOutcome::Skipped);
    }

    let Some(source_branch) = resolve_task_source_branch(task) else {
        return Ok(PostMergeOutcome::Skipped);
    };
    let mut merged_successfully = false;

    let source_push_cwd = task
        .worktree_path
        .as_deref()
        .filter(|path| Path::new(path).exists())
        .unwrap_or(project_root);
    if cfg.auto_commit_before_merge {
        auto_commit_pending_source_changes(source_push_cwd, &task.id)?;
    }
    if do_pr_flow {
        let pushed_source_branch = match push_branch(
            source_push_cwd,
            cfg.auto_push_remote.as_str(),
            source_branch.as_str(),
        ) {
            Ok(()) => true,
            Err(_) => {
                enqueue_git_integration_operation(
                    project_root,
                    GitIntegrationOperation::PushBranch {
                        cwd: project_root.to_string(),
                        remote: cfg.auto_push_remote.clone(),
                        branch: source_branch.clone(),
                    },
                )?;
                false
            }
        };

        let pr_title = pull_request_title(task);
        let pr_body = pull_request_body(task);
        let open_pr_operation = GitIntegrationOperation::OpenPullRequest {
            cwd: project_root.to_string(),
            base_branch: cfg.auto_merge_target_branch.clone(),
            head_branch: source_branch.clone(),
            title: pr_title.clone(),
            body: pr_body.clone(),
            draft: false,
        };
        let enable_pr_auto_merge_operation = GitIntegrationOperation::EnablePullRequestAutoMerge {
            cwd: project_root.to_string(),
            head_branch: source_branch.clone(),
        };

        let opened_pr_now = if pushed_source_branch {
            if create_pull_request(
                project_root,
                cfg.auto_merge_target_branch.as_str(),
                source_branch.as_str(),
                pr_title.as_str(),
                pr_body.as_str(),
                false,
            )
            .is_ok()
            {
                true
            } else {
                enqueue_git_integration_operation(project_root, open_pr_operation.clone())?;
                false
            }
        } else {
            enqueue_git_integration_operation(project_root, open_pr_operation)?;
            false
        };

        if cfg.auto_merge_enabled {
            if opened_pr_now {
                if enable_pull_request_auto_merge(project_root, source_branch.as_str()).is_err() {
                    enqueue_git_integration_operation(
                        project_root,
                        enable_pr_auto_merge_operation,
                    )?;
                }
            } else {
                enqueue_git_integration_operation(project_root, enable_pr_auto_merge_operation)?;
            }
        }
    }

    if do_direct_merge {
        if push_branch(
            source_push_cwd,
            cfg.auto_push_remote.as_str(),
            source_branch.as_str(),
        )
        .is_err()
        {
            enqueue_git_integration_operation(
                project_root,
                GitIntegrationOperation::PushBranch {
                    cwd: project_root.to_string(),
                    remote: cfg.auto_push_remote.clone(),
                    branch: source_branch.clone(),
                },
            )?;
        }

        let merge_worktree_root = ensure_repo_worktree_root(project_root)?;
        let merge_worktree_path = merge_worktree_root.join(format!(
            "__merge-{}",
            protocol::sanitize_identifier(cfg.auto_merge_target_branch.as_str(), "branch")
        ));
        let merge_worktree_path_str = merge_worktree_path.to_string_lossy().to_string();
        let merge_context = MergeConflictContext {
            source_branch: source_branch.clone(),
            target_branch: cfg.auto_merge_target_branch.clone(),
            merge_worktree_path: merge_worktree_path_str.clone(),
            conflicted_files: Vec::new(),
            merge_queue_branch: merge_queue_branch_name(&task.id),
            push_remote: cfg.auto_push_remote.clone(),
        };

        if merge_worktree_path.exists() {
            remove_worktree_path(project_root, merge_worktree_path_str.as_str());
        }
        if let Some(parent) = merge_worktree_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let merge_result = (|| -> Result<PostMergeOutcome> {
            git_status(
                project_root,
                &[
                    "fetch",
                    cfg.auto_push_remote.as_str(),
                    cfg.auto_merge_target_branch.as_str(),
                ],
                "fetch target branch",
            )?;

            let target_ref = format!("refs/heads/{}", cfg.auto_merge_target_branch);
            let remote_ref = format!(
                "refs/remotes/{}/{}",
                cfg.auto_push_remote, cfg.auto_merge_target_branch
            );
            if !git_ref_exists(project_root, target_ref.as_str())
                && git_ref_exists(project_root, remote_ref.as_str())
            {
                git_status(
                    project_root,
                    &[
                        "branch",
                        cfg.auto_merge_target_branch.as_str(),
                        remote_ref.as_str(),
                    ],
                    "materialize local target branch",
                )?;
            }

            let target_checked_out_elsewhere = is_branch_checked_out_in_any_worktree(
                project_root,
                cfg.auto_merge_target_branch.as_str(),
            )?;
            if target_checked_out_elsewhere {
                let detached_base_ref = if git_ref_exists(project_root, remote_ref.as_str()) {
                    remote_ref.as_str().to_string()
                } else {
                    target_ref.as_str().to_string()
                };
                git_status(
                    project_root,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        merge_worktree_path_str.as_str(),
                        detached_base_ref.as_str(),
                    ],
                    "create merge worktree (detached)",
                )?;
            } else {
                git_status(
                    project_root,
                    &[
                        "worktree",
                        "add",
                        merge_worktree_path_str.as_str(),
                        cfg.auto_merge_target_branch.as_str(),
                    ],
                    "create merge worktree",
                )?;
                git_status(
                    merge_worktree_path_str.as_str(),
                    &[
                        "pull",
                        "--ff-only",
                        cfg.auto_push_remote.as_str(),
                        cfg.auto_merge_target_branch.as_str(),
                    ],
                    "sync target branch",
                )?;
            }

            let merge_message = format!(
                "Merge '{}' into '{}'",
                source_branch, cfg.auto_merge_target_branch
            );
            let mut merge_command = ProcessCommand::new("git");
            merge_command
                .arg("-C")
                .arg(merge_worktree_path_str.as_str())
                .arg("merge");
            if cfg.auto_merge_no_ff {
                merge_command.arg("--no-ff");
            }
            let merge_status = merge_command
                .arg(source_branch.as_str())
                .arg("-m")
                .arg(merge_message.as_str())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to merge source branch into target branch")?;
            if !merge_status.success() {
                let conflicted_files =
                    conflicted_files_in_worktree(merge_worktree_path_str.as_str())
                        .unwrap_or_default();
                if !conflicted_files.is_empty() {
                    let mut context = merge_context.clone();
                    context.conflicted_files = conflicted_files;
                    return Ok(PostMergeOutcome::Conflict { context });
                }

                anyhow::bail!(
                    "failed to merge '{}' into '{}'",
                    source_branch,
                    cfg.auto_merge_target_branch
                );
            }

            persist_merge_result_and_push(project_root, &merge_context)?;
            Ok(PostMergeOutcome::Completed)
        })();

        match merge_result {
            Ok(PostMergeOutcome::Completed) => {
                remove_worktree_path(project_root, merge_worktree_path_str.as_str());
                merged_successfully = true;
            }
            Ok(PostMergeOutcome::Conflict { context }) => {
                return Ok(PostMergeOutcome::Conflict { context });
            }
            Ok(PostMergeOutcome::Skipped) => {}
            Err(error) => {
                remove_worktree_path(project_root, merge_worktree_path_str.as_str());
                return Err(error);
            }
        }
    }

    cleanup_task_worktree_if_enabled(hub.clone(), project_root, task, &cfg).await?;
    if merged_successfully {
        let _ =
            auto_prune_completed_task_worktrees_after_merge(hub.clone(), project_root, &cfg).await;
        let _ = refresh_runtime_binaries_if_main_advanced(
            hub,
            project_root,
            RuntimeBinaryRefreshTrigger::PostMerge,
        )
        .await;
    }
    Ok(PostMergeOutcome::Completed)
}

pub async fn finalize_merge_conflict_resolution(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    cfg: &PostSuccessGitConfig,
    context: &MergeConflictContext,
) -> Result<()> {
    if !Path::new(context.merge_worktree_path.as_str()).exists() {
        anyhow::bail!(
            "merge conflict worktree no longer exists: {}",
            context.merge_worktree_path
        );
    }

    let unresolved = conflicted_files_in_worktree(context.merge_worktree_path.as_str())?;
    if !unresolved.is_empty() {
        anyhow::bail!(
            "merge conflict still unresolved in files: {}",
            unresolved.join(", ")
        );
    }
    if merge_head_exists(context.merge_worktree_path.as_str())? {
        anyhow::bail!("merge conflict recovery did not complete merge commit");
    }
    match git_is_ancestor(
        context.merge_worktree_path.as_str(),
        context.source_branch.as_str(),
        "HEAD",
    )? {
        Some(true) => {}
        Some(false) => anyhow::bail!(
            "merge conflict recovery did not integrate source branch '{}'",
            context.source_branch
        ),
        None => anyhow::bail!(
            "unable to verify merged source branch '{}' in {}",
            context.source_branch,
            context.merge_worktree_path
        ),
    }
    if head_parent_count(context.merge_worktree_path.as_str())? < 2 {
        anyhow::bail!("merge conflict recovery did not produce a merge commit");
    }

    persist_merge_result_and_push(project_root, context)?;
    remove_worktree_path(project_root, context.merge_worktree_path.as_str());

    cleanup_task_worktree_if_enabled(hub.clone(), project_root, task, &cfg).await?;
    let _ = auto_prune_completed_task_worktrees_after_merge(hub.clone(), project_root, &cfg).await;
    let _ = refresh_runtime_binaries_if_main_advanced(
        hub,
        project_root,
        RuntimeBinaryRefreshTrigger::PostMerge,
    )
    .await;
    Ok(())
}

pub fn cleanup_merge_conflict_worktree(project_root: &str, context: &MergeConflictContext) {
    remove_worktree_path(project_root, context.merge_worktree_path.as_str());
}
