use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use tracing::{debug, info, warn};

#[cfg(windows)]
use once_cell::sync::Lazy;
#[cfg(windows)]
use std::sync::Mutex;

#[cfg(windows)]
static JOB_HANDLES: Lazy<Mutex<HashMap<u32, windows::Win32::Foundation::HANDLE>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn cleanup_orphaned_clis() -> Result<()> {
    let tracker_path = protocol::cli_tracker_path();

    if !tracker_path.exists() {
        debug!(path = %tracker_path.display(), "No orphan tracker file found");
        return Ok(());
    }

    let content = fs::read_to_string(&tracker_path)?;
    let tracked: HashMap<String, u32> = serde_json::from_str(&content).unwrap_or_default();
    info!(
        tracked_count = tracked.len(),
        tracker_path = %tracker_path.display(),
        "Loaded tracked CLI processes for orphan cleanup"
    );

    let mut cleaned = 0;
    for (run_id, pid) in tracked {
        if !process_exists(pid as i32) {
            info!(run_id, pid, "Tracked process is already terminated");
            continue;
        }

        info!(run_id, pid, "Killing orphaned tracked process");
        if kill_process(pid as i32) {
            cleaned += 1;
        } else {
            warn!(run_id, pid, "Failed to kill orphaned process");
        }
    }

    fs::remove_file(&tracker_path)?;
    info!(
        cleaned_count = cleaned,
        tracker_path = %tracker_path.display(),
        "Finished orphaned process cleanup"
    );
    Ok(())
}

pub fn track_process(run_id: &str, pid: u32) -> Result<()> {
    let tracker_path = protocol::cli_tracker_path();
    let mut tracked: HashMap<String, u32> = if tracker_path.exists() {
        let content = fs::read_to_string(&tracker_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        HashMap::new()
    };

    tracked.insert(run_id.to_string(), pid);
    fs::write(&tracker_path, serde_json::to_string(&tracked)?)?;
    debug!(
        run_id,
        pid,
        tracked_count = tracked.len(),
        tracker_path = %tracker_path.display(),
        "Tracked CLI process"
    );
    Ok(())
}

pub fn untrack_process(run_id: &str) -> Result<()> {
    let tracker_path = protocol::cli_tracker_path();
    if !tracker_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&tracker_path)?;
    let mut tracked: HashMap<String, u32> = serde_json::from_str(&content).unwrap_or_default();
    let removed = tracked.remove(run_id).is_some();
    fs::write(&tracker_path, serde_json::to_string(&tracked)?)?;
    debug!(
        run_id,
        removed,
        remaining = tracked.len(),
        tracker_path = %tracker_path.display(),
        "Untracked CLI process"
    );
    Ok(())
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    if pid <= 0 {
        return false;
    }

    kill(Pid::from_raw(pid), None).is_ok()
}

#[cfg(windows)]
fn process_exists(pid: i32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION};

    if pid <= 0 {
        return false;
    }

    unsafe {
        match OpenProcess(PROCESS_QUERY_INFORMATION, false, pid as u32) {
            Ok(handle) => {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(unix)]
pub fn kill_process(pid: i32) -> bool {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    if pid <= 0 {
        return false;
    }

    kill(Pid::from_raw(-pid), Signal::SIGKILL).is_ok()
}

#[cfg(windows)]
pub fn kill_process(pid: i32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::TerminateJobObject;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    if pid <= 0 {
        return false;
    }

    let mut handles = JOB_HANDLES.lock().unwrap();
    if let Some(job_handle) = handles.remove(&(pid as u32)) {
        unsafe {
            let result = TerminateJobObject(job_handle, 1);
            let _ = CloseHandle(job_handle);
            return result.is_ok();
        }
    }

    unsafe {
        match OpenProcess(PROCESS_TERMINATE, false, pid as u32) {
            Ok(handle) => {
                let result = TerminateProcess(handle, 1);
                let _ = CloseHandle(handle);
                result.is_ok()
            }
            Err(_) => false,
        }
    }
}

#[cfg(windows)]
pub fn track_job(pid: u32, job_handle: windows::Win32::Foundation::HANDLE) {
    let mut handles = JOB_HANDLES.lock().unwrap();
    handles.insert(pid, job_handle);
}

#[cfg(windows)]
pub fn untrack_job(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;

    let mut handles = JOB_HANDLES.lock().unwrap();
    if let Some(job_handle) = handles.remove(&pid) {
        unsafe {
            let _ = CloseHandle(job_handle);
        }
    }
}
