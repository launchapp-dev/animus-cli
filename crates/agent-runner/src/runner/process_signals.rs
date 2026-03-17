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
pub(super) fn setup_windows_job_object(_pid: u32) {
    // Windows cleanup uses `taskkill /T` in protocol::process, so no raw job
    // object handles are needed in the release binary set.
}
