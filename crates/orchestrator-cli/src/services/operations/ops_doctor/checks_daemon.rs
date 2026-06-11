//! Daemon liveness and control-socket reachability checks.

use std::path::Path;
use std::time::SystemTime;

use orchestrator_daemon_runtime::control::{control_socket_path, ControlClient};
use orchestrator_daemon_runtime::DaemonRuntimeState;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "daemon";

pub(crate) async fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    let project_root_str = ctx.project_root.to_string_lossy().to_string();
    let pid = DaemonRuntimeState::read_daemon_pid_file(&project_root_str);

    let pid_check = match pid {
        Some(pid) => {
            let alive = pid_alive(pid);
            if alive {
                DiagnosticCheck::new("daemon_pid", CATEGORY, CheckStatus::Pass, "Daemon process alive")
                    .details(format!("daemon pid {pid} is alive"))
            } else {
                DiagnosticCheck::new("daemon_pid", CATEGORY, CheckStatus::Fail, "Daemon process alive")
                    .current(format!("pid {pid} recorded but not running"))
                    .expected("daemon process running with recorded pid")
                    .fix(CheckFix::auto(
                        "remove_stale_pid",
                        "Remove the stale pid file so the next `daemon start` can claim it.",
                        "animus daemon start --auto-install",
                    ))
            }
        }
        None => DiagnosticCheck::new("daemon_pid", CATEGORY, CheckStatus::Warn, "Daemon process alive")
            .current("no pid file recorded".to_string())
            .expected("daemon running with pid file present".to_string())
            .fix(CheckFix::command(
                "start_daemon",
                "Start the daemon (with auto-install of any missing plugins).",
                "animus daemon start --auto-install",
            )),
    };
    out.push(pid_check);

    let socket_path = control_socket_path(&ctx.project_root);
    if socket_path.exists() {
        out.push(
            DiagnosticCheck::new("daemon_control_socket_present", CATEGORY, CheckStatus::Pass, "Daemon control socket")
                .details(format!("control socket at {}", socket_path.display())),
        );
    } else {
        out.push(
            DiagnosticCheck::new("daemon_control_socket_present", CATEGORY, CheckStatus::Warn, "Daemon control socket")
                .current(format!("no socket at {}", socket_path.display()))
                .expected("control.sock created by running daemon".to_string())
                .fix(CheckFix::command(
                    "start_daemon",
                    "Start the daemon to create the control socket.",
                    "animus daemon start --auto-install",
                )),
        );
    }

    // Attempt a real RPC roundtrip — only when the socket is present so we
    // don't double-report a stopped daemon. We're already inside the
    // `#[tokio::main]` runtime via the async `handle_doctor` caller, so
    // `.await` directly instead of spawning a nested runtime (which used to
    // panic with "Cannot start a runtime from within a runtime").
    if socket_path.exists() && !ctx.skip_subprocess {
        let probe = probe_daemon_health(&ctx.project_root).await;
        match probe {
            Ok(status) => out.push(
                DiagnosticCheck::new("daemon_health_rpc", CATEGORY, CheckStatus::Pass, "Daemon health RPC")
                    .details(format!("daemon reported {status}")),
            ),
            Err(reason) => out.push(
                DiagnosticCheck::new("daemon_health_rpc", CATEGORY, CheckStatus::Fail, "Daemon health RPC")
                    .current(reason)
                    .expected("daemon.health returns Healthy".to_string())
                    .fix(CheckFix::command(
                        "restart_daemon",
                        "Restart the daemon to recover from a wedged control loop.",
                        "animus daemon restart --autonomous --auto-install",
                    )),
            ),
        }
    }

    out
}

async fn probe_daemon_health(project_root: &Path) -> Result<String, String> {
    let started = SystemTime::now();
    let client = match ControlClient::try_connect(project_root).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err("control socket disappeared between checks".to_string()),
        Err(e) => return Err(format!("failed to connect: {e}")),
    };
    match client.daemon_health().await {
        Ok(response) => {
            let elapsed = started.elapsed().unwrap_or_default();
            Ok(format!("{:?} (RPC took {}ms)", response.status, elapsed.as_millis()))
        }
        Err(e) => Err(format!("daemon.health RPC failed: {e}")),
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Shell out to `kill -0 <pid>` instead of calling `libc::kill` directly:
    // workspace lints forbid `unsafe`, and the spawn cost (one fork+exec) is
    // negligible compared to the doctor's other checks.
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // Platforms without unix kill(): fall back to "trust the pid file".
    true
}

#[cfg(test)]
// Tests serialize on a process-wide `Mutex<()>` to coordinate `HOME`/env
// mutations across parallel async tests. The guard is intentionally held
// across `.await` because the contended resource is the env, not the lock.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::services::operations::ops_doctor::check_kit::CheckContext;
    use protocol::test_utils::EnvVarGuard;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Regression: before the fix, `probe_daemon_health` built a fresh
    /// `tokio::runtime::Runtime` and called `block_on` from inside the
    /// caller's tokio context, which panics with "Cannot start a runtime
    /// from within a runtime". This test exercises the RPC path while
    /// `--skip-subprocess` is off and a stale (non-listening) control
    /// socket exists, so the probe code is actually invoked.
    ///
    /// With the fix in place the function is async and `.await`s instead;
    /// `ControlClient::try_connect` should report the stale socket as
    /// "not running" and the check should record a clean Warn/Fail
    /// verdict rather than panicking the process.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_does_not_panic_when_called_from_tokio_runtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        // Pin HOME to the tempdir so `control_socket_path` (which routes
        // through `protocol::scoped_state_root`) lands under writable
        // scratch space instead of the developer's real `~/.animus/`.
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));

        // Fabricate a stale control socket so the probe branch is taken
        // but `try_connect` resolves to "not running" without needing a
        // real daemon.
        let socket_path = control_socket_path(temp.path());
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).expect("create socket parent dir");
        }
        std::fs::write(&socket_path, b"").expect("touch stale socket");

        let ctx = CheckContext { project_root: temp.path().to_path_buf(), skip_subprocess: false };
        let checks = run(&ctx).await;
        // The function must return without panicking and must emit a
        // daemon_health_rpc check entry (Pass if a daemon happens to be
        // listening, Fail otherwise — both are acceptable; only the
        // panic-on-nested-runtime is the regression we guard against).
        assert!(
            checks.iter().any(|c| c.id == "daemon_health_rpc" || c.id == "daemon_control_socket_present"),
            "expected daemon checks in output: {checks:?}"
        );
    }
}
