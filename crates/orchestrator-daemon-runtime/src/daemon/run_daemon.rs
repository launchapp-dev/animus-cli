use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::Duration;

use animus_runtime_shared::workflow_event_emitter::SharedWorkflowEventEmitter;
use anyhow::Context;
use anyhow::Result;
use orchestrator_core::DaemonStatus;
use tokio::time::sleep;

use crate::control::{BroadcastWorkflowEventEmitter, WorkflowEventBroadcaster};
use crate::run_plugin_preflight;
use crate::run_project_tick;
use crate::DaemonRunEvent;
use crate::DaemonRunGuard;
use crate::DaemonRunHooks;
use crate::DaemonRuntimeOptions;
use crate::DaemonRuntimeState;
use crate::DiscoveredPluginSummary;
use crate::ProjectTickHooks;
use crate::ProjectTickRunMode;
use crate::TriggerSupervisor;
use crate::TriggerSupervisorEvent;
use crate::TriggerSupervisorSink;

/// Process-global holder for the daemon's broadcast-backed
/// [`SharedWorkflowEventEmitter`]. Installed by [`run_daemon`] at startup
/// (after [`WorkflowEventBroadcaster`] construction) and consumed by any
/// in-process call site that builds [`animus_runtime_shared::WorkflowExecuteParams`]
/// inside the daemon process.
///
/// SUBPROCESS GAP: workflow runs launched via `animus-workflow-runner` (the
/// scheduler's normal path) live in a separate process and cannot see this
/// holder. They emit no workflow_events. A subprocess back-channel
/// (per-run pipe / event log tail) is required for full coverage and is
/// scheduled for v0.5.
static DAEMON_WORKFLOW_EVENT_EMITTER: OnceLock<RwLock<Option<SharedWorkflowEventEmitter>>> = OnceLock::new();

/// Process-global slot for the daemon's [`WorkflowEventBroadcaster`]. This
/// is the *broadcaster* itself (not the trait-obj emitter), exposed so the
/// scheduler's subprocess spawn path can attach a per-run back-channel
/// reader without needing to downcast through
/// [`SharedWorkflowEventEmitter`]. Lifecycle: installed alongside the
/// emitter in [`run_daemon`]; cleared in
/// [`clear_workflow_event_emitter`].
static DAEMON_WORKFLOW_EVENT_BROADCASTER: OnceLock<RwLock<Option<Arc<WorkflowEventBroadcaster>>>> = OnceLock::new();

fn emitter_slot() -> &'static RwLock<Option<SharedWorkflowEventEmitter>> {
    DAEMON_WORKFLOW_EVENT_EMITTER.get_or_init(|| RwLock::new(None))
}

fn broadcaster_slot() -> &'static RwLock<Option<Arc<WorkflowEventBroadcaster>>> {
    DAEMON_WORKFLOW_EVENT_BROADCASTER.get_or_init(|| RwLock::new(None))
}

/// Returns the daemon's `WorkflowEventBroadcaster` when one has been
/// installed by [`run_daemon`]. Subprocess-dispatch callers use this to
/// attach a per-run pipe reader that forwards subprocess workflow_events
/// into the broadcaster. Returns `None` from CLI / one-shot processes
/// that never started a daemon.
pub fn current_workflow_event_broadcaster() -> Option<Arc<WorkflowEventBroadcaster>> {
    broadcaster_slot().read().ok().and_then(|guard| guard.clone())
}

fn install_workflow_event_broadcaster(broadcaster: Arc<WorkflowEventBroadcaster>) {
    if let Ok(mut guard) = broadcaster_slot().write() {
        *guard = Some(broadcaster);
    }
}

fn clear_workflow_event_broadcaster() {
    if let Ok(mut guard) = broadcaster_slot().write() {
        *guard = None;
    }
}

/// Returns the process-global daemon workflow event emitter when one has
/// been installed by [`run_daemon`]. Returns `None` when called from a
/// process that hasn't started the daemon (CLI one-shot commands,
/// `animus-workflow-runner` subprocess, etc.) — callers should default to a
/// noop emitter in that case.
pub fn current_workflow_event_emitter() -> Option<SharedWorkflowEventEmitter> {
    emitter_slot().read().ok().and_then(|guard| guard.clone())
}

fn install_workflow_event_emitter(emitter: SharedWorkflowEventEmitter) {
    if let Ok(mut guard) = emitter_slot().write() {
        *guard = Some(emitter);
    }
}

fn clear_workflow_event_emitter() {
    if let Ok(mut guard) = emitter_slot().write() {
        *guard = None;
    }
}

/// RAII guard that ensures the process-global [`crate::LogStorageHandle`]
/// is cleared and the plugin host is shut down even when the daemon
/// returns early through a `?` (preflight failure, control server bind
/// error, scheduler crash, …). The async shutdown is spawned onto the
/// current Tokio runtime in `Drop` so a misbehaving plugin can never
/// block daemon teardown; the global slot is cleared synchronously.
///
/// On the normal exit path, `run_daemon` calls
/// [`crate::LogStorageHandle::shutdown`] explicitly before this guard's
/// `Drop` runs — the guard then sees a host that already took the
/// `Option<PluginHost>` out and the spawned task is a no-op.
struct LogStorageHandleDropGuard {
    handle: Arc<crate::LogStorageHandle>,
    active: bool,
}

impl LogStorageHandleDropGuard {
    fn new(handle: Arc<crate::LogStorageHandle>) -> Self {
        Self { handle, active: true }
    }

    /// Disarm the guard so [`Drop`] becomes a no-op. Called from the
    /// normal exit path after the explicit `shutdown().await` + slot
    /// clear have completed.
    fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for LogStorageHandleDropGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        crate::clear_log_storage_handle();
        // TODO(codex-p2): Drop is synchronous and cannot await the
        // graceful shutdown sequence. We schedule it onto the current
        // runtime, but if the daemon returns through `?` and the runtime
        // is torn down immediately afterwards the spawned task may be
        // cancelled before it can drain the plugin host. Restructuring
        // run_daemon to catch errors before propagating (so it can
        // explicitly await `handle.shutdown()` on every exit path) is
        // tracked separately; the OS reaps the orphan when the daemon
        // process exits.
        if self.handle.is_plugin() {
            let handle = self.handle.clone();
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    handle.shutdown().await;
                });
            }
        }
    }
}

