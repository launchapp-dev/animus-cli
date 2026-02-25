use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
struct PostSuccessGitConfig {
    auto_merge_enabled: bool,
    auto_commit_before_merge: bool,
    auto_merge_target_branch: String,
    auto_merge_no_ff: bool,
    auto_push_remote: String,
    auto_cleanup_worktree_enabled: bool,
}

fn load_post_success_git_config(project_root: &str) -> PostSuccessGitConfig {
    let mut cfg = PostSuccessGitConfig {
        auto_merge_enabled: false,
        auto_commit_before_merge: false,
        auto_merge_target_branch: "main".to_string(),
        auto_merge_no_ff: true,
        auto_push_remote: "origin".to_string(),
        auto_cleanup_worktree_enabled: true,
    };

    let config_path = Path::new(project_root).join(".ao").join("pm-config.json");
    if let Ok(content) = fs::read_to_string(config_path) {
        if let Ok(value) = serde_json::from_str::<Value>(&content) {
            if let Some(enabled) = value.get("auto_merge_enabled").and_then(Value::as_bool) {
                cfg.auto_merge_enabled = enabled;
            }
            if let Some(enabled) = value.get("auto_commit_before_merge").and_then(Value::as_bool) {
                cfg.auto_commit_before_merge = enabled;
            }
            if let Some(branch) = value
                .get("auto_merge_target_branch")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                cfg.auto_merge_target_branch = branch.to_string();
            }
            if let Some(no_ff) = value.get("auto_merge_no_ff").and_then(Value::as_bool) {
                cfg.auto_merge_no_ff = no_ff;
            }
            if let Some(remote) = value
                .get("auto_push_remote")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                cfg.auto_push_remote = remote.to_string();
            }
            if let Some(cleanup) = value
                .get("auto_cleanup_worktree_enabled")
                .and_then(Value::as_bool)
            {
                cfg.auto_cleanup_worktree_enabled = cleanup;
            }
        }
    }

    if let Some(enabled) = env_bool_override("AO_AUTO_MERGE_ENABLED") {
        cfg.auto_merge_enabled = enabled;
    }
    if let Some(enabled) = env_bool_override("AO_AUTO_COMMIT_BEFORE_MERGE") {
        cfg.auto_commit_before_merge = enabled;
    }

    if cfg.auto_push_remote != "origin" {
        cfg.auto_push_remote = "origin".to_string();
    }
    if cfg.auto_merge_target_branch != "main" {
        cfg.auto_merge_target_branch = "main".to_string();
    }

    cfg
}

fn env_bool_override(key: &str) -> Option<bool> {
    let value = std::env::var(key).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn resolve_task_source_branch(task: &orchestrator_core::OrchestratorTask) -> Option<String> {
    if let Some(branch_name) = task
        .branch_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(branch_name.to_string());
    }

    let worktree_path = task
        .worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if !Path::new(worktree_path).exists() {
        return None;
    }

    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn git_status(cwd: &str, args: &[&str], operation: &str) -> Result<()> {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run git operation '{operation}' in {}", cwd))?;
    if !status.success() {
        anyhow::bail!(
            "git operation '{}' failed in {}: git {}",
            operation,
            cwd,
            args.join(" ")
        );
    }
    Ok(())
}

fn git_has_pending_changes(cwd: &str) -> Result<bool> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to inspect git status in {}", cwd))?;

    if !output.status.success() {
        anyhow::bail!("git status --porcelain failed in {}", cwd);
    }

    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn ensure_git_identity(cwd: &str) -> Result<()> {
    for (key, default_value) in [
        ("user.name", "AO Daemon"),
        ("user.email", "ao-daemon@local"),
    ] {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["config", "--get", key])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to read git config {} in {}", key, cwd))?;

        let configured =
            output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty();
        if !configured {
            git_status(
                cwd,
                &["config", key, default_value],
                "configure git identity",
            )?;
        }
    }

    Ok(())
}

