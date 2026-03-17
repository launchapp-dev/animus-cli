#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
#[cfg(any(unix, windows))]
use std::process::Command;
#[cfg(any(unix, windows))]
use std::time::Duration;

#[cfg(windows)]
pub fn untrack_job(_pid: u32) {}

#[cfg(windows)]
fn windows_process_exists(pid: i32) -> bool {
    let pid_filter = format!("PID eq {pid}");
    Command::new("tasklist")
        .args(["/FI", pid_filter.as_str(), "/FO", "CSV", "/NH"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).lines().map(str::trim).any(|line| line.starts_with('"')))
        .unwrap_or(false)
}

#[cfg(windows)]
fn taskkill_process_tree(pid: u32, force: bool) -> Result<()> {
    let pid_arg = pid.to_string();
    let mut command = Command::new("taskkill");
    command.args(["/PID", pid_arg.as_str(), "/T"]);
    if force {
        command.arg("/F");
    }

    let output = command.output().with_context(|| format!("failed to launch taskkill for process {pid}"))?;

    if output.status.success() || !is_process_alive(pid) {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr.trim();
    let detail = if detail.is_empty() { stdout.trim() } else { detail };

    if detail.is_empty() {
        Err(anyhow::anyhow!("taskkill failed for process {pid}"))
    } else {
        Err(anyhow::anyhow!("taskkill failed for process {pid}: {detail}"))
    }
}

pub fn process_exists(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        if kill(Pid::from_raw(pid), None).is_ok() {
            !is_zombie_process(pid)
        } else {
            false
        }
    }

    #[cfg(windows)]
    {
        windows_process_exists(pid)
    }

    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    process_exists(pid as i32)
}

#[cfg(unix)]
fn is_zombie_process(pid: i32) -> bool {
    let output = Command::new("ps").args(["-o", "state=", "-p", &pid.to_string()]).output();
    match output {
        Ok(out) => {
            let state = String::from_utf8_lossy(&out.stdout);
            state.trim().starts_with('Z')
        }
        Err(_) => false,
    }
}

pub fn graceful_kill_process(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let _ = kill(Pid::from_raw(-pid), Signal::SIGTERM);
        let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);

        for _ in 0..50 {
            if !process_exists(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);

        std::thread::sleep(Duration::from_millis(100));
        !process_exists(pid)
    }

    #[cfg(not(unix))]
    {
        kill_process(pid)
    }
}

pub fn kill_process(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        // Try killing the process group first if it might be one
        // Note: some callers expect this to kill the process group (like agent-runner cleanup)
        if kill(Pid::from_raw(-pid), Signal::SIGKILL).is_ok() {
            return true;
        }

        kill(Pid::from_raw(pid), Signal::SIGKILL).is_ok()
    }

    #[cfg(windows)]
    {
        taskkill_process_tree(pid as u32, true).is_ok()
    }

    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

pub fn terminate_process(pid: u32) -> Result<bool> {
    if pid == 0 || !is_process_alive(pid) {
        return Ok(false);
    }

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);

        for _ in 0..20 {
            if !is_process_alive(pid) {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        Ok(!is_process_alive(pid))
    }

    #[cfg(windows)]
    {
        let _ = taskkill_process_tree(pid, false);

        for _ in 0..20 {
            if !is_process_alive(pid) {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        taskkill_process_tree(pid, true)?;
        Ok(!is_process_alive(pid))
    }

    #[cfg(not(any(unix, windows)))]
    {
        Ok(false)
    }
}