pub async fn run_daemon<D, H>(
    project_root: &str,
    options: &mut DaemonRuntimeOptions,
    driver: &mut D,
    hooks: &mut H,
    mut active_process_count: impl FnMut(&D) -> usize,
) -> Result<()>
where
    D: ProjectTickHooks,
    H: DaemonRunHooks,
{
    let _run_guard = DaemonRunGuard::acquire(project_root)?;
    let daemon_pid = std::process::id();
    let primary_root = canonicalize_lossy(project_root);

    crate::metrics::install_workflow_runner_metrics_bridge();

    // Resolve and install the process-wide runtime quotas before any
    // subsystem that consults them (trigger backlog, subscriber buffers,
    // workflow concurrency, plugin process count). First-installer-wins:
    // tests that pre-install a tweaked quota set keep their values.
    crate::quotas::install_runtime_quotas(crate::quotas::RuntimeQuotas::from_env());

    // Wire the plugin host's spawn-site quota check into the runtime
    // quota counter. Without this install the plugin host falls back to
    // a no-op (no cap enforced); with it, every plugin spawn claims a
    // slot bounded by `RuntimeQuotas::plugin_process_max`.
    crate::quotas::install_runtime_quota_process_slot_factory();

    // v0.5.8 secrets: wire keychain-backed secrets into the plugin spawn
    // path. First-installer-wins, so tests that pre-install a mock keep theirs.
    // v0.6: the workflow-YAML interpolator is env-only — `${secret.*}` is no
    // longer resolved at config-parse time, so there is no keychain resolver to
    // install here. Secrets resolve at consume/spawn time via the snapshot
    // provider above.
    let secrets_project_root = std::path::Path::new(project_root);
    let _ = crate::quotas::install_keychain_secret_provider_for(secrets_project_root);

    hooks.handle_event(DaemonRunEvent::Startup { project_root: primary_root.clone(), daemon_pid })?;

    // Preflight BEFORE flipping persisted daemon status to Running. A first-time
    // `animus daemon start` whose preflight fails must not leave behind a stale
    // "running" record that future `daemon status` calls report as live.
    let mut preflight_spec = hooks.plugin_preflight_spec();
    if options.auto_install_plugins {
        preflight_spec.auto_install = true;
    }
    let installer = hooks.plugin_installer();
    let preflight_outcome = run_plugin_preflight(
        project_root,
        &primary_root,
        preflight_spec,
        installer.as_deref(),
        options.skip_plugin_preflight,
        hooks,
    )
    .await?;
    if preflight_outcome.should_abort_startup() {
        let message = preflight_outcome.render_abort_message();
        return Err(anyhow::anyhow!("{message}"));
    }

    let initial_status = hooks.daemon_status(&primary_root).await?;
    let mut stop_daemon_on_exit = false;
    if !matches!(initial_status, DaemonStatus::Running | DaemonStatus::Paused) {
        hooks.start_daemon(&primary_root).await?;
        stop_daemon_on_exit = true;
    }
    let _ = DaemonRuntimeState::set_runtime_paused(project_root, false);

    hooks.handle_event(DaemonRunEvent::Status { project_root: primary_root.clone(), status: "running".to_string() })?;

    if options.startup_cleanup {
        hooks.handle_event(DaemonRunEvent::StartupCleanup { project_root: primary_root.clone() })?;

        let startup_orphans = hooks.recover_startup_orphans(&primary_root).await?;
        if startup_orphans > 0 {
            hooks.handle_event(DaemonRunEvent::OrphanDetection {
                project_root: primary_root.clone(),
                orphaned_workflows_recovered: startup_orphans,
            })?;
        }

        emit_orphan_agent_scan_events(project_root, &primary_root, hooks)?;

        // v0.5.1 decision-log compaction: compress + expire archived
        // `decisions-*.jsonl.bak` files. Runs synchronously on the
        // daemon startup path so no new background task is introduced
        // (see `recording::sweeper` docs).
        if let Some(runs_root) =
            animus_runtime_shared::recording::sweeper::runs_root_for_project(std::path::Path::new(&primary_root))
        {
            let policy = animus_runtime_shared::recording::sweeper::SweepPolicy::from_env();
            match animus_runtime_shared::recording::sweeper::compact_and_expire(&runs_root, policy) {
                Ok(report) => {
                    if report.compressed > 0 || report.expired > 0 || report.failed > 0 {
                        tracing::info!(
                            compressed = report.compressed,
                            expired = report.expired,
                            failed = report.failed,
                            runs_root = %runs_root.display(),
                            "decision-log sweeper completed"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, runs_root = %runs_root.display(), "decision-log sweeper failed");
                }
            }
        }
    }

    let plugin_status_registry = orchestrator_plugin_host::PluginStatusRegistry::new();
    orchestrator_plugin_host::install_global_status_registry(plugin_status_registry.clone());
    // TODO(codex-p2): subject_backend / trigger / log_storage / transport /
    // queue / workflow_runner plugins are spawned via paths that do not
    // currently call into the status registry, so they remain in the
    // `discovered` state for the lifetime of the daemon. Provider plugins
    // (PluginSessionBackend::spawn_and_handshake + graceful_shutdown) are
    // wired through and report live runtime state. Widening coverage requires
    // touching the trigger supervisor, log-storage dispatch, and subject
    // router spawn sites — left to a follow-up commit.
    discover_plugins_for_daemon(project_root, &primary_root, hooks, &plugin_status_registry)?;

    let log_storage_handle = resolve_log_storage_dispatch_for_daemon(project_root, &primary_root, hooks).await;
    let log_storage_drop_guard = LogStorageHandleDropGuard::new(log_storage_handle.clone());

    let subject_dispatch = resolve_subject_dispatch_for_daemon(project_root, &primary_root, hooks).await;

    let workflow_event_broadcaster = WorkflowEventBroadcaster::new();
    install_workflow_event_emitter(BroadcastWorkflowEventEmitter::new(workflow_event_broadcaster.clone()));
    install_workflow_event_broadcaster(workflow_event_broadcaster.clone());

    let control_server_handle = start_control_server_for_daemon(
        project_root,
        &primary_root,
        hooks,
        workflow_event_broadcaster.clone(),
        plugin_status_registry.clone(),
        subject_dispatch,
    )
    .await;

    // v0.5.1 P2 #6.2 round-3: now that the broadcaster AND the control
    // server are live, attempt to reattach to any live orphan agents
    // detected by the earlier startup scan. The order matters: gap replay
    // permanently advances the consumed offset, so it must run after the
    // `workflow/events` subscription surface exists — replaying before the
    // control server starts fans the events out to zero possible
    // subscribers and marks them consumed forever. When the control server
    // did not come up at all (disabled or bind failure) the reattach is
    // skipped entirely so the un-replayed gap survives for the next daemon
    // start instead of being consumed unobserved. Best-effort: a failure
    // here is logged and the rest of daemon startup proceeds. Stub
    // `if options.startup_cleanup` guards parity with the orphan-scan
    // trigger.
    // TODO(codex-p2): even with the server up, a `workflow/events`
    // subscriber may not have registered yet when the replay runs, so the
    // replayed events can still fan out to zero subscribers before the
    // offset advances. And when the server is unavailable the skipped gap
    // only survives while the orphan stays alive — if it exits first, the
    // next scan_orphans_for_project deletes the spawn record and the
    // decision-log gap becomes unreachable. Closing both residual windows
    // needs the replayed events persisted into the daemon event log (or
    // offset advancement deferred until first delivery) — tracked
    // separately.
    if options.startup_cleanup && control_server_handle.is_some() {
        attempt_orphan_agent_reattach(project_root, &primary_root, workflow_event_broadcaster.clone(), hooks)?;
    }

    // Trigger backend plugins. Off by default behind an env flag mirroring
    // the provider-plugin opt-out shape.
    let trigger_event_queue: Arc<Mutex<Vec<TriggerSupervisorEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let trigger_supervisor = if triggers_disabled() {
        None
    } else {
        let queue = trigger_event_queue.clone();
        let sink: TriggerSupervisorSink = Arc::new(move |event| {
            // Routed trigger events queue dispatchable work — wake the
            // scheduler loop so it drains now instead of on the fallback
            // heartbeat. Coalescing + no-op-before-install semantics live
            // in `nudge_scheduler_local`; nudging on supervisor lifecycle
            // events too is harmless (one cheap dispatch-leg pass).
            if let Ok(mut guard) = queue.lock() {
                guard.push(event);
            }
            super::nudge_scheduler_local();
        });
        match TriggerSupervisor::start(Path::new(project_root), sink).await {
            Ok(supervisor) => Some(supervisor),
            Err(error) => {
                hooks.handle_event(DaemonRunEvent::TriggerPluginStartFailed {
                    project_root: primary_root.clone(),
                    plugin_name: "<supervisor>".to_string(),
                    error: format!("{error:#}"),
                })?;
                None
            }
        }
    };
    drain_trigger_events(&primary_root, &trigger_event_queue, hooks)?;

    match orchestrator_core::validate_and_compile_yaml_workflows(Path::new(project_root)) {
        Ok(Some(result)) => {
            hooks.handle_event(DaemonRunEvent::YamlCompileSucceeded {
                project_root: primary_root.clone(),
                source_files: result.source_files.len(),
                output_path: result.output_path.display().to_string(),
                phase_definitions: result.config.phase_definitions.len(),
                agent_profiles: result.config.agent_profiles.len(),
            })?;
        }
        Ok(None) => {}
        Err(error) => {
            hooks.handle_event(DaemonRunEvent::YamlCompileFailed {
                project_root: primary_root.clone(),
                error: error.to_string(),
            })?;
        }
    }

    // v0.5.8 hot-reload: prime the workflow-config snapshot from the
    // current YAML state and spawn the watcher. Watcher failure is logged
    // but never aborts daemon startup — a misbehaving FS notify backend
    // must not block the daemon from coming up.
    let workflow_config_snapshot = crate::config::workflow_config_snapshot();
    let initial_reload = crate::config::reload_workflow_config_once(
        Path::new(project_root),
        &workflow_config_snapshot,
        Some(workflow_event_broadcaster.as_ref()),
    );
    if initial_reload.reloaded {
        hooks.handle_event(DaemonRunEvent::WorkflowConfigReloaded {
            project_root: primary_root.clone(),
            phase_definitions: initial_reload.phase_definitions,
            workflows: initial_reload.workflows,
            agent_profiles: initial_reload.agent_profiles,
            source_files: initial_reload.source_files.iter().map(|p| p.display().to_string()).collect(),
            config_hash: initial_reload.config_hash.clone().unwrap_or_default(),
        })?;
    }
    let workflow_config_watcher = match crate::config::spawn_workflow_config_watcher(
        PathBuf::from(project_root),
        workflow_config_snapshot.clone(),
        workflow_event_broadcaster.clone(),
        Duration::from_millis(crate::config::WATCHER_DEBOUNCE_DEFAULT_MS),
    ) {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!(
                target: "animus.config.hot_reload",
                error = %error,
                "workflow config watcher failed to start; manual `animus workflow config reload` still works"
            );
            None
        }
    };

    let mut interval = Duration::from_secs(options.interval_secs.max(1));
    let mut sigterm_stream = SigtermStream::new()?;
    let mut sigint_stream = SigintStream::new()?;

    // Event-driven scheduler wake-ups. The loop below parks on a select
    // whose arms are (a) shutdown signals, (b) this Notify (control-socket
    // `daemon/nudge`, workflow-config hot-reload, and the completion
    // forwarder), (c) the next cron deadline, and (d) the fallback
    // heartbeat (`interval_secs`). Notify::notify_one stores at most one
    // permit, so nudge bursts coalesce into at most one extra pass.
    let scheduler_nudge = Arc::new(tokio::sync::Notify::new());
    super::install_scheduler_nudge(scheduler_nudge.clone());

    // Completion wake: workflow-runner subprocesses stream phase/workflow
    // lifecycle events into the daemon's broadcaster via the per-run
    // back-channel pipe. Forward completion-shaped events into the nudge so
    // follow-on work dispatches immediately instead of on the next
    // heartbeat. Crashed runners that emit nothing are still picked up by
    // the heartbeat's completed-process reaping.
    let (completion_sub_id, mut completion_rx) =
        workflow_event_broadcaster.subscribe(crate::control::WorkflowEventFilter {
            workflow_id: None,
            kinds: Some(vec![
                "phase_completed".to_string(),
                "workflow_completed".to_string(),
                "workflow_failed".to_string(),
            ]),
        });
    let completion_nudge = scheduler_nudge.clone();
    let completion_forwarder = tokio::spawn(async move {
        while let Some(item) = completion_rx.recv().await {
            match item {
                crate::control::SubscriberItem::Event(_) => completion_nudge.notify_one(),
                crate::control::SubscriberItem::Closed { .. } => break,
            }
        }
    });

    // Housekeeping debounce: heavy reconciliation legs run at most once
    // per heartbeat period even when event wakes drive extra passes.
    // `None` means "never ran" so the first pass always sweeps.
    let mut last_housekeeping: Option<tokio::time::Instant> = None;
    loop {
        // Hot-reload runtime-reconfigurable settings from persisted project config
        // so that `animus.daemon config-set` changes take effect without restart.
        let prev_interval = options.interval_secs;
        options.reload_from_project_config(Path::new(project_root));
        if options.interval_secs != prev_interval {
            interval = Duration::from_secs(options.interval_secs.max(1));
            hooks.handle_event(DaemonRunEvent::ConfigReloaded {
                project_root: primary_root.clone(),
                setting: "interval_secs".to_string(),
            })?;
        }

        let housekeeping_due = last_housekeeping.is_none_or(|at| at.elapsed() >= interval);
        let externally_paused = DaemonRuntimeState::is_runtime_paused(project_root).unwrap_or(false);
        // Anchor for the cron-deadline arm below: captured BEFORE the tick
        // so an occurrence that lands while the tick is running (after the
        // tick's own schedule evaluation instant) still produces a past
        // deadline → immediate catch-up pass, instead of being silently
        // deferred to the next occurrence.
        let tick_anchor = chrono::Utc::now();
        let tick_result = run_project_tick(
            &primary_root,
            options,
            ProjectTickRunMode { active_process_count: active_process_count(driver), housekeeping: housekeeping_due },
            externally_paused,
            driver,
        )
        .await;
        if housekeeping_due {
            last_housekeeping = Some(tokio::time::Instant::now());
        }

        match tick_result {
            Ok(summary) => hooks.handle_event(DaemonRunEvent::TickSummary { summary })?,
            Err(error) => hooks.handle_event(DaemonRunEvent::TickError {
                project_root: primary_root.clone(),
                message: error.to_string(),
            })?,
        }

        drain_trigger_events(&primary_root, &trigger_event_queue, hooks)?;

        if let Err(error) = hooks.flush_notifications(&primary_root).await {
            hooks.handle_event(DaemonRunEvent::NotificationRuntimeError {
                project_root: Some(primary_root.clone()),
                stage: "flush".to_string(),
                message: error.to_string(),
            })?;
        }

        if options.once {
            break;
        }

        let shutdown = DaemonRuntimeState::is_shutdown_requested(project_root).unwrap_or((false, None));
        if shutdown.0 {
            hooks.handle_event(DaemonRunEvent::GracefulShutdown {
                project_root: primary_root.clone(),
                timeout_secs: shutdown.1,
            })?;
            let _ = DaemonRuntimeState::set_shutdown_requested(project_root, false, None);
            break;
        }

        // Cron deadline arm: sleep precisely until the earliest upcoming
        // schedule occurrence so cron fires on time (±ms) instead of on
        // the next heartbeat. Recomputed every pass, which also picks up
        // workflow-config reloads (the hot-reload watcher additionally
        // nudges the loop so a reload mid-sleep re-arms immediately).
        //
        // The computation is anchored at `tick_anchor` (just before the
        // tick that just finished evaluated its schedules), NOT at
        // wall-now: an occurrence crossed by a long-running tick lies
        // strictly after the anchor, yields an already-elapsed deadline
        // (clamped to ZERO), and triggers an immediate catch-up pass —
        // otherwise a long heartbeat could outlive the 10-minute catch-up
        // horizon and the fire would be lost. No busy loop results: the
        // catch-up pass dispatches the occurrence (advancing `last_run`)
        // and re-anchors at its own start time, after which the deadline
        // is strictly in the future again. An occurrence the tick already
        // dispatched (anchor races the tick's own evaluation instant by
        // sub-millisecond) costs at most one extra no-op pass.
        let cron_deadline = crate::ScheduleDispatch::next_schedule_deadline(project_root, tick_anchor);
        // Precise wake for deferred queue entries: the earliest future
        // `run_at` reported by the queue plugin. Folded into the timed arm
        // alongside the cron deadline so a deferred entry fires on time
        // instead of waiting for the heartbeat. Errors degrade to `None`.
        let queue_deadline = hooks.queue_next_deadline(&primary_root).await;
        let next_deadline = match (cron_deadline, queue_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        };
        let cron_sleep =
            next_deadline.map(|deadline| (deadline - chrono::Utc::now()).to_std().unwrap_or(Duration::ZERO));

        // Retry-sweep ceiling: a cron occurrence that woke the loop but
        // could not dispatch (pool/budget full, transient spawn failure)
        // is only retryable by the catch-up scan while it remains inside
        // the 10-minute horizon. When any schedule is enabled, cap the
        // sleep at half that horizon so at least one retry pass lands in
        // time even when `interval_secs` is much longer. Housekeeping
        // cadence is unaffected — `housekeeping_due` keys off the
        // configured `interval`, not off which arm woke the loop.
        let max_sleep =
            if cron_deadline.is_some() { interval.min(crate::schedule::SCHEDULE_RETRY_SWEEP_MAX) } else { interval };

        // Every arm either breaks the loop or falls through to the next
        // pass, which re-arms a fresh sleep (heartbeat) and a fresh
        // notified() future (nudge) — no arm can spin without sleeping.
        tokio::select! {
            _ = sigint_stream.recv() => {
                hooks.handle_event(DaemonRunEvent::Draining {
                    project_root: primary_root.clone(),
                    trigger: "ctrl_c".to_string(),
                })?;
                break;
            }
            _ = sigterm_stream.recv() => {
                hooks.handle_event(DaemonRunEvent::Draining {
                    project_root: primary_root.clone(),
                    trigger: "sigterm".to_string(),
                })?;
                break;
            }
            // Event wake: subject/queue writes (via `daemon/nudge`),
            // completion events, and config reloads land here.
            _ = scheduler_nudge.notified() => {}
            // Cron deadline wake; pending forever when nothing is scheduled.
            _ = async {
                match cron_sleep {
                    Some(duration) => sleep(duration).await,
                    None => std::future::pending::<()>().await,
                }
            } => {}
            // Fallback heartbeat: catches out-of-band state mutations made
            // without the CLI/MCP surfaces and paces housekeeping. Clamped
            // to the schedule retry-sweep ceiling when schedules exist.
            _ = sleep(max_sleep) => {}
        }
    }

    completion_forwarder.abort();
    workflow_event_broadcaster.unsubscribe(completion_sub_id);
    super::clear_scheduler_nudge();

    if let Some(supervisor) = trigger_supervisor {
        let _ = supervisor.shutdown().await;
        drain_trigger_events(&primary_root, &trigger_event_queue, hooks)?;
    }

    if let Some(watcher) = workflow_config_watcher {
        watcher.shutdown().await;
    }

    if let Some(server) = control_server_handle {
        let _ = server.shutdown().await;
    }

    clear_workflow_event_emitter();
    clear_workflow_event_broadcaster();

    if stop_daemon_on_exit {
        let _ = hooks.stop_daemon(&primary_root).await;
    }

    // Emit final status events BEFORE tearing down the log_storage
    // plugin host so those records are still forwarded to the backend
    // (otherwise an operator tailing the plugin would never see the
    // daemon stopping).
    hooks.handle_event(DaemonRunEvent::Status { project_root: primary_root.clone(), status: "stopped".to_string() })?;
    hooks.handle_event(DaemonRunEvent::Shutdown { project_root: primary_root.clone(), daemon_pid })?;

    // Await every in-flight notifier dispatch so the Status/Shutdown
    // events above reach installed notifier plugins before the Tokio
    // runtime drops. Closes codex v0.5.3 Task D round-5 P2 (the
    // shutdown_drain wiring was missing). Errors are logged inside the
    // impl; never propagate.
    if let Err(error) = hooks.shutdown_drain_notifications(&primary_root).await {
        tracing::warn!(%error, "notifier shutdown drain failed");
    }

    // Give the fire-and-forget `log_storage/store` tasks spawned by
    // `DaemonEventLog::append` a brief window to flush before reaping
    // the plugin host. Bounded to keep teardown deterministic.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    log_storage_handle.shutdown().await;
    crate::clear_log_storage_handle();
    // Normal exit — disarm the drop guard so it doesn't double-shutdown.
    log_storage_drop_guard.disarm();

    Ok(())
}

