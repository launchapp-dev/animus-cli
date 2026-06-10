//! Verifies that a `run_daemon` invocation whose preflight check fails on a
//! project whose daemon was not previously running does NOT leave behind a
//! `DaemonStatus::Running` record. Regression test for the codex round-3 P1
//! finding (run_daemon.rs:115-145): preflight aborts must short-circuit
//! BEFORE the daemon is marked running, so subsequent status commands report
//! "stopped" rather than a phantom "running" daemon.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orchestrator_core::{DaemonStatus, PluginPreflightSpec, PreflightResult, RequiredRole};
use orchestrator_daemon_runtime::{
    run_daemon, DaemonRunEvent, DaemonRunHooks, DaemonRuntimeOptions, DaemonRuntimeState, DispatchWorkflowStartSummary,
    PreflightOutcome, ProjectTickHooks, ProjectTickSnapshot, ProjectTickSummary, ProjectTickSummaryInput, TickBudget,
};
use serde_json::Value;
use tempfile::TempDir;

static HOME_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default, Clone)]
struct LifecycleCounts {
    start_daemon_calls: Arc<Mutex<usize>>,
    stop_daemon_calls: Arc<Mutex<usize>>,
    current_status: Arc<Mutex<DaemonStatus>>,
}

impl LifecycleCounts {
    fn new() -> Self {
        Self {
            start_daemon_calls: Arc::new(Mutex::new(0)),
            stop_daemon_calls: Arc::new(Mutex::new(0)),
            current_status: Arc::new(Mutex::new(DaemonStatus::Stopped)),
        }
    }

    fn start_calls(&self) -> usize {
        *self.start_daemon_calls.lock().unwrap()
    }

    fn stop_calls(&self) -> usize {
        *self.stop_daemon_calls.lock().unwrap()
    }

    fn status(&self) -> DaemonStatus {
        self.current_status.lock().unwrap().clone()
    }
}

struct StubHooks {
    counts: LifecycleCounts,
    spec: PluginPreflightSpec,
}

#[async_trait(?Send)]
impl DaemonRunHooks for StubHooks {
    fn handle_event(&mut self, _event: DaemonRunEvent) -> Result<()> {
        Ok(())
    }

    async fn daemon_status(&mut self, _project_root: &str) -> Result<DaemonStatus> {
        Ok(self.counts.status())
    }

    async fn start_daemon(&mut self, _project_root: &str) -> Result<()> {
        *self.counts.start_daemon_calls.lock().unwrap() += 1;
        *self.counts.current_status.lock().unwrap() = DaemonStatus::Running;
        Ok(())
    }

    async fn stop_daemon(&mut self, _project_root: &str) -> Result<()> {
        *self.counts.stop_daemon_calls.lock().unwrap() += 1;
        *self.counts.current_status.lock().unwrap() = DaemonStatus::Stopped;
        Ok(())
    }

    async fn recover_startup_orphans(&mut self, _project_root: &str) -> Result<usize> {
        Ok(0)
    }

    fn plugin_preflight_spec(&self) -> PluginPreflightSpec {
        self.spec.clone()
    }
}

struct StubDriver;

#[async_trait(?Send)]
impl ProjectTickHooks for StubDriver {
    fn process_due_schedules(&mut self, _root: &str, _now: DateTime<Utc>, _budget: &mut TickBudget) {}

    async fn capture_snapshot(&mut self, _root: &str) -> Result<ProjectTickSnapshot> {
        Ok(ProjectTickSnapshot {
            requirements_before: Vec::new(),
            tasks_before: Vec::new(),
            started_daemon: false,
            daemon_health: None,
        })
    }

    async fn reconcile_completed_processes(&mut self, _root: &str) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _root: &str,
        _limit: usize,
        _queue_drain_limit: usize,
    ) -> Result<DispatchWorkflowStartSummary> {
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
        })
    }
}

fn pin_test_home() -> TempDir {
    let home = TempDir::new().expect("tempdir home");
    std::env::set_var("HOME", home.path());
    home
}

