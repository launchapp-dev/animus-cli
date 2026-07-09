//! `SessionBackend` adapter that dispatches `agent/run` to a discovered Animus STDIO plugin.
//!
//! Each call spawns the plugin binary, completes the handshake, sends an `agent/run`
//! request, drains the response into a single `SessionEvent::FinalText` followed by
//! `Finished`, and shuts the plugin down. Lightweight per-call lifecycle keeps the
//! v1 surface simple; pooling can come later.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use crate::{HostError, PluginHost, PluginSpawnOptions, PluginStderrSink, PLUGIN_BASE_ENV_ALLOWLIST};
use animus_plugin_protocol::{EnvRequirement, RpcError, RpcNotification};
use async_trait::async_trait;
use orchestrator_logging::Logger;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::plugin_supervisor::{
    classify, DispatchObserver, NoopDispatchObserver, PluginSupervisor, RetryDecision, SupervisorError,
};

/// Default cap on how long the plugin host will wait for a single `agent/run`
/// or `agent/resume` reply before giving up. Caller-supplied `timeout_secs` on
/// the SessionRequest takes precedence.
const DEFAULT_PLUGIN_RUN_TIMEOUT_SECS: u64 = 1_800; // 30 min
/// Default cap for the synchronous `agent/cancel` round trip.
const DEFAULT_PLUGIN_CANCEL_TIMEOUT_SECS: u64 = 10;
/// Wait at most this long for the plugin to gracefully shut down before we
/// drop its connection.
const PLUGIN_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

use super::error::{Error, Result};
use animus_session_backend::session::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities, session_event::SessionEvent,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
};

/// Terminal outcome reported by `resume_agent_for_restart` after the daemon
/// drains a resumed session to completion or surfaces a failure.
#[derive(Debug)]
pub enum ResumeAgentOutcome {
    Resumed { session_id: Option<String> },
    Failed { reason: String },
}

/// In-memory record of a live `agent/run`-initiated session whose plugin host
/// must be reused for subsequent control-plane calls (currently just
/// `agent/cancel`).
///
/// Keyed by the `control_session_id` minted in [`PluginSessionBackend::dispatch`]
/// and returned to callers via [`SessionRun::session_id`].
///
/// TODO(audit-gap-7-followup): add TTL-based background eviction so handles
/// for plugins that crash or fail to send `Finished` don't accumulate.
pub(crate) struct SessionHandle {
    pub(crate) host: PluginHost,
    /// When the session was registered; reserved for TTL eviction in a
    /// follow-up commit.
    #[allow(dead_code)]
    pub(crate) started_at: Instant,
    /// Cached from the plugin's `initialize` response so cancel routing can
    /// short-circuit with [`HostError::CapabilityNotSupported`] without paying
    /// for a wasted RPC round trip.
    pub(crate) cancellation: bool,
}

/// Map of `control_session_id` -> live [`SessionHandle`]. Guarded by a sync
/// mutex because the operations are all CPU-bound (HashMap insert/get/remove);
/// we never hold this lock across `.await`.
pub(crate) type SessionMap = Arc<StdMutex<HashMap<String, SessionHandle>>>;

/// Wraps a discovered Animus STDIO plugin so the resolver can route `agent/run`
/// through it as if it were any other in-tree backend.
#[derive(Clone)]
pub struct PluginSessionBackend {
    pub(crate) plugin_name: String,
    pub(crate) binary_path: PathBuf,
    pub(crate) provider_tool: String,
    pub(crate) display_name: String,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) env_required: Vec<EnvRequirement>,
    pub(crate) sessions: SessionMap,
    pub(crate) supervisor: Arc<PluginSupervisor>,
    pub(crate) dispatch_observer: Arc<dyn DispatchObserver>,
    /// Method names the plugin's manifest claims to implement (e.g.
    /// `"agent/run"`, `"agent/resume"`, `"agent/cancel"`). Drives the honest
    /// reporting in [`PluginSessionBackend::capabilities`]: a plugin that
    /// does not list `agent/resume` here reports `supports_resume: false`,
    /// so higher-level runtime code does not blindly take a resume path the
    /// plugin will reject. Empty means "no manifest data plumbed; fall
    /// back to all-false for plugin-specific capabilities" (the most
    /// conservative posture — never overclaim).
    pub(crate) declared_methods: Vec<String>,
    /// Whether the plugin DECLARES it consumes host-injected MCP servers.
    ///
    /// This backs `capabilities().supports_mcp`, which is the SOURCE OF TRUTH
    /// for whether the daemon injects an agent's profile/skill MCP servers into
    /// the wire request for this provider tool — consulted in place of the
    /// kernel's name-based capability table. Every provider plugin consumes MCP,
    /// so it defaults to `true`. The plugin protocol has no per-plugin
    /// MCP-consume flag yet, so discovery leaves it at the default today;
    /// sourcing it from a first-class manifest/handshake capability is tracked
    /// as TASK-277. The field + `with_declared_supports_mcp` builder already
    /// make it the single overridable source of truth.
    pub(crate) declared_supports_mcp: bool,
    /// The plugin manifest's `notification_buffer_size` hint, forwarded to
    /// the spawn options so the host's broadcast capacity honors the
    /// documented priority chain.
    pub(crate) notification_buffer_hint: Option<usize>,
}

impl PluginSessionBackend {
    pub fn new(plugin_name: impl Into<String>, binary_path: PathBuf, provider_tool: impl Into<String>) -> Self {
        let plugin_name = plugin_name.into();
        let provider_tool = provider_tool.into();
        let display_name = format!("Plugin Provider ({plugin_name})");
        let supervisor = Arc::new(PluginSupervisor::with_defaults(plugin_name.clone()));
        // Adopt the process-global plugin status registry as the default
        // dispatch observer whenever one has been installed (i.e. inside the
        // daemon). CLI processes that never install a registry get a no-op,
        // preserving the historical zero-overhead default.
        let dispatch_observer: Arc<dyn DispatchObserver> = match crate::status::global_status_registry() {
            Some(registry) => crate::status::StatusRegistryObserver::new(registry),
            None => Arc::new(NoopDispatchObserver),
        };
        Self {
            plugin_name,
            binary_path,
            provider_tool,
            display_name,
            project_root: None,
            env_required: Vec::new(),
            sessions: Arc::new(StdMutex::new(HashMap::new())),
            supervisor,
            dispatch_observer,
            declared_methods: Vec::new(),
            // Default to MCP-capable: every provider plugin consumes host MCP
            // servers unless its manifest explicitly opts out. Overridden from
            // the manifest via `with_declared_supports_mcp` during discovery.
            declared_supports_mcp: true,
            notification_buffer_hint: None,
        }
    }

    /// Plumb the plugin manifest's `notification_buffer_size` hint so spawned
    /// hosts size their notification broadcast channel per the documented
    /// priority chain (spawn override → manifest hint → env → default).
    #[must_use]
    pub fn with_notification_buffer_hint(mut self, hint: Option<usize>) -> Self {
        self.notification_buffer_hint = hint;
        self
    }

    /// Plumb the plugin manifest's declared method list so
    /// [`PluginSessionBackend::capabilities`] reports honestly. When this
    /// is not set the backend reports `supports_resume = false` and
    /// `supports_terminate = false`, matching the documented "no manifest
    /// data, refuse to overclaim" posture.
    #[must_use]
    pub fn with_declared_methods(mut self, methods: Vec<String>) -> Self {
        self.declared_methods = methods;
        self
    }

    /// Plumb the plugin's DECLARED MCP capability (see
    /// [`PluginSessionBackend::declared_supports_mcp`]). Discovery derives this
    /// from the manifest; direct constructors default to `true` (MCP-capable).
    #[must_use]
    pub fn with_declared_supports_mcp(mut self, supports_mcp: bool) -> Self {
        self.declared_supports_mcp = supports_mcp;
        self
    }

    /// True iff the plugin's manifest declared `agent/resume` among its
    /// implemented methods.
    pub(crate) fn manifest_supports_resume(&self) -> bool {
        self.declared_methods.iter().any(|m| m == "agent/resume")
    }

    /// True iff the plugin's manifest declared `agent/cancel` among its
    /// implemented methods. The session backend trait's `supports_terminate`
    /// maps to the plugin protocol's `agent/cancel`.
    pub(crate) fn manifest_supports_terminate(&self) -> bool {
        self.declared_methods.iter().any(|m| m == "agent/cancel")
    }

    #[must_use]
    pub fn with_supervisor(mut self, supervisor: Arc<PluginSupervisor>) -> Self {
        self.supervisor = supervisor;
        self
    }

    pub fn supervisor(&self) -> Arc<PluginSupervisor> {
        self.supervisor.clone()
    }

    /// Install a dispatch observer that receives a duration sample for every
    /// `request_typed` round-trip. Daemon-runtime wires its metrics layer
    /// here so `plugin_request_duration_seconds` reflects every plugin call.
    #[must_use]
    pub fn with_dispatch_observer(mut self, observer: Arc<dyn DispatchObserver>) -> Self {
        self.dispatch_observer = observer;
        self
    }

    /// Test-only: insert a `SessionHandle` directly into the session map so a
    /// fake host can be exercised by `dispatch_cancel` without first running
    /// `dispatch`. Production code never touches this.
    #[cfg(test)]
    pub(crate) fn insert_session_for_test(&self, control_session_id: impl Into<String>, handle: SessionHandle) {
        self.sessions.lock().expect("session map mutex poisoned in test").insert(control_session_id.into(), handle);
    }

    /// Test-only: peek at whether a session is currently registered.
    #[cfg(test)]
    pub(crate) fn has_session_for_test(&self, control_session_id: &str) -> bool {
        self.sessions.lock().expect("session map mutex poisoned in test").contains_key(control_session_id)
    }

