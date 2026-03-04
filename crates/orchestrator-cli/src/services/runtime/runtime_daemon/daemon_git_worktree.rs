//! DEPRECATED: Will be replaced by GitProvider trait. See providers/git.rs
use super::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct GitWorktreeEntry {
    pub worktree_name: String,
    pub path: String,
    pub branch: Option<String>,
}

pub fn parse_git_worktree_list_porcelain(output: &str) -> Vec<GitWorktreeEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_branch: Option<String> = None;

    for line in output.lines() {
        if line.trim().is_empty() {
            if let Some(path) = current_path.take() {
                let worktree_name = PathBuf::from(&path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("worktree")
                    .to_string();
                entries.push(GitWorktreeEntry {
                    worktree_name,
                    path,
                    branch: current_branch.take(),
                });
            }
            current_branch = None;
            continue;
        }

        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(existing_path) = current_path.take() {
                let worktree_name = PathBuf::from(&existing_path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("worktree")
                    .to_string();
                entries.push(GitWorktreeEntry {
                    worktree_name,
                    path: existing_path,
                    branch: current_branch.take(),
                });
            }
            current_path = Some(path.trim().to_string());
            current_branch = None;
            continue;
        }

        if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = Some(branch.trim().trim_start_matches("refs/heads/").to_string());
        }
    }

    if let Some(path) = current_path.take() {
        let worktree_name = PathBuf::from(&path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("worktree")
            .to_string();
        entries.push(GitWorktreeEntry {
            worktree_name,
            path,
            branch: current_branch,
        });
    }

    entries
}

fn normalize_branch_for_match(branch: &str) -> String {
    branch.trim().trim_start_matches("refs/heads/").to_string()
}

fn normalize_path_for_match(path: &str) -> String {
    let candidate = PathBuf::from(path.trim());
    if let Ok(canonical) = candidate.canonicalize() {
        return canonical.to_string_lossy().to_string();
    }
    candidate.to_string_lossy().to_string()
}

