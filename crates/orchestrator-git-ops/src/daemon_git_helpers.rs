use super::*;

#[derive(Debug, Clone)]
pub struct PostSuccessGitConfig {
    pub auto_merge_enabled: bool,
    pub auto_pr_enabled: bool,
    pub auto_commit_before_merge: bool,
    pub auto_merge_target_branch: String,
    pub auto_merge_no_ff: bool,
    pub auto_push_remote: String,
    pub auto_cleanup_worktree_enabled: bool,
    pub auto_prune_worktrees_after_merge: bool,
}

pub fn load_post_success_git_config(project_root: &str) -> PostSuccessGitConfig {
    let mut cfg = PostSuccessGitConfig {
        auto_merge_enabled: false,
        auto_pr_enabled: false,
        auto_commit_before_merge: false,
        auto_merge_target_branch: "main".to_string(),
        auto_merge_no_ff: true,
        auto_push_remote: "origin".to_string(),
        auto_cleanup_worktree_enabled: true,
        auto_prune_worktrees_after_merge: false,
    };

    if let Ok(value) = orchestrator_core::load_daemon_project_config(Path::new(project_root)) {
        cfg.auto_merge_enabled = value.auto_merge_enabled;
        cfg.auto_pr_enabled = value.auto_pr_enabled;
        cfg.auto_commit_before_merge = value.auto_commit_before_merge;
        if let Some(branch) = Some(value.auto_merge_target_branch.trim()).filter(|v| !v.is_empty())
        {
            cfg.auto_merge_target_branch = branch.to_string();
        }
        cfg.auto_merge_no_ff = value.auto_merge_no_ff;
        if let Some(remote) = Some(value.auto_push_remote.trim()).filter(|v| !v.is_empty()) {
            cfg.auto_push_remote = remote.to_string();
        }
        cfg.auto_cleanup_worktree_enabled = value.auto_cleanup_worktree_enabled;
        cfg.auto_prune_worktrees_after_merge = value.auto_prune_worktrees_after_merge;
    }

    if let Some(enabled) = protocol::parse_env_bool_opt("AO_AUTO_MERGE_ENABLED") {
        cfg.auto_merge_enabled = enabled;
    }
    if let Some(enabled) = protocol::parse_env_bool_opt("AO_AUTO_PR_ENABLED") {
        cfg.auto_pr_enabled = enabled;
    }
    if let Some(enabled) = protocol::parse_env_bool_opt("AO_AUTO_COMMIT_BEFORE_MERGE") {
        cfg.auto_commit_before_merge = enabled;
    }
    if let Some(enabled) = protocol::parse_env_bool_opt("AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE") {
        cfg.auto_prune_worktrees_after_merge = enabled;
    }

    if cfg.auto_push_remote != "origin" {
        cfg.auto_push_remote = "origin".to_string();
    }
    if cfg.auto_merge_target_branch != "main" {
        cfg.auto_merge_target_branch = "main".to_string();
    }

    cfg
}

pub fn resolve_task_source_branch(task: &orchestrator_core::OrchestratorTask) -> Option<String> {
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

pub fn git_status(cwd: &str, args: &[&str], operation: &str) -> Result<()> {
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

pub(crate) fn summarize_command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();

    if !stderr_text.is_empty() {
        return stderr_text;
    }
    if !stdout_text.is_empty() {
        return stdout_text;
    }
    "command failed without output".to_string()
}

pub(crate) fn run_external_command(
    cwd: &str,
    program: &str,
    args: &[&str],
    operation: &str,
) -> Result<()> {
    let output = ProcessCommand::new(program)
        .current_dir(cwd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run '{program}' for {operation} in {}", cwd))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} failed in {}: {}",
            operation,
            cwd,
            summarize_command_output(&output.stdout, &output.stderr)
        );
    }
    Ok(())
}

pub(crate) fn is_terminal_task_status(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Cancelled)
}

pub fn default_task_worktree_name(task_id: &str) -> String {
    format!("task-{}", protocol::sanitize_identifier(task_id, "task"))
}

pub fn default_task_branch_name(task_id: &str) -> String {
    format!("ao/{}", protocol::sanitize_identifier(task_id, "task"))
}

pub fn repo_ao_root(project_root: &str) -> Result<PathBuf> {
    protocol::scoped_state_root(std::path::Path::new(project_root))
        .ok_or_else(|| anyhow!("failed to resolve scoped state root for {project_root}"))
}

pub fn repo_worktrees_root(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?.join("worktrees"))
}

pub fn ensure_repo_worktree_root(project_root: &str) -> Result<PathBuf> {
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

pub fn default_task_worktree_path(project_root: &str, task_id: &str) -> Result<PathBuf> {
    Ok(repo_worktrees_root(project_root)?.join(default_task_worktree_name(task_id)))
}

pub fn path_is_within_root(path: &Path, root: &Path) -> bool {
    let Ok(path_canonical) = path.canonicalize() else {
        return false;
    };
    let Ok(root_canonical) = root.canonicalize() else {
        return false;
    };
    path_canonical.starts_with(root_canonical)
}

pub fn is_git_repo(project_root: &str) -> bool {
    ::workflow_runner_v2::is_git_repo(project_root)
}

pub fn git_ref_exists(project_root: &str, reference: &str) -> bool {
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

pub(crate) fn git_default_target_refs(project_root: &str) -> Vec<String> {
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

pub(crate) fn git_is_ancestor(
    project_root: &str,
    source_ref: &str,
    target_ref: &str,
) -> Result<Option<bool>> {
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

pub fn is_branch_merged(project_root: &str, branch_name: &str) -> Result<Option<bool>> {
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

fn git_has_pending_changes(cwd: &str) -> Result<bool> {
    ::workflow_runner_v2::git_has_pending_changes(cwd)
}

fn ensure_git_identity(cwd: &str) -> Result<()> {
    ::workflow_runner_v2::ensure_git_identity(cwd)
}

pub(crate) fn auto_commit_pending_source_changes(cwd: &str, task_id: &str) -> Result<()> {
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
