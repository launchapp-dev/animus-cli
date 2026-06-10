//! Supervises external trigger backend plugins.
//!
//! When the daemon starts it discovers all installed stdio plugins. The
//! [`TriggerSupervisor`] filters that set down to plugins whose manifest
//! declares `plugin_kind = "trigger_backend"`, spawns each as a long-lived
//! subprocess via [`orchestrator_plugin_host::PluginHost`], drives the
//! initialize handshake, sends `trigger/watch`, and forwards `trigger/event`
//! notifications into a router callback that enqueues them against the
//! matching [`WorkflowTrigger`] in `pending_events`.
//!
//! The supervisor owns one Tokio task per plugin. If a plugin's stdio closes
//! cleanly OR the session errors out, the supervisor respawns it inside an
//! outer retry loop with exponential backoff (1s, 2s, 4s, 8s, 16s, capped at
//! 60s). After [`MAX_RESTART_ATTEMPTS`] consecutive failures it emits a final
//! [`TriggerSupervisorEvent::Crashed`] event and stops trying. Plugins that
//! run cleanly for [`HEALTHY_WINDOW`] reset their attempt counter, so a flaky
//! plugin that crashes every couple minutes doesn't drain its budget.
//!
//! On daemon shutdown the supervisor sends `trigger/ack` for every event id
//! it observed plus a `shutdown` request to each plugin, then waits up to 2
//! seconds for each child to exit.
//!
//! Plugin events are routed to the existing webhook dispatch path by
//! appending to [`orchestrator_core::TriggerRunState::pending_events`]. The
//! tick loop's [`crate::TriggerDispatch::process_due_triggers`] drains those
//! events each tick and spawns the configured workflow.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::{
    TriggerAckParams, TriggerAckStatus, TriggerEvent, TriggerWatchParams, PLUGIN_KIND_TRIGGER_BACKEND,
    TRIGGER_METHOD_ACK, TRIGGER_METHOD_EVENT, TRIGGER_METHOD_WATCH,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use orchestrator_core::workflow_config::TriggerType;
use orchestrator_core::WebhookEvent;
use orchestrator_logging::Logger;
use orchestrator_plugin_host::{discover_plugins, DiscoveredPlugin, PluginHost, PluginSpawnOptions, PluginStderrSink};
use serde_json::json;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, warn};

/// Maximum restart attempts before a trigger plugin is considered crashed
/// and the supervisor stops respawning it.
pub const MAX_RESTART_ATTEMPTS: u32 = 5;

/// Base of the exponential backoff schedule between restart attempts. The
/// resulting series is 1s, 2s, 4s, 8s, 16s, then clamped at
/// [`MAX_BACKOFF`] thereafter.
const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// Hard cap on the exponential backoff between restart attempts. Picked to
/// avoid a single broken plugin starving a daemon's restart budget for hours.
const MAX_BACKOFF: Duration = Duration::from_mins(1);

/// A plugin that has run for at least this long without exiting is treated
/// as "healthy enough" — the attempt counter resets so transient crashes
/// later don't drain the restart budget.
const HEALTHY_WINDOW: Duration = Duration::from_mins(1);

/// Lifecycle event the daemon observes from the supervisor.
#[derive(Debug, Clone)]
pub enum TriggerSupervisorEvent {
    Started { plugin_count: usize },
    StartFailed { plugin_name: String, error: String },
    Event { plugin_name: String, event_id: String, trigger_id: Option<String>, routed: bool },
    Restart { plugin_name: String, attempt: u32, delay_ms: u64 },
    Crashed { plugin_name: String, attempts: u32, error: String },
}

/// Sink the supervisor uses to surface lifecycle events back to the daemon.
pub type TriggerSupervisorSink = Arc<dyn Fn(TriggerSupervisorEvent) + Send + Sync>;

/// One supervised trigger plugin.
struct TriggerHandle {
    #[allow(dead_code)]
    plugin_name: String,
    /// Background task that drives the plugin's outer supervise loop.
    task: JoinHandle<()>,
    /// Shutdown channel; sending `()` asks the supervise loop to exit cleanly.
    shutdown_tx: mpsc::Sender<()>,
}

/// Supervises the lifecycle of every `trigger_backend` plugin for a project.
pub struct TriggerSupervisor {
    project_root: PathBuf,
    handles: Vec<TriggerHandle>,
}

impl TriggerSupervisor {
    /// Discover, spawn, and watch every installed trigger backend plugin.
    ///
    /// Returns immediately after each plugin's initial handshake completes —
    /// per-plugin background tasks then take over receiving `trigger/event`
    /// notifications. Failures discovering or spawning an individual plugin
    /// are surfaced via `sink(StartFailed)` but do not abort the supervisor.
    pub async fn start(project_root: &Path, sink: TriggerSupervisorSink) -> Result<Self> {
        Self::start_with_runner(project_root, sink, Arc::new(ProcessTriggerRunner)).await
    }

    async fn start_with_runner(
        project_root: &Path,
        sink: TriggerSupervisorSink,
        runner: Arc<dyn TriggerPluginRunner>,
    ) -> Result<Self> {
        let trigger_plugins = discover_trigger_plugins(project_root).context("trigger plugin discovery failed")?;

        let mut handles = Vec::new();
        for plugin in trigger_plugins {
            let handle = spawn_supervised_plugin(project_root, plugin, runner.clone(), sink.clone());
            handles.push(handle);
        }

        let plugin_count = handles.len();
        sink(TriggerSupervisorEvent::Started { plugin_count });

        Ok(Self { project_root: project_root.to_path_buf(), handles })
    }

    /// Number of currently supervised plugins.
    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }

    /// Send shutdown to each supervised plugin task and wait briefly for it to
    /// exit. Inside each task the runner is responsible for acking the last
    /// observed event id before closing the underlying child.
    pub async fn shutdown(self) -> Result<()> {
        for handle in self.handles {
            let _ = handle.shutdown_tx.send(()).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), handle.task).await;
        }
        debug!(project_root = %self.project_root.display(), "trigger supervisor shutdown complete");
        Ok(())
    }
}

/// Filter the project's installed plugins down to trigger backends.
pub fn discover_trigger_plugins(project_root: &Path) -> Result<Vec<DiscoveredPlugin>> {
    let plugins = discover_plugins(project_root)?;
    Ok(plugins.into_iter().filter(|p| p.manifest.plugin_kind == PLUGIN_KIND_TRIGGER_BACKEND).collect())
}

/// Outcome reported by one trigger plugin session.
#[derive(Debug, Clone)]
pub enum SessionOutcome {
    /// The session was asked to terminate by the supervisor (e.g. shutdown).
    /// The outer loop should NOT respawn.
    ShutdownRequested,
    /// The plugin's stdio closed without an explicit shutdown — the supervisor
    /// should respawn (subject to the restart budget).
    StdioClosed { reason: String },
    /// The session failed to spawn / handshake / send `trigger/watch` — the
    /// supervisor should respawn (subject to the restart budget).
    SpawnError(String),
    /// The plugin reported a transport-level error mid-session. The supervisor
    /// should respawn (subject to the restart budget). Reserved for future use
    /// by runner implementations that surface mid-session protocol errors.
    #[allow(dead_code)]
    SessionError(String),
}

