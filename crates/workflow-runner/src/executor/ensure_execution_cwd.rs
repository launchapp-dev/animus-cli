use anyhow::Result;
use orchestrator_core::{services::ServiceHub, OrchestratorTask};
use std::sync::Arc;

pub async fn ensure_execution_cwd(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: Option<&OrchestratorTask>,
) -> Result<String> {
    hub.project_adapter()
        .ensure_execution_cwd(project_root, task)
        .await
}

#[cfg(test)]
mod tests {
    use super::ensure_execution_cwd;
    use orchestrator_core::{
        services::ServiceHub, FileServiceHub, Priority, TaskCreateInput, TaskType,
    };
    use std::path::Path;
    use std::process::Command as ProcessCommand;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn init_git_repo(temp: &TempDir) {
        let init_main = ProcessCommand::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        if !init_main.success() {
            let init = ProcessCommand::new("git")
                .arg("init")
                .current_dir(temp.path())
                .status()
                .expect("git init should run");
            assert!(init.success(), "git init should succeed");
            let rename = ProcessCommand::new("git")
                .args(["branch", "-M", "main"])
                .current_dir(temp.path())
                .status()
                .expect("git branch -M should run");
            assert!(rename.success(), "git branch -M main should succeed");
        }

        let email = ProcessCommand::new("git")
            .args(["config", "user.email", "ao-test@example.com"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.email should run");
        assert!(email.success(), "git config user.email should succeed");
        let name = ProcessCommand::new("git")
            .args(["config", "user.name", "AO Test"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.name should run");
        assert!(name.success(), "git config user.name should succeed");

        std::fs::write(temp.path().join("README.md"), "# test\n")
            .expect("readme should be written");
        let add = ProcessCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(temp.path())
            .status()
            .expect("git add should run");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "initial commit should succeed");
    }

    #[tokio::test]
    async fn ensure_execution_cwd_provisions_worktree_and_updates_task_metadata() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "worktree task".to_string(),
                description: "needs isolated execution cwd".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let execution_cwd = ensure_execution_cwd(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root,
            Some(&task),
        )
        .await
        .expect("execution cwd should be provisioned");

        assert!(execution_cwd.contains("/.ao/"));
        assert!(execution_cwd.contains("/worktrees/"));
        assert!(Path::new(&execution_cwd).exists());

        let updated = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(
            updated.worktree_path.as_deref(),
            Some(execution_cwd.as_str())
        );
        assert!(updated
            .branch_name
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn ensure_execution_cwd_fails_closed_when_worktree_creation_fails() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let mut task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "invalid branch".to_string(),
                description: "should fail closed".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        task.branch_name = Some("invalid branch name".to_string());
        hub.tasks()
            .replace(task.clone())
            .await
            .expect("task should update");

        let error = ensure_execution_cwd(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root,
            Some(&task),
        )
        .await
        .expect_err("invalid branch should fail worktree provisioning");

        assert!(
            error
                .to_string()
                .contains("failed to provision managed worktree"),
            "unexpected error: {error}"
        );
        let updated = hub.tasks().get(&task.id).await.expect("task should load");
        assert!(updated.worktree_path.is_none());
        assert_eq!(updated.branch_name.as_deref(), Some("invalid branch name"));
    }
}
