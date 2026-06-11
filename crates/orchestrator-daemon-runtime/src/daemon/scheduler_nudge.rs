//! Process-global scheduler nudge for the event-driven daemon loop.
//!
//! The daemon main loop in [`super::run_daemon`] parks on a
//! `tokio::select!` whose arms include a [`tokio::sync::Notify`] installed
//! here. Anything running inside the daemon process (control-socket
//! `daemon/nudge` handler, workflow-config hot-reload, completion
//! forwarders) can call [`nudge_scheduler_local`] to wake the loop for an
//! immediate dispatch pass instead of waiting for the fallback heartbeat.
//!
//! Semantics are best-effort and coalescing:
//!
//! - `Notify::notify_one` stores at most ONE permit when no waiter is
//!   parked, so a burst of N nudges while a tick is in flight collapses
//!   into at most one extra pass (storm safety).
//! - When no daemon loop is running in this process the slot is empty and
//!   the nudge silently no-ops.
//!
//! Lifecycle mirrors the workflow-event emitter slots in
//! [`super::run_daemon`]: installed right before the steady-state loop
//! starts, cleared on daemon teardown.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;

use tokio::sync::Notify;

static SCHEDULER_NUDGE: OnceLock<RwLock<Option<Arc<Notify>>>> = OnceLock::new();

fn nudge_slot() -> &'static RwLock<Option<Arc<Notify>>> {
    SCHEDULER_NUDGE.get_or_init(|| RwLock::new(None))
}

/// Install the daemon loop's wake handle. Called by `run_daemon` before
/// entering the steady-state loop.
pub(crate) fn install_scheduler_nudge(notify: Arc<Notify>) {
    if let Ok(mut guard) = nudge_slot().write() {
        *guard = Some(notify);
    }
}

/// Clear the wake handle on daemon teardown so late nudges no-op.
pub(crate) fn clear_scheduler_nudge() {
    if let Ok(mut guard) = nudge_slot().write() {
        *guard = None;
    }
}

/// Returns the currently installed wake handle, if a daemon loop is
/// running in this process.
pub fn current_scheduler_nudge() -> Option<Arc<Notify>> {
    nudge_slot().read().ok().and_then(|guard| guard.clone())
}

/// Best-effort, in-process scheduler wake. No-ops when no daemon loop is
/// running in this process. Never blocks and never fails.
pub fn nudge_scheduler_local() {
    if let Some(notify) = current_scheduler_nudge() {
        notify.notify_one();
    }
}

/// Test-only lock serializing in-crate unit tests that install/clear the
/// process-global nudge slot, so parallel test threads cannot swap each
/// other's handles mid-assertion.
#[cfg(test)]
pub(crate) fn nudge_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn nudge_is_noop_without_installed_handle() {
        let _lock = nudge_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        clear_scheduler_nudge();
        // Must not panic or block.
        nudge_scheduler_local();
        assert!(current_scheduler_nudge().is_none());
    }

    #[tokio::test]
    async fn nudge_targets_installed_handle() {
        let _lock = nudge_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        let notify = Arc::new(Notify::new());
        install_scheduler_nudge(notify.clone());

        nudge_scheduler_local();
        // The permit must land on the installed handle.
        tokio::time::timeout(Duration::from_secs(1), notify.notified())
            .await
            .expect("stored permit should wake immediately");

        clear_scheduler_nudge();
    }

    /// Coalescing is a property of `tokio::sync::Notify::notify_one`
    /// (which `nudge_scheduler_local` delegates to): N notifications with
    /// no parked waiter store exactly ONE permit. Verified on a local
    /// handle so concurrent tests that legitimately nudge the global slot
    /// (e.g. hot-reload watcher tests) cannot inject spurious permits.
    #[tokio::test]
    async fn notify_one_coalesces_bursts_into_single_permit() {
        let notify = Notify::new();
        for _ in 0..16 {
            notify.notify_one();
        }
        // First wait consumes the stored permit immediately.
        tokio::time::timeout(Duration::from_secs(1), notify.notified())
            .await
            .expect("stored permit should wake immediately");
        // Second wait must NOT observe a second permit from the burst.
        let second = tokio::time::timeout(Duration::from_millis(50), notify.notified()).await;
        assert!(second.is_err(), "burst of notifications must coalesce into a single stored permit");
    }
}