/// Abstraction over "run one plugin session end-to-end". The production
/// implementation spawns a real subprocess via [`PluginHost`] and drives the
/// stdio loop; tests provide a mock that scripts the sequence of outcomes
/// without ever forking a child process.
#[async_trait]
pub trait TriggerPluginRunner: Send + Sync {
    /// Run one session of the plugin until it terminates. Returns the reason
    /// the session ended so the outer supervisor can decide whether to respawn.
    ///
    /// `notify_started` MUST be called by the implementation once the session
    /// has reached steady-state (handshake complete, watch issued). The outer
    /// loop uses that signal as the start of the healthy window for resetting
    /// the attempt counter.
    async fn run_session(
        &self,
        project_root: &Path,
        plugin: &DiscoveredPlugin,
        sink: TriggerSupervisorSink,
        shutdown_rx: &mut mpsc::Receiver<()>,
        notify_started: &mut (dyn FnMut() + Send),
    ) -> SessionOutcome;
}

/// Build a stderr sink that routes every line emitted by a supervised trigger
/// plugin into the project's structured `events.jsonl` log.
///
/// Mirrors the shape used by the provider plugin path: lines land at `warn`
/// level under category `trigger.stderr` with `plugin` and `emitter` metadata
/// so an operator diagnosing a flaky Slack auth or an expired webhook secret
/// can grep the same surface they already use for provider issues.
///
/// Pure helper, exported at module scope so tests can build the sink directly
/// and read back what was written without spinning up a real plugin.
pub(crate) fn trigger_plugin_stderr_sink(project_root: &Path, plugin_name: &str) -> Option<PluginStderrSink> {
    let project_root = project_root.to_path_buf();
    let plugin_name = plugin_name.to_string();
    Some(Arc::new(move |emitting_plugin: &str, line: &str| {
        let logger = Logger::for_project(&project_root);
        logger
            .warn("trigger.stderr", line)
            .meta(json!({
                "plugin": plugin_name,
                "emitter": emitting_plugin,
            }))
            .emit();
    }))
}

/// Production runner backed by a real [`PluginHost`].
struct ProcessTriggerRunner;

#[async_trait]
impl TriggerPluginRunner for ProcessTriggerRunner {
    async fn run_session(
        &self,
        project_root: &Path,
        plugin: &DiscoveredPlugin,
        sink: TriggerSupervisorSink,
        shutdown_rx: &mut mpsc::Receiver<()>,
        notify_started: &mut (dyn FnMut() + Send),
    ) -> SessionOutcome {
        let stderr_sink = trigger_plugin_stderr_sink(project_root, &plugin.name);
        let options = PluginSpawnOptions::for_manifest(
            plugin.name.clone(),
            &plugin.manifest.env_required,
            std::iter::empty::<String>(),
            stderr_sink,
        )
        .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
        .with_working_dir(project_root);
        let host = match PluginHost::spawn_with_options(&plugin.path, &[], options).await {
            Ok(host) => host,
            Err(error) => return SessionOutcome::SpawnError(format!("spawn failed: {error:#}")),
        };
        if let Err(error) = host.handshake().await {
            return SessionOutcome::SpawnError(format!("handshake failed: {error:#}"));
        }
        let mut notifications = host.subscribe_notifications();
        let watch = TriggerWatchParams { cursor: None, config: Some(collect_plugin_trigger_config(project_root)) };
        let watch_params = match serde_json::to_value(watch) {
            Ok(value) => value,
            Err(error) => return SessionOutcome::SpawnError(format!("encode watch params: {error}")),
        };
        let _ = host.request(TRIGGER_METHOD_WATCH, Some(watch_params)).await;

        notify_started();

        let last_event_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let plugin_name = plugin.name.clone();
        let project_root = project_root.to_path_buf();

        let outcome = loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    if let Some(last) = last_event_id.lock().await.clone() {
                        let ack = TriggerAckParams { event_id: last, status: Some(TriggerAckStatus::Shutdown) };
                        let _ = host
                            .notify(TRIGGER_METHOD_ACK, Some(serde_json::to_value(ack).unwrap_or_default()))
                            .await;
                    }
                    let _ = host.shutdown().await;
                    break SessionOutcome::ShutdownRequested;
                }
                maybe_notification = notifications.recv() => {
                    let notification = match maybe_notification {
                        Ok(notification) => notification,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(
                                plugin = %plugin_name,
                                skipped,
                                "trigger subscriber lagged behind plugin broadcast; events dropped"
                            );
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break SessionOutcome::StdioClosed {
                                reason: "plugin stdio closed".to_string(),
                            };
                        }
                    };
                    if notification.method != TRIGGER_METHOD_EVENT {
                        continue;
                    }
                    let event = match notification
                        .params
                        .and_then(|v| serde_json::from_value::<TriggerEvent>(v).ok())
                    {
                        Some(event) => event,
                        None => {
                            warn!(plugin = %plugin_name, "trigger/event notification with malformed params dropped");
                            continue;
                        }
                    };
                    let event_id = event.event_id.clone();
                    let trigger_id = event.trigger_id.clone();
                    *last_event_id.lock().await = Some(event_id.clone());
                    let routed = route_event(&project_root, &event);
                    sink(TriggerSupervisorEvent::Event {
                        plugin_name: plugin_name.clone(),
                        event_id: event_id.clone(),
                        trigger_id: trigger_id.clone(),
                        routed,
                    });
                    let ack = TriggerAckParams {
                        event_id,
                        status: Some(if routed { TriggerAckStatus::Queued } else { TriggerAckStatus::Unmatched }),
                    };
                    let _ = host
                        .notify(TRIGGER_METHOD_ACK, Some(serde_json::to_value(ack).unwrap_or_default()))
                        .await;
                }
            }
        };

        debug!(plugin = %plugin_name, ?outcome, "trigger plugin watch session exited");
        outcome
    }
}

fn spawn_supervised_plugin(
    project_root: &Path,
    plugin: DiscoveredPlugin,
    runner: Arc<dyn TriggerPluginRunner>,
    sink: TriggerSupervisorSink,
) -> TriggerHandle {
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
    let project_root = project_root.to_path_buf();
    let plugin_name = plugin.name.clone();
    let kill_switch: KillSwitch = Arc::new(triggers_disabled_env);
    let task = tokio::spawn(supervise_plugin_loop(project_root, plugin, runner, sink, shutdown_rx, kill_switch));
    TriggerHandle { plugin_name, task, shutdown_tx }
}

/// Predicate the supervise loop polls between sessions and during backoff
/// sleeps. The production wiring reads `ANIMUS_DAEMON_DISABLE_TRIGGERS`; tests
/// inject an in-process `AtomicBool` so they don't race on global env state.
type KillSwitch = Arc<dyn Fn() -> bool + Send + Sync>;