#[tokio::test]
async fn daemon_start_with_preflight_failure_does_not_leave_running_state() {
    let _env = HOME_ENV_LOCK.lock().await;
    let _home = pin_test_home();
    let project = TempDir::new().expect("tempdir project");
    let project_root = project.path().to_string_lossy().to_string();

    // Force preflight to fail by requiring a subject kind that no installed
    // plugin can satisfy. Auto-install is off (the spec carries no fix repo
    // for the unknown kind) so the runner reports missing and aborts.
    let counts = LifecycleCounts::new();
    let mut hooks = StubHooks {
        counts: counts.clone(),
        spec: PluginPreflightSpec {
            required_roles: vec![RequiredRole::SubjectKind("nonexistent-kind".to_string())],
            auto_install: false,
            auto_install_defaults: Vec::new(),
        },
    };
    let mut driver = StubDriver;
    let mut options = DaemonRuntimeOptions { once: true, ..DaemonRuntimeOptions::default() };

    let result = run_daemon(&project_root, &mut options, &mut driver, &mut hooks, |_| 0).await;
    assert!(result.is_err(), "preflight failure must propagate as an error");
    let message = result.unwrap_err().to_string();
    assert!(message.contains("preflight failed"), "expected preflight failure message, got: {message}");

    assert_eq!(
        counts.start_calls(),
        0,
        "start_daemon must NOT be invoked when preflight aborts (otherwise daemon status leaks `running` after a failed boot)"
    );
    assert_eq!(counts.stop_calls(), 0, "stop_daemon must not be needed when start_daemon was never called");
    assert!(
        matches!(counts.status(), DaemonStatus::Stopped),
        "persisted daemon status must remain stopped after a preflight abort"
    );
}

struct RecordingHooks {
    counts: LifecycleCounts,
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait(?Send)]
impl DaemonRunHooks for RecordingHooks {
    fn handle_event(&mut self, event: DaemonRunEvent) -> Result<()> {
        let rendered = format!("{event:?}");
        let name = rendered.split([' ', '{']).next().unwrap_or_default().to_string();
        self.events.lock().unwrap().push(name);
        Ok(())
    }

    async fn daemon_status(&mut self, _project_root: &str) -> Result<DaemonStatus> {
        Ok(self.counts.status())
    }

    async fn start_daemon(&mut self, _project_root: &str) -> Result<()> {
        *self.counts.start_daemon_calls.lock().unwrap() += 1;
        *self.counts.current_status.lock().unwrap() = DaemonStatus::Running;
        Ok(())
    }

    async fn stop_daemon(&mut self, _project_root: &str) -> Result<()> {
        *self.counts.stop_daemon_calls.lock().unwrap() += 1;
        *self.counts.current_status.lock().unwrap() = DaemonStatus::Stopped;
        Ok(())
    }

    async fn recover_startup_orphans(&mut self, _project_root: &str) -> Result<usize> {
        Ok(0)
    }

    fn plugin_preflight_spec(&self) -> PluginPreflightSpec {
        PluginPreflightSpec { required_roles: Vec::new(), auto_install: false, auto_install_defaults: Vec::new() }
    }
}

