use super::*;
use protocol::{IpcAuthRequest, IpcAuthResult};
#[cfg(unix)]
use std::hash::{Hash, Hasher};

#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_LEN: usize = 100;

pub(super) fn max_agents_override_from_env() -> Option<usize> {
    std::env::var("AO_MAX_AGENTS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
}

pub(super) fn should_skip_runner_start() -> bool {
    std::env::var("AO_SKIP_RUNNER_START")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            normalized == "1" || normalized == "true" || normalized == "yes"
        })
        .unwrap_or(false)
}

pub(super) fn default_global_config_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.launchpad.agent-orchestrator")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.launchpad.agent-orchestrator")
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agent-orchestrator")
    }
}

pub(super) fn runner_scope_from_env() -> String {
    std::env::var("AO_RUNNER_SCOPE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

pub(super) fn runner_config_dir(project_root: &Path) -> PathBuf {
    let config_dir = if let Some(override_path) = std::env::var("AO_RUNNER_CONFIG_DIR")
        .ok()
        .or_else(|| std::env::var("AO_CONFIG_DIR").ok())
        .or_else(|| std::env::var("AGENT_ORCHESTRATOR_CONFIG_DIR").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        PathBuf::from(override_path)
    } else if runner_scope_from_env() == "global" {
        default_global_config_dir()
    } else {
        project_runtime_root(project_root)
            .unwrap_or_else(|| project_root.join(".ao"))
            .join("runner")
    };

    normalize_runner_config_dir(config_dir)
}

fn project_runtime_root(project_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".ao")
            .join(protocol::repository_scope_for_path(project_root)),
    )
}

fn normalize_runner_config_dir(config_dir: PathBuf) -> PathBuf {
    #[cfg(unix)]
    {
        shorten_runner_config_dir_if_needed(config_dir)
    }

    #[cfg(not(unix))]
    {
        config_dir
    }
}

#[cfg(unix)]
fn shorten_runner_config_dir_if_needed(config_dir: PathBuf) -> PathBuf {
    let socket_path = runner_socket_path(&config_dir);
    let socket_len = socket_path.as_os_str().to_string_lossy().len();
    if socket_len <= MAX_UNIX_SOCKET_PATH_LEN {
        return config_dir;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config_dir.to_string_lossy().hash(&mut hasher);
    let digest = hasher.finish();

    let shortened = std::env::temp_dir()
        .join("ao-runner")
        .join(format!("{digest:016x}"));
    let _ = std::fs::create_dir_all(&shortened);
    let _ = std::fs::write(
        shortened.join("origin-path.txt"),
        config_dir.to_string_lossy().as_bytes(),
    );
    shortened
}

pub(super) fn runner_lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join("agent-runner.lock")
}

#[cfg(unix)]
pub(super) fn runner_socket_path(config_dir: &Path) -> PathBuf {
    config_dir.join("agent-runner.sock")
}

pub(super) fn parse_runner_lock(lock_content: &str) -> Option<(u32, String)> {
    let mut parts = lock_content.trim().splitn(2, '|');
    let pid = parts.next()?.trim().parse::<u32>().ok()?;
    let address = parts.next().unwrap_or_default().trim().to_string();
    Some((pid, address))
}

pub(super) fn read_runner_lock(config_dir: &Path) -> Option<(u32, String)> {
    let lock_path = runner_lock_path(config_dir);
    let contents = std::fs::read_to_string(lock_path).ok()?;
    parse_runner_lock(&contents)
}

pub(super) fn read_runner_pid_from_lock(config_dir: &Path) -> Option<u32> {
    read_runner_lock(config_dir).map(|(pid, _)| pid)
}

fn remove_malformed_runner_lock_if_present(config_dir: &Path) -> bool {
    let lock_path = runner_lock_path(config_dir);
    let Ok(contents) = std::fs::read_to_string(&lock_path) else {
        return false;
    };
    if parse_runner_lock(&contents).is_some() {
        return false;
    }
    std::fs::remove_file(lock_path).is_ok()
}

#[cfg(unix)]
pub(super) fn is_runner_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub(super) fn is_runner_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}")])
        .output()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn is_runner_process_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