/// Outer retry loop for a single supervised plugin. Each iteration delegates
/// to [`TriggerPluginRunner::run_session`] for one end-to-end session, then
/// reacts to its outcome.
async fn supervise_plugin_loop(
    project_root: PathBuf,
    plugin: DiscoveredPlugin,
    runner: Arc<dyn TriggerPluginRunner>,
    sink: TriggerSupervisorSink,
    mut shutdown_rx: mpsc::Receiver<()>,
    kill_switch: KillSwitch,
) {
    let plugin_name = plugin.name.clone();
    let mut attempts: u32 = 0;

    loop {
        // Respect the operator kill-switch at every iteration.
        if kill_switch() {
            debug!(plugin = %plugin_name, "trigger kill-switch tripped; supervisor exiting");
            return;
        }

        let session_start = Instant::now();
        let session_started = Arc::new(AtomicBool::new(false));
        let session_started_for_cb = session_started.clone();
        let mut notify_started = move || {
            session_started_for_cb.store(true, Ordering::SeqCst);
        };
        let outcome =
            runner.run_session(&project_root, &plugin, sink.clone(), &mut shutdown_rx, &mut notify_started).await;
        let session_reached_started = session_started.load(Ordering::SeqCst);

        match outcome {
            SessionOutcome::ShutdownRequested => {
                debug!(plugin = %plugin_name, "supervisor session ended by shutdown");
                return;
            }
            SessionOutcome::StdioClosed { reason } | SessionOutcome::SessionError(reason) => {
                let was_healthy = session_reached_started && session_start.elapsed() >= HEALTHY_WINDOW;
                if was_healthy {
                    // A session that ran past the healthy window before exiting
                    // doesn't count against the restart budget. Reset the
                    // counter and respawn immediately without any backoff.
                    attempts = 0;
                    debug!(
                        plugin = %plugin_name,
                        reason = %reason,
                        "trigger plugin closed cleanly after healthy window; respawning without backoff"
                    );
                    continue;
                }
                attempts = attempts.saturating_add(1);

                if attempts >= MAX_RESTART_ATTEMPTS {
                    warn!(
                        plugin = %plugin_name,
                        attempts,
                        reason = %reason,
                        "trigger plugin exhausted restart budget; to suppress further restart attempts at daemon startup, set ANIMUS_DAEMON_DISABLE_TRIGGERS=1"
                    );
                    sink(TriggerSupervisorEvent::Crashed {
                        plugin_name: plugin_name.clone(),
                        attempts,
                        error: format!(
                            "{reason}; to suppress further restart attempts at daemon startup, set ANIMUS_DAEMON_DISABLE_TRIGGERS=1"
                        ),
                    });
                    return;
                }

                let delay = backoff_for_attempt(attempts);
                sink(TriggerSupervisorEvent::Restart {
                    plugin_name: plugin_name.clone(),
                    attempt: attempts,
                    delay_ms: delay.as_millis() as u64,
                });
                if !sleep_with_interrupts(delay, &mut shutdown_rx, kill_switch.as_ref()).await {
                    return;
                }
            }
            SessionOutcome::SpawnError(reason) => {
                // SpawnError means we never reached steady state. Treat exactly
                // like StdioClosed for budget purposes, but log distinctly.
                attempts = attempts.saturating_add(1);
                if attempts >= MAX_RESTART_ATTEMPTS {
                    warn!(
                        plugin = %plugin_name,
                        attempts,
                        reason = %reason,
                        "trigger plugin failed to start repeatedly; to suppress further restart attempts at daemon startup, set ANIMUS_DAEMON_DISABLE_TRIGGERS=1"
                    );
                    sink(TriggerSupervisorEvent::Crashed {
                        plugin_name: plugin_name.clone(),
                        attempts,
                        error: format!(
                            "{reason}; to suppress further restart attempts at daemon startup, set ANIMUS_DAEMON_DISABLE_TRIGGERS=1"
                        ),
                    });
                    return;
                }
                let delay = backoff_for_attempt(attempts);
                sink(TriggerSupervisorEvent::Restart {
                    plugin_name: plugin_name.clone(),
                    attempt: attempts,
                    delay_ms: delay.as_millis() as u64,
                });
                if !sleep_with_interrupts(delay, &mut shutdown_rx, kill_switch.as_ref()).await {
                    return;
                }
            }
        }
    }
}

/// Exponential backoff for the Nth restart attempt: 1s, 2s, 4s, 8s, 16s, …
/// then clamped at [`MAX_BACKOFF`].
fn backoff_for_attempt(attempt: u32) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    // attempt 1 -> base * 2^0 = 1s
    // attempt 2 -> base * 2^1 = 2s
    let shift = attempt.saturating_sub(1);
    let base_secs = BACKOFF_BASE.as_secs();
    // Once `shift` would overflow a u64, we know we're well past the cap.
    if shift >= 63 {
        return MAX_BACKOFF;
    }
    let scaled_secs = base_secs.saturating_mul(1u64 << shift);
    let max_secs = MAX_BACKOFF.as_secs();
    Duration::from_secs(scaled_secs.min(max_secs))
}

/// Sleep for `delay` but bail out early if a shutdown signal arrives or the
/// kill switch flips on. Returns `true` if the sleep elapsed normally, `false`
/// if the outer loop should exit.
async fn sleep_with_interrupts(
    delay: Duration,
    shutdown_rx: &mut mpsc::Receiver<()>,
    kill_switch: &(dyn Fn() -> bool + Send + Sync),
) -> bool {
    if delay.is_zero() {
        return true;
    }
    // Poll the kill switch on a coarse cadence while sleeping so an operator
    // flipping the env mid-backoff doesn't have to wait the full 60s.
    let poll_step = Duration::from_millis(250);
    let mut remaining = delay;
    while !remaining.is_zero() {
        let step = std::cmp::min(poll_step, remaining);
        tokio::select! {
            biased;
            _ = shutdown_rx.recv() => return false,
            _ = tokio::time::sleep(step) => {
                remaining = remaining.saturating_sub(step);
                if kill_switch() {
                    return false;
                }
            }
        }
    }
    true
}

fn triggers_disabled_env() -> bool {
    std::env::var("ANIMUS_DAEMON_DISABLE_TRIGGERS").map(|v| v != "0" && !v.is_empty()).unwrap_or(false)
}

/// Reserved key prefix for routing metadata the supervisor injects into the
/// WebhookEvent payload. Keys under this prefix carry the
/// `TriggerEvent.subject_id`, `subject_kind`, `action_hint`, and `event_id`
/// fields that the plugin emitted — downstream tick handlers and workflow
/// YAML branch on these without losing context.
///
/// We use a double-underscore namespace because:
/// 1. JSON keys starting with `__` are universally treated as
///    "framework-reserved" by convention.
/// 2. The reserved keys are namespaced under `__animus_*` so a plugin payload
///    containing its own `subject_id` or `action_hint` cannot collide with the
///    routing metadata.
pub(crate) const RESERVED_KEY_PREFIX: &str = "__animus_";
pub(crate) const RESERVED_KEY_SUBJECT_ID: &str = "__animus_subject_id";
pub(crate) const RESERVED_KEY_SUBJECT_KIND: &str = "__animus_subject_kind";
pub(crate) const RESERVED_KEY_ACTION_HINT: &str = "__animus_action_hint";
pub(crate) const RESERVED_KEY_EVENT_ID: &str = "__animus_event_id";
pub(crate) const RESERVED_KEY_TRIGGER_ID: &str = "__animus_trigger_id";

/// Build the WebhookEvent payload from a plugin-emitted [`TriggerEvent`].
///
/// Merge strategy:
///
/// 1. Start with the plugin-supplied `event.payload` if it is an object;
///    otherwise wrap a scalar/array under `{ "value": <payload> }`.
/// 2. Strip any keys the caller supplied under the [`RESERVED_KEY_PREFIX`]
///    namespace. The `__animus_*` namespace is reserved for the supervisor;
///    a malicious or careless plugin that tries to spoof its own
///    `__animus_subject_id` is silently sandboxed by being moved under
///    `__animus_user_overrides`.
/// 3. Inject the supervisor's authoritative `subject_id`, `subject_kind`,
///    `action_hint`, `event_id`, and `trigger_id` (when present) under the
///    reserved keys.
fn build_routed_payload(event: &TriggerEvent) -> serde_json::Value {
    let user_payload = if event.payload.is_null() { serde_json::json!({}) } else { event.payload.clone() };
    let mut object = match user_payload {
        serde_json::Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        }
    };

    // Sandbox any caller-supplied reserved keys. We don't simply drop them
    // because that would silently lose data; we move them under a single
    // sub-object so downstream handlers can still see them for forensics if
    // ever needed, but they cannot impersonate the supervisor's metadata.
    let mut sandboxed: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let conflicting_keys: Vec<String> = object.keys().filter(|k| k.starts_with(RESERVED_KEY_PREFIX)).cloned().collect();
    for key in conflicting_keys {
        if let Some(value) = object.remove(&key) {
            sandboxed.insert(key, value);
        }
    }
    if !sandboxed.is_empty() {
        object.insert("__animus_user_overrides".to_string(), serde_json::Value::Object(sandboxed));
    }

    if let Some(ref subject_id) = event.subject_id {
        object.insert(RESERVED_KEY_SUBJECT_ID.to_string(), serde_json::Value::String(subject_id.clone()));
    }
    if let Some(ref subject_kind) = event.subject_kind {
        object.insert(RESERVED_KEY_SUBJECT_KIND.to_string(), serde_json::Value::String(subject_kind.clone()));
    }
    if let Some(ref action_hint) = event.action_hint {
        object
            .insert(RESERVED_KEY_ACTION_HINT.to_string(), serde_json::Value::String(action_hint.as_str().to_string()));
    }
    object.insert(RESERVED_KEY_EVENT_ID.to_string(), serde_json::Value::String(event.event_id.clone()));
    if let Some(ref trigger_id) = event.trigger_id {
        object.insert(RESERVED_KEY_TRIGGER_ID.to_string(), serde_json::Value::String(trigger_id.clone()));
    }

    serde_json::Value::Object(object)
}