fn triggers_disabled() -> bool {
    std::env::var("ANIMUS_DAEMON_DISABLE_TRIGGERS").map(|v| v != "0" && !v.is_empty()).unwrap_or(false)
}

fn drain_trigger_events<H: DaemonRunHooks>(
    primary_root: &str,
    queue: &Arc<Mutex<Vec<TriggerSupervisorEvent>>>,
    hooks: &mut H,
) -> Result<()> {
    let drained: Vec<TriggerSupervisorEvent> = {
        let mut guard = match queue.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *guard)
    };
    for event in drained {
        let daemon_event = match event {
            TriggerSupervisorEvent::Started { plugin_count } => {
                DaemonRunEvent::TriggerPluginsStarted { project_root: primary_root.to_string(), plugin_count }
            }
            TriggerSupervisorEvent::StartFailed { plugin_name, error } => {
                crate::metrics::incr(&crate::metrics::labeled(
                    "plugin_start_failures_total",
                    &[("plugin", plugin_name.as_str())],
                ));
                DaemonRunEvent::TriggerPluginStartFailed { project_root: primary_root.to_string(), plugin_name, error }
            }
            TriggerSupervisorEvent::Event { plugin_name, event_id, trigger_id, routed } => {
                crate::metrics::incr(&crate::metrics::labeled(
                    "trigger_events_total",
                    &[("plugin", plugin_name.as_str()), ("routed", if routed { "true" } else { "false" })],
                ));
                DaemonRunEvent::TriggerPluginEvent {
                    project_root: primary_root.to_string(),
                    plugin_name,
                    event_id,
                    trigger_id,
                    routed,
                }
            }
            TriggerSupervisorEvent::Restart { plugin_name, attempt, delay_ms } => {
                crate::metrics::incr(&crate::metrics::labeled(
                    "plugin_restarts_total",
                    &[("plugin", plugin_name.as_str())],
                ));
                DaemonRunEvent::TriggerPluginRestart {
                    project_root: primary_root.to_string(),
                    plugin_name,
                    attempt,
                    delay_ms,
                }
            }
            TriggerSupervisorEvent::Crashed { plugin_name, attempts, error } => {
                crate::metrics::incr(&crate::metrics::labeled(
                    "plugin_disabled_total",
                    &[("plugin", plugin_name.as_str())],
                ));
                DaemonRunEvent::TriggerPluginCrashed {
                    project_root: primary_root.to_string(),
                    plugin_name,
                    attempts,
                    error,
                }
            }
        };
        hooks.handle_event(daemon_event)?;
    }
    Ok(())
}

