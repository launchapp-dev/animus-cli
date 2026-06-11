//! Loop-level tests for the event-driven daemon scheduler.
//!
//! Verifies that:
//!
//! - a scheduler nudge wakes the loop for an immediate dispatch pass
//!   without waiting for the fallback heartbeat (`interval_secs` is set
//!   to 3600 so any heartbeat-driven pass would blow the test timeout);
//! - a burst of N rapid nudges coalesces into a bounded number of extra
//!   passes (at most two: one for the consumed wake, one for the single
//!   stored permit);
//! - the heavy housekeeping legs (probed via `reconcile_zombie_workflows`)
//!   run on heartbeat cadence only, not on every nudge-driven pass;
//! - pause (`runtime_paused`) still gates dispatch on event wakes.
//!
//! Harness pattern mirrors `tests/preflight_lifecycle.rs`: pinned HOME,
//! a static env lock, stub `DaemonRunHooks`, and a counting
//! `ProjectTickHooks` driver. The control server and trigger supervisor
//! are disabled — the nudge under test is delivered in-process via
//! `nudge_scheduler_local` (the same call the `daemon/nudge` control
//! method makes; the wire round-trip is covered by the control-server
//! test suite).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orchestrator_core::{DaemonStatus, PluginPreflightSpec};
use orchestrator_daemon_runtime::{
    nudge_scheduler_local, run_daemon, CompletedProcessReconciliation, DaemonRunEvent, DaemonRunHooks,
    DaemonRuntimeOptions, DaemonRuntimeState, DispatchWorkflowStartSummary, ProjectTickHooks, ProjectTickSnapshot,
    ProjectTickSummary, ProjectTickSummaryInput, TickBudget,
};
use protocol::test_utils::EnvVarGuard;
use serde_json::Value;
use tempfile::TempDir;

static HOME_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Whole-test deadline: far below the 3600s heartbeat, so completion
/// inside this window proves the passes were event-driven.
const TEST_DEADLINE: Duration = Duration::from_mins(1);

fn pin_test_home() -> TempDir {
    let home = TempDir::new().expect("tempdir home");
    std::env::set_var("HOME", home.path());
    home
}

struct StubHooks;

#[async_trait(?Send)]
impl DaemonRunHooks for StubHooks {
    fn handle_event(&mut self, _event: DaemonRunEvent) -> Result<()> {
        Ok(())
    }

    async fn daemon_status(&mut self, _project_root: &str) -> Result<DaemonStatus> {
        Ok(DaemonStatus::Stopped)
    }

    async fn start_daemon(&mut self, _project_root: &str) -> Result<()> {
        Ok(())
    }

    async fn stop_daemon(&mut self, _project_root: &str) -> Result<()> {
        Ok(())
    }

    async fn recover_startup_orphans(&mut self, _project_root: &str) -> Result<usize> {
        Ok(0)
    }

    fn plugin_preflight_spec(&self) -> PluginPreflightSpec {
        PluginPreflightSpec { required_roles: Vec::new(), auto_install: false, auto_install_defaults: Vec::new() }
    }
}