pub fn infer_task_id_from_worktree(
    branch: Option<&str>,
    worktree_name: &str,
) -> Option<String> {
    let token_to_task_id = |token: &str| -> Option<String> {
        let suffix = token.trim().strip_prefix("task-")?;
        if suffix.is_empty() {
            return None;
        }
        Some(format!("TASK-{}", suffix.to_ascii_uppercase()))
    };

    if let Some(branch_name) = branch {
        let normalized = normalize_branch_for_match(branch_name);
        if let Some(rest) = normalized.strip_prefix("ao/") {
            if let Some(task_id) = token_to_task_id(rest) {
                return Some(task_id);
            }
        }
        if let Some(task_id) = token_to_task_id(&normalized) {
            return Some(task_id);
        }
    }

    let name = worktree_name.trim();
    if let Some(rest) = name.strip_prefix("task-") {
        return token_to_task_id(rest);
    }
    token_to_task_id(name)
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

pub async fn ensure_task_execution_cwd(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Result<String> {
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
        return Ok(project_root.to_string());
    }

    let mut updated = task.clone();
    updated.worktree_path = Some(worktree_path_str.clone());
    updated.branch_name = Some(branch_name);
    let _ = hub.tasks().replace(updated).await?;
    Ok(worktree_path_str)
}

pub fn rebase_worktree_on_main(project_root: &str, worktree_cwd: &str) {
    if worktree_cwd == project_root {
        return;
    }

    let merge_base = "origin/main";
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(worktree_cwd)
        .args(["rebase", merge_base])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {}
        _ => {
            let _ = ProcessCommand::new("git")
                .arg("-C")
                .arg(worktree_cwd)
                .args(["rebase", "--abort"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

pub async fn auto_prune_completed_task_worktrees_after_merge(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    cfg: &PostSuccessGitConfig,
) -> Result<()> {
    if !cfg.auto_prune_worktrees_after_merge {
        return Ok(());
    }

    let managed_root = match repo_worktrees_root(project_root) {
        Ok(path) => path,
        Err(_) => return Ok(()),
    };
    if !managed_root.exists() {
        return Ok(());
    }

    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "list", "--porcelain"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to inspect git worktrees in {}", project_root))?;
    if !output.status.success() {
        return Ok(());
    }

    let worktrees = parse_git_worktree_list_porcelain(&String::from_utf8_lossy(&output.stdout));
    if worktrees.is_empty() {
        return Ok(());
    }

    let project_root_normalized = normalize_path_for_match(project_root);
    let tasks = hub.tasks().list().await?;

    let mut task_by_id: HashMap<String, orchestrator_core::OrchestratorTask> = HashMap::new();
    let mut task_id_by_path: HashMap<String, String> = HashMap::new();
    let mut task_id_by_branch: HashMap<String, String> = HashMap::new();
    for task in tasks {
        let task_id = task.id.clone();
        if let Some(path) = task
            .worktree_path
            .as_deref()
            .map(normalize_path_for_match)
            .filter(|value| !value.is_empty())
        {
            task_id_by_path.insert(path, task_id.clone());
        }
        if let Some(branch) = task
            .branch_name
            .as_deref()
            .map(normalize_branch_for_match)
            .filter(|value| !value.is_empty())
        {
            task_id_by_branch.insert(branch.to_ascii_lowercase(), task_id.clone());
        }
        task_by_id.insert(task_id, task);
    }

    let mut candidates = Vec::new();
    for entry in worktrees {
        let normalized_path = normalize_path_for_match(&entry.path);
        if normalized_path == project_root_normalized {
            continue;
        }
        if !path_is_within_root(Path::new(&entry.path), &managed_root) {
            continue;
        }

        let task_id = task_id_by_path
            .get(&normalized_path)
            .cloned()
            .or_else(|| {
                entry
                    .branch
                    .as_deref()
                    .map(normalize_branch_for_match)
                    .and_then(|branch| task_id_by_branch.get(&branch.to_ascii_lowercase()).cloned())
            })
            .or_else(|| infer_task_id_from_worktree(entry.branch.as_deref(), &entry.worktree_name));

        let Some(task_id) = task_id else {
            continue;
        };
        let Some(task) = task_by_id.get(&task_id).cloned() else {
            continue;
        };
        if !is_terminal_task_status(task.status) {
            continue;
        }

        candidates.push((entry, normalized_path, task));
    }

    if candidates.is_empty() {
        return Ok(());
    }

    let mut updated_tasks = HashSet::new();
    for (entry, normalized_path, task) in candidates {
        let task_worktree_normalized = task
            .worktree_path
            .as_deref()
            .map(normalize_path_for_match)
            .unwrap_or_default();
        remove_worktree_path(project_root, &entry.path);

        if updated_tasks.contains(&task.id) {
            continue;
        }
        if task_worktree_normalized != normalized_path {
            continue;
        }

        let mut updated = task.clone();
        updated.worktree_path = None;
        updated.metadata.updated_by = protocol::ACTOR_DAEMON.to_string();
        hub.tasks().replace(updated).await?;
        updated_tasks.insert(task.id);
    }

    Ok(())
}

pub async fn cleanup_task_worktree_if_enabled(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    cfg: &PostSuccessGitConfig,
) -> Result<()> {
    if !cfg.auto_cleanup_worktree_enabled {
        return Ok(());
    }

    let Some(worktree_path_raw) = task
        .worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let worktree_path = PathBuf::from(worktree_path_raw);
    let worktree_path_str = worktree_path.to_string_lossy().to_string();

    let remove_status = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "remove", "--force", worktree_path_str.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to remove task worktree")?;
    if !remove_status.success() && worktree_path.exists() {
        fs::remove_dir_all(&worktree_path)?;
    }

    let mut updated = task.clone();
    updated.worktree_path = None;
    updated.metadata.updated_by = protocol::ACTOR_DAEMON.to_string();
    hub.tasks().replace(updated).await?;
    Ok(())
}

pub fn remove_worktree_path(project_root: &str, worktree_path: &str) {
    let _ = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "remove", "--force", worktree_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let path = Path::new(worktree_path);
    if path.exists() {
        let _ = fs::remove_dir_all(path);
    }
}

pub fn is_branch_checked_out_in_any_worktree(
    project_root: &str,
    branch_name: &str,
) -> Result<bool> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "list", "--porcelain"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to inspect git worktrees in {}", project_root))?;
    if !output.status.success() {
        return Ok(false);
    }

    let target = format!("refs/heads/{}", branch_name.trim());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("branch ") {
            if value.trim() == target {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