    /// Bind a project root so structured log entries about every spawn / call /
    /// stderr line land in `~/.animus/<repo-scope>/logs/events.jsonl` for that project.
    #[must_use]
    pub fn with_project_root(mut self, project_root: impl Into<PathBuf>) -> Self {
        self.project_root = Some(project_root.into());
        self
    }

    /// Set the plugin's manifest-declared env requirements. The host scrubs
    /// the daemon environment at spawn time and forwards only these vars (on
    /// top of the universal base allowlist).
    #[must_use]
    pub fn with_env_required(mut self, env_required: Vec<EnvRequirement>) -> Self {
        self.env_required = env_required;
        self
    }

    /// Build the host-vs-plugin env gate as a set of allowed variable names:
    /// the universal shell/locale base allowlist union the plugin manifest's
    /// declared `env_required`.
    fn allowed_env_keys(&self) -> std::collections::HashSet<String> {
        PLUGIN_BASE_ENV_ALLOWLIST
            .iter()
            .map(|s| (*s).to_string())
            .chain(self.env_required.iter().map(|req| req.name.clone()))
            .collect()
    }

    /// Intersect the runner-supplied request env with the manifest gate so we
    /// only forward variables the plugin actually declared (plus the universal
    /// shell/locale base allowlist). Anything the runner happened to inherit
    /// from the daemon's sanitized environment that the plugin manifest
    /// doesn't list is dropped on the floor — closing the leak path where
    /// e.g. `OPENAI_API_KEY` would have been handed to a plugin that never
    /// declared a need for it.
    ///
    /// Used for the spawn-time `env_allowlist` extras AND the per-call RPC
    /// `env` param in [`PluginSessionBackend::build_run_params`]. The
    /// `extras.runtime_contract.cli.launch.env` duplicate channel is also
    /// scrubbed at the dispatch boundary via [`scrub_runtime_contract_env`]
    /// so a plugin can't fish a secret out of the runtime contract payload
    /// even though the top-level RPC `env` was filtered. Contract documented
    /// in `docs/guides/plugin-author-guide.md` § 9.
    fn filter_request_env_vars(&self, request_env_vars: &[(String, String)]) -> Vec<(String, String)> {
        let allowed = self.allowed_env_keys();
        request_env_vars.iter().filter(|(name, _)| allowed.contains(name.as_str())).cloned().collect()
    }

    fn spawn_options(&self, request_env_vars: &[(String, String)]) -> PluginSpawnOptions {
        // request_env_vars has already been filtered against the manifest gate
        // by the caller (see `filter_request_env_vars`). Passing the filtered
        // keys as `extras` is a belt-and-braces: even though every name should
        // already be in the manifest set, listing them as extras keeps the
        // existing for_manifest API surface honest about what the host plans
        // to forward.
        let extras = request_env_vars.iter().map(|(name, _)| name.clone());
        let mut options = PluginSpawnOptions::for_manifest(
            self.plugin_name.clone(),
            &self.env_required,
            extras,
            self.stderr_sink_for(),
        )
        .with_notification_buffer_hint(self.notification_buffer_hint);
        // Pin cwd to the project root so provider plugins (and the CLIs
        // they spawn) resolve project-local files predictably regardless
        // of which shell started the daemon.
        if let Some(root) = self.project_root.as_ref() {
            options = options.with_working_dir(root);
        }
        options
    }

    fn project_logger(&self) -> Option<Logger> {
        self.project_root.as_ref().map(|root| Logger::for_project(root))
    }

    fn stderr_sink_for(&self) -> Option<PluginStderrSink> {
        let root = self.project_root.clone()?;
        let plugin_name = self.plugin_name.clone();
        let logger = Logger::for_project(&root);
        Some(Arc::new(move |emitting_plugin: &str, line: &str| {
            logger
                .warn("plugin.stderr", line)
                .meta(json!({
                    "plugin": plugin_name,
                    "emitter": emitting_plugin,
                }))
                .emit();
        }))
    }

    fn build_run_params(
        &self,
        request: &SessionRequest,
        resume_session: Option<&str>,
        control_session_id: &str,
    ) -> Value {
        let extras = request.extras.as_object().cloned().unwrap_or_default();
        let mut params = json!({
            "prompt": request.prompt,
            "model": request.model,
            "cwd": request.cwd,
            "env": request.env_vars.iter().cloned().collect::<std::collections::HashMap<_, _>>(),
            "control_session_id": control_session_id,
        });

        if let Some(project_root) = &request.project_root {
            params["project_root"] = json!(project_root);
        }
        if let Some(mode) = &request.permission_mode {
            params["permission_mode"] = json!(mode);
        }
        if let Some(timeout) = request.timeout_secs {
            params["timeout_secs"] = json!(timeout);
        }
        if let Some(sid) = resume_session {
            params["session_id"] = json!(sid);
        } else if let Some(sid) = extras.get("session_id") {
            params["session_id"] = sid.clone();
        }
        for key in [
            "system_prompt",
            "claude_profile",
            "mcp_servers",
            "tools",
            "response_schema",
            "runtime_contract",
            "reasoning_effort",
            "approvals",
        ] {
            if let Some(value) = extras.get(key) {
                params[key] = value.clone();
            }
        }
        params
    }

    async fn spawn_and_handshake(
        &self,
        request_env_vars: &[(String, String)],
    ) -> Result<(PluginHost, animus_plugin_protocol::InitializeResult)> {
        let spawn_options = self.spawn_options(request_env_vars);
        let host = match PluginHost::spawn_with_options(&self.binary_path, &[], spawn_options).await {
            Ok(host) => host,
            Err(error) => {
                if let Some(registry) = crate::status::global_status_registry() {
                    registry.record_exit(&self.plugin_name, Some(("SpawnFailed".to_string(), error.to_string())));
                }
                let message = format!("plugin '{}' spawn failed: {error}", self.plugin_name);
                if let Some(logger) = self.project_logger() {
                    logger.error("plugin.dispatch.spawn", &message).err(error.to_string()).emit();
                }
                return Err(Error::execution_failed(message));
            }
        };

        if let Some(registry) = crate::status::global_status_registry() {
            registry.record_spawn(&self.plugin_name, host.child_pid());
        }

        let init_result = match host.handshake().await {
            Ok(result) => result,
            Err(error) => {
                if let Some(registry) = crate::status::global_status_registry() {
                    registry.record_exit(&self.plugin_name, Some(("HandshakeFailed".to_string(), error.to_string())));
                }
                graceful_shutdown(host).await;
                let message = format!(
                    "plugin '{}' handshake failed: {error}; reinstall or upgrade the provider plugins with `animus plugin install-defaults` and restart the daemon",
                    self.plugin_name
                );
                if let Some(logger) = self.project_logger() {
                    logger.error("plugin.dispatch.handshake", &message).err(error.to_string()).emit();
                }
                return Err(Error::execution_failed(message));
            }
        };
        Ok((host, init_result))
    }