pub(super) fn cleanup_stale_runner_socket(config_dir: &Path) {
    let socket_path = runner_socket_path(config_dir);
    if !socket_path.exists() {
        return;
    }

    match read_runner_lock(config_dir) {
        Some((pid, _)) if is_runner_process_alive(pid) => {
            // Active PID still exists; keep socket as-is.
        }
        Some(_) => {
            let _ = std::fs::remove_file(&socket_path);
        }
        None => {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_err() {
                let _ = std::fs::remove_file(&socket_path);
            }
        }
    }
}

pub(super) fn clear_stale_runner_artifacts(config_dir: &Path) {
    let lock_path = runner_lock_path(config_dir);
    let _ = remove_malformed_runner_lock_if_present(config_dir);
    let lock = read_runner_lock(config_dir);

    if let Some((pid, _)) = lock {
        if !is_runner_process_alive(pid) {
            let _ = std::fs::remove_file(lock_path);
        }
    }

    #[cfg(unix)]
    cleanup_stale_runner_socket(config_dir);
}

#[cfg(unix)]
pub(super) async fn is_agent_runner_ready(config_dir: &Path) -> bool {
    let socket_path = runner_socket_path(config_dir);
    let Ok(Ok(mut stream)) = tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::UnixStream::connect(&socket_path),
    )
    .await
    else {
        return false;
    };

    authenticate_runner_stream(&mut stream, config_dir)
        .await
        .is_some()
}

#[cfg(not(unix))]
pub(super) async fn is_agent_runner_ready(config_dir: &Path) -> bool {
    let Ok(Ok(mut stream)) = tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::TcpStream::connect("127.0.0.1:9001"),
    )
    .await
    else {
        return false;
    };

    authenticate_runner_stream(&mut stream, config_dir)
        .await
        .is_some()
}

async fn authenticate_runner_stream<S>(stream: &mut S, config_dir: &Path) -> Option<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let token = protocol::Config::load_from_dir(config_dir)
        .ok()?
        .get_token()
        .ok()?;
    let request = serde_json::to_string(&IpcAuthRequest::new(token)).ok()?;
    stream.write_all(request.as_bytes()).await.ok()?;
    stream.write_all(b"\n").await.ok()?;
    stream.flush().await.ok()?;

    let mut line = String::new();
    let read_len = tokio::time::timeout(Duration::from_millis(900), async {
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut line).await
    })
    .await
    .ok()?
    .ok()?;

    if read_len == 0 {
        return None;
    }

    let response = serde_json::from_str::<IpcAuthResult>(line.trim()).ok()?;
    response.ok.then_some(())
}

#[cfg(unix)]
pub(super) async fn query_runner_status(config_dir: &Path) -> Option<RunnerStatusResponse> {
    let socket_path = runner_socket_path(config_dir);
    let mut stream = tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::UnixStream::connect(&socket_path),
    )
    .await
    .ok()?
    .ok()?;

    authenticate_runner_stream(&mut stream, config_dir).await?;

    let request = serde_json::to_string(&RunnerStatusRequest::default()).ok()?;
    stream.write_all(request.as_bytes()).await.ok()?;
    stream.write_all(b"\n").await.ok()?;
    stream.flush().await.ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read_len = tokio::time::timeout(Duration::from_millis(900), reader.read_line(&mut line))
        .await
        .ok()?
        .ok()?;
    if read_len == 0 {
        return None;
    }

    serde_json::from_str::<RunnerStatusResponse>(line.trim()).ok()
}

#[cfg(not(unix))]
pub(super) async fn query_runner_status(config_dir: &Path) -> Option<RunnerStatusResponse> {
    let mut stream = tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::TcpStream::connect("127.0.0.1:9001"),
    )
    .await
    .ok()?
    .ok()?;

    authenticate_runner_stream(&mut stream, config_dir).await?;

    let request = serde_json::to_string(&RunnerStatusRequest::default()).ok()?;
    stream.write_all(request.as_bytes()).await.ok()?;
    stream.write_all(b"\n").await.ok()?;
    stream.flush().await.ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read_len = tokio::time::timeout(Duration::from_millis(900), reader.read_line(&mut line))
        .await
        .ok()?
        .ok()?;
    if read_len == 0 {
        return None;
    }

    serde_json::from_str::<RunnerStatusResponse>(line.trim()).ok()
}