/// Append a plugin-emitted event to the matching trigger's `pending_events`.
///
/// Returns `true` if the event was queued against a known trigger id, `false`
/// otherwise. Unmatched events are logged but not enqueued — there's nowhere
/// to route them to without a configured `workflow_ref`.
///
/// The `TriggerEvent.subject_id`, `subject_kind`, `action_hint`, and
/// `event_id` fields are propagated into the WebhookEvent payload under
/// `__animus_*` reserved keys so downstream tick handlers (and workflow YAML)
/// can branch on them without losing context.
/// Build the merged `trigger/watch` config payload from every enabled
/// `type: plugin` entry in the project's workflow YAML.
///
/// The payload shape is:
///
/// ```json
/// {
///   "triggers": [
///     {"id": "fswatch-default", "workflow_ref": "...", "config": {...}, "input": {...}},
///     ...
///   ]
/// }
/// ```
///
/// Plugins read this list, decide which entries are theirs (by `id`,
/// shape of `config`, or convention) and start emitting events with
/// the matching `trigger_id`. The supervisor is intentionally agnostic
/// about per-plugin filtering — it can't reliably tell which plugin
/// owns which workflow entry without an explicit `plugin_name` field
/// on the trigger, so it ships the full list and lets each plugin
/// pick the entries it understands. Plugins SHOULD ignore unknown
/// entries silently rather than erroring.
///
/// Each entry's top-level `config` block is what authors write under
/// `triggers[].config` in YAML — for the reference fswatch plugin,
/// for example, that contains `trigger_id`, `globs`, and
/// `debounce_ms`. Bridge plugins authored against an older protocol
/// version that doesn't read this list still get an empty config
/// block and degrade gracefully.
pub(crate) fn collect_plugin_trigger_config(project_root: &Path) -> serde_json::Value {
    let workflow_config = orchestrator_core::load_workflow_config_or_default(project_root);
    let triggers: Vec<serde_json::Value> = workflow_config
        .config
        .triggers
        .iter()
        .filter(|trigger| trigger.enabled && matches!(trigger.trigger_type, TriggerType::Plugin))
        .map(|trigger| {
            let mut entry = serde_json::Map::new();
            entry.insert("id".to_string(), serde_json::Value::String(trigger.id.clone()));
            if let Some(workflow_ref) = trigger.workflow_ref.as_ref() {
                entry.insert("workflow_ref".to_string(), serde_json::Value::String(workflow_ref.clone()));
            }
            entry.insert("config".to_string(), trigger.config.clone());
            if let Some(input) = trigger.input.as_ref() {
                entry.insert("input".to_string(), input.clone());
            }
            serde_json::Value::Object(entry)
        })
        .collect();
    json!({ "triggers": triggers })
}