    async fn dispatch(
        &self,
        method: &str,
        mut request: SessionRequest,
        resume_session: Option<String>,
    ) -> Result<SessionRun> {
        // Tighten the env_vars contract right at the dispatch boundary so the
        // filtered list flows to BOTH the spawn-time allowlist and the
        // per-call RPC `env` param built by `build_run_params`. The runner
        // supplies its full sanitized launch env here; without this filter
        // any var the runner happens to inherit would leak to a plugin that
        // never declared a need for it.
        request.env_vars = self.filter_request_env_vars(&request.env_vars);
        // The agent-runner duplicates the full merged launch env under
        // `extras.runtime_contract.cli.launch.env`. `build_run_params`
        // forwards `runtime_contract` to the plugin verbatim, so without
        // this scrub a plugin could still read secrets from the runtime
        // contract payload even after the top-level RPC `env` was filtered.
        //
        // TODO(env-override): the agent-runner currently merges intentional
        // per-run launch env overrides (e.g. `SKILL_MODE` injected via
        // `inject_cli_launch_env`) into the SAME map as inherited daemon
        // env, so the manifest gate strips both. A future refactor in
        // `agent-runner::session_process` should split "intentional
        // override" from "inherited env" into two distinct fields so the
        // host can pass overrides through while still gating inherited
        // secrets. See codex round-1 P2.
        let allowed = self.allowed_env_keys();
        scrub_runtime_contract_env(&mut request.extras, &allowed);
        let backend_label = format!("plugin:{}", self.plugin_name);
        let control_session_id = Uuid::new_v4().to_string();
        let params = self.build_run_params(&request, resume_session.as_deref(), &control_session_id);
        let run_timeout = Duration::from_secs(request.timeout_secs.unwrap_or(DEFAULT_PLUGIN_RUN_TIMEOUT_SECS));
        let started_at = Instant::now();

        if self.supervisor.is_disabled() {
            let retry_after = self.supervisor.disabled_remaining().unwrap_or_default();
            let message = format!(
                "plugin '{}' is currently disabled by supervisor (retry after {}s)",
                self.plugin_name,
                retry_after.as_secs()
            );
            if let Some(logger) = self.project_logger() {
                logger
                    .warn("plugin.dispatch.disabled", &message)
                    .meta(json!({
                        "plugin": self.plugin_name,
                        "retry_after_secs": retry_after.as_secs(),
                    }))
                    .emit();
            }
            return Err(Error::execution_failed(message));
        }

        if let Some(logger) = self.project_logger() {
            logger
                .info("plugin.dispatch.start", format!("{} → {}", self.plugin_name, method))
                .meta(json!({
                    "plugin": self.plugin_name,
                    "method": method,
                    "tool": request.tool,
                    "model": request.model,
                    "control_session_id": control_session_id,
                    "resume_session": resume_session,
                }))
                .emit();
        }

        let (host, init_result) = self.spawn_and_handshake(&request.env_vars).await?;
        let notifications = host.subscribe_notifications();

        // Register the session-keyed host so dispatch_cancel can route through
        // the SAME transport instead of spawning a brand-new plugin process
        // that has no knowledge of the in-flight session. Insert BEFORE the
        // streaming task starts so cancel callers observe the handle even on
        // very fast plugins. The streaming task removes the entry when the
        // run finishes (success, error, or timeout).
        {
            let mut guard = self.sessions.lock().expect("session map mutex poisoned");
            guard.insert(
                control_session_id.clone(),
                SessionHandle {
                    host: host.clone(),
                    started_at: Instant::now(),
                    cancellation: init_result.capabilities.cancellation,
                },
            );
        }
        let sessions_for_cleanup = self.sessions.clone();
        let control_session_id_for_cleanup = control_session_id.clone();

        // Hand the SessionRun receiver out IMMEDIATELY so the caller can read live
        // stream events while the plugin is still working. The actual JSON-RPC call
        // runs in a background task that forwards notifications and produces the
        // final result.
        let (tx, rx) = mpsc::channel::<SessionEvent>(64);
        let session_id_for_started = Some(control_session_id.clone());
        let backend_for_started = backend_label.clone();
        let _ = tx
            .send(SessionEvent::Started { backend: backend_for_started, session_id: session_id_for_started, pid: None })
            .await;

        let plugin_name_for_task = self.plugin_name.clone();
        let backend_label_for_task = backend_label.clone();
        let project_root_for_task = self.project_root.clone();
        let method_string = method.to_string();
        let stream_tx = tx.clone();
        let supervisor_for_task = self.supervisor.clone();
        let backend_for_retry = self.clone();
        let request_env_for_retry = request.env_vars.clone();
        let sessions_for_retry = self.sessions.clone();
        let dispatch_observer = self.dispatch_observer.clone();
        tokio::spawn(async move {
            let notifications_forwarded = Arc::new(AtomicBool::new(false));

            let response = run_request_with_notifications(
                host.clone(),
                notifications,
                method_string.clone(),
                params.clone(),
                run_timeout,
                stream_tx.clone(),
                plugin_name_for_task.clone(),
                notifications_forwarded.clone(),
                dispatch_observer.clone(),
            )
            .await;

            let logger = project_root_for_task.as_ref().map(|root| Logger::for_project(root));
            let duration_ms = started_at.elapsed().as_millis() as u64;

            // Retry-once on death-like errors only when:
            //   (a) classify() returns DeathLike (process death / timeout /
            //       transport collapse — never a structured plugin-side error)
            //   (b) no notifications were forwarded yet (otherwise retry would
            //       double-emit deltas to the caller)
            //   (c) the supervisor's restart budget has not been exhausted
            //
            // Best-effort idempotency: if the plugin's first request had already
            // executed side effects before crashing (e.g. it spawned a sub-tool
            // that mutated state but died before writing the JSON-RPC response),
            // the retry could re-execute those side effects. Plugin authors must
            // make their request handlers idempotent or accept the duplicate.
            let (outcome, host_to_shutdown) = match response {
                ResponseOutcome::Ok(value) => (Some(value), Some(host)),
                ResponseOutcome::Timeout => {
                    let message = format!(
                        "plugin '{}' {method_string} timed out after {}s",
                        plugin_name_for_task,
                        run_timeout.as_secs()
                    );
                    let _ = stream_tx.send(SessionEvent::Error { message: message.clone(), recoverable: false }).await;
                    if let Some(logger) = logger.as_ref() {
                        logger
                            .error("plugin.dispatch.timeout", &message)
                            .duration(duration_ms)
                            .meta(json!({ "plugin": plugin_name_for_task, "method": method_string, "timeout_secs": run_timeout.as_secs() }))
                            .emit();
                    }
                    let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(124) }).await;
                    remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                    graceful_shutdown(host).await;
                    return;
                }
                ResponseOutcome::Err(host_err) => {
                    let already_forwarded = notifications_forwarded.load(AtomicOrdering::SeqCst);
                    let can_retry = matches!(classify(&host_err), RetryDecision::DeathLike) && !already_forwarded;
                    let rpc_error: RpcError = host_err.into();

                    if can_retry {
                        if let Some(logger) = logger.as_ref() {
                            logger
                                .warn(
                                    "plugin.dispatch.retry",
                                    format!(
                                        "plugin '{}' died mid-call ({}); attempting retry-once",
                                        plugin_name_for_task, rpc_error.message
                                    ),
                                )
                                .meta(json!({
                                    "plugin": plugin_name_for_task,
                                    "method": method_string,
                                    "code": rpc_error.code,
                                }))
                                .emit();
                        }
                        let _ = graceful_shutdown(host).await;

                        if let Some(registry) = crate::status::global_status_registry() {
                            registry.record_restart(&plugin_name_for_task);
                        }
                        match supervisor_for_task.record_restart() {
                            Ok(()) => {}
                            Err(SupervisorError::TooManyRestarts { plugin, count, window }) => {
                                if let Some(registry) = crate::status::global_status_registry() {
                                    registry.record_supervisor_disabled(
                                        &plugin,
                                        supervisor_for_task.config().disable_cooldown,
                                    );
                                }
                                let message = format!(
                                    "plugin '{plugin}' exhausted restart budget ({count} restarts in {}s); marked disabled",
                                    window.as_secs()
                                );
                                let _ = stream_tx
                                    .send(SessionEvent::Error { message: message.clone(), recoverable: false })
                                    .await;
                                if let Some(logger) = logger.as_ref() {
                                    logger
                                        .error("plugin.dispatch.disabled", &message)
                                        .duration(duration_ms)
                                        .meta(json!({ "plugin": plugin, "method": method_string, "restart_count": count }))
                                        .emit();
                                }
                                let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
                                remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                                return;
                            }
                            Err(SupervisorError::PluginDisabled { plugin, retry_after }) => {
                                if let Some(registry) = crate::status::global_status_registry() {
                                    registry.record_supervisor_disabled(&plugin, retry_after);
                                }
                                let message = format!(
                                    "plugin '{plugin}' disabled by supervisor (retry after {}s)",
                                    retry_after.as_secs()
                                );
                                let _ = stream_tx
                                    .send(SessionEvent::Error { message: message.clone(), recoverable: false })
                                    .await;
                                let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
                                remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                                return;
                            }
                        }

                        match backend_for_retry.spawn_and_handshake(&request_env_for_retry).await {
                            Ok((new_host, new_init)) => {
                                {
                                    let mut guard = sessions_for_retry.lock().expect("session map mutex poisoned");
                                    guard.insert(
                                        control_session_id_for_cleanup.clone(),
                                        SessionHandle {
                                            host: new_host.clone(),
                                            started_at: Instant::now(),
                                            cancellation: new_init.capabilities.cancellation,
                                        },
                                    );
                                }
                                let new_notifications = new_host.subscribe_notifications();
                                let retry_response = run_request_with_notifications(
                                    new_host.clone(),
                                    new_notifications,
                                    method_string.clone(),
                                    params.clone(),
                                    run_timeout,
                                    stream_tx.clone(),
                                    plugin_name_for_task.clone(),
                                    notifications_forwarded.clone(),
                                    dispatch_observer.clone(),
                                )
                                .await;
                                match retry_response {
                                    ResponseOutcome::Ok(value) => (Some(value), Some(new_host)),
                                    ResponseOutcome::Timeout => {
                                        let message = format!(
                                            "plugin '{}' {method_string} timed out after retry",
                                            plugin_name_for_task
                                        );
                                        let _ = stream_tx
                                            .send(SessionEvent::Error { message: message.clone(), recoverable: false })
                                            .await;
                                        let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(124) }).await;
                                        remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                                        graceful_shutdown(new_host).await;
                                        return;
                                    }
                                    ResponseOutcome::Err(retry_err) => {
                                        let retry_rpc: RpcError = retry_err.into();
                                        let message = format!(
                                            "plugin '{}' {method_string} failed after retry ({}): {}",
                                            plugin_name_for_task, retry_rpc.code, retry_rpc.message
                                        );
                                        let _ = stream_tx
                                            .send(SessionEvent::Error { message: message.clone(), recoverable: false })
                                            .await;
                                        if let Some(logger) = logger.as_ref() {
                                            logger
                                                .error("plugin.dispatch.retry_failed", &message)
                                                .duration(duration_ms)
                                                .meta(json!({
                                                    "plugin": plugin_name_for_task,
                                                    "method": method_string,
                                                    "code": retry_rpc.code,
                                                }))
                                                .emit();
                                        }
                                        let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
                                        remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                                        graceful_shutdown(new_host).await;
                                        return;
                                    }
                                }
                            }
                            Err(spawn_error) => {
                                let message =
                                    format!("plugin '{}' retry spawn failed: {spawn_error}", plugin_name_for_task);
                                let _ = stream_tx
                                    .send(SessionEvent::Error { message: message.clone(), recoverable: false })
                                    .await;
                                let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
                                remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                                return;
                            }
                        }
                    } else {
                        let message = format!(
                            "plugin '{}' {method_string} failed ({}): {}",
                            plugin_name_for_task, rpc_error.code, rpc_error.message
                        );
                        let _ =
                            stream_tx.send(SessionEvent::Error { message: message.clone(), recoverable: false }).await;
                        if let Some(logger) = logger.as_ref() {
                            logger
                                .error("plugin.dispatch.error", &message)
                                .duration(duration_ms)
                                .meta(json!({ "plugin": plugin_name_for_task, "method": method_string }))
                                .emit();
                        }
                        let _ = stream_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
                        remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
                        graceful_shutdown(host).await;
                        return;
                    }
                }
            };

            remove_session(&sessions_for_cleanup, &control_session_id_for_cleanup);
            if let Some(host) = host_to_shutdown {
                graceful_shutdown(host).await;
            }

            if let Some(response) = outcome {
                let exit_code = response.get("exit_code").and_then(Value::as_i64).map(|n| n as i32);
                let output = response.get("output").and_then(Value::as_str).unwrap_or_default().to_string();
                let metadata = response.get("metadata").cloned().unwrap_or(Value::Null);

                if let Some(provider_sid) = extract_provider_session_id(&response) {
                    let _ = stream_tx
                        .send(SessionEvent::Started {
                            backend: backend_label_for_task.clone(),
                            session_id: Some(provider_sid),
                            pid: None,
                        })
                        .await;
                }

                if !output.is_empty() {
                    let _ = stream_tx.send(SessionEvent::FinalText { text: output }).await;
                }
                if !matches!(metadata, Value::Null) {
                    let _ = stream_tx.send(SessionEvent::Metadata { metadata }).await;
                }

                if let Some(logger) = logger.as_ref() {
                    logger
                        .info("plugin.dispatch.complete", format!("{} → {} ok", plugin_name_for_task, method_string))
                        .duration(duration_ms)
                        .meta(json!({
                            "plugin": plugin_name_for_task,
                            "method": method_string,
                            "exit_code": exit_code,
                            "backend": backend_label_for_task,
                        }))
                        .emit();
                }
                let _ = stream_tx.send(SessionEvent::Finished { exit_code }).await;
            }
        });

        Ok(SessionRun {
            session_id: Some(control_session_id),
            events: rx,
            selected_backend: backend_label,
            fallback_reason: None,
            pid: None,
        })
    }

    /// Daemon-restart helper: rebuild a minimal `SessionRequest` from the
    /// `AgentRunRequest` JSON captured in the phase session checkpoint and
    /// dispatch `agent/resume` against the live plugin. Drains the resulting
    /// event stream synchronously until a terminal `Finished` / `Error` event
    /// arrives, then returns a [`ResumeAgentOutcome`] describing the result.
    ///
    /// Returns `ResumeAgentOutcome::Failed { reason }` on every error path
    /// (malformed checkpoint, spawn/handshake failure, RPC error, timeout) so
    /// the caller can record the reason verbatim in the blocked-state message.
    pub async fn resume_agent_for_restart(&self, session_id: &str, agent_run_request: &Value) -> ResumeAgentOutcome {
        let session_request = match session_request_from_agent_run_request(&self.provider_tool, agent_run_request) {
            Ok(req) => req,
            Err(reason) => return ResumeAgentOutcome::Failed { reason },
        };
        let run = match self.dispatch("agent/resume", session_request, Some(session_id.to_string())).await {
            Ok(run) => run,
            Err(err) => return ResumeAgentOutcome::Failed { reason: format!("resume_agent failed: {err}") },
        };
        drain_resume_events(run.session_id, run.events).await
    }

    async fn dispatch_cancel(&self, session_id: &str) -> Result<()> {
        let cancel_timeout = Duration::from_secs(DEFAULT_PLUGIN_CANCEL_TIMEOUT_SECS);
        if let Some(logger) = self.project_logger() {
            logger
                .info("plugin.cancel", format!("{} → cancel {}", self.plugin_name, session_id))
                .meta(json!({ "plugin": self.plugin_name, "session_id": session_id }))
                .emit();
        }

        // Look the session up under the lock; CLONE the host out (PluginHost is
        // an Arc<PluginHostInner>, so cloning is cheap and shares the same
        // transport with the original dispatch task), then DROP the lock
        // before doing any await. Holding a sync mutex across .await would
        // deadlock under tokio's current-thread scheduler.
        let (host, cancellation) = {
            let guard = self.sessions.lock().expect("session map mutex poisoned");
            match guard.get(session_id) {
                Some(handle) => (handle.host.clone(), handle.cancellation),
                None => {
                    return Err(Error::execution_failed(format!(
                        "no active session '{session_id}' for plugin '{}'; nothing to cancel",
                        self.plugin_name
                    )));
                }
            }
        };

        if !cancellation {
            return Err(Error::CapabilityNotSupported {
                plugin: self.plugin_name.clone(),
                capability: "cancellation".to_string(),
            });
        }

        let cancel_start = Instant::now();
        let request_future = host.request_typed("agent/cancel".to_string(), Some(json!({ "session_id": session_id })));
        let result = tokio::time::timeout(cancel_timeout, request_future).await;
        self.dispatch_observer.observe_duration(&self.plugin_name, "agent/cancel", cancel_start.elapsed());
        match result {
            Ok(Ok(_)) => {
                remove_session(&self.sessions, session_id);
                Ok(())
            }
            Ok(Err(error)) => {
                let rpc: RpcError = error.into();
                Err(Error::execution_failed(format!(
                    "plugin '{}' agent/cancel failed: {}",
                    self.plugin_name, rpc.message
                )))
            }
            Err(_) => Err(Error::execution_failed(format!(
                "plugin '{}' agent/cancel timed out after {}s",
                self.plugin_name,
                cancel_timeout.as_secs()
            ))),
        }
    }
}

