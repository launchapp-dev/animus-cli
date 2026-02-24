use super::*;
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
        project_root.join(".ao").join("runner")
    };

    normalize_runner_config_dir(config_dir)
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
    matches!(
        tokio::time::timeout(
            Duration::from_millis(750),
            tokio::net::UnixStream::connect(&socket_path)
        )
        .await,
        Ok(Ok(_))
    )
}

#[cfg(not(unix))]
pub(super) async fn is_agent_runner_ready(_config_dir: &Path) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(750),
            tokio::net::TcpStream::connect("127.0.0.1:9001")
        )
        .await,
        Ok(Ok(_))
    )
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
pub(super) async fn query_runner_status(_config_dir: &Path) -> Option<RunnerStatusResponse> {
    let mut stream = tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::TcpStream::connect("127.0.0.1:9001"),
    )
    .await
    .ok()?
    .ok()?;

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
                cwd.join(format!("agent-runner/target/{build_dir}/{binary_name}")),
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
    std::fs::create_dir_all(&config_dir).ok();
    clear_stale_runner_artifacts(&config_dir);

    if is_agent_runner_ready(&config_dir).await {
        return Ok(read_runner_pid_from_lock(&config_dir));
    }

    let binary = find_agent_runner_binary()?;
    let mut command = Command::new(&binary);
    command
        .env("AO_CONFIG_DIR", &config_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());

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
    let lock_path = runner_lock_path(&config_dir);
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