/// Resolve which subject-backend plugins the daemon will route through
/// and emit a [`DaemonRunEvent::SubjectRouterResolved`] so operators see
/// the choice on every startup. Failures (discovery error, plugin spawn
/// failure, duplicate-kind collision) are degraded to an empty router
/// plus a warning rather than aborting startup — a misbehaving subject
/// plugin must never block the daemon from coming up. CLI `animus subject`
/// calls against unrouted kinds will surface `NotFound`.
async fn resolve_subject_dispatch_for_daemon<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    hooks: &mut H,
) -> Arc<crate::subject_dispatch::SubjectPluginDispatch> {
    let disable_env_set = crate::subject_plugins_disable_env_set();
    match crate::resolve_subject_dispatch(Path::new(project_root)).await {
        Ok(resolution) => {
            let plugin_count = resolution.selected.plugin_count();
            let kinds = resolution.selected.kinds().to_vec();
            let _ = hooks.handle_event(DaemonRunEvent::SubjectRouterResolved {
                project_root: primary_root.to_string(),
                plugin_count,
                kinds,
                disable_env_set,
                warnings: resolution.warnings,
            });
            // Hand the resolved dispatch to the control surface so
            // control-routed subject ops (subject/list,get,create,update,
            // next,status,watch — used by the HTTP/GraphQL transports) reach
            // the same backends the in-process CLI path does. Without this the
            // surface's subject_dispatch stays None and every transport
            // subject call returns "subject dispatch not initialized".
            Arc::new(resolution.selected)
        }
        Err(error) => {
            let _ = hooks.handle_event(DaemonRunEvent::SubjectRouterResolved {
                project_root: primary_root.to_string(),
                plugin_count: 0,
                kinds: Vec::new(),
                disable_env_set,
                warnings: vec![format!(
                    "subject_backend discovery failed; subject CLI calls will return NotFound: {error:#}"
                )],
            });
            Arc::new(crate::subject_dispatch::SubjectPluginDispatch::empty())
        }
    }
}