fn auto_commit_pending_source_changes(cwd: &str, task_id: &str) -> Result<()> {
    if !git_has_pending_changes(cwd)? {
        return Ok(());
    }

    ensure_git_identity(cwd)?;
    git_status(cwd, &["add", "-A"], "stage pending source branch changes")?;
    let commit_message = format!("chore(ao): auto-commit {task_id} before merge");
    git_status(
        cwd,
        &["commit", "-m", commit_message.as_str()],
        "auto-commit source branch changes before merge",
    )?;
    Ok(())
}

pub(super) fn commit_implementation_changes(cwd: &str, commit_message: &str) -> Result<()> {
    let commit_message = commit_message.trim();
    if commit_message.is_empty() {
        anyhow::bail!("implementation phase requires a non-empty commit message");
    }
    if !is_git_repo(cwd) {
        anyhow::bail!(
            "implementation phase requires a git repository for commit at {}",
            cwd
        );
    }
    if !git_has_pending_changes(cwd)? {
        anyhow::bail!(
            "implementation phase requires file changes to commit, but no changes were detected in {}",
            cwd
        );
    }

    ensure_git_identity(cwd)?;
    git_status(cwd, &["add", "-A"], "stage implementation changes")?;
    git_status(
        cwd,
        &["commit", "-m", commit_message],
        "commit implementation changes",
    )?;
    Ok(())
}

