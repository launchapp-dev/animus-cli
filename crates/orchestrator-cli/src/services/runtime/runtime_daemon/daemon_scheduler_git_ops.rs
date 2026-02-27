use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
struct PostSuccessGitConfig {
    auto_merge_enabled: bool,
    auto_pr_enabled: bool,
    auto_commit_before_merge: bool,
    auto_merge_target_branch: String,
    auto_merge_no_ff: bool,
    auto_push_remote: String,
    auto_cleanup_worktree_enabled: bool,
}

fn load_post_success_git_config(project_root: &str) -> PostSuccessGitConfig {
    let mut cfg = PostSuccessGitConfig {
        auto_merge_enabled: false,
        auto_pr_enabled: false,
        auto_commit_before_merge: false,
        auto_merge_target_branch: "main".to_string(),
        auto_merge_no_ff: true,
        auto_push_remote: "origin".to_string(),
        auto_cleanup_worktree_enabled: true,
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
    }

    if let Some(enabled) = env_bool_override("AO_AUTO_MERGE_ENABLED") {
        cfg.auto_merge_enabled = enabled;
    }
    if let Some(enabled) = env_bool_override("AO_AUTO_PR_ENABLED") {
        cfg.auto_pr_enabled = enabled;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GitIntegrationOperation {
    PushBranch {
        cwd: String,
        remote: String,
        branch: String,
    },
    PushRef {
        cwd: String,
        remote: String,
        source_ref: String,
        target_ref: String,
    },
    OpenPullRequest {
        cwd: String,
        base_branch: String,
        head_branch: String,
        title: String,
        body: String,
        draft: bool,
    },
    EnablePullRequestAutoMerge {
        cwd: String,
        head_branch: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitIntegrationOutboxEntry {
    id: String,
    key: String,
    created_at: String,
    attempts: u32,
    next_attempt_unix_secs: i64,
    last_error: Option<String>,
    operation: GitIntegrationOperation,
}

fn integration_outbox_path(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?
        .join("sync")
        .join("outbox.jsonl"))
}

fn load_git_integration_outbox(project_root: &str) -> Result<Vec<GitIntegrationOutboxEntry>> {
    let path = integration_outbox_path(project_root)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read git integration outbox at {}",
            path.display()
        )
    })?;
    let mut entries = Vec::new();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Ok(entry) = serde_json::from_str::<GitIntegrationOutboxEntry>(line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn save_git_integration_outbox(
    project_root: &str,
    entries: &[GitIntegrationOutboxEntry],
) -> Result<()> {
    let path = integration_outbox_path(project_root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if entries.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    let mut payload = String::new();
    for entry in entries {
        payload.push_str(&serde_json::to_string(entry)?);
        payload.push('\n');
    }

    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("outbox"),
        Uuid::new_v4()
    ));
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

fn git_integration_operation_key(operation: &GitIntegrationOperation) -> String {
    match operation {
        GitIntegrationOperation::PushBranch {
            cwd,
            remote,
            branch,
        } => format!("push-branch:{cwd}:{remote}:{branch}"),
        GitIntegrationOperation::PushRef {
            cwd,
            remote,
            source_ref,
            target_ref,
        } => format!("push-ref:{cwd}:{remote}:{source_ref}:{target_ref}"),
        GitIntegrationOperation::OpenPullRequest {
            cwd,
            base_branch,
            head_branch,
            ..
        } => format!("open-pr:{cwd}:{base_branch}:{head_branch}"),
        GitIntegrationOperation::EnablePullRequestAutoMerge { cwd, head_branch } => {
            format!("enable-pr-auto-merge:{cwd}:{head_branch}")
        }
    }
}

fn enqueue_git_integration_operation(
    project_root: &str,
    operation: GitIntegrationOperation,
) -> Result<()> {
    let mut entries = load_git_integration_outbox(project_root)?;
    let key = git_integration_operation_key(&operation);
    if entries.iter().any(|entry| entry.key == key) {
        return Ok(());
    }

    entries.push(GitIntegrationOutboxEntry {
        id: Uuid::new_v4().to_string(),
        key,
        created_at: Utc::now().to_rfc3339(),
        attempts: 0,
        next_attempt_unix_secs: Utc::now().timestamp(),
        last_error: None,
        operation,
    });
    save_git_integration_outbox(project_root, &entries)
}

fn summarize_command_output(stdout: &[u8], stderr: &[u8]) -> String {
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

fn run_external_command(cwd: &str, program: &str, args: &[&str], operation: &str) -> Result<()> {
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

fn push_branch(cwd: &str, remote: &str, branch: &str) -> Result<()> {
    run_external_command(cwd, "git", &["push", remote, branch], "push source branch")
}

fn push_ref(cwd: &str, remote: &str, source_ref: &str, target_ref: &str) -> Result<()> {
    let refspec = format!("{source_ref}:{target_ref}");
    run_external_command(
        cwd,
        "git",
        &["push", remote, refspec.as_str()],
        "push target ref",
    )
}

fn create_pull_request(
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

fn enable_pull_request_auto_merge(cwd: &str, head_branch: &str) -> Result<()> {
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

fn execute_git_integration_operation(operation: &GitIntegrationOperation) -> Result<()> {
    match operation {
        GitIntegrationOperation::PushBranch {
            cwd,
            remote,
            branch,
        } => push_branch(cwd, remote, branch),
        GitIntegrationOperation::PushRef {
            cwd,
            remote,
            source_ref,
            target_ref,
        } => push_ref(cwd, remote, source_ref, target_ref),
        GitIntegrationOperation::OpenPullRequest {
            cwd,
            base_branch,
            head_branch,
            title,
            body,
            draft,
        } => create_pull_request(cwd, base_branch, head_branch, title, body, *draft),
        GitIntegrationOperation::EnablePullRequestAutoMerge { cwd, head_branch } => {
            enable_pull_request_auto_merge(cwd, head_branch)
        }
    }
}

fn git_integration_retry_delay_secs(attempts: u32) -> i64 {
    let shift = attempts.min(8);
    (1_i64 << shift).clamp(2, 300)
}

pub(super) fn flush_git_integration_outbox(project_root: &str) -> Result<()> {
    let entries = load_git_integration_outbox(project_root)?;
    if entries.is_empty() {
        return Ok(());
    }

    let now = Utc::now().timestamp();
    let mut remaining = Vec::new();
    for mut entry in entries {
        if entry.next_attempt_unix_secs > now {
            remaining.push(entry);
            continue;
        }

        match execute_git_integration_operation(&entry.operation) {
            Ok(()) => {}
            Err(error) => {
                entry.attempts = entry.attempts.saturating_add(1);
                entry.next_attempt_unix_secs =
                    now.saturating_add(git_integration_retry_delay_secs(entry.attempts));
                entry.last_error = Some(error.to_string());
                remaining.push(entry);
            }
        }
    }

    save_git_integration_outbox(project_root, &remaining)
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

fn is_branch_checked_out_in_any_worktree(project_root: &str, branch_name: &str) -> Result<bool> {
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

fn merge_queue_branch_name(task_id: &str) -> String {
    format!("ao/merge-queue/{}", sanitize_identifier_for_git(task_id))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MergeConflictContext {
    pub(super) source_branch: String,
    pub(super) target_branch: String,
    pub(super) merge_worktree_path: String,
    pub(super) conflicted_files: Vec<String>,
    pub(super) merge_queue_branch: String,
    pub(super) push_remote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum PostMergeOutcome {
    Skipped,
    Completed,
    Conflict { context: MergeConflictContext },
}

fn remove_worktree_path(project_root: &str, worktree_path: &str) {
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

fn persist_merge_result_and_push(project_root: &str, context: &MergeConflictContext) -> Result<()> {
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
) -> Result<PostMergeOutcome> {
    let cfg = load_post_success_git_config(project_root);
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
            sanitize_identifier_for_git(cfg.auto_merge_target_branch.as_str())
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

    cleanup_task_worktree_if_enabled(hub, project_root, task, &cfg).await?;
    Ok(PostMergeOutcome::Completed)
}

async fn cleanup_task_worktree_if_enabled(
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
    updated.metadata.updated_by = "ao-daemon".to_string();
    hub.tasks().replace(updated).await?;
    Ok(())
}

pub(super) async fn finalize_merge_conflict_resolution(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
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

    let cfg = load_post_success_git_config(project_root);
    cleanup_task_worktree_if_enabled(hub, project_root, task, &cfg).await?;
    Ok(())
}

pub(super) fn cleanup_merge_conflict_worktree(project_root: &str, context: &MergeConflictContext) {
    remove_worktree_path(project_root, context.merge_worktree_path.as_str());
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