/// Resolve which log storage backend the daemon will route through,
/// spawn the supervised [`PluginHost`] when the dispatch resolves to a
/// plugin, install the resulting [`crate::LogStorageHandle`] in the
/// process-global slot, and emit a
/// [`DaemonRunEvent::LogStorageDispatchResolved`] so operators see the
/// choice on every startup.
///
/// Failures (discovery, spawn, handshake) degrade to an in-tree handle
/// plus a warning rather than aborting startup — a misbehaving
/// log_storage plugin must never block the daemon from coming up.
async fn resolve_log_storage_dispatch_for_daemon<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    hooks: &mut H,
) -> Arc<crate::LogStorageHandle> {
    let outcome = crate::spawn_log_storage_supervisor(Path::new(project_root)).await;
    // Install BEFORE emitting the dispatch-resolved event so the event
    // itself is forwarded to the plugin when one is active; otherwise
    // the first daemon event after startup lands in events.jsonl only
    // and operators tailing the plugin never see which backend the
    // daemon picked.
    crate::install_log_storage_handle(outcome.handle.clone());
    let _ = hooks.handle_event(DaemonRunEvent::LogStorageDispatchResolved {
        project_root: primary_root.to_string(),
        plugin_name: outcome.plugin_name.clone(),
        candidate_count: outcome.candidate_count,
        disable_env_set: outcome.disable_env_set,
        warnings: outcome.warnings,
    });
    outcome.handle
}