pub(super) async fn post_success_merge_push_and_cleanup(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Result<bool> {
    let cfg = load_post_success_git_config(project_root);
    if !cfg.auto_merge_enabled || !is_git_repo(project_root) {
        return Ok(false);
    }

    let Some(source_branch) = resolve_task_source_branch(task) else {
        return Ok(false);
    };

    let source_push_cwd = task
        .worktree_path
        .as_deref()
        .filter(|path| Path::new(path).exists())
        .unwrap_or(project_root);
    if cfg.auto_commit_before_merge {
        auto_commit_pending_source_changes(source_push_cwd, &task.id)?;
    }
    git_status(
        source_push_cwd,
        &[
            "push",
            cfg.auto_push_remote.as_str(),
            source_branch.as_str(),
        ],
        "push source branch",
    )?;

    let merge_worktree_root = ensure_repo_worktree_root(project_root)?;
    let merge_worktree_path = merge_worktree_root.join(format!(
        "__merge-{}",
        sanitize_identifier_for_git(cfg.auto_merge_target_branch.as_str())
    ));
    let merge_worktree_path_str = merge_worktree_path.to_string_lossy().to_string();

    if merge_worktree_path.exists() {
        let _ = ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args([
                "worktree",
                "remove",
                "--force",
                merge_worktree_path_str.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if merge_worktree_path.exists() {
            let _ = fs::remove_dir_all(&merge_worktree_path);
        }
    }
    if let Some(parent) = merge_worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let merge_result = (|| -> Result<()> {
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
        if !git_ref_exists(project_root, target_ref.as_str()) {
            let remote_ref = format!(
                "refs/remotes/{}/{}",
                cfg.auto_push_remote, cfg.auto_merge_target_branch
            );
            if git_ref_exists(project_root, remote_ref.as_str()) {
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
        }

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
            anyhow::bail!(
                "failed to merge '{}' into '{}'",
                source_branch,
                cfg.auto_merge_target_branch
            );
        }

        git_status(
            merge_worktree_path_str.as_str(),
            &[
                "push",
                cfg.auto_push_remote.as_str(),
                cfg.auto_merge_target_branch.as_str(),
            ],
            "push target branch",
        )?;
        Ok(())
    })();

    let _ = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args([
            "worktree",
            "remove",
            "--force",
            merge_worktree_path_str.as_str(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if merge_worktree_path.exists() {
        let _ = fs::remove_dir_all(&merge_worktree_path);
    }
    merge_result?;

    if !cfg.auto_cleanup_worktree_enabled {
        return Ok(true);
    }

    let Some(worktree_path_raw) = task
        .worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(true);
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
    updated.metadata.updated_by = "ao-daemon".to_string();
    hub.tasks().replace(updated).await?;
    Ok(true)
}

pub(super) fn task_status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Backlog => "backlog",
        TaskStatus::Ready => "ready",
        TaskStatus::InProgress => "in-progress",
        TaskStatus::Blocked => "blocked",
        TaskStatus::OnHold => "on-hold",
        TaskStatus::Done => "done",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn sanitize_identifier_for_git(value: &str) -> String {
    let mut sanitized = String::new();
    let mut previous_dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            sanitized.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            sanitized.push('-');
            previous_dash = true;
        }
    }
    sanitized = sanitized.trim_matches('-').to_string();
    if sanitized.is_empty() {
        "task".to_string()
    } else {
        sanitized
    }
}

fn default_task_worktree_name(task_id: &str) -> String {
    format!("task-{}", sanitize_identifier_for_git(task_id))
}

fn default_task_branch_name(task_id: &str) -> String {
    format!("ao/{}", sanitize_identifier_for_git(task_id))
}

fn ao_root_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory for ~/.ao"))?;
    Ok(home.join(".ao"))
}

fn repo_worktree_scope(project_root: &str) -> String {
    let canonical = Path::new(project_root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(project_root));
    let canonical_display = canonical.to_string_lossy();
    let repo_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_identifier_for_git)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repo".to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical_display.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    );

    format!("{repo_name}-{suffix}")
}

fn repo_ao_root(project_root: &str) -> Result<PathBuf> {
    Ok(ao_root_dir()?.join(repo_worktree_scope(project_root)))
}

fn repo_worktrees_root(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?.join("worktrees"))
}

fn ensure_repo_worktree_root(project_root: &str) -> Result<PathBuf> {
    let repo_root = repo_ao_root(project_root)?;
    let root = repo_worktrees_root(project_root)?;
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&root)?;

    let canonical = Path::new(project_root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(project_root));
    let marker_path = repo_root.join(".project-root");
    let marker_content = format!("{}\n", canonical.to_string_lossy());
    let should_write_marker = fs::read_to_string(&marker_path)
        .map(|existing| existing != marker_content)
        .unwrap_or(true);
    if should_write_marker {
        fs::write(&marker_path, marker_content)?;
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
    Ok(repo_worktrees_root(project_root)?.join(default_task_worktree_name(task_id)))
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

pub(super) async fn ensure_task_execution_cwd(
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

fn is_git_repo(project_root: &str) -> bool {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

fn git_default_target_refs(project_root: &str) -> Vec<String> {
    let mut refs = Vec::new();

    if let Ok(output) = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
    {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !value.is_empty() {
                refs.push(value);
            }
        }
    }

    for reference in ["refs/heads/main", "refs/remotes/origin/main", "HEAD"] {
        if git_ref_exists(project_root, reference) {
            refs.push(reference.to_string());
        }
    }

    refs.sort();
    refs.dedup();
    refs
}

fn git_is_ancestor(project_root: &str, source_ref: &str, target_ref: &str) -> Result<Option<bool>> {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["merge-base", "--is-ancestor", source_ref, target_ref])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| {
            format!("failed merge-base check for {source_ref} -> {target_ref} in {project_root}")
        })?;

    Ok(match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    })
}

pub(super) fn is_branch_merged(project_root: &str, branch_name: &str) -> Result<Option<bool>> {
    let branch_name = branch_name.trim();
    if branch_name.is_empty() {
        return Ok(Some(true));
    }
    if !is_git_repo(project_root) {
        return Ok(None);
    }

    let mut source_refs = Vec::new();
    for reference in [
        format!("refs/heads/{branch_name}"),
        format!("refs/remotes/origin/{branch_name}"),
        branch_name.to_string(),
    ] {
        if git_ref_exists(project_root, &reference) {
            source_refs.push(reference);
        }
    }
    source_refs.sort();
    source_refs.dedup();
    if source_refs.is_empty() {
        return Ok(None);
    }

    let target_refs = git_default_target_refs(project_root);
    if target_refs.is_empty() {
        return Ok(None);
    }

    let mut saw_false = false;
    for source_ref in &source_refs {
        for target_ref in &target_refs {
            match git_is_ancestor(project_root, source_ref, target_ref)? {
                Some(true) => return Ok(Some(true)),
                Some(false) => saw_false = true,
                None => {}
            }
        }
    }

    Ok(if saw_false { Some(false) } else { None })
}