fn route_event(project_root: &Path, event: &TriggerEvent) -> bool {
    let Some(trigger_id) = event.trigger_id.as_deref() else {
        debug!(event_id = %event.event_id, "trigger event lacks trigger_id; dropping (router has no default workflow)");
        return false;
    };

    let config = orchestrator_core::load_workflow_config_or_default(project_root);
    let Some(trigger) = config.config.triggers.iter().find(|t| t.id == trigger_id && t.enabled) else {
        warn!(event_id = %event.event_id, trigger_id, "trigger event references unknown or disabled trigger");
        return false;
    };
    if !matches!(trigger.trigger_type, TriggerType::Plugin | TriggerType::Webhook | TriggerType::GithubWebhook) {
        warn!(
            event_id = %event.event_id,
            trigger_id,
            trigger_type = ?trigger.trigger_type,
            "trigger event routed to non-pluggable trigger type"
        );
        return false;
    }

    let mut state: orchestrator_core::TriggerState =
        orchestrator_core::load_trigger_state(project_root).unwrap_or_default();
    let run_state = state.triggers.entry(trigger_id.to_string()).or_default();
    let payload = build_routed_payload(event);
    run_state.pending_events.push(WebhookEvent { event_id: event.event_id.clone(), received_at: Utc::now(), payload });

    // Trigger backlog cap (v0.4.13): bound the per-trigger pending queue at
    // `RuntimeQuotas::trigger_backlog_max`. Excess events are dropped from
    // the front (oldest first) with a `tracing::warn!` so an operator
    // diagnosing a wedged trigger can see the loss in `events.jsonl`.
    let backlog_cap = crate::quotas::runtime_quotas().trigger_backlog_max;
    if backlog_cap > 0 && run_state.pending_events.len() > backlog_cap {
        let overflow = run_state.pending_events.len() - backlog_cap;
        let dropped: Vec<WebhookEvent> = run_state.pending_events.drain(..overflow).collect();
        for dropped_event in &dropped {
            warn!(
                trigger_id,
                event_id = %dropped_event.event_id,
                cap = backlog_cap,
                "trigger backlog exceeded cap; dropping oldest event"
            );
        }
        crate::metrics::incr(&crate::metrics::labeled("trigger_backlog_dropped_total", &[("trigger_id", trigger_id)]));
    }

    if let Err(error) = orchestrator_core::save_trigger_state(project_root, &state) {
        warn!(trigger_id, error = %error, "failed to persist plugin trigger event");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_plugin_protocol::PluginManifest;
    use orchestrator_core::workflow_config::WorkflowTrigger;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use tempfile::tempdir;

    fn write_plugin_trigger_config(project_root: &Path, trigger_id: &str) {
        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "plugin-flow".to_string(),
            name: "Plugin Flow".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            post_success: None,
            variables: Vec::new(),
            worktree: None,
            budget: None,
        });
        config.triggers.push(WorkflowTrigger {
            id: trigger_id.to_string(),
            trigger_type: TriggerType::Plugin,
            workflow_ref: Some("plugin-flow".to_string()),
            enabled: true,
            config: json!({}),
            input: None,
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("write workflow config");
    }

    #[test]
    fn collect_plugin_trigger_config_emits_enabled_plugin_entries() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "plugin-flow".to_string(),
            name: "Plugin Flow".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            post_success: None,
            variables: Vec::new(),
            worktree: None,
            budget: None,
        });
        config.triggers.push(WorkflowTrigger {
            id: "fswatch-default".to_string(),
            trigger_type: TriggerType::Plugin,
            workflow_ref: Some("plugin-flow".to_string()),
            enabled: true,
            config: json!({"trigger_id": "fswatch-default", "globs": ["src/**/*.rs"]}),
            input: None,
        });
        config.triggers.push(WorkflowTrigger {
            id: "disabled".to_string(),
            trigger_type: TriggerType::Plugin,
            workflow_ref: Some("plugin-flow".to_string()),
            enabled: false,
            config: json!({}),
            input: None,
        });
        config.triggers.push(WorkflowTrigger {
            id: "ignored-webhook".to_string(),
            trigger_type: TriggerType::Webhook,
            workflow_ref: Some("plugin-flow".to_string()),
            enabled: true,
            config: json!({"max_triggers_per_minute": 10}),
            input: None,
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("write workflow config");

        let payload = collect_plugin_trigger_config(project_root);
        let triggers = payload.get("triggers").and_then(|v| v.as_array()).expect("triggers array");
        assert_eq!(triggers.len(), 1, "only enabled plugin trigger should be included");
        let entry = &triggers[0];
        assert_eq!(entry.get("id").and_then(|v| v.as_str()), Some("fswatch-default"));
        assert_eq!(entry.get("workflow_ref").and_then(|v| v.as_str()), Some("plugin-flow"));
        assert_eq!(entry.pointer("/config/trigger_id").and_then(|v| v.as_str()), Some("fswatch-default"));
    }

    #[test]
    fn route_event_queues_payload_for_known_plugin_trigger() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        write_plugin_trigger_config(project_root, "slack-incoming");

        let event = TriggerEvent {
            event_id: "evt-1".to_string(),
            trigger_id: Some("slack-incoming".to_string()),
            subject_id: None,
            subject_kind: None,
            action_hint: None,
            payload: json!({ "text": "hello from slack" }),
        };

        let routed = route_event(project_root, &event);

        assert!(routed, "event with matching trigger id should be routed");
        let state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        let run = state.triggers.get("slack-incoming").expect("run state");
        assert_eq!(run.pending_events.len(), 1);
        assert_eq!(run.pending_events[0].event_id, "evt-1");
        // User payload survives intact; reserved __animus_event_id/__animus_trigger_id
        // are injected by the router so downstream handlers can correlate.
        let payload = &run.pending_events[0].payload;
        assert_eq!(payload["text"], json!("hello from slack"));
        assert_eq!(payload[RESERVED_KEY_EVENT_ID], json!("evt-1"));
        assert_eq!(payload[RESERVED_KEY_TRIGGER_ID], json!("slack-incoming"));
        // subject_id/subject_kind/action_hint were None on the event so they
        // must NOT appear under reserved keys at all.
        assert!(payload.get(RESERVED_KEY_SUBJECT_ID).is_none());
        assert!(payload.get(RESERVED_KEY_SUBJECT_KIND).is_none());
        assert!(payload.get(RESERVED_KEY_ACTION_HINT).is_none());
    }

    // ---------- Gap #9 — subject_id / action_hint propagation tests ----------

    #[test]
    fn routes_event_with_subject_id_into_payload() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        write_plugin_trigger_config(project_root, "linear-incoming");

        let event = TriggerEvent {
            event_id: "evt-linear-1".to_string(),
            trigger_id: Some("linear-incoming".to_string()),
            subject_id: Some("linear:ENG-123".to_string()),
            subject_kind: Some("issue".to_string()),
            action_hint: None,
            payload: json!({ "title": "fix login bug" }),
        };

        assert!(route_event(project_root, &event));
        let state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        let payload = &state.triggers.get("linear-incoming").expect("run state").pending_events[0].payload;
        assert_eq!(payload[RESERVED_KEY_SUBJECT_ID], json!("linear:ENG-123"));
        assert_eq!(payload[RESERVED_KEY_SUBJECT_KIND], json!("issue"));
        assert_eq!(payload[RESERVED_KEY_EVENT_ID], json!("evt-linear-1"));
        // User payload intact.
        assert_eq!(payload["title"], json!("fix login bug"));
    }

    #[test]
    fn routes_event_with_action_hint_into_payload() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        write_plugin_trigger_config(project_root, "slack-incoming");

        let event = TriggerEvent {
            event_id: "evt-mention-1".to_string(),
            trigger_id: Some("slack-incoming".to_string()),
            subject_id: None,
            subject_kind: None,
            action_hint: Some(animus_plugin_protocol::TriggerActionHint::CreateTask),
            payload: json!({ "user": "alice", "text": "@bot create a task for the login bug" }),
        };

        assert!(route_event(project_root, &event));
        let state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        let payload = &state.triggers.get("slack-incoming").expect("run state").pending_events[0].payload;
        assert_eq!(payload[RESERVED_KEY_ACTION_HINT], json!("create_task"));
        assert_eq!(payload["user"], json!("alice"));
        assert_eq!(payload["text"], json!("@bot create a task for the login bug"));
    }

    #[test]
    fn merges_user_payload_with_reserved_keys_correctly() {
        // build_routed_payload is the unit under test here — exercises the
        // pure merge without going through workflow-config plumbing.
        let event = TriggerEvent {
            event_id: "evt-merge".to_string(),
            trigger_id: Some("merge-trigger".to_string()),
            subject_id: Some("foo".to_string()),
            subject_kind: None,
            action_hint: None,
            payload: json!({ "x": 1 }),
        };
        let merged = build_routed_payload(&event);
        // User key survives; reserved keys land alongside it.
        assert_eq!(merged["x"], json!(1));
        assert_eq!(merged[RESERVED_KEY_SUBJECT_ID], json!("foo"));
        assert_eq!(merged[RESERVED_KEY_EVENT_ID], json!("evt-merge"));
        assert_eq!(merged[RESERVED_KEY_TRIGGER_ID], json!("merge-trigger"));
        // Absent optional fields must NOT show up in the payload at all.
        assert!(merged.get(RESERVED_KEY_SUBJECT_KIND).is_none());
        assert!(merged.get(RESERVED_KEY_ACTION_HINT).is_none());
    }

    #[test]
    fn does_not_overwrite_user_payload_with_same_reserved_key() {
        // A plugin that tries to spoof __animus_subject_id in its own payload
        // must NOT be able to override the supervisor's authoritative value.
        // Caller-supplied reserved keys are sandboxed under
        // __animus_user_overrides for forensics, never honored as routing
        // metadata.
        let event = TriggerEvent {
            event_id: "evt-malicious".to_string(),
            trigger_id: Some("trigger-x".to_string()),
            subject_id: Some("authoritative-subject".to_string()),
            subject_kind: None,
            action_hint: None,
            payload: json!({
                "__animus_subject_id": "spoofed-subject",
                "__animus_action_hint": "spoofed_action",
                "normal_key": "ok"
            }),
        };
        let merged = build_routed_payload(&event);
        // Authoritative supervisor value wins.
        assert_eq!(merged[RESERVED_KEY_SUBJECT_ID], json!("authoritative-subject"));
        // The plugin did NOT supply action_hint, so the spoofed value must
        // not bleed into the action_hint slot — the action_hint key must be
        // absent from the top level.
        assert!(
            merged.get(RESERVED_KEY_ACTION_HINT).is_none(),
            "spoofed __animus_action_hint must not be promoted to top level when event.action_hint is None"
        );
        // User's legitimate non-reserved key survives.
        assert_eq!(merged["normal_key"], json!("ok"));
        // Spoofed reserved keys are sandboxed under __animus_user_overrides.
        let overrides = merged
            .get("__animus_user_overrides")
            .expect("sandboxed overrides should be present when conflicts occurred");
        assert_eq!(overrides["__animus_subject_id"], json!("spoofed-subject"));
        assert_eq!(overrides["__animus_action_hint"], json!("spoofed_action"));
    }

    #[test]
    fn trigger_backlog_drops_oldest_when_cap_exceeded() {
        // v0.4.13: when the per-trigger pending_events queue exceeds the
        // cap from `RuntimeQuotas::trigger_backlog_max`, the oldest events
        // are dropped so the queue stays bounded. We exercise this by
        // installing a tiny cap, pushing more than that, and asserting the
        // queue ends at exactly the cap with the newest event_ids retained.
        //
        // The cap defaults to 1000 in `RuntimeQuotas::default()` so this
        // test exercises the install_runtime_quotas override path. Because
        // the OnceLock is process-global we install the smallest
        // possible-but-useful cap to keep the test deterministic without
        // colliding with other tests that may be reading the quotas.
        let _ = crate::quotas::install_runtime_quotas(crate::quotas::RuntimeQuotas {
            trigger_backlog_max: 3,
            ..crate::quotas::RuntimeQuotas::default()
        });
        // Tolerate either outcome of the install race: if some prior test
        // pinned the global to a different cap, we observe what the
        // process-wide reader sees and adapt the assertions to that.
        let observed_cap = crate::quotas::runtime_quotas().trigger_backlog_max;
        // Skip the test cleanly if the observed cap is too large to drive
        // overflow in a reasonable number of pushes — this still proves
        // the override-or-default semantics without flaking under
        // arbitrary process-global state.
        if observed_cap > 50 {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        write_plugin_trigger_config(project_root, "noisy-trigger");

        let total_pushes = observed_cap + 2;
        for i in 0..total_pushes {
            let event = TriggerEvent {
                event_id: format!("evt-{i}"),
                trigger_id: Some("noisy-trigger".to_string()),
                subject_id: None,
                subject_kind: None,
                action_hint: None,
                payload: json!({"i": i}),
            };
            assert!(route_event(project_root, &event), "event {i} should route under cap");
        }

        let state = orchestrator_core::load_trigger_state(project_root).expect("load state");
        let run = state.triggers.get("noisy-trigger").expect("run state");
        assert_eq!(run.pending_events.len(), observed_cap, "queue must be clamped at the cap");
        // The retained events should be the newest ones; the first
        // `total_pushes - observed_cap` should have been dropped.
        let first_retained_index = total_pushes - observed_cap;
        let first_retained = &run.pending_events[0];
        assert_eq!(first_retained.event_id, format!("evt-{first_retained_index}"));
        let last_retained = run.pending_events.last().expect("at least one event");
        assert_eq!(last_retained.event_id, format!("evt-{}", total_pushes - 1));
    }

    #[test]
    fn route_event_drops_event_without_trigger_id() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        write_plugin_trigger_config(project_root, "slack-incoming");

        let event = TriggerEvent {
            event_id: "evt-bare".to_string(),
            trigger_id: None,
            subject_id: None,
            subject_kind: None,
            action_hint: None,
            payload: json!({}),
        };

        let routed = route_event(project_root, &event);

        assert!(!routed, "event without trigger_id has nowhere to go");
        let state = orchestrator_core::load_trigger_state(project_root).unwrap_or_default();
        assert!(state.triggers.is_empty());
    }

    #[test]
    fn route_event_drops_event_for_unknown_trigger_id() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        write_plugin_trigger_config(project_root, "slack-incoming");

        let event = TriggerEvent {
            event_id: "evt-orphan".to_string(),
            trigger_id: Some("does-not-exist".to_string()),
            subject_id: None,
            subject_kind: None,
            action_hint: None,
            payload: json!({}),
        };

        let routed = route_event(project_root, &event);
        assert!(!routed);
    }

    #[test]
    fn route_event_refuses_file_watcher_trigger_target() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();

        // Hand-craft a config with a file_watcher trigger named the same as
        // the plugin event's trigger_id — events should refuse to route there.
        let mut config = orchestrator_core::builtin_workflow_config();
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "fw-flow".to_string(),
            name: "FW".to_string(),
            description: String::new(),
            phases: vec![orchestrator_core::WorkflowPhaseEntry::Simple("requirements".to_string())],
            post_success: None,
            variables: Vec::new(),
            worktree: None,
            budget: None,
        });
        config.triggers.push(WorkflowTrigger {
            id: "fw-collision".to_string(),
            trigger_type: TriggerType::FileWatcher,
            workflow_ref: Some("fw-flow".to_string()),
            enabled: true,
            config: json!({ "paths": ["**/*.rs"] }),
            input: None,
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("write");

        let event = TriggerEvent {
            event_id: "evt-fw".to_string(),
            trigger_id: Some("fw-collision".to_string()),
            subject_id: None,
            subject_kind: None,
            action_hint: None,
            payload: json!({}),
        };
        assert!(!route_event(project_root, &event));
    }

    #[tokio::test]
    async fn supervisor_starts_with_zero_plugins_emits_started_event() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        let events: Arc<StdMutex<Vec<TriggerSupervisorEvent>>> = Arc::new(StdMutex::new(Vec::new()));
        let events_clone = events.clone();
        let sink: TriggerSupervisorSink = Arc::new(move |event| {
            if let Ok(mut guard) = events_clone.lock() {
                guard.push(event);
            }
        });

        let supervisor = TriggerSupervisor::start(project_root, sink).await.expect("supervisor start");
        assert_eq!(supervisor.handle_count(), 0);

        let captured = events.lock().unwrap().clone();
        assert!(captured.iter().any(|e| matches!(e, TriggerSupervisorEvent::Started { plugin_count: 0 })));

        supervisor.shutdown().await.expect("shutdown");
    }

    // ---------- Restart-loop tests ----------

    /// Build a synthetic `DiscoveredPlugin` we can reference in tests without
    /// ever spawning a subprocess.
    fn fake_discovered_plugin(name: &str) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{name}")),
            manifest: PluginManifest {
                name: name.to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: PLUGIN_KIND_TRIGGER_BACKEND.to_string(),
                description: "fake trigger plugin for unit tests".to_string(),
                protocol_version: animus_plugin_protocol::PROTOCOL_VERSION.to_string(),
                capabilities: Vec::new(),
                env_required: Vec::new(),
                notification_buffer_size: None,
            },
            source: orchestrator_plugin_host::DiscoverySource::ExplicitConfig,
        }
    }

    fn make_event_sink() -> (TriggerSupervisorSink, Arc<StdMutex<Vec<TriggerSupervisorEvent>>>) {
        let bucket: Arc<StdMutex<Vec<TriggerSupervisorEvent>>> = Arc::new(StdMutex::new(Vec::new()));
        let bucket_clone = bucket.clone();
        let sink: TriggerSupervisorSink = Arc::new(move |event| {
            if let Ok(mut g) = bucket_clone.lock() {
                g.push(event);
            }
        });
        (sink, bucket)
    }

    fn count_restart_events(events: &[TriggerSupervisorEvent]) -> usize {
        events.iter().filter(|e| matches!(e, TriggerSupervisorEvent::Restart { .. })).count()
    }

    fn count_crashed_events(events: &[TriggerSupervisorEvent]) -> usize {
        events.iter().filter(|e| matches!(e, TriggerSupervisorEvent::Crashed { .. })).count()
    }

    /// A scriptable fake runner. Each call to `run_session` pops the next
    /// scripted outcome and (optionally) sleeps for an "elapsed" duration so
    /// the healthy-window math gets exercised.
    struct ScriptedRunner {
        script: StdMutex<Vec<ScriptStep>>,
        call_count: Arc<StdMutex<u32>>,
    }

    #[derive(Clone)]
    struct ScriptStep {
        /// How long this session should appear to run before terminating. We
        /// advance Tokio's mock clock by this much to simulate uptime.
        run_for: Duration,
        /// Whether the session reached steady state (handshake + watch) before
        /// ending. Used by the supervisor's healthy-window logic.
        reached_started: bool,
        /// What outcome to report when the session ends.
        outcome: SessionOutcome,
    }

    impl ScriptedRunner {
        fn new(steps: Vec<ScriptStep>) -> Arc<Self> {
            Arc::new(Self { script: StdMutex::new(steps), call_count: Arc::new(StdMutex::new(0)) })
        }

        fn call_count(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl TriggerPluginRunner for ScriptedRunner {
        async fn run_session(
            &self,
            _project_root: &Path,
            _plugin: &DiscoveredPlugin,
            _sink: TriggerSupervisorSink,
            shutdown_rx: &mut mpsc::Receiver<()>,
            notify_started: &mut (dyn FnMut() + Send),
        ) -> SessionOutcome {
            let step = {
                let mut script = self.script.lock().unwrap();
                if script.is_empty() {
                    // Once the script is exhausted, default to a clean stdio
                    // close. The supervisor's restart budget will eventually
                    // give up and stop calling us.
                    ScriptStep {
                        run_for: Duration::from_millis(0),
                        reached_started: true,
                        outcome: SessionOutcome::StdioClosed { reason: "fake script exhausted".to_string() },
                    }
                } else {
                    script.remove(0)
                }
            };
            {
                let mut c = self.call_count.lock().unwrap();
                *c += 1;
            }
            if step.reached_started {
                notify_started();
            }
            if !step.run_for.is_zero() {
                // Race the simulated uptime against an explicit shutdown so
                // the test can still bail out promptly.
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => return SessionOutcome::ShutdownRequested,
                    _ = tokio::time::sleep(step.run_for) => {}
                }
            }
            step.outcome
        }
    }

    fn always_alive_kill_switch() -> KillSwitch {
        Arc::new(|| false)
    }

    /// Drive the supervise loop in isolation. Returns the bucket of emitted
    /// events once the loop exits.
    async fn run_supervise_loop(plugin_name: &str, runner: Arc<ScriptedRunner>) -> Vec<TriggerSupervisorEvent> {
        run_supervise_loop_with_kill_switch(plugin_name, runner, always_alive_kill_switch()).await
    }

    async fn run_supervise_loop_with_kill_switch(
        plugin_name: &str,
        runner: Arc<ScriptedRunner>,
        kill_switch: KillSwitch,
    ) -> Vec<TriggerSupervisorEvent> {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path().to_path_buf();
        let plugin = fake_discovered_plugin(plugin_name);
        let (sink, events_bucket) = make_event_sink();

        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        let runner_dyn: Arc<dyn TriggerPluginRunner> = runner.clone();
        let handle = tokio::spawn(supervise_plugin_loop(
            project_root,
            plugin,
            runner_dyn,
            sink.clone(),
            shutdown_rx,
            kill_switch,
        ));

        // Run until completion. Bail with a hard ceiling so a runaway loop in
        // the implementation fails the test rather than hanging.
        match tokio::time::timeout(Duration::from_mins(10), handle).await {
            Ok(Ok(())) => {}
            Ok(Err(join)) => panic!("supervise loop panicked: {join}"),
            Err(_) => {
                let _ = shutdown_tx.send(()).await;
                panic!("supervise loop hung past 600s of mock time");
            }
        }

        let snapshot = events_bucket.lock().unwrap().clone();
        snapshot
    }

    #[tokio::test(start_paused = true)]
    async fn respawns_after_clean_stdio_close() {
        // Script: first session closes stdio cleanly after some uptime; second
        // session also closes cleanly after some uptime; etc — eventually the
        // restart budget is exhausted. We assert the runner was invoked
        // multiple times (proving the respawn actually happened) and that
        // each respawn emitted a Restart event.
        let runner = ScriptedRunner::new(vec![
            ScriptStep {
                run_for: Duration::from_secs(1),
                reached_started: true,
                outcome: SessionOutcome::StdioClosed { reason: "scripted close #1".to_string() },
            },
            ScriptStep {
                run_for: Duration::from_secs(1),
                reached_started: true,
                outcome: SessionOutcome::StdioClosed { reason: "scripted close #2".to_string() },
            },
        ]);

        let _events = run_supervise_loop("respawn-clean", runner.clone()).await;
        // Even with only two scripted steps the loop keeps calling run_session
        // (defaulted to clean close) until MAX_RESTART_ATTEMPTS is hit. So
        // run_session is invoked exactly MAX_RESTART_ATTEMPTS times.
        assert_eq!(runner.call_count(), MAX_RESTART_ATTEMPTS, "supervisor should respawn until budget exhausted");
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_max_restart_attempts() {
        // Plugin that always crashes immediately. Assert run_session is called
        // exactly MAX_RESTART_ATTEMPTS times and we emit a final Crashed event.
        let runner = ScriptedRunner::new(
            (0..(MAX_RESTART_ATTEMPTS + 5))
                .map(|i| ScriptStep {
                    run_for: Duration::from_millis(0),
                    reached_started: false,
                    outcome: SessionOutcome::SpawnError(format!("always-broken attempt {i}")),
                })
                .collect(),
        );

        let events = run_supervise_loop("always-broken", runner.clone()).await;
        assert_eq!(
            runner.call_count(),
            MAX_RESTART_ATTEMPTS,
            "supervisor should stop calling run_session after MAX_RESTART_ATTEMPTS"
        );
        assert_eq!(count_crashed_events(&events), 1, "exactly one terminal Crashed event");
        // MAX_RESTART_ATTEMPTS-1 Restart events: we restart between each of
        // the first MAX_RESTART_ATTEMPTS-1 sessions and the next one, then
        // the MAX-th failure exhausts the budget and produces Crashed.
        assert_eq!(count_restart_events(&events), (MAX_RESTART_ATTEMPTS - 1) as usize);
    }

    #[tokio::test(start_paused = true)]
    async fn emits_restart_event_with_backoff_metadata() {
        let runner = ScriptedRunner::new(vec![
            ScriptStep {
                run_for: Duration::from_millis(0),
                reached_started: false,
                outcome: SessionOutcome::SpawnError("crash 1".to_string()),
            },
            ScriptStep {
                run_for: Duration::from_millis(0),
                reached_started: false,
                outcome: SessionOutcome::SpawnError("crash 2".to_string()),
            },
            ScriptStep {
                run_for: Duration::from_millis(0),
                reached_started: false,
                outcome: SessionOutcome::SpawnError("crash 3".to_string()),
            },
        ]);

        let events = run_supervise_loop("backoff-metadata", runner).await;
        let restarts: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let TriggerSupervisorEvent::Restart { plugin_name, attempt, delay_ms } = e {
                    Some((plugin_name.clone(), *attempt, *delay_ms))
                } else {
                    None
                }
            })
            .collect();
        assert!(restarts.len() >= 3, "expected at least 3 Restart events, got {}", restarts.len());
        // First three attempts: 1s, 2s, 4s
        assert_eq!(restarts[0].1, 1);
        assert_eq!(restarts[0].2, 1_000);
        assert_eq!(restarts[1].1, 2);
        assert_eq!(restarts[1].2, 2_000);
        assert_eq!(restarts[2].1, 3);
        assert_eq!(restarts[2].2, 4_000);
        assert_eq!(restarts[0].0, "backoff-metadata");
    }

    #[tokio::test(start_paused = true)]
    async fn resets_attempt_counter_after_healthy_window() {
        // First session runs > HEALTHY_WINDOW then closes cleanly. Subsequent
        // sessions all crash immediately. The counter should reset after the
        // healthy session, so we get the FULL MAX_RESTART_ATTEMPTS budget for
        // the post-healthy run instead of fewer.
        let mut steps = vec![ScriptStep {
            run_for: HEALTHY_WINDOW + Duration::from_secs(5),
            reached_started: true,
            outcome: SessionOutcome::StdioClosed { reason: "clean after long uptime".to_string() },
        }];
        for i in 0..(MAX_RESTART_ATTEMPTS + 3) {
            steps.push(ScriptStep {
                run_for: Duration::from_millis(0),
                reached_started: false,
                outcome: SessionOutcome::SpawnError(format!("post-healthy crash {i}")),
            });
        }
        let runner = ScriptedRunner::new(steps);

        let events = run_supervise_loop("flaky", runner.clone()).await;
        // Expected total invocations: 1 healthy session + MAX_RESTART_ATTEMPTS
        // crash sessions (the post-healthy series fully refills the budget).
        assert_eq!(
            runner.call_count(),
            1 + MAX_RESTART_ATTEMPTS,
            "healthy window should fully reset the attempt counter"
        );
        assert_eq!(count_crashed_events(&events), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_caps_at_60s() {
        // The schedule is 1, 2, 4, 8, 16, then capped at 60s. With
        // MAX_RESTART_ATTEMPTS == 5 we don't reach the cap via the supervisor
        // alone, so verify the math directly. (Also exercise it via the loop
        // to ensure no Restart event exceeds the cap.)
        for attempt in 0..20u32 {
            let delay = backoff_for_attempt(attempt);
            assert!(
                delay <= MAX_BACKOFF,
                "attempt {attempt} produced delay {:?} > MAX_BACKOFF {:?}",
                delay,
                MAX_BACKOFF
            );
        }
        // Spot-check known values.
        assert_eq!(backoff_for_attempt(1), Duration::from_secs(1));
        assert_eq!(backoff_for_attempt(2), Duration::from_secs(2));
        assert_eq!(backoff_for_attempt(3), Duration::from_secs(4));
        assert_eq!(backoff_for_attempt(4), Duration::from_secs(8));
        assert_eq!(backoff_for_attempt(5), Duration::from_secs(16));
        assert_eq!(backoff_for_attempt(6), Duration::from_secs(32));
        assert_eq!(backoff_for_attempt(7), MAX_BACKOFF);
        assert_eq!(backoff_for_attempt(50), MAX_BACKOFF);

        // And make sure no Restart event the loop emits ever advertises a
        // delay above the cap.
        let runner = ScriptedRunner::new(
            (0..MAX_RESTART_ATTEMPTS)
                .map(|i| ScriptStep {
                    run_for: Duration::from_millis(0),
                    reached_started: false,
                    outcome: SessionOutcome::SpawnError(format!("crash {i}")),
                })
                .collect(),
        );
        let events = run_supervise_loop("capped", runner).await;
        for event in &events {
            if let TriggerSupervisorEvent::Restart { delay_ms, .. } = event {
                assert!(
                    *delay_ms <= MAX_BACKOFF.as_millis() as u64,
                    "supervisor advertised delay {delay_ms}ms > {}ms cap",
                    MAX_BACKOFF.as_millis()
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn respect_animus_daemon_disable_triggers_during_restart() {
        // First session crashes immediately. During the backoff sleep before
        // the second spawn the operator flips the kill switch — the loop
        // should abort cleanly instead of respawning.
        let runner = ScriptedRunner::new(vec![
            ScriptStep {
                run_for: Duration::from_millis(0),
                reached_started: false,
                outcome: SessionOutcome::SpawnError("crash before disable".to_string()),
            },
            // Subsequent steps should never be consumed.
            ScriptStep {
                run_for: Duration::from_millis(0),
                reached_started: false,
                outcome: SessionOutcome::SpawnError("should not reach".to_string()),
            },
        ]);

        let kill_flag = Arc::new(AtomicBool::new(false));
        let kill_flag_clone = kill_flag.clone();
        let kill_switch: KillSwitch = Arc::new(move || kill_flag_clone.load(Ordering::SeqCst));

        // Flip the switch on a delay that lands inside the first backoff (1s)
        // so the supervisor observes it mid-sleep.
        let trip = kill_flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            trip.store(true, Ordering::SeqCst);
        });

        let events = run_supervise_loop_with_kill_switch("disable-mid-backoff", runner.clone(), kill_switch).await;

        assert_eq!(runner.call_count(), 1, "supervisor should not respawn after kill switch trips");
        assert_eq!(count_crashed_events(&events), 0, "kill-switch exit is not a crash; should not emit Crashed event");
    }

    #[tokio::test(start_paused = true)]
    async fn final_crashed_event_includes_total_attempts() {
        let runner = ScriptedRunner::new(
            (0..MAX_RESTART_ATTEMPTS)
                .map(|i| ScriptStep {
                    run_for: Duration::from_millis(0),
                    reached_started: false,
                    outcome: SessionOutcome::SpawnError(format!("final-crash attempt {i}")),
                })
                .collect(),
        );
        let events = run_supervise_loop("final", runner).await;
        let crashed = events
            .iter()
            .find_map(|e| {
                if let TriggerSupervisorEvent::Crashed { plugin_name, attempts, error } = e {
                    Some((plugin_name.clone(), *attempts, error.clone()))
                } else {
                    None
                }
            })
            .expect("Crashed event should be emitted");
        assert_eq!(crashed.0, "final");
        assert_eq!(crashed.1, MAX_RESTART_ATTEMPTS);
        assert!(
            crashed.2.contains("final-crash attempt"),
            "Crashed event should carry the last attempt's error; got: {}",
            crashed.2
        );
        // Gap #12 audit: the final Crashed payload must point operators at the
        // kill-switch env var so they don't have to read the source during an
        // incident to find the escape hatch.
        assert!(
            crashed.2.contains("ANIMUS_DAEMON_DISABLE_TRIGGERS=1"),
            "Crashed event should include the kill-switch env-var hint; got: {}",
            crashed.2
        );
    }

    // ---- Gap #14: trigger plugin stderr routing -------------------------

    /// Drives the trigger stderr sink directly and verifies a line shows up in
    /// the project's `events.jsonl` tagged `trigger.stderr` with the plugin
    /// name carried as metadata. Skipping the real plugin subprocess keeps the
    /// test deterministic — we only care that the sink wiring routes through
    /// the project Logger.
    #[test]
    fn trigger_plugin_stderr_routed_to_events_jsonl() {
        use orchestrator_logging::Level;

        let temp = tempdir().expect("tempdir");
        let project_root = temp.path();
        let plugin_name = "animus-plugin-flaky-slack";

        let sink = trigger_plugin_stderr_sink(project_root, plugin_name).expect("sink should be built");
        sink(plugin_name, "AuthenticationError: token expired");
        sink(plugin_name, "ConnectionReset: peer hung up");

        let logger = Logger::for_project(project_root);
        let entries = logger.read_entries(16, Some("trigger.stderr"), Some(Level::Warn));

        assert_eq!(entries.len(), 2, "expected exactly the two stderr lines we emitted to the sink");
        let first = &entries[0];
        assert_eq!(first.cat, "trigger.stderr");
        assert_eq!(first.level, Level::Warn);
        assert!(
            first.msg.contains("AuthenticationError"),
            "first message should carry the stderr line text; got: {}",
            first.msg
        );
        let meta = first.meta.as_ref().expect("trigger.stderr entries should carry plugin metadata");
        assert_eq!(meta.get("plugin").and_then(|v| v.as_str()), Some(plugin_name));
        assert_eq!(meta.get("emitter").and_then(|v| v.as_str()), Some(plugin_name));
    }
}