/// Briefly lock the session map and drop the entry for `session_id`. The
/// dropped [`SessionHandle`] (and its embedded `PluginHost`) is released
/// outside the lock, so nothing async runs under the mutex guard.
fn remove_session(sessions: &SessionMap, session_id: &str) {
    let removed = {
        let mut guard = sessions.lock().expect("session map mutex poisoned");
        guard.remove(session_id)
    };
    drop(removed);
}

#[async_trait]
impl SessionBackend for PluginSessionBackend {
    fn info(&self) -> SessionBackendInfo {
        SessionBackendInfo {
            kind: SessionBackendKind::Subprocess,
            provider_tool: self.provider_tool.clone(),
            stability: SessionStability::Experimental,
            display_name: self.display_name.clone(),
        }
    }

    fn capabilities(&self) -> SessionCapabilities {
        // Plugin-specific capabilities reflect what the manifest actually
        // claims. Defaulting to true (the pre-fix behaviour) made the
        // runtime take resume / terminate paths against plugins that
        // returned `MethodNotFound`, surfacing as opaque dispatch errors
        // instead of clean "feature unsupported" branches. See
        // PluginSessionBackend::with_declared_methods for the plumbing.
        SessionCapabilities {
            supports_resume: self.manifest_supports_resume(),
            supports_terminate: self.manifest_supports_terminate(),
            supports_permissions: true,
            // Plugin-declared (manifest-derived), NOT hardcoded: the injection
            // decision for this provider tool's profile/skill MCP servers reads
            // this value. Defaults true; a provider opts out via the manifest.
            supports_mcp: self.declared_supports_mcp,
            supports_tool_events: false,
            supports_thinking_events: false,
            supports_artifact_events: false,
            supports_usage_metadata: true,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> animus_session_backend::error::Result<SessionRun> {
        self.dispatch("agent/run", request, None).await.map_err(Into::into)
    }

    async fn resume_session(
        &self,
        request: SessionRequest,
        session_id: &str,
    ) -> animus_session_backend::error::Result<SessionRun> {
        self.dispatch("agent/resume", request, Some(session_id.to_string())).await.map_err(Into::into)
    }

    async fn terminate_session(&self, session_id: &str) -> animus_session_backend::error::Result<()> {
        self.dispatch_cancel(session_id).await.map_err(Into::into)
    }
}

fn session_request_from_agent_run_request(
    fallback_tool: &str,
    agent_run_request: &Value,
) -> std::result::Result<SessionRequest, String> {
    let context = agent_run_request.get("context").cloned().unwrap_or(Value::Null);
    let model = agent_run_request.get("model").and_then(Value::as_str).unwrap_or_default().to_string();
    let timeout_secs = agent_run_request.get("timeout_secs").and_then(Value::as_u64);

    let tool = context
        .get("tool")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| context.pointer("/runtime_contract/cli/name").and_then(Value::as_str).map(ToOwned::to_owned))
        .unwrap_or_else(|| fallback_tool.to_string());
    let prompt = context.get("prompt").and_then(Value::as_str).unwrap_or_default().to_string();
    let cwd = context
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "resume_agent failed: checkpoint missing context.cwd".to_string())?;
    let project_root = context.get("project_root").and_then(Value::as_str).map(PathBuf::from);

    let mut extras = serde_json::Map::new();
    for key in
        ["runtime_contract", "mcp_servers", "agent_id", "phase_id", "workflow_id", "subject_id", "phase_capabilities"]
    {
        if let Some(value) = context.get(key) {
            extras.insert(key.to_string(), value.clone());
        }
    }

    Ok(SessionRequest {
        tool,
        model,
        prompt,
        cwd,
        project_root,
        mcp_endpoint: None,
        permission_mode: None,
        timeout_secs,
        env_vars: Vec::new(),
        mcp_servers: context.get("mcp_servers").cloned(),
        extras: Value::Object(extras),
        // The resume path carries no authenticated control identity. The actor
        // reaches a session only when relayed from the workflow runner's
        // WorkflowExecuteRequest; it is never synthesized from a checkpoint.
        actor: None,
    })
}

/// Scrub `extras.runtime_contract.cli.launch.env` so the manifest gate that
/// already filtered `SessionRequest.env_vars` also applies to the duplicate
/// launch-env payload the agent-runner stuffs into the runtime contract.
///
/// `build_run_params` forwards the entire `runtime_contract` blob to the
/// plugin verbatim; without this scrub a plugin whose manifest declared no
/// `env_required` could still fish e.g. `OPENAI_API_KEY` out of
/// `runtime_contract.cli.launch.env` even after the top-level RPC `env`
/// surface was filtered.
///
/// Best-effort: silently no-op when the runtime_contract or the
/// `cli.launch.env` field is absent / not the expected shape. The dispatch
/// layer doesn't make any guarantees about what `extras` carries beyond
/// what `build_run_params` reads explicitly.
fn scrub_runtime_contract_env(extras: &mut Value, allowed: &std::collections::HashSet<String>) {
    let Some(extras_obj) = extras.as_object_mut() else {
        return;
    };
    let Some(runtime_contract) = extras_obj.get_mut("runtime_contract").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(cli) = runtime_contract.get_mut("cli").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(launch) = cli.get_mut("launch").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(env) = launch.get_mut("env").and_then(Value::as_object_mut) else {
        return;
    };
    env.retain(|key, _| allowed.contains(key));
}