/// Regression guard for the orphan gap-replay ordering bug: orphan agent
/// reattach (and the decision-log gap replay it performs) must run AFTER
/// the control server is started. Gap replay permanently advances the
/// spawn record's `last_consumed_offset`, so replaying before the
/// `workflow/events` subscription surface exists fans the events out to
/// zero possible subscribers and marks them consumed forever.
///
/// Unix-gated: the orphan scan and the control-server socket are both
/// unavailable on non-Unix targets, where reattach is intentionally
/// skipped.
#[cfg(unix)]
#[tokio::test]
async fn control_server_resolves_before_orphan_agent_reattach() {
    let _env = HOME_ENV_LOCK.lock().await;
    let _home = pin_test_home();
    // The reattach pass is (correctly) skipped when the control server is
    // disabled — make sure an ambient disable env cannot turn this test
    // into a false failure.
    let _control_env = protocol::test_utils::EnvVarGuard::set("ANIMUS_DAEMON_DISABLE_CONTROL_SERVER", None);
    let project = TempDir::new().expect("tempdir project");
    let project_root = project.path().to_string_lossy().to_string();

    // Plant a live-orphan spawn record: our own PID is alive, and an empty
    // command_line skips the process-identity check, so the startup scan
    // reports it as detected and the reattach pass runs.
    let scope = protocol::scoped_state_root(project.path()).expect("scoped state root");
    let agents_dir = scope.join("runs").join("_pending").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let record = serde_json::json!({
        "agent_session_id": "agent-ordering",
        "pid": std::process::id(),
        "started_at": "2026-06-10T00:00:00Z",
        "subject_id": "TASK-ORDER",
        "subject_kind": "task",
        "workflow_ref": "standard",
        "task_id": "TASK-ORDER",
        "command_line": [],
        "stdio_socket_path": null,
    });
    std::fs::write(agents_dir.join("agent-ordering.json"), serde_json::to_vec_pretty(&record).expect("serialize"))
        .expect("write spawn record");

    let events = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = RecordingHooks { counts: LifecycleCounts::new(), events: events.clone() };
    let mut driver = StubDriver;
    let mut options = DaemonRuntimeOptions {
        once: true,
        startup_cleanup: true,
        skip_plugin_preflight: true,
        ..DaemonRuntimeOptions::default()
    };

    let result = run_daemon(&project_root, &mut options, &mut driver, &mut hooks, |_| 0).await;
    assert!(result.is_ok(), "daemon must run one tick and exit cleanly, got: {result:?}");

    let events = events.lock().unwrap();
    let control_idx = events
        .iter()
        .position(|name| name == "ControlServerResolved")
        .unwrap_or_else(|| panic!("ControlServerResolved must be emitted during startup, got: {events:?}"));
    let reattach_idx = events
        .iter()
        .position(|name| name.starts_with("OrphanAgentReattach") || name.starts_with("OrphanAgentGapReplay"))
        .unwrap_or_else(|| panic!("orphan reattach must be attempted for the planted record, got: {events:?}"));
    assert!(
        control_idx < reattach_idx,
        "orphan reattach/gap-replay (index {reattach_idx}) ran before the control server start \
         (index {control_idx}); replayed events would broadcast to zero subscribers and the consumed \
         offset would still advance permanently. events: {events:?}"
    );
}

struct PauseProbeDriver {
    project_root: String,
    ticks: Arc<Mutex<usize>>,
}

#[async_trait(?Send)]
impl ProjectTickHooks for PauseProbeDriver {
    fn process_due_schedules(&mut self, _root: &str, _now: DateTime<Utc>, _budget: &mut TickBudget) {}

    async fn capture_snapshot(&mut self, _root: &str) -> Result<ProjectTickSnapshot> {
        Ok(ProjectTickSnapshot {
            requirements_before: Vec::new(),
            tasks_before: Vec::new(),
            started_daemon: false,
            daemon_health: None,
        })
    }

    async fn reconcile_completed_processes(&mut self, _root: &str) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _root: &str,
        _limit: usize,
        _queue_drain_limit: usize,
    ) -> Result<DispatchWorkflowStartSummary> {
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
        let tick = {
            let mut guard = self.ticks.lock().unwrap();
            *guard += 1;
            *guard
        };
        if tick == 1 {
            DaemonRuntimeState::set_runtime_paused(&self.project_root, true)?;
        }
        if tick >= 3 {
            DaemonRuntimeState::set_shutdown_requested(&self.project_root, true, None)?;
        }
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
        })
    }
}