/// Start the daemon's control RPC server (Unix socket speaking the
/// `animus-control-protocol` wire format).
///
/// Honors [`crate::control::CONTROL_SERVER_DISABLE_ENV`]: when the env
/// var is set truthy the server is skipped and the
/// [`DaemonRunEvent::ControlServerResolved`] event carries
/// `disable_env_set = true`. Any bind / chmod / IO failure degrades to
/// "no server, warning emitted" rather than aborting the daemon — a
/// misbehaving socket must never block startup. The handle is returned
/// for graceful shutdown on daemon teardown.
async fn start_control_server_for_daemon<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    hooks: &mut H,
    workflow_event_broadcaster: Arc<WorkflowEventBroadcaster>,
    plugin_status_registry: Arc<orchestrator_plugin_host::PluginStatusRegistry>,
    subject_dispatch: Arc<crate::subject_dispatch::SubjectPluginDispatch>,
) -> Option<crate::control::ControlServerHandle> {
    let project_root_path = Path::new(project_root);
    let socket_path = crate::control::control_socket_path(project_root_path);
    let disable_env_set = crate::control::control_server_disable_env_set();

    if disable_env_set {
        let _ = hooks.handle_event(DaemonRunEvent::ControlServerResolved {
            project_root: primary_root.to_string(),
            socket_path: socket_path.clone(),
            disable_env_set: true,
            warnings: vec![format!(
                "control server skipped because {} is set",
                crate::control::CONTROL_SERVER_DISABLE_ENV
            )],
        });
        return None;
    }

    let mut surface_builder = crate::control::InProcessSurface::builder(project_root_path.to_path_buf())
        .daemon_version(env!("CARGO_PKG_VERSION").to_string())
        .subject_dispatch(subject_dispatch);
    if let Some(routing) = hooks.plugin_routing() {
        surface_builder = surface_builder.plugin_routing(routing);
    }
    if let Some(routing) = hooks.daemon_ops_routing() {
        surface_builder = surface_builder.daemon_ops_routing(routing);
    }
    if let Some(routing) = hooks.workflow_routing() {
        surface_builder = surface_builder.workflow_routing(routing);
    }
    if let Some(routing) = hooks.queue_routing() {
        surface_builder = surface_builder.queue_routing(routing);
    }
    let surface = surface_builder.build();
    let surface_arc: Arc<dyn animus_control_protocol::ControlSurface> = Arc::new(surface);

    // v0.5.8 small-core RBAC: bootstrap ~/.animus/principals.yaml on
    // first daemon startup (collision-guarded: never overwrites an
    // existing file). Then load policy. Missing file falls back to
    // single-user (the bit-identical default). Parse errors fail
    // closed — we refuse to start the control server rather than
    // silently degrading from `enforce` to single-user after a YAML
    // typo. (codex round-4 P2: bootstrap before load so first-run
    // operators see a generated default rather than nothing.)
    let principals_path = orchestrator_core::default_principals_path();
    if let Err(err) = orchestrator_core::bootstrap_principals_file_if_absent(&principals_path) {
        tracing::warn!(
            target: "animus.control.server",
            path = %principals_path.display(),
            error = %err,
            "principals.yaml bootstrap failed; policy loader will fall through to single-user"
        );
    }
    let policy = match crate::control::PolicyState::load(principals_path) {
        Ok(policy) => policy,
        Err(error) => {
            // Codex round-8 P1: keep the control server running but
            // in deny-all mode. If we just refused to start, CLI
            // helpers that treat a missing socket as "daemon down"
            // would silently fall back to in-process services that
            // bypass chokepoint #1. The fail-closed posture only
            // works if every wire entry point still hits the hook.
            tracing::error!(
                target: "animus.control.server",
                error = %error,
                "principals.yaml unparseable; control server entering deny-all mode (fail-closed under v0.5.8 RBAC)"
            );
            crate::control::PolicyState::deny_all(format!("principals.yaml unparseable: {error}"))
        }
    };

    match crate::control::ControlServer::start_with_policy_and_observability(
        project_root_path,
        surface_arc,
        Some(workflow_event_broadcaster),
        policy,
        Some(plugin_status_registry),
    )
    .await
    {
        Ok(handle) => {
            let _ = hooks.handle_event(DaemonRunEvent::ControlServerResolved {
                project_root: primary_root.to_string(),
                socket_path: handle.socket_path().to_path_buf(),
                disable_env_set: false,
                warnings: Vec::new(),
            });
            Some(handle)
        }
        Err(error) => {
            let _ = hooks.handle_event(DaemonRunEvent::ControlServerResolved {
                project_root: primary_root.to_string(),
                socket_path: socket_path.clone(),
                disable_env_set: false,
                warnings: vec![format!(
                    "control server failed to start; CLI/MCP must fall back to in-process services: {error}"
                )],
            });
            None
        }
    }
}

