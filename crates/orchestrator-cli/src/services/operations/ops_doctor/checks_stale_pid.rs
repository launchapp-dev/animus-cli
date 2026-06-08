//! Stale daemon pid file detection + cleanup.
//!
//! Detects a `daemon.pid` whose recorded PID no longer points at a live
//! process. The matching socket file (`control.sock`) is included in the
//! fix payload so the next `animus daemon start` can recreate both cleanly.

use std::path::PathBuf;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "stale_pid";

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    let Some(daemon_dir) = daemon_dir(&ctx.project_root) else {
        out.push(
            DiagnosticCheck::new("stale_daemon_pid", CATEGORY, CheckStatus::Skipped, "Stale daemon pid file")
                .details("scoped state root unavailable (no HOME)"),
        );
        return out;
    };

    let pid_path = daemon_dir.join("daemon.pid");
    if !pid_path.exists() {
        out.push(
            DiagnosticCheck::new("stale_daemon_pid", CATEGORY, CheckStatus::Pass, "Stale daemon pid file")
                .details("no daemon.pid present"),
        );
        return out;
    }

    let pid = match std::fs::read_to_string(&pid_path).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
        Some(pid) => pid,
        None => {
            out.push(
                DiagnosticCheck::new("stale_daemon_pid", CATEGORY, CheckStatus::Warn, "Stale daemon pid file")
                    .current(format!("{} contains unparseable PID", pid_path.display()))
                    .expected("file holds a valid u32 PID or does not exist".to_string())
                    .fix(CheckFix::auto_no_command(
                        "remove_stale_daemon_pid",
                        &format!("Delete the unparseable pid file at {}.", pid_path.display()),
                    )),
            );
            return out;
        }
    };

    if protocol::is_process_alive(pid) {
        out.push(
            DiagnosticCheck::new("stale_daemon_pid", CATEGORY, CheckStatus::Pass, "Stale daemon pid file")
                .details(format!("pid {pid} from {} is alive", pid_path.display())),
        );
    } else {
        out.push(
            DiagnosticCheck::new("stale_daemon_pid", CATEGORY, CheckStatus::Warn, "Stale daemon pid file")
                .current(format!("pid {pid} from {} is no longer running", pid_path.display()))
                .expected("daemon.pid is removed or points at a live process".to_string())
                .fix(CheckFix::auto_no_command(
                    "remove_stale_daemon_pid",
                    &format!(
                        "Delete {} (and control.sock if present) so the next `animus daemon start` is clean.",
                        pid_path.display()
                    ),
                )),
        );
    }

    out
}

pub(crate) fn collect_stale_artifacts_for_fix(project_root: &std::path::Path) -> Vec<PathBuf> {
    let Some(daemon_dir) = daemon_dir(project_root) else {
        return Vec::new();
    };
    let pid_path = daemon_dir.join("daemon.pid");
    if !pid_path.exists() {
        return Vec::new();
    }
    // Distinguish three cases:
    //   * parseable + dead    → safe to remove both pid file AND socket
    //   * parseable + alive   → nothing to fix
    //   * unparseable / empty → remove ONLY the pid file. The socket may
    //     still be bound by a live daemon whose pid file was truncated
    //     out-of-band; unlinking it would silently break `animus daemon
    //     status` and every other control call.
    let pid_state = std::fs::read_to_string(&pid_path).ok().and_then(|s| s.trim().parse::<u32>().ok());
    let mut targets = Vec::with_capacity(2);
    match pid_state {
        Some(pid) if !protocol::is_process_alive(pid) => {
            targets.push(pid_path);
            // Only unlink the control socket if it is genuinely unused.
            // A foreground/non-autonomous daemon can be live while the
            // recorded autonomous pid is dead, in which case the socket
            // is bound to that newer daemon's process and removing it
            // breaks every subsequent control call. Probe by attempting
            // a non-blocking connect; if it succeeds, leave the socket.
            if let Some(sock_path) = control_socket_path(project_root) {
                if sock_path.exists() && !unix_socket_is_serving(&sock_path) {
                    targets.push(sock_path);
                }
            }
        }
        None => {
            targets.push(pid_path);
        }
        Some(_) => {}
    }
    targets
}