/// Regression guard for the pause-terminates-the-daemon bug: an external
/// `animus daemon pause` (runtime_paused=true) must NOT exit the main
/// loop. The daemon keeps ticking in a paused/draining state until a
/// shutdown is actually requested, so a later `animus daemon resume` can
/// revive dispatch without restarting the process.
#[tokio::test]
async fn external_pause_does_not_terminate_daemon_loop() {
    let _env = HOME_ENV_LOCK.lock().await;
    let _home = pin_test_home();
    let project = TempDir::new().expect("tempdir project");
    let project_root = project.path().to_string_lossy().to_string();

    let counts = LifecycleCounts::new();
    let mut hooks = StubHooks {
        counts: counts.clone(),
        spec: PluginPreflightSpec {
            required_roles: Vec::new(),
            auto_install: false,
            auto_install_defaults: Vec::new(),
        },
    };
    let ticks = Arc::new(Mutex::new(0usize));
    let mut driver = PauseProbeDriver { project_root: project_root.clone(), ticks: ticks.clone() };
    let mut options = DaemonRuntimeOptions {
        interval_secs: 1,
        startup_cleanup: false,
        skip_plugin_preflight: true,
        ..DaemonRuntimeOptions::default()
    };

    let result = run_daemon(&project_root, &mut options, &mut driver, &mut hooks, |_| 0).await;
    assert!(result.is_ok(), "daemon must exit cleanly via shutdown_requested, got: {result:?}");

    let total_ticks = *ticks.lock().unwrap();
    assert_eq!(
        total_ticks, 3,
        "daemon must keep ticking through an external pause (tick 1 pauses, tick 3 requests shutdown); \
         exiting earlier means pause terminated the loop"
    );
    assert!(
        DaemonRuntimeState::is_runtime_paused(&project_root).unwrap_or(false),
        "the externally-set pause flag must survive daemon exit (only `daemon resume` / a fresh start clears it)"
    );
}

/// Regression guard for the user-reported P2: when plugin discovery
/// itself fails (registry permission denied, manifest parse error, etc),
/// the abort message must surface the *actual* error -- not the generic
/// "install plugins" advice that masked the real problem.
///
/// Previously `plugin_preflight_wiring.rs:46` called
/// `discover_installed_plugins(...).unwrap_or_default()`, collapsing every
/// failure into "no plugins installed" and steering operators toward
/// install hints for plugins they may already have. The fix:
///   - On Err, set `PreflightOutcome.discovery_error = Some(error)`
///   - `should_abort_startup()` returns true
///   - `render_abort_message()` reports the specific I/O error and points
///     the operator at the plugins dir, not at `plugin install`.
#[test]
fn discovery_io_error_surfaces_specific_message_not_install_hint() {
    let outcome = PreflightOutcome {
        result: PreflightResult::default(),
        skipped: false,
        auto_install: false,
        discovery_error: Some("permission denied: ~/.animus/plugins/manifest.json".to_string()),
    };

    assert!(
        outcome.should_abort_startup(),
        "a discovery error must abort startup -- otherwise the daemon comes up unable to dispatch any plugin-backed work"
    );

    let message = outcome.render_abort_message();
    assert!(
        message.contains("could not read installed plugins"),
        "abort message must name the actual failure mode (discovery, not missing plugins). got: {message}"
    );
    assert!(
        message.contains("permission denied: ~/.animus/plugins/manifest.json"),
        "abort message must include the underlying error so operators can act. got: {message}"
    );
    assert!(
        message.contains("animus plugin list") || message.contains("~/.animus/plugins/"),
        "abort message must point operators at the diagnostic surface (plugin list or plugins dir). got: {message}"
    );
    assert!(
        !message.contains("Re-run with `--auto-install`"),
        "abort message must NOT recommend install advice -- that was the bug. got: {message}"
    );
    assert!(
        !message.contains("the daemon requires plugins that are not installed"),
        "abort message must NOT use the missing-plugins template -- that mislabels the failure. got: {message}"
    );

    // And the inverse: a normal missing-plugins outcome must still use
    // the existing install-advice template (i.e. we did not regress the
    // happy path).
    let missing_outcome = PreflightOutcome {
        result: PreflightResult::default(),
        skipped: false,
        auto_install: false,
        discovery_error: None,
    };
    assert!(!missing_outcome.should_abort_startup(), "an empty PreflightResult with no missing plugins is healthy");
}
