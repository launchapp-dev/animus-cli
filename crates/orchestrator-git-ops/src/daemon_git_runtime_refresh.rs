use super::*;

const RUNTIME_BINARY_REFRESH_STATE_FILE: &str = "runtime-binary-refresh.json";
const RUNTIME_BINARY_REFRESH_RETRY_BACKOFF_SECS: i64 = 300;
pub const RUNTIME_BINARY_REFRESH_ENABLED_ENV: &str = "AO_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBinaryRefreshTrigger {
    Tick,
    PostMerge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBinaryRefreshOutcome {
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
pub struct RuntimeBinaryRefreshState {
    #[serde(default)]
    pub last_successful_main_head: Option<String>,
    #[serde(default)]
    pub last_attempt_main_head: Option<String>,
    #[serde(default)]
    pub last_attempt_unix_secs: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
}

fn runtime_binary_refresh_enabled() -> bool {
    protocol::parse_env_bool_opt(RUNTIME_BINARY_REFRESH_ENABLED_ENV).unwrap_or(true)
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


fn runtime_binary_refresh_state_path(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?
        .join("sync")
        .join(RUNTIME_BINARY_REFRESH_STATE_FILE))
}

pub fn load_runtime_binary_refresh_state(project_root: &str) -> RuntimeBinaryRefreshState {
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
    orchestrator_core::write_json_pretty(&path, state)
}

pub fn resolve_main_head_commit(project_root: &str) -> Option<String> {
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
    elapsed < RUNTIME_BINARY_REFRESH_RETRY_BACKOFF_SECS
}

#[cfg(test)]
#[derive(Default)]
pub struct RuntimeBinaryRefreshTestHooks {
    pub active_agents_override: Option<usize>,
    pub build_results: std::collections::VecDeque<Result<()>>,
    pub runner_refresh_results: std::collections::VecDeque<Result<()>>,
    pub build_calls: usize,
    pub runner_refresh_calls: usize,
}

#[cfg(test)]
pub fn runtime_binary_refresh_test_hooks(
) -> &'static std::sync::Mutex<RuntimeBinaryRefreshTestHooks> {
    static HOOKS: std::sync::OnceLock<std::sync::Mutex<RuntimeBinaryRefreshTestHooks>> =
        std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(RuntimeBinaryRefreshTestHooks::default()))
}

#[cfg(test)]
pub fn with_runtime_binary_refresh_test_hooks<T>(
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

pub fn run_runtime_binary_build(project_root: &str) -> Result<()> {
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
            .start(Default::default())
            .await
            .context("failed to restart runner after runtime binary refresh")?;
        if previous_status == orchestrator_core::DaemonStatus::Paused {
            let _ = daemon.pause().await;
        }
        Ok(())
    }
}

pub async fn refresh_runtime_binaries_if_main_advanced(
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