#[cfg(unix)]
fn unix_socket_is_serving(path: &std::path::Path) -> bool {
    // A live daemon's `accept()` loop responds to connect() immediately;
    // a stale/abandoned socket file rejects with ECONNREFUSED. We only
    // need the connect-side handshake, so the connection is dropped on
    // return.
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn unix_socket_is_serving(_path: &std::path::Path) -> bool {
    // Non-Unix platforms don't host the control socket at all, so the
    // worst-case here is failing closed (treating it as serving so we
    // never unlink).
    true
}

fn daemon_dir(project_root: &std::path::Path) -> Option<PathBuf> {
    Some(protocol::scoped_state_root(project_root)?.join("daemon"))
}

fn control_socket_path(project_root: &std::path::Path) -> Option<PathBuf> {
    Some(protocol::scoped_state_root(project_root)?.join("control.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn detects_stale_pid_pointing_at_dead_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let scoped = protocol::scoped_state_root(&project).unwrap();
        let daemon_dir = scoped.join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        // PID 999999 is overwhelmingly unlikely to exist; if the test host
        // happens to recycle it we accept the false negative — `kill(0)` is
        // the canonical liveness check the rest of the codebase uses.
        std::fs::write(daemon_dir.join("daemon.pid"), "999999\n").unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: true };
        let checks = run(&ctx);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert_eq!(checks[0].id, "stale_daemon_pid");
        assert_eq!(checks[0].fixes.len(), 1);
        assert!(checks[0].fixes[0].auto_applicable);

        let targets = collect_stale_artifacts_for_fix(&project);
        assert_eq!(targets, vec![daemon_dir.join("daemon.pid")]);
    }

    #[test]
    fn includes_sock_path_when_dead_socket_present() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project2");
        std::fs::create_dir_all(&project).unwrap();
        let scoped = protocol::scoped_state_root(&project).unwrap();
        let daemon_dir = scoped.join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        std::fs::write(daemon_dir.join("daemon.pid"), "999998").unwrap();
        // Plain file masquerading as a socket — connect() will fail, so
        // the doctor treats it as stale and includes it for removal.
        std::fs::write(scoped.join("control.sock"), b"").unwrap();

        let targets = collect_stale_artifacts_for_fix(&project);
        assert!(targets.iter().any(|p| p.ends_with("daemon.pid")));
        assert!(targets.iter().any(|p| p.ends_with("control.sock")));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_live_socket_even_when_recorded_pid_is_dead() {
        // Repro for the round-7 codex finding: a foreground/non-autonomous
        // daemon may be bound to control.sock while daemon.pid still holds
        // the dead PID of the previous autonomous daemon. We must not
        // unlink the live socket — that would break every subsequent
        // control call.
        //
        // macOS limits Unix socket paths to SUN_LEN (~104 bytes), and
        // the scoped state root nested under tempfile() blows past that.
        // We host the listener under /tmp directly with the shortest
        // possible path and only relocate the scope for the doctor scan.
        use std::os::unix::net::UnixListener;

        let short = std::path::PathBuf::from(format!("/tmp/aod-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&short);
        std::fs::create_dir_all(&short).expect("short tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(short.to_string_lossy().as_ref()));
        let project = short.join("p");
        std::fs::create_dir_all(&project).unwrap();
        let scoped = protocol::scoped_state_root(&project).unwrap();
        let daemon_dir = scoped.join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        std::fs::write(daemon_dir.join("daemon.pid"), "999997").unwrap();
        let sock_path = scoped.join("control.sock");
        let _listener = UnixListener::bind(&sock_path)
            .unwrap_or_else(|e| panic!("bind live unix socket at {}: {e}", sock_path.display()));

        let targets = collect_stale_artifacts_for_fix(&project);
        assert!(targets.iter().any(|p| p.ends_with("daemon.pid")));
        assert!(
            !targets.iter().any(|p| p.ends_with("control.sock")),
            "must NOT include the still-serving socket: {:?}",
            targets,
        );
        let _ = std::fs::remove_dir_all(&short);
    }

    #[test]
    fn unparseable_pid_only_targets_pid_file_not_socket() {
        // Repro: pid file was corrupted/truncated while the daemon is
        // still running. We must NOT remove the live control socket.
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project-unparseable");
        std::fs::create_dir_all(&project).unwrap();
        let scoped = protocol::scoped_state_root(&project).unwrap();
        let daemon_dir = scoped.join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        std::fs::write(daemon_dir.join("daemon.pid"), "not-a-pid").unwrap();
        std::fs::write(scoped.join("control.sock"), b"").unwrap();

        let targets = collect_stale_artifacts_for_fix(&project);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].ends_with("daemon.pid"));
        assert!(!targets.iter().any(|p| p.ends_with("control.sock")));
    }

    #[test]
    fn passes_when_no_pid_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project3");
        std::fs::create_dir_all(&project).unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: true };
        let checks = run(&ctx);
        assert_eq!(checks[0].status, CheckStatus::Pass);
    }
}