async fn graceful_shutdown(host: PluginHost) {
    let plugin_name = host.name().to_string();
    let _ = tokio::time::timeout(Duration::from_secs(PLUGIN_SHUTDOWN_TIMEOUT_SECS), host.shutdown()).await;
    if let Some(registry) = crate::status::global_status_registry() {
        registry.record_exit(&plugin_name, None);
    }
}

enum ResponseOutcome {
    Ok(Value),
    Err(HostError),
    Timeout,
}

#[allow(clippy::too_many_arguments)]
async fn run_request_with_notifications(
    host: PluginHost,
    mut notifications: tokio::sync::broadcast::Receiver<RpcNotification>,
    method: String,
    params: Value,
    run_timeout: Duration,
    stream_tx: mpsc::Sender<SessionEvent>,
    plugin_name: String,
    notifications_forwarded: Arc<AtomicBool>,
    dispatch_observer: Arc<dyn DispatchObserver>,
) -> ResponseOutcome {
    let request_start = Instant::now();
    let outcome = {
        let mut request_future = Box::pin(host.request_typed(method.clone(), Some(params)));
        let timeout_future = tokio::time::sleep(run_timeout);
        tokio::pin!(timeout_future);
        loop {
            tokio::select! {
                biased;
                notification = notifications.recv() => {
                    match notification {
                        Ok(notification) => {
                            forward_plugin_notification(&plugin_name, notification, &stream_tx).await;
                            notifications_forwarded.store(true, AtomicOrdering::SeqCst);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                plugin = %plugin_name,
                                skipped,
                                "plugin notification subscriber lagged; some events were dropped"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Reader task exited (plugin closed). Keep awaiting the
                            // request future; it'll resolve with ConnectionLost soon.
                        }
                    }
                }
                result = &mut request_future => {
                    break Some(result);
                }
                _ = &mut timeout_future => {
                    break None;
                }
            }
        }
    };

    dispatch_observer.observe_duration(&plugin_name, &method, request_start.elapsed());

    loop {
        match notifications.try_recv() {
            Ok(notification) => {
                forward_plugin_notification(&plugin_name, notification, &stream_tx).await;
                notifications_forwarded.store(true, AtomicOrdering::SeqCst);
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                tracing::warn!(
                    plugin = %plugin_name,
                    skipped,
                    "plugin notification subscriber lagged during drain; some events were dropped"
                );
            }
            Err(_) => break,
        }
    }

    match outcome {
        Some(Ok(value)) => ResponseOutcome::Ok(value),
        Some(Err(error)) => ResponseOutcome::Err(error),
        None => ResponseOutcome::Timeout,
    }
}

/// Extract the provider plugin's own `session_id` from an `agent/run`
/// response payload. The plugin runtime serializes it under the top-level
/// `session_id` key (see animus_plugin_runtime::handle_agent_run). The
/// caller surfaces this value into a follow-up `SessionEvent::Started` so
/// the agent-runner sidecar replaces its initial control_session_id with
/// the plugin's real id — daemon-restart resume then dispatches a session
/// id the plugin process actually issued.
fn extract_provider_session_id(response: &Value) -> Option<String> {
    response.get("session_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(ToOwned::to_owned)
}

/// Drain a resumed `agent/resume` event stream to terminal state. Mirrors the
/// run-path logic in `dispatch`: when the plugin emits a late
/// `SessionEvent::Started` with its own real `session_id`, we replace the
/// initial control id with that provider id so the daemon persists an id the
/// plugin process actually issued — never the host-local UUID. Without this
/// the next daemon restart resumes with an id the provider never minted and
/// the workflow blocks.
async fn drain_resume_events(
    initial_session_id: Option<String>,
    mut events: mpsc::Receiver<SessionEvent>,
) -> ResumeAgentOutcome {
    let mut resumed_session_id = initial_session_id;
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::Started { session_id: Some(real_sid), .. } if !real_sid.trim().is_empty() => {
                resumed_session_id = Some(real_sid);
            }
            SessionEvent::Finished { exit_code } => {
                if exit_code.unwrap_or(0) == 0 {
                    return ResumeAgentOutcome::Resumed { session_id: resumed_session_id };
                }
                return ResumeAgentOutcome::Failed {
                    reason: format!(
                        "resume_agent failed: plugin exited with code {}",
                        exit_code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string())
                    ),
                };
            }
            SessionEvent::Error { message, recoverable: false } => {
                return ResumeAgentOutcome::Failed { reason: format!("resume_agent failed: {message}") };
            }
            _ => {}
        }
    }
    ResumeAgentOutcome::Failed { reason: "resume_agent failed: stream closed before Finished".to_string() }
}

/// Translate a JSON-RPC notification emitted by a provider plugin into the
/// matching SessionEvent and forward it to the SessionRun receiver.
async fn forward_plugin_notification(
    plugin_name: &str,
    notification: RpcNotification,
    tx: &mpsc::Sender<SessionEvent>,
) {
    match notification.method.as_str() {
        "agent/output" => {
            if let Some(text) = notification.params.as_ref().and_then(|p| p.get("text")).and_then(Value::as_str) {
                let _ = tx.send(SessionEvent::TextDelta { text: text.to_string() }).await;
            }
        }
        "agent/thinking" => {
            if let Some(text) = notification.params.as_ref().and_then(|p| p.get("text")).and_then(Value::as_str) {
                let _ = tx.send(SessionEvent::Thinking { text: text.to_string() }).await;
            }
        }
        "agent/toolCall" => {
            let params = notification.params.unwrap_or(Value::Null);
            let tool_name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let server = params.get("server").and_then(Value::as_str).map(ToOwned::to_owned);
            let _ = tx.send(SessionEvent::ToolCall { tool_name, arguments, server }).await;
        }
        "agent/toolResult" => {
            let params = notification.params.unwrap_or(Value::Null);
            let tool_name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let output = params.get("output").cloned().unwrap_or(Value::Null);
            let success = params.get("success").and_then(Value::as_bool).unwrap_or(true);
            let _ = tx.send(SessionEvent::ToolResult { tool_name, output, success }).await;
        }
        "agent/error" => {
            let params = notification.params.unwrap_or(Value::Null);
            let message = params.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
            let recoverable = params.get("recoverable").and_then(Value::as_bool).unwrap_or(true);
            let _ = tx.send(SessionEvent::Error { message, recoverable }).await;
        }
        "$/progress" | "agent/progress" => {
            if let Some(params) = notification.params {
                let _ = tx.send(SessionEvent::Metadata { metadata: params }).await;
            }
        }
        other => {
            // Unrecognized notification: surface as metadata so consumers can still inspect.
            tracing::debug!(plugin = %plugin_name, method = %other, "unrecognized plugin notification");
            if let Some(params) = notification.params {
                let _ =
                    tx.send(SessionEvent::Metadata { metadata: json!({ "method": other, "params": params }) }).await;
            }
        }
    }
}

/// Snapshot of a discovered provider plugin used to lazily build a `PluginSessionBackend`.
#[derive(Debug, Clone)]
pub struct DiscoveredProviderPlugin {
    pub plugin_name: String,
    pub provider_tool: String,
    pub binary_path: PathBuf,
    pub project_root: Option<PathBuf>,
    pub env_required: Vec<EnvRequirement>,
    /// Method names from the plugin's manifest (`PluginManifest::capabilities`).
    /// Forwarded into `PluginSessionBackend::declared_methods` so the
    /// backend reports honest `SessionCapabilities`.
    pub declared_methods: Vec<String>,
    /// The plugin's DECLARED MCP capability, derived from the manifest via
    /// [`provider_capabilities_declare_mcp`]. Forwarded into
    /// `PluginSessionBackend::declared_supports_mcp` so the backend's
    /// `capabilities().supports_mcp` — the source of truth for MCP-server
    /// injection — reflects the plugin's own declaration.
    pub declared_supports_mcp: bool,
    /// The manifest's `notification_buffer_size` hint, forwarded so spawned
    /// hosts honor the documented broadcast-capacity priority chain.
    pub notification_buffer_size: Option<usize>,
}

impl DiscoveredProviderPlugin {
    pub fn into_backend(self) -> Arc<PluginSessionBackend> {
        let mut backend = PluginSessionBackend::new(self.plugin_name, self.binary_path, self.provider_tool)
            .with_env_required(self.env_required)
            .with_declared_methods(self.declared_methods)
            .with_declared_supports_mcp(self.declared_supports_mcp)
            .with_notification_buffer_hint(self.notification_buffer_size);
        if let Some(root) = self.project_root {
            backend = backend.with_project_root(root);
        }
        Arc::new(backend)
    }
}

