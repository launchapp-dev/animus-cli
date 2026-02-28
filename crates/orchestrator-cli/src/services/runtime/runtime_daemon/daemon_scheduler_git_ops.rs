use super::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
struct PostSuccessGitConfig {
    auto_merge_enabled: bool,
    auto_pr_enabled: bool,
    auto_commit_before_merge: bool,
    auto_merge_target_branch: String,
    auto_merge_no_ff: bool,
    auto_push_remote: String,
    auto_cleanup_worktree_enabled: bool,
    auto_prune_worktrees_after_merge: bool,
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

    if let Some(enabled) = env_bool_override("AO_AUTO_MERGE_ENABLED") {
        cfg.auto_merge_enabled = enabled;
    }
    if let Some(enabled) = env_bool_override("AO_AUTO_PR_ENABLED") {
        cfg.auto_pr_enabled = enabled;
    }
    if let Some(enabled) = env_bool_override("AO_AUTO_COMMIT_BEFORE_MERGE") {
        cfg.auto_commit_before_merge = enabled;
    }
    if let Some(enabled) = env_bool_override("AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE") {
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

fn env_bool_override(key: &str) -> Option<bool> {
    let value = std::env::var(key).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

const RUNTIME_BINARY_REFRESH_STATE_FILE: &str = "runtime-binary-refresh.json";
const RUNTIME_BINARY_REFRESH_RETRY_BACKOFF_SECS: i64 = 300;
const RUNTIME_BINARY_REFRESH_ENABLED_ENV: &str = "AO_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeBinaryRefreshTrigger {
    Tick,
    PostMerge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeBinaryRefreshOutcome {
    Disabled,
    NotGitRepo,
    NotSupported,
    MainHeadUnavailable,
    Unchanged,
    DeferredActiveAgents,
    DeferredBackoff,
    BuildFailed,
    RunnerRefreshFailed,
    Refreshed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RuntimeBinaryRefreshState {
    #[serde(default)]
    last_successful_main_head: Option<String>,
    #[serde(default)]
    last_attempt_main_head: Option<String>,
    #[serde(default)]
    last_attempt_unix_secs: Option<i64>,
    #[serde(default)]
    last_error: Option<String>,
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

fn runtime_binary_refresh_enabled() -> bool {
    env_bool_override(RUNTIME_BINARY_REFRESH_ENABLED_ENV).unwrap_or(true)
}

fn runtime_binary_refresh_supported(project_root: &str) -> bool {
    #[cfg(test)]
    {
        let _ = project_root;
        return true;
    }

    #[allow(unreachable_code)]
    {
        let config_path = Path::new(project_root).join(".cargo").join("config.toml");
        let Ok(content) = fs::read_to_string(config_path) else {
            return false;
        };
        content.contains("ao-bin-build")
    }
}

fn runtime_binary_refresh_retry_backoff_secs() -> i64 {
    std::env::var("AO_RUNTIME_BINARY_REFRESH_RETRY_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(RUNTIME_BINARY_REFRESH_RETRY_BACKOFF_SECS)
}

fn runtime_binary_refresh_state_path(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?
        .join("sync")
        .join(RUNTIME_BINARY_REFRESH_STATE_FILE))
}

fn load_runtime_binary_refresh_state(project_root: &str) -> RuntimeBinaryRefreshState {
    let Ok(path) = runtime_binary_refresh_state_path(project_root) else {
        return RuntimeBinaryRefreshState::default();
    };
    if !path.exists() {
        return RuntimeBinaryRefreshState::default();
    }

    let Ok(content) = fs::read_to_string(&path) else {
        return RuntimeBinaryRefreshState::default();
    };
    if content.trim().is_empty() {
        return RuntimeBinaryRefreshState::default();
    }

    serde_json::from_str::<RuntimeBinaryRefreshState>(&content).unwrap_or_default()
}

fn save_runtime_binary_refresh_state(
    project_root: &str,
    state: &RuntimeBinaryRefreshState,
) -> Result<()> {
    let path = runtime_binary_refresh_state_path(project_root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(RUNTIME_BINARY_REFRESH_STATE_FILE);
    let tmp_path = path.with_file_name(format!("{file_name}.{}.tmp", Uuid::new_v4()));
    let payload = serde_json::to_string_pretty(state)?;
    fs::write(&tmp_path, format!("{payload}\n"))?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

fn resolve_main_head_commit(project_root: &str) -> Option<String> {
    for reference in [
        "refs/heads/main",
        "refs/remotes/origin/main",
        "main",
        "origin/main",
        "HEAD",
    ] {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args(["rev-parse", "--verify", reference])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !sha.is_empty() {
            return Some(sha);
        }
    }
    None
}

fn runtime_binary_refresh_backoff_active(
    state: &RuntimeBinaryRefreshState,
    main_head: &str,
    trigger: RuntimeBinaryRefreshTrigger,
) -> bool {
    if trigger != RuntimeBinaryRefreshTrigger::Tick {
        return false;
    }
    if state.last_attempt_main_head.as_deref() != Some(main_head) {
        return false;
    }
    if state
        .last_error
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return false;
    }

    let Some(last_attempt) = state.last_attempt_unix_secs else {
        return false;
    };
    let elapsed = Utc::now().timestamp().saturating_sub(last_attempt);
    elapsed < runtime_binary_refresh_retry_backoff_secs()
}

#[cfg(test)]
#[derive(Default)]
struct RuntimeBinaryRefreshTestHooks {
    active_agents_override: Option<usize>,
    build_results: std::collections::VecDeque<Result<()>>,
    runner_refresh_results: std::collections::VecDeque<Result<()>>,
    build_calls: usize,
    runner_refresh_calls: usize,
}

#[cfg(test)]
fn runtime_binary_refresh_test_hooks() -> &'static std::sync::Mutex<RuntimeBinaryRefreshTestHooks> {
    static HOOKS: std::sync::OnceLock<std::sync::Mutex<RuntimeBinaryRefreshTestHooks>> =
        std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(RuntimeBinaryRefreshTestHooks::default()))
}

#[cfg(test)]
fn with_runtime_binary_refresh_test_hooks<T>(
    f: impl FnOnce(&mut RuntimeBinaryRefreshTestHooks) -> T,
) -> T {
    let mut hooks = runtime_binary_refresh_test_hooks()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut hooks)
}

#[cfg(test)]
fn runtime_binary_refresh_test_active_agents_override() -> Option<usize> {
    with_runtime_binary_refresh_test_hooks(|hooks| hooks.active_agents_override)
}

#[cfg(test)]
fn take_runtime_binary_refresh_build_result() -> Option<Result<()>> {
    with_runtime_binary_refresh_test_hooks(|hooks| {
        hooks.build_calls = hooks.build_calls.saturating_add(1);
        hooks.build_results.pop_front()
    })
}

#[cfg(test)]
fn take_runtime_binary_refresh_runner_refresh_result() -> Option<Result<()>> {
    with_runtime_binary_refresh_test_hooks(|hooks| {
        hooks.runner_refresh_calls = hooks.runner_refresh_calls.saturating_add(1);
        hooks.runner_refresh_results.pop_front()
    })
}

async fn runtime_binary_refresh_active_agents(hub: Arc<dyn ServiceHub>) -> usize {
    #[cfg(test)]
    if let Some(override_count) = runtime_binary_refresh_test_active_agents_override() {
        return override_count;
    }

    hub.daemon().active_agents().await.unwrap_or(usize::MAX)
}

fn run_runtime_binary_build(project_root: &str) -> Result<()> {
    #[cfg(test)]
    {
        let _ = project_root;
        if let Some(result) = take_runtime_binary_refresh_build_result() {
            return result;
        }
        return Ok(());
    }

    #[allow(unreachable_code)]
    let output = ProcessCommand::new("cargo")
        .current_dir(project_root)
        .arg("ao-bin-build")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run cargo ao-bin-build in {}", project_root))?;
    if !output.status.success() {
        anyhow::bail!(
            "cargo ao-bin-build failed in {}: {}",
            project_root,
            summarize_command_output(&output.stdout, &output.stderr)
        );
    }
    Ok(())
}

async fn refresh_runner_after_runtime_binary_build(hub: Arc<dyn ServiceHub>) -> Result<()> {
    #[cfg(test)]
    {
        let _ = &hub;
        if let Some(result) = take_runtime_binary_refresh_runner_refresh_result() {
            return result;
        }
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        let daemon = hub.daemon();
        let previous_status = daemon
            .status()
            .await
            .unwrap_or(orchestrator_core::DaemonStatus::Running);
        let _ = daemon.stop().await;
        daemon
            .start()
            .await
            .context("failed to restart runner after runtime binary refresh")?;
        if previous_status == orchestrator_core::DaemonStatus::Paused {
            let _ = daemon.pause().await;
        }
        Ok(())
    }
}

pub(super) async fn refresh_runtime_binaries_if_main_advanced(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    trigger: RuntimeBinaryRefreshTrigger,
) -> RuntimeBinaryRefreshOutcome {
    if !runtime_binary_refresh_enabled() {
        return RuntimeBinaryRefreshOutcome::Disabled;
    }
    if !is_git_repo(project_root) {
        return RuntimeBinaryRefreshOutcome::NotGitRepo;
    }
    if !runtime_binary_refresh_supported(project_root) {
        return RuntimeBinaryRefreshOutcome::NotSupported;
    }

    let Some(main_head) = resolve_main_head_commit(project_root) else {
        return RuntimeBinaryRefreshOutcome::MainHeadUnavailable;
    };
    let mut state = load_runtime_binary_refresh_state(project_root);
    if state.last_successful_main_head.as_deref() == Some(main_head.as_str()) {
        return RuntimeBinaryRefreshOutcome::Unchanged;
    }

    let active_agents = runtime_binary_refresh_active_agents(hub.clone()).await;
    if active_agents > 0 {
        return RuntimeBinaryRefreshOutcome::DeferredActiveAgents;
    }
    if runtime_binary_refresh_backoff_active(&state, main_head.as_str(), trigger) {
        return RuntimeBinaryRefreshOutcome::DeferredBackoff;
    }

    state.last_attempt_main_head = Some(main_head.clone());
    state.last_attempt_unix_secs = Some(Utc::now().timestamp());
    state.last_error = None;
    let _ = save_runtime_binary_refresh_state(project_root, &state);

    if let Err(error) = run_runtime_binary_build(project_root) {
        state.last_error = Some(error.to_string());
        let _ = save_runtime_binary_refresh_state(project_root, &state);
        return RuntimeBinaryRefreshOutcome::BuildFailed;
    }

    if let Err(error) = refresh_runner_after_runtime_binary_build(hub).await {
        state.last_error = Some(error.to_string());
        let _ = save_runtime_binary_refresh_state(project_root, &state);
        return RuntimeBinaryRefreshOutcome::RunnerRefreshFailed;
    }

    state.last_successful_main_head = Some(main_head);
    state.last_attempt_unix_secs = Some(Utc::now().timestamp());
    state.last_error = None;
    let _ = save_runtime_binary_refresh_state(project_root, &state);
    RuntimeBinaryRefreshOutcome::Refreshed
}

fn git_has_pending_changes(cwd: &str) -> Result<bool> {
    crate::services::runtime::workflow_executor::phase_git::git_has_pending_changes(cwd)
}

fn ensure_git_identity(cwd: &str) -> Result<()> {
    crate::services::runtime::workflow_executor::phase_git::ensure_git_identity(cwd)
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

#[derive(Debug, Clone)]
struct GitWorktreeEntry {
    worktree_name: String,
    path: String,
    branch: Option<String>,
}

fn parse_git_worktree_list_porcelain(output: &str) -> Vec<GitWorktreeEntry> {
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

fn infer_task_id_from_worktree(branch: Option<&str>, worktree_name: &str) -> Option<String> {
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

fn is_terminal_task_status(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Cancelled)
}

async fn auto_prune_completed_task_worktrees_after_merge(
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
        updated.metadata.updated_by = "ao-daemon".to_string();
        hub.tasks().replace(updated).await?;
        updated_tasks.insert(task.id);
    }

    Ok(())
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

pub(super) fn daemon_repo_runtime_root(project_root: &str) -> Result<PathBuf> {
    repo_ao_root(project_root)
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

pub(super) fn rebase_worktree_on_main(project_root: &str, worktree_cwd: &str) {
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

fn is_git_repo(project_root: &str) -> bool {
    crate::services::runtime::workflow_executor::phase_git::is_git_repo(project_root)
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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::InMemoryServiceHub;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
        let _lock = env_lock().lock().expect("env lock should be available");
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
