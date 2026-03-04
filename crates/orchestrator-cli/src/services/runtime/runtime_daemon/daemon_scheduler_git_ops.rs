use super::*;

#[path = "daemon_git_helpers.rs"]
mod git_helpers;

#[allow(dead_code)]
#[path = "daemon_git_worktree.rs"]
mod git_worktree;

#[allow(dead_code)]
#[path = "daemon_git_merge.rs"]
mod git_merge;

#[allow(dead_code)]
#[path = "daemon_git_integration.rs"]
mod git_integration;
#[path = "daemon_git_runtime_refresh.rs"]
mod git_runtime_refresh;

pub(super) use git_helpers::*;
pub(super) use git_worktree::*;
pub(super) use git_merge::*;
pub(super) use git_integration::*;
pub(super) use git_runtime_refresh::*;

pub(crate) use git_merge::MergeConflictContext;

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::InMemoryServiceHub;
    use tempfile::TempDir;

    use protocol::test_utils::EnvVarGuard;

    fn reset_runtime_binary_refresh_hooks() {
        with_runtime_binary_refresh_test_hooks(|hooks| {
            *hooks = RuntimeBinaryRefreshTestHooks::default();
        });
    }

    fn runtime_binary_refresh_build_calls() -> usize {
        with_runtime_binary_refresh_test_hooks(|hooks| hooks.build_calls)
    }

    fn runtime_binary_refresh_runner_refresh_calls() -> usize {
        with_runtime_binary_refresh_test_hooks(|hooks| hooks.runner_refresh_calls)
    }

    fn init_git_repo(project_root: &Path) {
        let init_main = ProcessCommand::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(project_root)
            .status()
            .expect("git init should run");
        if !init_main.success() {
            let init = ProcessCommand::new("git")
                .arg("init")
                .current_dir(project_root)
                .status()
                .expect("git init should run");
            assert!(init.success(), "git init should succeed");
            let rename = ProcessCommand::new("git")
                .args(["branch", "-M", "main"])
                .current_dir(project_root)
                .status()
                .expect("git branch -M should run");
            assert!(rename.success(), "git branch -M main should succeed");
        }

        let email = ProcessCommand::new("git")
            .args(["config", "user.email", "ao-test@example.com"])
            .current_dir(project_root)
            .status()
            .expect("git config user.email should run");
        assert!(email.success(), "git config user.email should succeed");
        let name = ProcessCommand::new("git")
            .args(["config", "user.name", "AO Test"])
            .current_dir(project_root)
            .status()
            .expect("git config user.name should run");
        assert!(name.success(), "git config user.name should succeed");

        std::fs::write(project_root.join("README.md"), "# test\n")
            .expect("readme should be written");
        run_git(project_root, &["add", "README.md"], "git add readme");
        run_git(project_root, &["commit", "-m", "init"], "git commit readme");
    }

    fn run_git(cwd: &Path, args: &[&str], operation: &str) {
        let status = ProcessCommand::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git command should run");
        assert!(
            status.success(),
            "git command failed for operation '{operation}': git {}",
            args.join(" ")
        );
    }

    fn prune_config(enabled: bool) -> PostSuccessGitConfig {
        PostSuccessGitConfig {
            auto_merge_enabled: false,
            auto_pr_enabled: false,
            auto_commit_before_merge: false,
            auto_merge_target_branch: "main".to_string(),
            auto_merge_no_ff: true,
            auto_push_remote: "origin".to_string(),
            auto_cleanup_worktree_enabled: true,
            auto_prune_worktrees_after_merge: enabled,
        }
    }

    async fn create_task_with_worktree(
        hub: &Arc<FileServiceHub>,
        project_root: &str,
        status: TaskStatus,
        title: &str,
    ) -> (String, PathBuf, String) {
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: title.to_string(),
                description: format!("{title} description"),
                task_type: Some(TaskType::Feature),
                priority: None,
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, status)
            .await
            .expect("task status should be updated");

        let branch_name = format!("ao/{}", task.id.to_ascii_lowercase());
        let worktree_name = format!("task-{}", task.id.to_ascii_lowercase());
        let worktree_path = repo_worktrees_root(project_root)
            .expect("repo worktree root should resolve")
            .join(worktree_name);
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent).expect("worktree parent should be created");
        }
        let worktree_path_string = worktree_path.to_string_lossy().to_string();
        run_git(
            Path::new(project_root),
            &[
                "worktree",
                "add",
                "-b",
                branch_name.as_str(),
                worktree_path_string.as_str(),
                "main",
            ],
            "create task worktree",
        );

        let mut updated = hub
            .tasks()
            .get(&task.id)
            .await
            .expect("task should be readable");
        updated.branch_name = Some(branch_name);
        updated.worktree_path = Some(worktree_path_string.clone());
        updated.metadata.updated_by = "test".to_string();
        hub.tasks()
            .replace(updated)
            .await
            .expect("task worktree metadata should be saved");

        (task.id, worktree_path, worktree_path_string)
    }

    #[tokio::test]
    async fn auto_prune_completed_task_worktrees_after_merge_prunes_terminal_tasks() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let (done_task_id, done_worktree_path, done_worktree_path_string) =
            create_task_with_worktree(&hub, &project_root, TaskStatus::Done, "done candidate")
                .await;
        let (active_task_id, active_worktree_path, active_worktree_path_string) =
            create_task_with_worktree(
                &hub,
                &project_root,
                TaskStatus::InProgress,
                "active candidate",
            )
            .await;

        auto_prune_completed_task_worktrees_after_merge(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root,
            &prune_config(true),
        )
        .await
        .expect("auto-prune should succeed");

        assert!(
            !done_worktree_path.exists(),
            "done task worktree should be removed"
        );
        assert!(
            active_worktree_path.exists(),
            "non-terminal task worktree should remain"
        );

        let done_after = hub
            .tasks()
            .get(&done_task_id)
            .await
            .expect("done task should be readable");
        assert!(
            done_after.worktree_path.is_none(),
            "done task worktree_path metadata should be cleared"
        );

        let active_after = hub
            .tasks()
            .get(&active_task_id)
            .await
            .expect("active task should be readable");
        assert_eq!(
            active_after.worktree_path.as_deref(),
            Some(active_worktree_path_string.as_str()),
            "non-terminal task worktree metadata should be unchanged"
        );

        let listed = ProcessCommand::new("git")
            .arg("-C")
            .arg(&project_root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list should run");
        assert!(listed.status.success(), "git worktree list should succeed");
        let listed_stdout = String::from_utf8_lossy(&listed.stdout);
        assert!(
            !listed_stdout.contains(done_worktree_path_string.as_str()),
            "pruned done task worktree should be removed from git metadata"
        );
        assert!(
            listed_stdout.contains(active_worktree_path_string.as_str()),
            "active task worktree should remain in git metadata"
        );
    }

    #[tokio::test]
    async fn auto_prune_completed_task_worktrees_after_merge_skips_when_disabled() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let (done_task_id, done_worktree_path, done_worktree_path_string) =
            create_task_with_worktree(
                &hub,
                &project_root,
                TaskStatus::Cancelled,
                "cancelled candidate",
            )
            .await;

        auto_prune_completed_task_worktrees_after_merge(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root,
            &prune_config(false),
        )
        .await
        .expect("disabled auto-prune should return ok");

        assert!(
            done_worktree_path.exists(),
            "worktree should remain when auto-prune is disabled"
        );
        let done_after = hub
            .tasks()
            .get(&done_task_id)
            .await
            .expect("task should be readable");
        assert_eq!(
            done_after.worktree_path.as_deref(),
            Some(done_worktree_path_string.as_str()),
            "task worktree_path should remain unchanged when auto-prune is disabled"
        );
    }

    #[tokio::test]
    async fn auto_prune_completed_task_worktrees_after_merge_skips_paths_outside_managed_root() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "outside managed root candidate".to_string(),
                description: "outside managed root candidate".to_string(),
                task_type: Some(TaskType::Feature),
                priority: None,
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::Done)
            .await
            .expect("task status should be updated");

        let managed_root = repo_worktrees_root(&project_root).expect("managed root should resolve");
        let managed_root_name = managed_root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("worktrees")
            .to_string();
        let sibling_root = managed_root.with_file_name(format!("{managed_root_name}-shadow"));

        let branch_name = format!("ao/{}", task.id.to_ascii_lowercase());
        let worktree_name = format!("task-{}", task.id.to_ascii_lowercase());
        let worktree_path = sibling_root.join(worktree_name);
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent).expect("outside worktree parent should be created");
        }
        let worktree_path_string = worktree_path.to_string_lossy().to_string();
        run_git(
            Path::new(&project_root),
            &[
                "worktree",
                "add",
                "-b",
                branch_name.as_str(),
                worktree_path_string.as_str(),
                "main",
            ],
            "create outside managed root worktree",
        );

        let mut updated = hub
            .tasks()
            .get(&task.id)
            .await
            .expect("task should be readable");
        updated.branch_name = Some(branch_name);
        updated.worktree_path = Some(worktree_path_string.clone());
        updated.metadata.updated_by = "test".to_string();
        hub.tasks()
            .replace(updated)
            .await
            .expect("task worktree metadata should be saved");

        auto_prune_completed_task_worktrees_after_merge(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root,
            &prune_config(true),
        )
        .await
        .expect("auto-prune should succeed");

        assert!(
            worktree_path.exists(),
            "outside managed-root worktree should never be pruned"
        );

        let task_after = hub
            .tasks()
            .get(&task.id)
            .await
            .expect("task should be readable");
        assert_eq!(
            task_after.worktree_path.as_deref(),
            Some(worktree_path_string.as_str()),
            "outside managed-root task metadata should remain unchanged"
        );

        let listed = ProcessCommand::new("git")
            .arg("-C")
            .arg(&project_root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list should run");
        assert!(listed.status.success(), "git worktree list should succeed");
        let listed_stdout = String::from_utf8_lossy(&listed.stdout);
        assert!(
            listed_stdout.contains(worktree_path_string.as_str()),
            "outside managed-root worktree should remain registered"
        );
    }

    #[tokio::test]
    async fn runtime_binary_refresh_noops_when_main_head_unchanged() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        reset_runtime_binary_refresh_hooks();

        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));
        let _enabled = EnvVarGuard::set(RUNTIME_BINARY_REFRESH_ENABLED_ENV, Some("1"));

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new()) as Arc<dyn ServiceHub>;

        let first = refresh_runtime_binaries_if_main_advanced(
            hub.clone(),
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(first, RuntimeBinaryRefreshOutcome::Refreshed);

        let second = refresh_runtime_binaries_if_main_advanced(
            hub,
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(second, RuntimeBinaryRefreshOutcome::Unchanged);
        assert_eq!(runtime_binary_refresh_build_calls(), 1);
        assert_eq!(runtime_binary_refresh_runner_refresh_calls(), 1);

        let state = load_runtime_binary_refresh_state(&project_root);
        let main_head = resolve_main_head_commit(&project_root).expect("main head should resolve");
        assert_eq!(
            state.last_successful_main_head.as_deref(),
            Some(main_head.as_str())
        );
    }

    #[tokio::test]
    async fn runtime_binary_refresh_defers_when_active_agents_are_present() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        reset_runtime_binary_refresh_hooks();

        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));
        let _enabled = EnvVarGuard::set(RUNTIME_BINARY_REFRESH_ENABLED_ENV, Some("1"));

        with_runtime_binary_refresh_test_hooks(|hooks| {
            hooks.active_agents_override = Some(2);
        });

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new()) as Arc<dyn ServiceHub>;

        let outcome = refresh_runtime_binaries_if_main_advanced(
            hub,
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;

        assert_eq!(outcome, RuntimeBinaryRefreshOutcome::DeferredActiveAgents);
        assert_eq!(runtime_binary_refresh_build_calls(), 0);
        assert_eq!(runtime_binary_refresh_runner_refresh_calls(), 0);
        let state = load_runtime_binary_refresh_state(&project_root);
        assert!(
            state.last_successful_main_head.is_none(),
            "deferred refresh should not advance successful watermark"
        );
    }

    #[tokio::test]
    async fn runtime_binary_refresh_applies_tick_backoff_after_build_failure() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        reset_runtime_binary_refresh_hooks();

        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));
        let _enabled = EnvVarGuard::set(RUNTIME_BINARY_REFRESH_ENABLED_ENV, Some("1"));

        with_runtime_binary_refresh_test_hooks(|hooks| {
            hooks
                .build_results
                .push_back(Err(anyhow::anyhow!("simulated build failure")));
        });

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new()) as Arc<dyn ServiceHub>;

        let first = refresh_runtime_binaries_if_main_advanced(
            hub.clone(),
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(first, RuntimeBinaryRefreshOutcome::BuildFailed);

        let second = refresh_runtime_binaries_if_main_advanced(
            hub,
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(second, RuntimeBinaryRefreshOutcome::DeferredBackoff);
        assert_eq!(runtime_binary_refresh_build_calls(), 1);
        assert_eq!(runtime_binary_refresh_runner_refresh_calls(), 0);

        let state = load_runtime_binary_refresh_state(&project_root);
        assert!(
            state.last_error.is_some(),
            "failed build should persist an error for retry logic"
        );
        assert!(
            state.last_successful_main_head.is_none(),
            "failed build should not advance successful watermark"
        );
    }

    #[tokio::test]
    async fn runtime_binary_refresh_applies_tick_backoff_after_runner_refresh_failure() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        reset_runtime_binary_refresh_hooks();

        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));
        let _enabled = EnvVarGuard::set(RUNTIME_BINARY_REFRESH_ENABLED_ENV, Some("1"));

        with_runtime_binary_refresh_test_hooks(|hooks| {
            hooks
                .runner_refresh_results
                .push_back(Err(anyhow::anyhow!("simulated runner refresh failure")));
        });

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new()) as Arc<dyn ServiceHub>;

        let first = refresh_runtime_binaries_if_main_advanced(
            hub.clone(),
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(first, RuntimeBinaryRefreshOutcome::RunnerRefreshFailed);

        let second = refresh_runtime_binaries_if_main_advanced(
            hub,
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(second, RuntimeBinaryRefreshOutcome::DeferredBackoff);
        assert_eq!(runtime_binary_refresh_build_calls(), 1);
        assert_eq!(runtime_binary_refresh_runner_refresh_calls(), 1);

        let state = load_runtime_binary_refresh_state(&project_root);
        assert!(
            state.last_error.is_some(),
            "failed runner refresh should persist an error for retry logic"
        );
        assert!(
            state.last_successful_main_head.is_none(),
            "failed runner refresh should not advance successful watermark"
        );
    }

    #[tokio::test]
    async fn runtime_binary_refresh_post_merge_trigger_bypasses_tick_backoff() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        reset_runtime_binary_refresh_hooks();

        let home = TempDir::new().expect("temp home");
        let home_path = home.path().to_string_lossy().to_string();
        let _home = EnvVarGuard::set("HOME", Some(home_path.as_str()));
        let _enabled = EnvVarGuard::set(RUNTIME_BINARY_REFRESH_ENABLED_ENV, Some("1"));

        with_runtime_binary_refresh_test_hooks(|hooks| {
            hooks
                .build_results
                .push_back(Err(anyhow::anyhow!("simulated build failure")));
            hooks.build_results.push_back(Ok(()));
        });

        let repo = TempDir::new().expect("temp repo");
        init_git_repo(repo.path());
        let project_root = repo.path().to_string_lossy().to_string();
        let hub = Arc::new(InMemoryServiceHub::new()) as Arc<dyn ServiceHub>;

        let first = refresh_runtime_binaries_if_main_advanced(
            hub.clone(),
            &project_root,
            RuntimeBinaryRefreshTrigger::Tick,
        )
        .await;
        assert_eq!(first, RuntimeBinaryRefreshOutcome::BuildFailed);

        let second = refresh_runtime_binaries_if_main_advanced(
            hub,
            &project_root,
            RuntimeBinaryRefreshTrigger::PostMerge,
        )
        .await;
        assert_eq!(second, RuntimeBinaryRefreshOutcome::Refreshed);
        assert_eq!(runtime_binary_refresh_build_calls(), 2);
        assert_eq!(runtime_binary_refresh_runner_refresh_calls(), 1);

        let state = load_runtime_binary_refresh_state(&project_root);
        let main_head = resolve_main_head_commit(&project_root).expect("main head should resolve");
        assert_eq!(
            state.last_successful_main_head.as_deref(),
            Some(main_head.as_str())
        );
        assert!(state.last_error.is_none());
    }
}