/// Inspect manifests of every discovered plugin under `project_root` and return only
/// the provider-kind plugins. Provider tool name defaults to the plugin name minus the
/// `animus-provider-` prefix.
pub fn discover_provider_plugins(project_root: &std::path::Path) -> Vec<DiscoveredProviderPlugin> {
    use crate::discover_by_kind;
    let project_root = project_root.to_path_buf();
    // v0.7 multi-kind: `discover_by_kind` matches a plugin's primary
    // `plugin_kind` OR any of its additional `plugin_kinds`, so a
    // consolidated plugin that serves `provider` as a secondary role is
    // resolved for agent dispatch.
    discover_by_kind(&project_root, animus_plugin_protocol::PLUGIN_KIND_PROVIDER)
        .unwrap_or_default()
        .into_iter()
        .map(|plugin| {
            let provider_tool = plugin
                .name
                .strip_prefix("animus-provider-")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| plugin.name.clone());
            let env_required = plugin.manifest.env_required.clone();
            let declared_methods = plugin.manifest.capabilities.clone();
            // Every provider plugin consumes MCP; the protocol carries no
            // per-plugin MCP-consume flag yet (TASK-277 sources this from a
            // first-class manifest/handshake capability). Until then, a
            // discovered provider declares MCP-capable.
            let declared_supports_mcp = true;
            let notification_buffer_size = plugin.manifest.notification_buffer_size;
            DiscoveredProviderPlugin {
                plugin_name: plugin.name,
                provider_tool,
                binary_path: plugin.path,
                project_root: Some(project_root.clone()),
                env_required,
                declared_methods,
                declared_supports_mcp,
                notification_buffer_size,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! Minimum-viable coverage for the cancel-routes-through-existing-host
    //! property. Heavier scenarios (concurrent runs, in-flight cancel
    //! propagation) intentionally deferred — see audit gap #7 follow-up.
    use super::*;
    use animus_plugin_protocol::{RpcRequest, RpcResponse};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex2;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Spin up an in-process fake plugin reachable through a `PluginHost`. The
    /// fake echoes every incoming JSON-RPC request back as a success response,
    /// recording how many requests it observed in a shared counter the caller
    /// can inspect.
    fn spawn_fake_host(name: &str) -> (PluginHost, Arc<AtomicUsize>) {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_task = counter.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let request: RpcRequest = match serde_json::from_str(trimmed) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };
                        if request.id.is_none() {
                            // Notification (e.g. `initialized`, `exit`); never echo.
                            continue;
                        }
                        counter_for_task.fetch_add(1, AtomicOrdering::SeqCst);
                        let response =
                            RpcResponse::ok(request.id, json!({ "method": request.method, "echo": request.params }));
                        let mut encoded = serde_json::to_string(&response).expect("encode response");
                        encoded.push('\n');
                        if plugin_writer.write_all(encoded.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let host = PluginHost::from_streams(name.to_string(), host_reader, host_writer);
        (host, counter)
    }

    fn fresh_backend() -> PluginSessionBackend {
        // binary_path is never spawned in these tests because we exclusively
        // exercise dispatch_cancel against pre-inserted handles. Any path works.
        PluginSessionBackend::new("test-plugin", PathBuf::from("/nonexistent/test-plugin"), "test")
    }

    /// Recording observer used by histogram-related tests. Captures every
    /// (plugin, method, elapsed) tuple so assertions can verify a sample was
    /// observed exactly once per dispatch round-trip.
    #[derive(Default)]
    struct RecordingObserver {
        samples: StdMutex2<Vec<(String, String, Duration)>>,
    }

    impl RecordingObserver {
        fn samples(&self) -> Vec<(String, String, Duration)> {
            self.samples.lock().expect("samples mutex poisoned").clone()
        }
    }

    impl DispatchObserver for RecordingObserver {
        fn observe_duration(&self, plugin: &str, method: &str, elapsed: Duration) {
            self.samples.lock().expect("samples mutex poisoned").push((
                plugin.to_string(),
                method.to_string(),
                elapsed,
            ));
        }
    }

    #[tokio::test]
    async fn cancel_unknown_session_returns_not_found() {
        let backend = fresh_backend();
        let err = backend.dispatch_cancel("never-existed").await.expect_err("cancel must error");
        let message = format!("{err}");
        assert!(
            message.contains("no active session") && message.contains("never-existed"),
            "unexpected error message: {message}"
        );
    }

    #[tokio::test]
    async fn cancel_routes_through_existing_host() {
        let backend = fresh_backend();
        let (host, request_counter) = spawn_fake_host("test-plugin");
        // Sanity: brand-new fake host has not seen any requests yet.
        assert_eq!(request_counter.load(AtomicOrdering::SeqCst), 0);

        backend.insert_session_for_test(
            "session-1",
            SessionHandle { host, started_at: Instant::now(), cancellation: true },
        );

        backend.dispatch_cancel("session-1").await.expect("cancel must succeed against the fake host");

        // The fake host's request counter went up by exactly one (the
        // agent/cancel call). If dispatch_cancel had spawned a NEW host and
        // bypassed our session, the counter would still be zero.
        assert_eq!(
            request_counter.load(AtomicOrdering::SeqCst),
            1,
            "cancel must route through the registered host, not a fresh transport"
        );
    }

    #[tokio::test]
    async fn cancel_removes_session_from_map_on_success() {
        let backend = fresh_backend();
        let (host, _counter) = spawn_fake_host("test-plugin");
        backend.insert_session_for_test(
            "session-2",
            SessionHandle { host, started_at: Instant::now(), cancellation: true },
        );
        assert!(backend.has_session_for_test("session-2"), "precondition: session must be registered");

        backend.dispatch_cancel("session-2").await.expect("cancel must succeed");

        assert!(
            !backend.has_session_for_test("session-2"),
            "session must be removed from the map after a successful cancel"
        );
    }

    #[tokio::test]
    async fn cancel_rejects_when_plugin_lacks_capability() {
        let backend = fresh_backend();
        let (host, request_counter) = spawn_fake_host("test-plugin");
        backend.insert_session_for_test(
            "session-3",
            SessionHandle { host, started_at: Instant::now(), cancellation: false },
        );

        let err = backend.dispatch_cancel("session-3").await.expect_err("cancel must error");
        // Callers should be able to pattern-match the typed variant.
        match &err {
            Error::CapabilityNotSupported { capability, .. } => {
                assert_eq!(capability, "cancellation");
            }
            Error::Upstream(other) => panic!("expected CapabilityNotSupported, got upstream: {other:?}"),
        }
        // And the Display impl is still descriptive for human-readable logs.
        let message = format!("{err}");
        assert!(message.contains("does not support capability 'cancellation'"), "unexpected error message: {message}");
        // The fake plugin must NOT have been called at all.
        assert_eq!(
            request_counter.load(AtomicOrdering::SeqCst),
            0,
            "cancel must short-circuit before issuing an RPC when capability is unset"
        );
    }

    /// Drives `run_request_with_notifications` through a live in-memory plugin
    /// host so we can assert that the typed `HostError`-based path resolves
    /// successful responses end-to-end and feeds the dispatch observer with a
    /// non-zero duration sample (Part B histogram wiring).
    #[tokio::test]
    async fn plugin_request_duration_observed_per_dispatch() {
        let (host, _counter) = spawn_fake_host("metrics-plugin");
        let notifications = host.subscribe_notifications();
        let observer = Arc::new(RecordingObserver::default());
        let observer_trait: Arc<dyn DispatchObserver> = observer.clone();
        let (tx, _rx) = mpsc::channel::<SessionEvent>(16);
        let outcome = run_request_with_notifications(
            host.clone(),
            notifications,
            "agent/ping".to_string(),
            json!({}),
            Duration::from_secs(5),
            tx,
            "metrics-plugin".to_string(),
            Arc::new(AtomicBool::new(false)),
            observer_trait,
        )
        .await;
        assert!(matches!(outcome, ResponseOutcome::Ok(_)), "fake host must echo a successful response");
        let _ = graceful_shutdown(host).await;

        let samples = observer.samples();
        assert_eq!(samples.len(), 1, "observer must be called exactly once per dispatch");
        let (plugin, method, _elapsed) = &samples[0];
        assert_eq!(plugin, "metrics-plugin");
        assert_eq!(method, "agent/ping");
    }

    /// Sanity-check that the dispatch path's retry decision now sources from
    /// `classify(&HostError)` rather than the deleted `is_death_like_error`.
    /// Asserting the classifier directly here closes the type-level
    /// guarantee — if Agent A ever renames or removes `HostError` variants,
    /// this test forces a compile failure on the session module side.
    #[test]
    fn dispatch_uses_typed_host_error_for_retry_decision() {
        use super::super::plugin_supervisor::{classify, RetryDecision};
        assert_eq!(classify(&HostError::ConnectionLost), RetryDecision::DeathLike);
        let plugin_side = HostError::Rpc(animus_plugin_protocol::RpcError {
            code: -32602,
            message: "bad params".to_string(),
            data: None,
        });
        assert_eq!(classify(&plugin_side), RetryDecision::StructuredError);
    }

    /// `reasoning_effort` placed on `SessionRequest.extras` must reach the
    /// plugin as a top-level RPC param. The provider plugins flatten unknown
    /// top-level params into `AgentRunRequest.extras` and forward them onto
    /// the session backend's `extras`, where the codex/claude transports read
    /// `extras.reasoning_effort`. If `build_run_params` drops the key, the
    /// `--reasoning-effort` flag is a silent no-op for plugin-backed runs.
    #[test]
    fn build_run_params_forwards_reasoning_effort() {
        let backend = fresh_backend();
        let request = SessionRequest {
            tool: "codex".to_string(),
            model: "gpt-5-codex".to_string(),
            prompt: "hi".to_string(),
            cwd: PathBuf::from("/tmp"),
            project_root: None,
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({ "reasoning_effort": "high" }),
            actor: None,
        };
        let params = backend.build_run_params(&request, None, "ctrl-test");
        assert_eq!(
            params.get("reasoning_effort").and_then(Value::as_str),
            Some("high"),
            "reasoning_effort must be forwarded as a top-level RPC param"
        );

        // Absent => the key must not appear at all (regression: every provider
        // behaves exactly as before when no effort is requested).
        let bare = SessionRequest { extras: json!({}), ..request };
        let bare_params = backend.build_run_params(&bare, None, "ctrl-test");
        assert!(bare_params.get("reasoning_effort").is_none(), "absent reasoning_effort must not inject the key");
    }

    /// `extras.approvals` placed on the `SessionRequest` must reach the plugin
    /// as a top-level RPC param. The claude transport reads `extras.approvals`
    /// to wire `--permission-prompt-tool` (and strip
    /// `--dangerously-skip-permissions`). If `build_run_params` drops the key,
    /// kernel-mediated approvals are a silent no-op for plugin-backed runs and
    /// the gate fails OPEN — claude runs every tool unprompted.
    #[test]
    fn build_run_params_forwards_approvals() {
        let backend = fresh_backend();
        let request = SessionRequest {
            tool: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            prompt: "hi".to_string(),
            cwd: PathBuf::from("/tmp"),
            project_root: None,
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({ "approvals": true }),
            actor: None,
        };
        let params = backend.build_run_params(&request, None, "ctrl-test");
        assert_eq!(
            params.get("approvals").and_then(Value::as_bool),
            Some(true),
            "approvals must be forwarded as a top-level RPC param so the gate wires"
        );

        // Absent => the key must not appear at all.
        let bare = SessionRequest { extras: json!({}), ..request };
        let bare_params = backend.build_run_params(&bare, None, "ctrl-test");
        assert!(bare_params.get("approvals").is_none(), "absent approvals must not inject the key");
    }

    /// The host-minted control_session_id must ride along on every
    /// `agent/run` / `agent/resume` request so the plugin runtime can map it
    /// to the backend's own session id and translate mid-run `agent/cancel`
    /// calls. Without it the plugin only learns the control id at cancel
    /// time, when its backend no longer recognizes the id.
    #[test]
    fn build_run_params_includes_control_session_id() {
        let backend = fresh_backend();
        let request = SessionRequest {
            tool: "test".to_string(),
            model: "m".to_string(),
            prompt: "hi".to_string(),
            cwd: PathBuf::from("/tmp"),
            project_root: None,
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
            actor: None,
        };
        let control_session_id = Uuid::new_v4().to_string();
        let params = backend.build_run_params(&request, None, &control_session_id);
        assert_eq!(
            params.get("control_session_id").and_then(Value::as_str),
            Some(control_session_id.as_str()),
            "agent/run params must carry the host's control_session_id for cancel translation"
        );
        // Resume path keeps both ids distinct: `session_id` is the provider's
        // resume target, `control_session_id` is the host's cancel key.
        let resume_params = backend.build_run_params(&request, Some("sess-provider"), &control_session_id);
        assert_eq!(resume_params.get("session_id").and_then(Value::as_str), Some("sess-provider"));
        assert_eq!(resume_params.get("control_session_id").and_then(Value::as_str), Some(control_session_id.as_str()));
    }

    /// The daemon-restart resume path rebuilds a `SessionRequest` from the
    /// checkpointed `AgentRunRequest` JSON. A `context.mcp_servers` map (the
    /// per-phase resolved server set the workflow runner populates alongside
    /// `context.runtime_contract`) must survive into `extras` so
    /// `build_run_params` forwards it to the provider on resume exactly as on
    /// the original dispatch.
    #[test]
    fn resume_request_forwards_checkpointed_mcp_servers() {
        let checkpoint = json!({
            "model": "gpt-5-codex",
            "context": {
                "tool": "codex",
                "prompt": "continue",
                "cwd": "/tmp",
                "runtime_contract": { "cli": { "name": "codex" } },
                "mcp_servers": { "animus": { "command": "/usr/local/bin/animus", "args": ["mcp", "serve"] } },
            },
        });
        let request = session_request_from_agent_run_request("codex", &checkpoint).expect("request builds");
        assert_eq!(
            request.extras.pointer("/mcp_servers/animus/command").and_then(Value::as_str),
            Some("/usr/local/bin/animus"),
            "context.mcp_servers must be copied into extras for resume; extras: {}",
            request.extras
        );
        let backend = fresh_backend();
        let params = backend.build_run_params(&request, Some("sess-1"), "ctrl-test");
        assert!(
            params.pointer("/mcp_servers/animus").is_some(),
            "build_run_params must forward the resumed mcp_servers; params: {params}"
        );
    }

    /// The plugin runtime serializes the provider's own session_id under the
    /// top-level `session_id` key of every agent/run response (see
    /// animus_plugin_runtime::handle_agent_run). The session module MUST
    /// surface that value verbatim into the follow-up `Started` event so the
    /// agent-runner persists the plugin's real id — never the host-local
    /// control_session_id. Regression test for the codex round-3 P2 finding.
    #[test]
    fn plugin_backed_provider_returns_provider_session_id_not_control_id() {
        let plugin_response = json!({
            "session_id": "sess-plugin-real",
            "output": "ok",
            "exit_code": 0,
        });
        let extracted = extract_provider_session_id(&plugin_response);
        assert_eq!(
            extracted.as_deref(),
            Some("sess-plugin-real"),
            "extractor must return the plugin's session_id from the response payload"
        );

        let control_session_id = Uuid::new_v4().to_string();
        assert_ne!(
            extracted.as_deref(),
            Some(control_session_id.as_str()),
            "extractor must NOT return a freshly-minted control_session_id"
        );

        // Whitespace / empty / missing all degrade to None so the dispatch
        // path skips the follow-up Started emission rather than persisting
        // an unusable id.
        assert!(extract_provider_session_id(&json!({})).is_none(), "missing session_id => None");
        assert!(extract_provider_session_id(&json!({"session_id": ""})).is_none(), "empty session_id => None");
        assert!(
            extract_provider_session_id(&json!({"session_id": "   "})).is_none(),
            "whitespace-only session_id => None"
        );
        assert!(
            extract_provider_session_id(&json!({"session_id": 42})).is_none(),
            "non-string session_id => None (would otherwise leak a junk id into the sidecar)"
        );
    }

    /// Resume path mirror of the run-path session-id fix: when the resumed
    /// stream emits a late `Started` event carrying the plugin's REAL session
    /// id, `drain_resume_events` must replace the initial control id with
    /// that provider id. Otherwise `auto_resume_running_checkpoints` persists
    /// the host-local control UUID as `provider_session_id`, and the next
    /// daemon restart resumes against an id the provider never issued —
    /// blocking the workflow. Regression test for the codex round-5 P2.
    #[tokio::test]
    async fn resume_agent_for_restart_returns_provider_session_id_not_control_id() {
        let control_session_id = Uuid::new_v4().to_string();
        let provider_session_id = "sess-plugin-real-resumed".to_string();

        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        tx.send(SessionEvent::Started {
            backend: "plugin:test".to_string(),
            session_id: Some(provider_session_id.clone()),
            pid: None,
        })
        .await
        .unwrap();
        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        drop(tx);

        let outcome = drain_resume_events(Some(control_session_id.clone()), rx).await;
        match outcome {
            ResumeAgentOutcome::Resumed { session_id } => {
                assert_eq!(
                    session_id.as_deref(),
                    Some(provider_session_id.as_str()),
                    "resume must surface the provider's real session_id, not the control id"
                );
                assert_ne!(
                    session_id.as_deref(),
                    Some(control_session_id.as_str()),
                    "resume must NOT return the host-local control_session_id"
                );
            }
            other @ ResumeAgentOutcome::Failed { .. } => panic!("expected Resumed, got: {other:?}"),
        }

        // Whitespace / empty Started session_ids must be ignored — fall back
        // to the original control id rather than overwriting it with junk.
        let (tx2, rx2) = mpsc::channel::<SessionEvent>(8);
        tx2.send(SessionEvent::Started {
            backend: "plugin:test".to_string(),
            session_id: Some("   ".to_string()),
            pid: None,
        })
        .await
        .unwrap();
        tx2.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        drop(tx2);
        let outcome2 = drain_resume_events(Some(control_session_id.clone()), rx2).await;
        match outcome2 {
            ResumeAgentOutcome::Resumed { session_id } => {
                assert_eq!(
                    session_id.as_deref(),
                    Some(control_session_id.as_str()),
                    "whitespace-only provider id must not displace the control id"
                );
            }
            other @ ResumeAgentOutcome::Failed { .. } => panic!("expected Resumed, got: {other:?}"),
        }

        // No Started event at all → fall back to the initial control id.
        let (tx3, rx3) = mpsc::channel::<SessionEvent>(8);
        tx3.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        drop(tx3);
        let outcome3 = drain_resume_events(Some(control_session_id.clone()), rx3).await;
        match outcome3 {
            ResumeAgentOutcome::Resumed { session_id } => {
                assert_eq!(session_id.as_deref(), Some(control_session_id.as_str()));
            }
            other @ ResumeAgentOutcome::Failed { .. } => panic!("expected Resumed, got: {other:?}"),
        }
    }

    /// `PluginSessionBackend::capabilities()` previously hardcoded
    /// `supports_resume = true` and `supports_terminate = true`, which made
    /// the runtime take resume/cancel paths against plugins that returned
    /// `MethodNotFound`. The honest reporting now reads the manifest's
    /// `capabilities` (the method list) plumbed through
    /// `with_declared_methods`, so plugins that do not declare
    /// `agent/resume` / `agent/cancel` advertise the corresponding flag as
    /// `false`.
    #[test]
    fn capabilities_reflect_plugin_manifest_not_hardcoded_true() {
        use animus_session_backend::session::session_backend::SessionBackend;

        // Manifest claims neither agent/resume nor agent/cancel.
        let backend_silent = PluginSessionBackend::new("silent-plugin", PathBuf::from("/nonexistent/silent"), "silent")
            .with_declared_methods(vec!["agent/run".to_string()]);
        let caps_silent = backend_silent.capabilities();
        assert!(!caps_silent.supports_resume, "plugin without agent/resume must report supports_resume=false");
        assert!(!caps_silent.supports_terminate, "plugin without agent/cancel must report supports_terminate=false");

        // Manifest only claims agent/resume.
        let backend_resume_only =
            PluginSessionBackend::new("resume-only-plugin", PathBuf::from("/nonexistent/resume-only"), "resume-only")
                .with_declared_methods(vec!["agent/run".to_string(), "agent/resume".to_string()]);
        let caps_resume_only = backend_resume_only.capabilities();
        assert!(caps_resume_only.supports_resume, "plugin with agent/resume must report supports_resume=true");
        assert!(!caps_resume_only.supports_terminate, "manifest without agent/cancel must NOT advertise terminate");

        // Manifest claims both.
        let backend_full =
            PluginSessionBackend::new("full-plugin", PathBuf::from("/nonexistent/full"), "full").with_declared_methods(
                vec!["agent/run".to_string(), "agent/resume".to_string(), "agent/cancel".to_string()],
            );
        let caps_full = backend_full.capabilities();
        assert!(caps_full.supports_resume);
        assert!(caps_full.supports_terminate);

        // Empty manifest plumbing (back-compat / direct constructor) defaults
        // to the conservative posture — never overclaim.
        let backend_default =
            PluginSessionBackend::new("default-plugin", PathBuf::from("/nonexistent/default"), "default");
        let caps_default = backend_default.capabilities();
        assert!(
            !caps_default.supports_resume && !caps_default.supports_terminate,
            "no declared_methods plumbed → must default to false, not the legacy hardcoded true"
        );
    }

    #[test]
    fn capabilities_supports_mcp_reflects_declared_field_not_a_hardcoded_true() {
        use animus_session_backend::session::session_backend::SessionBackend;

        // Default constructor => MCP-capable.
        let default_backend = PluginSessionBackend::new("p", PathBuf::from("/nonexistent/p"), "p");
        assert!(default_backend.capabilities().supports_mcp, "provider defaults to MCP-capable");

        // A provider that DECLARES it does not consume MCP reports false, so the
        // daemon injects no profile/skill MCP servers for it.
        let opted_out =
            PluginSessionBackend::new("q", PathBuf::from("/nonexistent/q"), "q").with_declared_supports_mcp(false);
        assert!(!opted_out.capabilities().supports_mcp, "declared supports_mcp:false must be honored");
    }

    fn env_req(name: &str) -> EnvRequirement {
        EnvRequirement { name: name.to_string(), description: None, required: false, ..Default::default() }
    }

    /// Plugin with an empty `env_required` manifest must NOT receive arbitrary
    /// runner-sourced env vars (e.g. `OPENAI_API_KEY`) on the spawn or RPC
    /// surface. Pre-fix, `dispatch` forwarded every key the runner had in
    /// its sanitized launch env, bypassing the manifest gate the plugin
    /// author guide documents as the contract.
    #[test]
    fn filter_request_env_vars_drops_keys_not_in_manifest_or_base() {
        let backend = PluginSessionBackend::new("silent-plugin", PathBuf::from("/nonexistent/silent"), "silent");
        // Runner-supplied env: a secret the plugin never asked for plus a
        // base-allowlist var that's always forwarded.
        let runner_env = vec![
            ("OPENAI_API_KEY".to_string(), "sk-leak".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-leak".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        let filtered = backend.filter_request_env_vars(&runner_env);
        let keys: Vec<&str> = filtered.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.contains(&"OPENAI_API_KEY"), "plugin with empty env_required must NOT receive OPENAI_API_KEY");
        assert!(
            !keys.contains(&"ANTHROPIC_API_KEY"),
            "plugin with empty env_required must NOT receive ANTHROPIC_API_KEY"
        );
        assert!(keys.contains(&"PATH"), "base-allowlist var PATH must still pass through");
    }

    /// Plugin whose manifest declares `OPENAI_API_KEY` MUST receive it; the
    /// filter is a gate, not a blanket scrub.
    #[test]
    fn filter_request_env_vars_passes_keys_declared_in_manifest() {
        let backend = PluginSessionBackend::new("oai-plugin", PathBuf::from("/nonexistent/oai"), "oai")
            .with_env_required(vec![env_req("OPENAI_API_KEY"), env_req("OPENAI_ORG_ID")]);
        let runner_env = vec![
            ("OPENAI_API_KEY".to_string(), "sk-real".to_string()),
            ("OPENAI_ORG_ID".to_string(), "org-1".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-not-mine".to_string()),
            ("HOME".to_string(), "/Users/x".to_string()),
        ];
        let filtered = backend.filter_request_env_vars(&runner_env);
        let keys: std::collections::HashSet<&str> = filtered.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains("OPENAI_API_KEY"), "manifest-declared key must pass through");
        assert!(keys.contains("OPENAI_ORG_ID"), "manifest-declared key must pass through");
        assert!(keys.contains("HOME"), "base-allowlist key must pass through");
        assert!(
            !keys.contains("ANTHROPIC_API_KEY"),
            "key NOT in manifest must be dropped even when manifest declares other secrets"
        );
    }

    /// The RPC `env` param built by `build_run_params` must use the SAME
    /// filtered list as the spawn-time allowlist — both surfaces are the
    /// only legs request env leaves the host on, and they have to apply the
    /// same gate. Regression for the RPC-side bypass that left every key in
    /// SessionRequest.env_vars visible to the plugin even when the host's
    /// spawn-time env scrub would have dropped it.
    #[test]
    fn build_run_params_env_only_contains_filtered_keys() {
        let backend = PluginSessionBackend::new("oai-plugin", PathBuf::from("/nonexistent/oai"), "oai")
            .with_env_required(vec![env_req("OPENAI_API_KEY")]);
        // Caller-side filter (matches what dispatch() does to request.env_vars
        // before handing it to build_run_params).
        let runner_env = vec![
            ("OPENAI_API_KEY".to_string(), "sk-real".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-leak".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        let filtered = backend.filter_request_env_vars(&runner_env);
        let req = SessionRequest {
            tool: "oai".to_string(),
            model: "gpt".to_string(),
            prompt: "hi".to_string(),
            cwd: PathBuf::from("/tmp"),
            project_root: None,
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: filtered,
            extras: Value::Object(Default::default()),
            actor: None,
        };
        let params = backend.build_run_params(&req, None, "ctrl-test");
        let env_obj = params.get("env").and_then(Value::as_object).expect("env param must be an object");
        assert!(env_obj.contains_key("OPENAI_API_KEY"), "manifest-declared key reaches RPC env param");
        assert!(env_obj.contains_key("PATH"), "base-allowlist key reaches RPC env param");
        assert!(
            !env_obj.contains_key("ANTHROPIC_API_KEY"),
            "non-manifest key must NOT appear in the RPC env param payload — closes the bypass"
        );
    }

    /// The agent-runner duplicates the merged launch env under
    /// `extras.runtime_contract.cli.launch.env`. `build_run_params` forwards
    /// `runtime_contract` to the plugin verbatim, so the manifest gate has to
    /// scrub THAT map too — otherwise a plugin can read a non-declared secret
    /// out of the runtime contract payload even after the top-level RPC `env`
    /// was filtered. Regression for codex round-1 P1.
    #[test]
    fn scrub_runtime_contract_env_strips_undeclared_keys() {
        let backend = PluginSessionBackend::new("silent-plugin", PathBuf::from("/nonexistent/silent"), "silent");
        let allowed = backend.allowed_env_keys();
        let mut extras = json!({
            "runtime_contract": {
                "cli": {
                    "name": "silent",
                    "launch": {
                        "command": "/usr/bin/silent",
                        "args": [],
                        "env": {
                            "OPENAI_API_KEY": "sk-leak",
                            "ANTHROPIC_API_KEY": "sk-leak",
                            "PATH": "/usr/bin",
                        },
                        "prompt_via_stdin": true,
                    }
                }
            }
        });
        scrub_runtime_contract_env(&mut extras, &allowed);
        let scrubbed_env = extras
            .pointer("/runtime_contract/cli/launch/env")
            .and_then(Value::as_object)
            .expect("env path must still exist after scrub");
        assert!(
            !scrubbed_env.contains_key("OPENAI_API_KEY"),
            "non-manifest key in runtime_contract launch env must be stripped"
        );
        assert!(
            !scrubbed_env.contains_key("ANTHROPIC_API_KEY"),
            "non-manifest key in runtime_contract launch env must be stripped"
        );
        assert!(scrubbed_env.contains_key("PATH"), "base-allowlist key must survive the scrub");
    }

    /// The same scrub must preserve manifest-declared keys so plugins that
    /// genuinely need a secret still receive it through the runtime contract
    /// pathway (this is the symmetric case to the test above).
    #[test]
    fn scrub_runtime_contract_env_keeps_manifest_declared_keys() {
        let backend = PluginSessionBackend::new("oai-plugin", PathBuf::from("/nonexistent/oai"), "oai")
            .with_env_required(vec![env_req("OPENAI_API_KEY")]);
        let allowed = backend.allowed_env_keys();
        let mut extras = json!({
            "runtime_contract": {
                "cli": {
                    "name": "oai",
                    "launch": {
                        "env": {
                            "OPENAI_API_KEY": "sk-real",
                            "ANTHROPIC_API_KEY": "sk-leak",
                        }
                    }
                }
            }
        });
        scrub_runtime_contract_env(&mut extras, &allowed);
        let scrubbed_env = extras
            .pointer("/runtime_contract/cli/launch/env")
            .and_then(Value::as_object)
            .expect("env path must still exist after scrub");
        assert!(scrubbed_env.contains_key("OPENAI_API_KEY"), "manifest-declared secret must survive");
        assert!(!scrubbed_env.contains_key("ANTHROPIC_API_KEY"), "non-manifest secret must still be stripped");
    }

    /// Best-effort scrub: when the runtime_contract path is absent (or has
    /// the wrong shape) the helper silently no-ops instead of panicking.
    #[test]
    fn scrub_runtime_contract_env_is_noop_when_path_absent() {
        let backend = PluginSessionBackend::new("noop-plugin", PathBuf::from("/nonexistent/noop"), "noop");
        let allowed = backend.allowed_env_keys();
        let mut empty = json!({});
        scrub_runtime_contract_env(&mut empty, &allowed);
        assert_eq!(empty, json!({}), "absent runtime_contract must leave extras unchanged");

        let mut wrong_shape = json!({ "runtime_contract": "not an object" });
        scrub_runtime_contract_env(&mut wrong_shape, &allowed);
        assert_eq!(
            wrong_shape,
            json!({ "runtime_contract": "not an object" }),
            "wrong-shape runtime_contract must leave extras unchanged"
        );
    }
}