fn runner_binary_build_id(binary: &Path) -> Option<String> {
    let fallback = format!("path:{}", binary.display());
    let Ok(metadata) = std::fs::metadata(binary) else {
        return Some(fallback);
    };
    let Ok(modified) = metadata.modified() else {
        return Some(format!("{}-{}", metadata.len(), fallback));
    };
    let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return Some(format!("{}-{}", metadata.len(), fallback));
    };
    Some(format!(
        "{}.{}-{}",
        duration.as_secs(),
        duration.subsec_nanos(),
        metadata.len()
    ))
}

fn runner_status_is_compatible(
    status: &RunnerStatusResponse,
    expected_build_id: Option<&str>,
) -> bool {
    if status.protocol_version != protocol::PROTOCOL_VERSION {
        return false;
    }
    match expected_build_id {
        Some(expected) => status.build_id.as_deref() == Some(expected),
        None => true,
    }
}

pub(super) fn lookup_binary_in_path(binary_name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let output = Command::new("which").arg(binary_name).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }

    #[cfg(windows)]
    {
        let output = Command::new("where").arg(binary_name).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let first_line = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if first_line.is_empty() {
            None
        } else {
            Some(PathBuf::from(first_line))
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = binary_name;
        None
    }
}

pub(super) fn find_agent_runner_binary() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let binary_name = "agent-runner.exe";
    #[cfg(not(target_os = "windows"))]
    let binary_name = "agent-runner";

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            #[cfg(target_os = "macos")]
            {
                let mac_resources = exe_dir.join("../Resources").join(binary_name);
                if mac_resources.exists() {
                    return Ok(mac_resources);
                }
            }

            let sibling = exe_dir.join(binary_name);
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for build_dir in ["debug", "release"] {
            let candidates = [
                cwd.join(format!("target/{build_dir}/{binary_name}")),
                cwd.join(format!(
                    "crates/agent-runner/target/{build_dir}/{binary_name}"
                )),
                cwd.join(format!("agent-runner/target/{build_dir}/{binary_name}")),
                cwd.join(format!(
                    "../crates/agent-runner/target/{build_dir}/{binary_name}"
                )),
                cwd.join(format!("../agent-runner/target/{build_dir}/{binary_name}")),
                cwd.join(format!("../target/{build_dir}/{binary_name}")),
            ];
            for candidate in candidates {
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    if let Some(path) = lookup_binary_in_path(binary_name) {
        return Ok(path);
    }

    Err(anyhow!(
        "Could not find agent-runner binary. Build it with `cargo build -p agent-runner`."
    ))
}

pub(super) async fn ensure_agent_runner_running(project_root: &Path) -> Result<Option<u32>> {
    if should_skip_runner_start() {
        return Ok(None);
    }

    let config_dir = runner_config_dir(project_root);
    let binary = find_agent_runner_binary()?;
    let expected_build_id = runner_binary_build_id(&binary);
    std::fs::create_dir_all(&config_dir).ok();
    protocol::Config::ensure_token_exists(&config_dir)
        .context("failed to provision IPC auth token")?;
    clear_stale_runner_artifacts(&config_dir);

    if is_agent_runner_ready(&config_dir).await {
        if let Some(status) = query_runner_status(&config_dir).await {
            if runner_status_is_compatible(&status, expected_build_id.as_deref()) {
                return Ok(read_runner_pid_from_lock(&config_dir));
            }
            let _ = stop_agent_runner_process(project_root).await;
        } else {
            return Err(anyhow!(
                "agent-runner authentication succeeded but status probe returned no response"
            ));
        }
    }

    let mut command = Command::new(&binary);
    command
        .env("AO_CONFIG_DIR", &config_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    if let Some(build_id) = expected_build_id.as_deref() {
        command.env("AO_RUNNER_BUILD_ID", build_id);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach from the parent session so runner survives short-lived CLI invocations.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = command
        .spawn()
        .with_context(|| format!("Failed to spawn agent-runner at {}", binary.display()))?;
    let spawned_pid = child.id();
    drop(child);

    let mut delay = Duration::from_millis(100);
    for _ in 0..20 {
        if is_agent_runner_ready(&config_dir).await {
            return Ok(read_runner_pid_from_lock(&config_dir).or(Some(spawned_pid)));
        }
        sleep(delay).await;
        delay = std::cmp::min(delay * 2, Duration::from_millis(2_000));
    }

    // In some environments the runner process is alive but needs additional
    // warm-up time before accepting socket connections.
    if is_runner_process_alive(spawned_pid) {
        for _ in 0..15 {
            if is_agent_runner_ready(&config_dir).await {
                return Ok(read_runner_pid_from_lock(&config_dir).or(Some(spawned_pid)));
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    Err(anyhow!(
        "agent-runner failed health check after start (pid {spawned_pid})"
    ))
}

pub(super) async fn stop_agent_runner_process(project_root: &Path) -> Result<bool> {
    let config_dir = runner_config_dir(project_root);
    stop_agent_runner_process_at_config_dir(&config_dir).await
}

async fn stop_agent_runner_process_at_config_dir(config_dir: &Path) -> Result<bool> {
    let lock_path = runner_lock_path(&config_dir);
    let _ = remove_malformed_runner_lock_if_present(config_dir);
    let Some((pid, _)) = read_runner_lock(&config_dir) else {
        #[cfg(unix)]
        cleanup_stale_runner_socket(&config_dir);
        return Ok(false);
    };

    if !is_runner_process_alive(pid) {
        let _ = std::fs::remove_file(lock_path);
        #[cfg(unix)]
        cleanup_stale_runner_socket(&config_dir);
        return Ok(false);
    }

    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to send SIGTERM to agent-runner")?;
        if !status.success() {
            return Err(anyhow!("kill -TERM {} failed", pid));
        }
    }

    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .context("failed to terminate agent-runner process")?;
        if !status.success() {
            return Err(anyhow!("taskkill failed for agent-runner pid {}", pid));
        }
    }

    for _ in 0..20 {
        if !is_runner_process_alive(pid) {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }

    if is_runner_process_alive(pid) {
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
    }

    let _ = std::fs::remove_file(lock_path);
    #[cfg(unix)]
    let _ = std::fs::remove_file(runner_socket_path(&config_dir));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_temp_runner_config_dir() -> (TempDir, PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let config_dir = temp.path().join("runner");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        (temp, config_dir)
    }

    #[test]
    fn runner_status_compatibility_requires_matching_protocol() {
        let status = RunnerStatusResponse {
            active_agents: 0,
            protocol_version: "0.9.0".to_string(),
            build_id: Some("123.456-789".to_string()),
        };
        assert!(!runner_status_is_compatible(&status, Some("123.456-789")));
    }

    #[test]
    fn runner_status_compatibility_requires_matching_build_id_when_expected() {
        let status = RunnerStatusResponse {
            active_agents: 1,
            protocol_version: protocol::PROTOCOL_VERSION.to_string(),
            build_id: Some("old-build".to_string()),
        };
        assert!(!runner_status_is_compatible(&status, Some("new-build")));
    }

    #[test]
    fn runner_status_compatibility_accepts_matching_protocol_and_build_id() {
        let status = RunnerStatusResponse {
            active_agents: 2,
            protocol_version: protocol::PROTOCOL_VERSION.to_string(),
            build_id: Some("build-1".to_string()),
        };
        assert!(runner_status_is_compatible(&status, Some("build-1")));
    }

    #[test]
    fn clear_stale_runner_artifacts_removes_dead_pid_lock_file() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let lock_path = runner_lock_path(&config_dir);
        std::fs::write(&lock_path, "0|unix://unused").expect("write stale lock");
        assert!(lock_path.exists());

        clear_stale_runner_artifacts(&config_dir);
        assert!(!lock_path.exists());
    }

    #[test]
    fn clear_stale_runner_artifacts_removes_malformed_lock_file() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let lock_path = runner_lock_path(&config_dir);
        std::fs::write(&lock_path, "not-a-valid-lock").expect("write malformed lock");
        assert!(lock_path.exists());

        clear_stale_runner_artifacts(&config_dir);
        assert!(!lock_path.exists());
    }

    #[cfg(unix)]
    fn create_stale_unix_socket(path: &Path) {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        std::fs::write(path, b"stale-socket-fixture").expect("write stale socket fixture");
    }

    #[cfg(unix)]
    #[test]
    fn clear_stale_runner_artifacts_removes_unreachable_socket_without_lock() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let socket_path = runner_socket_path(&config_dir);
        create_stale_unix_socket(&socket_path);
        assert!(socket_path.exists());

        clear_stale_runner_artifacts(&config_dir);
        assert!(!socket_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_runner_returns_false_and_cleans_socket_when_lock_is_missing() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let socket_path = runner_socket_path(&config_dir);
        create_stale_unix_socket(&socket_path);
        assert!(socket_path.exists());

        let stopped = stop_agent_runner_process_at_config_dir(&config_dir)
            .await
            .expect("stop runner");
        assert!(!stopped);
        assert!(!socket_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_runner_returns_false_and_cleans_dead_pid_lock() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let lock_path = runner_lock_path(&config_dir);
        std::fs::write(&lock_path, "0|unix://unused").expect("write stale lock");
        assert!(lock_path.exists());

        let stopped = stop_agent_runner_process_at_config_dir(&config_dir)
            .await
            .expect("stop runner");
        assert!(!stopped);
        assert!(!lock_path.exists());
    }

    #[tokio::test]
    async fn stop_runner_returns_false_and_cleans_malformed_lock() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let lock_path = runner_lock_path(&config_dir);
        std::fs::write(&lock_path, "malformed-lock-content").expect("write malformed lock");
        assert!(lock_path.exists());

        let stopped = stop_agent_runner_process_at_config_dir(&config_dir)
            .await
            .expect("stop runner");
        assert!(!stopped);
        assert!(!lock_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_runner_terminates_live_pid_and_cleans_artifacts() {
        let (_temp, config_dir) = new_temp_runner_config_dir();

        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep process");
        let pid = child.id();

        let lock_path = runner_lock_path(&config_dir);
        let socket_path = runner_socket_path(&config_dir);
        std::fs::write(&lock_path, format!("{pid}|unix://unused")).expect("write lock file");
        create_stale_unix_socket(&socket_path);
        assert!(lock_path.exists());
        assert!(socket_path.exists());

        let stop_result = stop_agent_runner_process_at_config_dir(&config_dir).await;
        let _ = child.kill();
        let _ = child.wait();

        let stopped = stop_result.expect("stop runner");
        assert!(stopped);
        assert!(!lock_path.exists());
        assert!(!socket_path.exists());
        assert!(!is_runner_process_alive(pid));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_runner_repeated_cycles_leave_no_lock_or_socket_artifacts() {
        let (_temp, config_dir) = new_temp_runner_config_dir();
        let lock_path = runner_lock_path(&config_dir);
        let socket_path = runner_socket_path(&config_dir);

        for cycle in 1..=20 {
            let mut child = Command::new("sleep")
                .arg("30")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn sleep process");
            let pid = child.id();

            std::fs::write(&lock_path, format!("{pid}|unix://unused")).expect("write lock file");
            create_stale_unix_socket(&socket_path);

            let stop_result = stop_agent_runner_process_at_config_dir(&config_dir).await;
            let _ = child.kill();
            let _ = child.wait();

            let stopped = stop_result.expect("stop runner");
            assert!(stopped, "cycle {cycle} should report a terminated runner");
            assert!(!lock_path.exists(), "cycle {cycle} should clean lock file");
            assert!(
                !socket_path.exists(),
                "cycle {cycle} should clean socket file"
            );
            assert!(
                !is_runner_process_alive(pid),
                "cycle {cycle} should not leave the spawned process alive"
            );
        }
    }
}
