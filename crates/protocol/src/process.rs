use anyhow::Result;
#[cfg(windows)]
use once_cell::sync::Lazy;
#[cfg(windows)]
use std::collections::HashMap;
#[cfg(unix)]
use std::process::Command;
#[cfg(windows)]
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
static JOB_HANDLES: Lazy<Mutex<HashMap<u32, windows::Win32::Foundation::HANDLE>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

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
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION};
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
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::JobObjects::TerminateJobObject;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

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
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

        unsafe {
            match OpenProcess(PROCESS_TERMINATE, false, pid) {
                Ok(handle) => {
                    let result = TerminateProcess(handle, 1);
                    let _ = CloseHandle(handle);
                    Ok(result.is_ok())
                }
                Err(e) => {
                    let err = anyhow::Error::new(e);
                    Err(err.context(format!("failed to terminate process {}", pid)))
                }
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        Ok(false)
    }
}
