use anyhow::{Context, Result};
use orchestrator_core::{services::ServiceHub, OrchestratorTask};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;

use super::phase_git::is_git_repo;

pub async fn ensure_execution_cwd(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: Option<&OrchestratorTask>,
) -> Result<String> {
    let Some(task) = task else {
        return Ok(project_root.to_string());
    };
    if !is_git_repo(project_root) {
        return Ok(project_root.to_string());
    }

    let worktree_root = ensure_repo_worktree_root(project_root)?;
    let branch_name = task
        .branch_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_task_branch_name(&task.id));

    if let Some(existing_path_raw) = task
        .worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let existing_path = PathBuf::from(existing_path_raw);
        if existing_path.exists() {
            if !path_is_within_root(&existing_path, &worktree_root) {
                anyhow::bail!(
                    "task {} worktree path '{}' is outside managed worktree root '{}'",
                    task.id,
                    existing_path.display(),
                    worktree_root.display()
                );
            }
            if task.branch_name.as_deref() != Some(branch_name.as_str()) {
                let mut updated = task.clone();
                updated.branch_name = Some(branch_name.clone());
                let _ = hub.tasks().replace(updated).await?;
            }
            return Ok(existing_path.to_string_lossy().to_string());
        }
    }

    let worktree_path = default_task_worktree_path(project_root, &task.id)?;
    if worktree_path.exists() {
        if !path_is_within_root(&worktree_path, &worktree_root) {
            anyhow::bail!(
                "task {} worktree path '{}' is outside managed worktree root '{}'",
                task.id,
                worktree_path.display(),
                worktree_root.display()
            );
        }
        let mut updated = task.clone();
        updated.worktree_path = Some(worktree_path.to_string_lossy().to_string());
        updated.branch_name = Some(branch_name);
        let _ = hub.tasks().replace(updated).await?;
        return Ok(worktree_path.to_string_lossy().to_string());
    }

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let worktree_path_str = worktree_path.to_string_lossy().to_string();
    let branch_ref = format!("refs/heads/{branch_name}");
    let status = if git_ref_exists(project_root, &branch_ref) {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args([
                "worktree",
                "add",
                worktree_path_str.as_str(),
                branch_name.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| {
                format!(
                    "failed to create worktree '{}' for existing branch '{}' in {}",
                    worktree_path_str, branch_name, project_root
                )
            })?
    } else {
        refresh_preferred_worktree_base_refs(project_root);
        let base_ref = preferred_worktree_base_ref(project_root);
        ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args([
                "worktree",
                "add",
                "-b",
                branch_name.as_str(),
                worktree_path_str.as_str(),
                base_ref.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| {
                format!(
                    "failed to create worktree '{}' for branch '{}' from '{}' in {}",
                    worktree_path_str, branch_name, base_ref, project_root
                )
            })?
    };

    if !status.success() {
        anyhow::bail!(
            "failed to provision managed worktree '{}' for task {} on branch '{}'",
            worktree_path_str,
            task.id,
            branch_name
        );
    }

    let mut updated = task.clone();
    updated.worktree_path = Some(worktree_path_str.clone());
    updated.branch_name = Some(branch_name);
    let _ = hub.tasks().replace(updated).await?;
    Ok(worktree_path_str)
}

fn default_task_branch_name(task_id: &str) -> String {
    format!("ao/{}", protocol::sanitize_identifier(task_id, "task"))
}

fn repo_ao_root(project_root: &str) -> Result<PathBuf> {
    protocol::scoped_state_root(std::path::Path::new(project_root))
        .ok_or_else(|| anyhow::anyhow!("failed to resolve scoped state root for {project_root}"))
}

fn repo_worktrees_root(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?.join("worktrees"))
}

fn ensure_repo_worktree_root(project_root: &str) -> Result<PathBuf> {
    let repo_root = repo_ao_root(project_root)?;
    let root = repo_worktrees_root(project_root)?;
    std::fs::create_dir_all(&repo_root)?;
    std::fs::create_dir_all(&root)?;

    let canonical = Path::new(project_root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(project_root));
    let marker_path = repo_root.join(".project-root");
    let marker_content = format!("{}\n", canonical.to_string_lossy());
    let should_write_marker = std::fs::read_to_string(&marker_path)
        .map(|existing| existing != marker_content)
        .unwrap_or(true);
    if should_write_marker {
        std::fs::write(&marker_path, marker_content)?;
    }

    #[cfg(unix)]
    {
        let link_path = repo_root.join("project-root");
        if !link_path.exists() {
            let _ = std::os::unix::fs::symlink(&canonical, &link_path);
        }
    }

    Ok(root)
}

fn default_task_worktree_path(project_root: &str, task_id: &str) -> Result<PathBuf> {
    Ok(repo_worktrees_root(project_root)?.join(format!(
        "task-{}",
        protocol::sanitize_identifier(task_id, "task")
    )))
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    let Ok(path_canonical) = path.canonicalize() else {
        return false;
    };
    let Ok(root_canonical) = root.canonicalize() else {
        return false;
    };
    path_canonical.starts_with(root_canonical)
}

fn git_ref_exists(project_root: &str, reference: &str) -> bool {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--verify", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn preferred_worktree_base_ref(project_root: &str) -> String {
    for reference in [
        "refs/remotes/origin/main",
        "refs/heads/main",
        "refs/remotes/origin/master",
        "refs/heads/master",
        "HEAD",
    ] {
        if git_ref_exists(project_root, reference) {
            return reference.to_string();
        }
    }
    "HEAD".to_string()
}

fn refresh_preferred_worktree_base_refs(project_root: &str) {
    for branch in ["main", "master"] {
        let _ = ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args(["fetch", "--no-tags", "origin", branch])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
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
