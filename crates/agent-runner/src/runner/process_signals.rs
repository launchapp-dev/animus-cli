use protocol::RunId;
use tracing::warn;

use crate::cleanup::{graceful_kill_process, untrack_process};

pub(super) fn terminate_and_untrack(run_id: &RunId, pid: u32, context: &str) {
    let killed = graceful_kill_process(pid as i32);
    if !killed {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            "Failed to terminate {context} process"
        );
    }
    if let Err(e) = untrack_process(&run_id.0) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to remove process from orphan tracker after {context}"
        );
    }
    #[cfg(windows)]
    crate::cleanup::untrack_job(pid);
}

pub(super) fn untrack_after_error(run_id: &RunId, pid: u32) {
    if let Err(e) = untrack_process(&run_id.0) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to remove process from orphan tracker after execution error"
        );
    }
}

pub(super) fn untrack_after_completion(run_id: &RunId, pid: u32) {
    if let Err(e) = untrack_process(&run_id.0) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to remove process from orphan tracker after completion"
        );
    }
    #[cfg(windows)]
    crate::cleanup::untrack_job(pid);
}

#[cfg(windows)]
pub(super) fn setup_windows_job_object(pid: u32) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Threading::OpenProcess;

    unsafe {
        if let Ok(job) = CreateJobObjectW(None, None) {
            if let Ok(process_handle) = OpenProcess(
                windows::Win32::System::Threading::PROCESS_SET_QUOTA
                    | windows::Win32::System::Threading::PROCESS_TERMINATE,
                false,
                pid,
            ) {
                if AssignProcessToJobObject(job, process_handle).is_ok() {
                    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                    if SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        &info as *const _ as *const _,
                        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                    .is_ok()
                    {
                        crate::cleanup::track_job(pid, job);
                    } else {
                        let _ = CloseHandle(job);
                    }
                } else {
                    let _ = CloseHandle(job);
                }
                let _ = CloseHandle(process_handle);
            } else {
                let _ = CloseHandle(job);
            }
        }
    }
}