/// v0.5.1 P2 #6.2 round-3: walk the orphan scan once more and try to
/// reconnect to each live orphan's reattach socket. Emits
/// [`DaemonRunEvent::OrphanAgentReattached`] / [`DaemonRunEvent::OrphanAgentReattachFailed`]
/// per orphan. The returned `ReattachConnection`s are held in a process
/// vector so the reader tasks survive the function return; we don't try
/// to address graceful per-orphan shutdown today.
fn attempt_orphan_agent_reattach<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    broadcaster: Arc<WorkflowEventBroadcaster>,
    hooks: &mut H,
) -> Result<()> {
    let report = crate::dispatch::agent_record::scan_orphans_for_project(Path::new(project_root)).unwrap_or_default();
    if report.detected.is_empty() {
        return Ok(());
    }

    let project_root_path = Path::new(project_root);
    let connections = orphan_reattach_connections();
    for detected in &report.detected {
        let parsed_record: Option<crate::dispatch::agent_record::AgentSpawnRecord> =
            std::fs::read_to_string(&detected.record_path).ok().and_then(|raw| serde_json::from_str(&raw).ok());

        // v0.5.1 fold-in (item 2): before opening the live socket, replay
        // any decision-log events that landed during the daemon gap so
        // workflow_events subscribers see them in order with anything the
        // live socket subsequently delivers. Best-effort: failures are
        // logged and we still try the socket reattach.
        if let Some(record) = parsed_record.as_ref() {
            replay_gap_for_orphan(project_root_path, primary_root, record, broadcaster.as_ref(), hooks);
        }

        let Some(record_path) = parsed_record.and_then(|rec| rec.stdio_socket_path) else {
            hooks.handle_event(DaemonRunEvent::OrphanAgentReattachFailed {
                project_root: primary_root.to_string(),
                agent_session_id: detected.agent_session_id.clone(),
                pid: detected.pid,
                socket_path: None,
                error: "spawn record has no stdio_socket_path (pre-v0.5.1 record or fallback path)".to_string(),
            })?;
            continue;
        };

        match crate::dispatch::reattach::try_reattach(&record_path, broadcaster.clone()) {
            Ok(conn) => {
                hooks.handle_event(DaemonRunEvent::OrphanAgentReattached {
                    project_root: primary_root.to_string(),
                    agent_session_id: detected.agent_session_id.clone(),
                    pid: detected.pid,
                    socket_path: record_path.clone(),
                })?;
                if let Ok(mut guard) = connections.lock() {
                    guard.push(conn);
                }
            }
            Err(err) => {
                hooks.handle_event(DaemonRunEvent::OrphanAgentReattachFailed {
                    project_root: primary_root.to_string(),
                    agent_session_id: detected.agent_session_id.clone(),
                    pid: detected.pid,
                    socket_path: Some(record_path),
                    error: format!("{err}"),
                })?;
            }
        }
    }

    Ok(())
}