/// Per-tick probe counters shared between the driver and the test body.
#[derive(Default, Clone)]
struct WakeProbe {
    ticks: Arc<Mutex<usize>>,
    zombie_reconciliations: Arc<Mutex<usize>>,
    dispatch_calls: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl WakeProbe {
    fn ticks(&self) -> usize {
        *self.ticks.lock().unwrap()
    }

    fn zombie_reconciliations(&self) -> usize {
        *self.zombie_reconciliations.lock().unwrap()
    }

    fn dispatch_calls(&self) -> Vec<(usize, usize)> {
        self.dispatch_calls.lock().unwrap().clone()
    }

    /// Poll until at least `n` ticks have completed; panics after `limit`.
    async fn wait_for_ticks(&self, n: usize, limit: Duration) {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if self.ticks() >= n {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for tick {n}; only {} completed — the scheduler wake never fired",
                self.ticks()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

struct WakeProbeDriver {
    probe: WakeProbe,
}

#[async_trait(?Send)]
impl ProjectTickHooks for WakeProbeDriver {
    fn process_due_schedules(&mut self, _root: &str, _now: DateTime<Utc>, _budget: &mut TickBudget) {}

    async fn capture_snapshot(&mut self, _root: &str) -> Result<ProjectTickSnapshot> {
        Ok(ProjectTickSnapshot {
            requirements_before: Vec::new(),
            tasks_before: Vec::new(),
            started_daemon: false,
            daemon_health: None,
        })
    }

    async fn reconcile_completed_processes(&mut self, _root: &str) -> Result<CompletedProcessReconciliation> {
        Ok(CompletedProcessReconciliation::default())
    }

    // Housekeeping probe: only invoked when the tick runs with
    // `housekeeping: true` (heartbeat cadence).
    async fn reconcile_zombie_workflows(&mut self, _root: &str) -> Result<usize> {
        *self.probe.zombie_reconciliations.lock().unwrap() += 1;
        Ok(0)
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _root: &str,
        limit: usize,
        queue_drain_limit: usize,
    ) -> Result<DispatchWorkflowStartSummary> {
        self.probe.dispatch_calls.lock().unwrap().push((limit, queue_drain_limit));
        Ok(DispatchWorkflowStartSummary::default())
    }

    async fn collect_health(&mut self, _root: &str) -> Result<Value> {
        Ok(Value::Null)
    }

    async fn build_summary(
        &mut self,
        _args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary> {
        *self.probe.ticks.lock().unwrap() += 1;
        Ok(ProjectTickSummary {
            project_root: input.project_root,
            started_daemon: input.started_daemon,
            health: input.health,
            tasks_total: 0,
            tasks_ready: 0,
            tasks_in_progress: 0,
            tasks_blocked: 0,
            tasks_done: 0,
            stale_in_progress_count: 0,
            stale_in_progress_threshold_hours: 0,
            stale_in_progress_task_ids: Vec::new(),
            workflows_running: 0,
            workflows_completed: 0,
            workflows_failed: 0,
            resumed_workflows: 0,
            cleaned_stale_workflows: 0,
            reconciled_workflows: 0,
            started_ready_workflows: 0,
            executed_workflow_phases: 0,
            failed_workflow_phases: 0,
            task_state_changes: Vec::new(),
            phase_execution_events: Vec::new(),
            workflow_failures: Vec::new(),
        })
    }
}

fn wake_test_options() -> DaemonRuntimeOptions {
    DaemonRuntimeOptions {
        // The heartbeat is the latency bound under test: with 3600s any
        // pass observed within the test timeout MUST have been event-driven.
        interval_secs: 3600,
        startup_cleanup: false,
        skip_plugin_preflight: true,
        once: false,
        ..DaemonRuntimeOptions::default()
    }
}

/// Nudge → immediate dispatch pass; burst of nudges → bounded passes;
/// housekeeping legs stay on heartbeat cadence.
#[tokio::test]
async fn nudge_wakes_dispatch_pass_without_heartbeat_and_debounces_bursts() {
    let _env = HOME_ENV_LOCK.lock().await;
    let _home = pin_test_home();
    let _no_control = EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_CONTROL_SERVER", Some("1"));
    let _no_triggers = EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_TRIGGERS", Some("1"));
    let project = TempDir::new().expect("tempdir project");
    let project_root = project.path().to_string_lossy().to_string();

    let probe = WakeProbe::default();
    let mut hooks = StubHooks;
    let mut driver = WakeProbeDriver { probe: probe.clone() };
    let mut options = wake_test_options();

    let controller_probe = probe.clone();
    let controller_root = project_root.clone();
    let controller = async move {
        // Pass 1 is the startup tick (housekeeping).
        controller_probe.wait_for_ticks(1, Duration::from_secs(30)).await;
        assert_eq!(controller_probe.zombie_reconciliations(), 1, "first pass runs the housekeeping sweep");

        // Single nudge: the loop must run another pass long before the
        // 3600s heartbeat could.
        nudge_scheduler_local();
        controller_probe.wait_for_ticks(2, Duration::from_secs(10)).await;

        // Storm safety: a synchronous burst of nudges while the loop is
        // parked coalesces into at most two passes (one consumed wake +
        // one stored permit; tokio::sync::Notify stores a single permit).
        let before_burst = controller_probe.ticks();
        for _ in 0..10 {
            nudge_scheduler_local();
        }
        controller_probe.wait_for_ticks(before_burst + 1, Duration::from_secs(10)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let after_burst = controller_probe.ticks();
        assert!(
            after_burst <= before_burst + 2,
            "10 rapid nudges must coalesce into at most 2 extra passes, got {} extra",
            after_burst - before_burst
        );

        // Housekeeping debounce: none of the nudge-driven passes may have
        // re-run the heavy reconciliation legs (heartbeat is 3600s away).
        assert_eq!(
            controller_probe.zombie_reconciliations(),
            1,
            "housekeeping must run on heartbeat cadence only, not per nudge"
        );
        // Every pass still ran the dispatch leg.
        assert_eq!(controller_probe.dispatch_calls().len(), after_burst, "each pass must run dispatch");

        // Shutdown: set the flag, then nudge so the loop notices now.
        DaemonRuntimeState::set_shutdown_requested(&controller_root, true, None).expect("request shutdown");
        nudge_scheduler_local();
    };

    let run = run_daemon(&project_root, &mut options, &mut driver, &mut hooks, |_| 0);
    let (result, ()) = tokio::time::timeout(TEST_DEADLINE, async { tokio::join!(run, controller) })
        .await
        .expect("event-driven daemon must complete well before any heartbeat (test timed out)");
    assert!(result.is_ok(), "daemon must exit cleanly via shutdown_requested, got: {result:?}");
    assert!(probe.ticks() >= 3, "startup + nudge + shutdown passes expected, got {}", probe.ticks());
}

/// Housekeeping must NOT be starved by steady nudges: the heartbeat select
/// arm is recreated after every pass (so it may never complete under a
/// constant nudge stream), but `housekeeping_due` keys off an absolute
/// `last_housekeeping` timestamp — once a full heartbeat period has
/// elapsed, the next pass (nudge-driven or not) runs the housekeeping
/// sweep.
#[tokio::test]
async fn housekeeping_recurs_on_nudge_passes_when_heartbeat_never_fires() {
    let _env = HOME_ENV_LOCK.lock().await;
    let _home = pin_test_home();
    let _no_control = EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_CONTROL_SERVER", Some("1"));
    let _no_triggers = EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_TRIGGERS", Some("1"));
    let project = TempDir::new().expect("tempdir project");
    let project_root = project.path().to_string_lossy().to_string();

    let probe = WakeProbe::default();
    let mut hooks = StubHooks;
    let mut driver = WakeProbeDriver { probe: probe.clone() };
    // 1s heartbeat, but the controller nudges every 25ms — every heartbeat
    // sleep is recreated long before it completes, so all passes after the
    // first are nudge-driven.
    let mut options = DaemonRuntimeOptions {
        interval_secs: 1,
        startup_cleanup: false,
        skip_plugin_preflight: true,
        once: false,
        ..DaemonRuntimeOptions::default()
    };

    let controller_probe = probe.clone();
    let controller_root = project_root.clone();
    let controller = async move {
        controller_probe.wait_for_ticks(1, Duration::from_secs(30)).await;
        // Nudge steadily for ~2.5 heartbeat periods.
        for _ in 0..100 {
            nudge_scheduler_local();
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let sweeps = controller_probe.zombie_reconciliations();
        let ticks = controller_probe.ticks();
        assert!(
            sweeps >= 2,
            "housekeeping must recur (~once per 1s heartbeat period) even when every wake is a nudge; got {sweeps} sweeps across {ticks} ticks"
        );
        assert!(
            sweeps < ticks,
            "housekeeping must not run on every nudge pass; got {sweeps} sweeps across {ticks} ticks"
        );

        DaemonRuntimeState::set_shutdown_requested(&controller_root, true, None).expect("request shutdown");
        nudge_scheduler_local();
    };

    let run = run_daemon(&project_root, &mut options, &mut driver, &mut hooks, |_| 0);
    let (result, ()) =
        tokio::time::timeout(TEST_DEADLINE, async { tokio::join!(run, controller) }).await.expect("test timed out");
    assert!(result.is_ok(), "daemon must exit cleanly, got: {result:?}");
}

/// Pause gating on event wakes: a nudge while `runtime_paused` is set
/// still wakes the loop, but the resulting tick must not dispatch
/// (limits are zeroed by the draining plan).
#[tokio::test]
async fn nudge_during_pause_does_not_dispatch() {
    let _env = HOME_ENV_LOCK.lock().await;
    let _home = pin_test_home();
    let _no_control = EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_CONTROL_SERVER", Some("1"));
    let _no_triggers = EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_TRIGGERS", Some("1"));
    let project = TempDir::new().expect("tempdir project");
    let project_root = project.path().to_string_lossy().to_string();

    let probe = WakeProbe::default();
    let mut hooks = StubHooks;
    let mut driver = WakeProbeDriver { probe: probe.clone() };
    let mut options = wake_test_options();

    let controller_probe = probe.clone();
    let controller_root = project_root.clone();
    let controller = async move {
        controller_probe.wait_for_ticks(1, Duration::from_secs(30)).await;

        // Pause, then nudge: the wake must happen (tick count advances)
        // but dispatch must be fully gated.
        DaemonRuntimeState::set_runtime_paused(&controller_root, true).expect("pause");
        let before = controller_probe.ticks();
        nudge_scheduler_local();
        controller_probe.wait_for_ticks(before + 1, Duration::from_secs(10)).await;
        let calls = controller_probe.dispatch_calls();
        assert!(
            calls.len() < controller_probe.ticks(),
            "paused event-wake tick must not reach the dispatch leg: {calls:?}"
        );

        // Resume + nudge: dispatch flows again.
        DaemonRuntimeState::set_runtime_paused(&controller_root, false).expect("resume");
        let before_resume = controller_probe.ticks();
        let calls_before_resume = controller_probe.dispatch_calls().len();
        nudge_scheduler_local();
        controller_probe.wait_for_ticks(before_resume + 1, Duration::from_secs(10)).await;
        assert!(
            controller_probe.dispatch_calls().len() > calls_before_resume,
            "post-resume event wake must dispatch again"
        );

        DaemonRuntimeState::set_shutdown_requested(&controller_root, true, None).expect("request shutdown");
        nudge_scheduler_local();
    };

    let run = run_daemon(&project_root, &mut options, &mut driver, &mut hooks, |_| 0);
    let (result, ()) =
        tokio::time::timeout(TEST_DEADLINE, async { tokio::join!(run, controller) }).await.expect("test timed out");
    assert!(result.is_ok(), "daemon must exit cleanly, got: {result:?}");
}