/// v0.5.1 fold-in (item 2): adapter that lifts an `Arc<WorkflowEventBroadcaster>`
/// into the trait shape `replay_gap_from_spawn_record` accepts, and updates
/// the spawn record's `last_consumed_offset` so subsequent daemon starts
/// don't replay the same events.
fn replay_gap_for_orphan<H: DaemonRunHooks>(
    project_root: &Path,
    primary_root: &str,
    record: &crate::dispatch::agent_record::AgentSpawnRecord,
    broadcaster: &WorkflowEventBroadcaster,
    hooks: &mut H,
) {
    struct Adapter<'a> {
        inner: &'a WorkflowEventBroadcaster,
    }
    impl<'a> crate::dispatch::reattach::WorkflowEventBroadcasterLike for Adapter<'a> {
        fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
            self.inner.emit(event);
        }
    }
    let adapter = Adapter { inner: broadcaster };
    match crate::dispatch::reattach::replay_gap_from_spawn_record(project_root, record, &adapter) {
        Ok(Some(report)) => {
            if report.emitted > 0 || report.next_offset != record.last_consumed_offset {
                crate::dispatch::agent_record::update_consumed_offset(
                    project_root,
                    &record.agent_session_id,
                    report.next_offset,
                );
            }
            let _ = hooks.handle_event(DaemonRunEvent::OrphanAgentGapReplayed {
                project_root: primary_root.to_string(),
                agent_session_id: record.agent_session_id.clone(),
                emitted: report.emitted,
                next_offset: report.next_offset,
                partial_tail: report.partial_tail,
            });
        }
        Ok(None) => {}
        Err(error) => {
            let _ = hooks.handle_event(DaemonRunEvent::OrphanAgentGapReplayFailed {
                project_root: primary_root.to_string(),
                agent_session_id: record.agent_session_id.clone(),
                error: format!("{error}"),
            });
        }
    }
}

/// Process-global retention point for active orphan reattach connections.
/// The reader threads run on std::thread; holding the `JoinHandle` keeps
/// the handle around for future graceful-shutdown hooks (and avoids
/// surprising operators who would expect a Drop to terminate forwarding).
fn orphan_reattach_connections() -> &'static std::sync::Mutex<Vec<crate::dispatch::reattach::ReattachConnection>> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<Vec<crate::dispatch::reattach::ReattachConnection>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn emit_orphan_agent_scan_events<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    hooks: &mut H,
) -> Result<()> {
    let unix_supported = cfg!(unix);
    let report = crate::dispatch::agent_record::scan_orphans_for_project(Path::new(project_root)).unwrap_or_default();

    for detected in &report.detected {
        hooks.handle_event(DaemonRunEvent::OrphanAgentDetected {
            project_root: primary_root.to_string(),
            agent_session_id: detected.agent_session_id.clone(),
            pid: detected.pid,
            subject_id: detected.subject_id.clone(),
            subject_kind: detected.subject_kind.clone(),
            workflow_ref: detected.workflow_ref.clone(),
            task_id: detected.task_id.clone(),
            command_line: detected.command_line.clone(),
            started_at: detected.started_at.clone(),
            record_path: detected.record_path.display().to_string(),
        })?;
    }

    for cleaned in &report.cleaned {
        hooks.handle_event(DaemonRunEvent::OrphanAgentCleanup {
            project_root: primary_root.to_string(),
            agent_session_id: cleaned.agent_session_id.clone(),
            pid: cleaned.pid,
            record_path: cleaned.record_path.display().to_string(),
        })?;
    }

    for unparseable in &report.unparseable {
        hooks.handle_event(DaemonRunEvent::OrphanAgentRecordUnparseable {
            project_root: primary_root.to_string(),
            record_path: unparseable.path.display().to_string(),
            error: unparseable.error.clone(),
        })?;
    }

    hooks.handle_event(DaemonRunEvent::OrphanAgentScan {
        project_root: primary_root.to_string(),
        detected_count: report.detected.len(),
        cleaned_count: report.cleaned.len(),
        unparseable_count: report.unparseable.len(),
        unix_scan_supported: unix_supported,
    })?;

    Ok(())
}

fn discover_plugins_for_daemon<H: DaemonRunHooks>(
    project_root: &str,
    primary_root: &str,
    hooks: &mut H,
    status_registry: &Arc<orchestrator_plugin_host::PluginStatusRegistry>,
) -> Result<()> {
    use orchestrator_plugin_host::DiscoverySource;
    match orchestrator_plugin_host::discover_plugins(Path::new(project_root)) {
        Ok(plugins) => {
            for plugin in &plugins {
                status_registry.record_discovered(
                    &plugin.name,
                    &plugin.manifest.plugin_kind,
                    Some(plugin.path.display().to_string()),
                    Some(plugin.manifest.name.clone()),
                );
            }
            let summaries = plugins
                .into_iter()
                .map(|p| DiscoveredPluginSummary {
                    name: p.name,
                    version: p.manifest.version,
                    plugin_kind: p.manifest.plugin_kind,
                    source: match p.source {
                        DiscoverySource::ExplicitConfig => "explicit_config",
                        DiscoverySource::ProjectLocal => "project_local",
                        DiscoverySource::PluginPath => "plugin_path",
                        DiscoverySource::SystemPath => "system_path",
                    },
                    path: p.path.display().to_string(),
                })
                .collect::<Vec<_>>();
            hooks.handle_event(DaemonRunEvent::PluginsDiscovered {
                project_root: primary_root.to_string(),
                plugins: summaries,
            })?;
        }
        Err(error) => {
            hooks.handle_event(DaemonRunEvent::PluginsDiscoveryFailed {
                project_root: primary_root.to_string(),
                error: error.to_string(),
            })?;
        }
    }
    Ok(())
}

struct SigintStream {
    #[cfg(unix)]
    inner: tokio::signal::unix::Signal,
}

impl SigintStream {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            let inner = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .context("failed to subscribe to SIGINT")?;
            Ok(Self { inner })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            self.inner.recv().await;
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

struct SigtermStream {
    #[cfg(unix)]
    inner: tokio::signal::unix::Signal,
}

impl SigtermStream {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            let inner = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("failed to subscribe to SIGTERM")?;
            Ok(Self { inner })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            self.inner.recv().await;
        }
        #[cfg(not(unix))]
        {
            std::future::pending::<()>().await;
        }
    }
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate.canonicalize().unwrap_or(candidate).to_string_lossy().to_string()
}
