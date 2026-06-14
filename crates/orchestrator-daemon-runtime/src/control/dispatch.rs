//! [`InProcessSurface`] — daemon-side [`ControlSurface`] implementation.
//!
//! Translates each control-protocol method into the existing in-process
//! service call and returns the wire-shaped response. **No behavior
//! change.** This commit is the wire wrapper only; CLI/MCP/WebAPI
//! migrations follow in C6/C7/C8.
//!
//! ## Method coverage
//!
//! - **Subject ops** (`subject/list`, `subject/get`, `subject/create`,
//!   `subject/update`, `subject/next`, `subject/status`, `subject/watch`):
//!   forwarded as `<kind>/<verb>` JSON-RPC calls through
//!   [`SubjectPluginDispatch::route_call`]. The kind is extracted from
//!   either the request filter, the embedded subject id, or the explicit
//!   `kind` field depending on the verb. When the requested kind has no
//!   mounted backend the wire response is
//!   [`ControlError::NotFound`].
//! - **Daemon ops** (`daemon/status`, `daemon/health`, `daemon/agents`):
//!   surfaced directly from process state and the active dispatch
//!   handles. Streaming variants (`daemon/events`, `daemon/logs`) drain
//!   the broadcast buses created at daemon startup.
//! - **Workflow / agent / queue / project / plugin ops**: return
//!   [`ControlError::NotSupported`] for now. C6 (CLI cutover) is the
//!   first commit that needs them on the wire; the underlying services
//!   exist and will be wired through then.
//!
//! The surface deliberately fails closed: a missing dispatch handle
//! returns [`ControlError::Unavailable`], an unknown subject kind returns
//! [`ControlError::NotFound`], and any in-process error becomes
//! [`ControlError::Internal`]. Transports never see panics from this
//! layer.
//!
//! ## Anti-deadlock rules
//!
//! - No `tokio::sync::Mutex` for surface state. The handles inside are
//!   `Arc` clones of services set once at daemon startup.
//! - No locks held across `.await`. JSON translation is synchronous;
//!   the only `.await` is on the underlying service call.
//! - No `Drop` impls perform lock work.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{json, Value};

use animus_control_protocol::{
    control_trait::{DaemonEventStream, DaemonLogStream, SubjectWatchStream},
    types::{
        AgentCancelRequest, AgentRunRequest, AgentRunResult, AgentStatus, AgentStatusRequest, DaemonAgentsResponse,
        DaemonEventsRequest, DaemonHealthResponse, DaemonHealthStatus, DaemonLogEntry, DaemonLogsRequest,
        DaemonRunEvent as WireDaemonRunEvent, DaemonStatusResponse, PluginBrowseRequest, PluginCallRequest,
        PluginCallResponse, PluginHealth, PluginInfo, PluginInfoRequest, PluginInstallRequest, PluginInstallResponse,
        PluginListRequest, PluginListResponse, PluginPingRequest, PluginPingResponse, PluginSearchRequest,
        PluginSearchResponse, PluginUninstallRequest, PluginUpdateRequest, PluginUpdateResponse, ProjectInfo,
        ProjectInitRequest, ProjectSetupRequest, ProjectStatusResponse, QueueDropRequest, QueueEnqueueRequest,
        QueueEntry, QueueHoldRequest, QueueListRequest, QueueListResponse, QueueReleaseRequest, QueueReorderRequest,
        QueueStats, SubjectCreateRequest, SubjectGetRequest, SubjectListRequest, SubjectListResponse,
        SubjectNextRequest, SubjectNextResponse, SubjectStatusRequest, SubjectUpdateRequest, SubjectWatchRequest, Unit,
        WorkflowCancelRequest, WorkflowExecuteRequest, WorkflowGetRequest, WorkflowListRequest, WorkflowListResponse,
        WorkflowPauseRequest, WorkflowResumeRequest, WorkflowRun, WorkflowRunRequest, WorkflowRunStart,
    },
    ControlError, ControlSurface,
};
use animus_subject_protocol_wire::{Subject as WireSubject, SubjectChangedEvent};
use tokio::sync::broadcast;

use crate::log_storage::{resolve_log_storage_dispatch, LogStorageDispatch};
use crate::subject_dispatch::SubjectPluginDispatch;

use super::routing::{DaemonOpsRouting, PluginRouting, QueueRouting, WorkflowRouting};
use super::streaming::{DaemonEventBus, DaemonLogBus};

/// In-process [`ControlSurface`] used by the daemon's control server.
///
/// Holds [`Arc`]-cloned handles into the daemon's running service set.
/// Construct one at daemon startup (after [`SubjectPluginDispatch`] and
/// the broadcast buses resolve) and pass it into [`super::ControlServer::start`].
///
/// Cheap to clone; clones share the same underlying handles. Surface
/// methods never mutate the surface itself — all mutations flow through
/// the in-process services the surface wraps.
#[derive(Clone)]
pub struct InProcessSurface {
    project_root: PathBuf,
    daemon_pid: u32,
    daemon_version: String,
    started_at: SystemTime,
    subject_dispatch: Option<Arc<SubjectPluginDispatch>>,
    event_bus: Option<DaemonEventBus>,
    log_bus: Option<DaemonLogBus>,
    plugin_routing: Option<Arc<dyn PluginRouting>>,
    daemon_ops_routing: Option<Arc<dyn DaemonOpsRouting>>,
    workflow_routing: Option<Arc<dyn WorkflowRouting>>,
    queue_routing: Option<Arc<dyn QueueRouting>>,
}

impl InProcessSurface {
    /// Start building a new surface for `project_root`.
    pub fn builder(project_root: PathBuf) -> InProcessSurfaceBuilder {
        InProcessSurfaceBuilder {
            project_root,
            daemon_pid: std::process::id(),
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: SystemTime::now(),
            subject_dispatch: None,
            event_bus: None,
            log_bus: None,
            plugin_routing: None,
            daemon_ops_routing: None,
            workflow_routing: None,
            queue_routing: None,
        }
    }

    /// Project root the surface is bound to.
    pub fn project_root(&self) -> &PathBuf {
        &self.project_root
    }

    /// Borrow the subject dispatch handle, if one was provided.
    pub fn subject_dispatch(&self) -> Option<&Arc<SubjectPluginDispatch>> {
        self.subject_dispatch.as_ref()
    }

    /// Borrow the daemon event bus, if one was provided.
    pub fn event_bus(&self) -> Option<&DaemonEventBus> {
        self.event_bus.as_ref()
    }

    /// Borrow the daemon log bus, if one was provided.
    pub fn log_bus(&self) -> Option<&DaemonLogBus> {
        self.log_bus.as_ref()
    }

    fn subject_dispatch_or_unavailable(&self) -> Result<&Arc<SubjectPluginDispatch>, ControlError> {
        self.subject_dispatch
            .as_ref()
            .ok_or_else(|| ControlError::Unavailable("subject dispatch not initialized".to_string()))
    }

    async fn route_subject_call(&self, kind: &str, verb: &str, params: Option<Value>) -> Result<Value, ControlError> {
        let dispatch = self.subject_dispatch_or_unavailable()?;
        let method = format!("{kind}/{verb}");
        dispatch.route_call(&method, params).await.map_err(rpc_error_to_control_error)
    }
}

impl std::fmt::Debug for InProcessSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcessSurface")
            .field("project_root", &self.project_root)
            .field("daemon_pid", &self.daemon_pid)
            .field("daemon_version", &self.daemon_version)
            .field("subject_dispatch", &self.subject_dispatch.is_some())
            .field("event_bus", &self.event_bus.is_some())
            .field("log_bus", &self.log_bus.is_some())
            .field("plugin_routing", &self.plugin_routing.is_some())
            .field("daemon_ops_routing", &self.daemon_ops_routing.is_some())
            .field("workflow_routing", &self.workflow_routing.is_some())
            .field("queue_routing", &self.queue_routing.is_some())
            .finish_non_exhaustive()
    }
}

/// Fluent builder for [`InProcessSurface`].
pub struct InProcessSurfaceBuilder {
    project_root: PathBuf,
    daemon_pid: u32,
    daemon_version: String,
    started_at: SystemTime,
    subject_dispatch: Option<Arc<SubjectPluginDispatch>>,
    event_bus: Option<DaemonEventBus>,
    log_bus: Option<DaemonLogBus>,
    plugin_routing: Option<Arc<dyn PluginRouting>>,
    daemon_ops_routing: Option<Arc<dyn DaemonOpsRouting>>,
    workflow_routing: Option<Arc<dyn WorkflowRouting>>,
    queue_routing: Option<Arc<dyn QueueRouting>>,
}

impl InProcessSurfaceBuilder {
    /// Override the reported daemon PID (defaults to the current process).
    pub fn daemon_pid(mut self, pid: u32) -> Self {
        self.daemon_pid = pid;
        self
    }

    /// Override the reported daemon version (defaults to the runtime crate's
    /// `CARGO_PKG_VERSION`).
    pub fn daemon_version(mut self, version: impl Into<String>) -> Self {
        self.daemon_version = version.into();
        self
    }

    /// Override the daemon start timestamp (defaults to `SystemTime::now()`
    /// at builder construction).
    pub fn started_at(mut self, started_at: SystemTime) -> Self {
        self.started_at = started_at;
        self
    }

    /// Attach the active [`SubjectPluginDispatch`] handle.
    pub fn subject_dispatch(mut self, dispatch: Arc<SubjectPluginDispatch>) -> Self {
        self.subject_dispatch = Some(dispatch);
        self
    }

    /// Attach a [`DaemonEventBus`] for `daemon/events` streaming.
    pub fn event_bus(mut self, bus: DaemonEventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Attach a [`DaemonLogBus`] for `daemon/logs` streaming.
    pub fn log_bus(mut self, bus: DaemonLogBus) -> Self {
        self.log_bus = Some(bus);
        self
    }

    /// Attach a [`PluginRouting`] handle so the surface can answer
    /// `plugin/*` calls over the control wire. When absent, all
    /// `plugin/*` methods return [`ControlError::NotSupported`].
    pub fn plugin_routing(mut self, routing: Arc<dyn PluginRouting>) -> Self {
        self.plugin_routing = Some(routing);
        self
    }

    /// Attach a [`DaemonOpsRouting`] handle so `daemon/status`,
    /// `daemon/health`, and `daemon/agents` reflect live process state
    /// instead of the surface's stub responses.
    pub fn daemon_ops_routing(mut self, routing: Arc<dyn DaemonOpsRouting>) -> Self {
        self.daemon_ops_routing = Some(routing);
        self
    }

    /// Attach a [`WorkflowRouting`] handle so the surface can answer
    /// `workflow/*` calls over the control wire. When absent, all
    /// `workflow/*` methods return [`ControlError::NotSupported`].
    pub fn workflow_routing(mut self, routing: Arc<dyn WorkflowRouting>) -> Self {
        self.workflow_routing = Some(routing);
        self
    }

    /// Attach a [`QueueRouting`] handle so the surface can answer
    /// `queue/*` calls over the control wire. When absent, all
    /// `queue/*` methods return [`ControlError::NotSupported`].
    pub fn queue_routing(mut self, routing: Arc<dyn QueueRouting>) -> Self {
        self.queue_routing = Some(routing);
        self
    }

    /// Finalize the surface.
    pub fn build(self) -> InProcessSurface {
        InProcessSurface {
            project_root: self.project_root,
            daemon_pid: self.daemon_pid,
            daemon_version: self.daemon_version,
            started_at: self.started_at,
            subject_dispatch: self.subject_dispatch,
            event_bus: self.event_bus,
            log_bus: self.log_bus,
            plugin_routing: self.plugin_routing,
            daemon_ops_routing: self.daemon_ops_routing,
            workflow_routing: self.workflow_routing,
            queue_routing: self.queue_routing,
        }
    }
}

/// Translate a plugin-protocol [`RpcError`] (from
/// [`SubjectPluginDispatch::route_call`]) into a wire-shape
/// [`ControlError`].
///
/// Method-not-found at the routing layer is the dispatcher saying "no
/// plugin handles this kind" → [`ControlError::NotFound`]. Method-not-
/// supported is the plugin recognizing the verb but not implementing it
/// (e.g. polling-only backends rejecting `subject/watch`) → maps to
/// [`ControlError::NotSupported`]. Everything else lands in
/// [`ControlError::Internal`] with the plugin's message preserved.
fn rpc_error_to_control_error(error: animus_plugin_protocol::RpcError) -> ControlError {
    use animus_plugin_protocol::error_codes;
    match error.code {
        error_codes::METHOD_NOT_FOUND => ControlError::NotFound(error.message),
        error_codes::METHOD_NOT_SUPPORTED => ControlError::NotSupported(error.message),
        error_codes::INVALID_PARAMS => ControlError::InvalidRequest(error.message),
        error_codes::INVALID_REQUEST => ControlError::InvalidRequest(error.message),
        error_codes::TIMEOUT | error_codes::PLUGIN_NOT_INITIALIZED => ControlError::Unavailable(error.message),
        _ => ControlError::Internal(error.message),
    }
}

/// Extract the subject `kind` from an id string of the form
/// `"<backend>:<native_id>"`.
///
/// Subject ids carry an implicit kind prefix that the daemon's in-tree
/// adapters encode (e.g. `"task:TASK-1"`, `"requirement:REQ-1"`). For
/// requests like `subject/get` that carry only the id, this is how we
/// recover the kind needed to route through [`SubjectPluginDispatch`].
fn extract_kind_from_subject_id(id: &str) -> Option<&str> {
    id.split(':').next().filter(|s| !s.is_empty())
}

#[async_trait]
impl ControlSurface for InProcessSurface {
    // ----- Subject ----------------------------------------------------

    async fn subject_list(&self, request: SubjectListRequest) -> Result<SubjectListResponse, ControlError> {
        let kinds = request.filter.kind.clone();
        if kinds.is_empty() {
            return Err(ControlError::InvalidRequest(
                "subject/list requires a kind filter (e.g. filter.kind=['task']); routing without a kind is not supported"
                    .to_string(),
            ));
        }

        let params = json!({"filter": request.filter});
        let mut all_subjects: Vec<WireSubject> = Vec::new();
        let mut last_cursor: Option<String> = None;
        for kind in kinds {
            let raw = self.route_subject_call(&kind, "list", Some(params.clone())).await?;
            let list: SubjectListResponseRaw =
                serde_json::from_value(raw).map_err(|e| ControlError::Internal(format!("subject/list decode: {e}")))?;
            all_subjects.extend(list.subjects);
            last_cursor = list.next_cursor.or(last_cursor);
        }
        Ok(SubjectListResponse { subjects: all_subjects, next_cursor: last_cursor, fetched_at: chrono::Utc::now() })
    }

    async fn subject_get(&self, request: SubjectGetRequest) -> Result<WireSubject, ControlError> {
        let id = request.id.as_str().to_string();
        let kind = extract_kind_from_subject_id(&id)
            .ok_or_else(|| {
                ControlError::InvalidRequest(format!(
                    "subject/get id '{id}' is missing the '<kind>:' prefix; cannot route"
                ))
            })?
            .to_string();
        let raw = self.route_subject_call(&kind, "get", Some(json!({"id": id}))).await?;
        serde_json::from_value(raw).map_err(|e| ControlError::Internal(format!("subject/get decode: {e}")))
    }

    async fn subject_create(&self, request: SubjectCreateRequest) -> Result<WireSubject, ControlError> {
        let kind = request.kind.clone();
        let raw = self
            .route_subject_call(
                &kind,
                "create",
                Some(
                    serde_json::to_value(&request)
                        .map_err(|e| ControlError::Internal(format!("subject/create encode: {e}")))?,
                ),
            )
            .await?;
        // Control-socket writes land in the daemon process — wake the
        // scheduler loop directly for an immediate dispatch pass.
        crate::daemon::nudge_scheduler_local();
        serde_json::from_value(raw).map_err(|e| ControlError::Internal(format!("subject/create decode: {e}")))
    }

    async fn subject_update(&self, request: SubjectUpdateRequest) -> Result<WireSubject, ControlError> {
        let id = request.id.as_str().to_string();
        let kind = extract_kind_from_subject_id(&id)
            .ok_or_else(|| {
                ControlError::InvalidRequest(format!("subject/update id '{id}' is missing the '<kind>:' prefix"))
            })?
            .to_string();
        let raw = self.route_subject_call(&kind, "update", Some(json!({"id": id, "patch": request.patch}))).await?;
        crate::daemon::nudge_scheduler_local();
        serde_json::from_value(raw).map_err(|e| ControlError::Internal(format!("subject/update decode: {e}")))
    }

    async fn subject_next(&self, request: SubjectNextRequest) -> Result<SubjectNextResponse, ControlError> {
        let kind = request.kind.clone().ok_or_else(|| {
            ControlError::InvalidRequest(
                "subject/next requires an explicit kind to route through the matching backend".to_string(),
            )
        })?;
        let raw = self.route_subject_call(&kind, "next", Some(json!({}))).await?;
        // Backends typically return either `null`, `Subject`, or
        // `{"subject": Subject | null}`. Accept both shapes.
        if raw.is_null() {
            return Ok(SubjectNextResponse { subject: None });
        }
        if let Some(inner) = raw.get("subject") {
            let subject: Option<WireSubject> = serde_json::from_value(inner.clone())
                .map_err(|e| ControlError::Internal(format!("subject/next decode: {e}")))?;
            return Ok(SubjectNextResponse { subject });
        }
        let subject: WireSubject =
            serde_json::from_value(raw).map_err(|e| ControlError::Internal(format!("subject/next decode: {e}")))?;
        Ok(SubjectNextResponse { subject: Some(subject) })
    }

    async fn subject_status(&self, request: SubjectStatusRequest) -> Result<WireSubject, ControlError> {
        let id = request.id.as_str().to_string();
        let kind = extract_kind_from_subject_id(&id)
            .ok_or_else(|| {
                ControlError::InvalidRequest(format!("subject/status id '{id}' is missing the '<kind>:' prefix"))
            })?
            .to_string();
        let raw = self.route_subject_call(&kind, "status", Some(json!({"id": id, "status": request.status}))).await?;
        crate::daemon::nudge_scheduler_local();
        serde_json::from_value(raw).map_err(|e| ControlError::Internal(format!("subject/status decode: {e}")))
    }

    async fn subject_watch(&self, request: SubjectWatchRequest) -> Result<SubjectWatchStream, ControlError> {
        // C5 lands the wire wrapper; the persistent subject/watch stream
        // routing through plugins is out of scope. CLI clients today
        // poll via `subject/list`. Returning an empty stream lets the
        // ack-and-stream path on the connection drive without behavior
        // change.
        let _ = request;
        let stream = futures_util::stream::empty::<SubjectChangedEvent>();
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = SubjectChangedEvent> + Send>>)
    }

    // ----- Plugin -----------------------------------------------------

    async fn plugin_list(&self, request: PluginListRequest) -> Result<PluginListResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_list(request).await,
            None => Err(ControlError::NotSupported("plugin/list routing not configured".to_string())),
        }
    }

    async fn plugin_info(&self, request: PluginInfoRequest) -> Result<PluginInfo, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_info(request).await,
            None => Err(ControlError::NotSupported("plugin/info routing not configured".to_string())),
        }
    }

    async fn plugin_install(&self, request: PluginInstallRequest) -> Result<PluginInstallResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_install(request).await,
            None => Err(ControlError::NotSupported("plugin/install routing not configured".to_string())),
        }
    }

    async fn plugin_uninstall(&self, request: PluginUninstallRequest) -> Result<Unit, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_uninstall(request).await,
            None => Err(ControlError::NotSupported("plugin/uninstall routing not configured".to_string())),
        }
    }

    async fn plugin_ping(&self, request: PluginPingRequest) -> Result<PluginPingResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_ping(request).await,
            None => Err(ControlError::NotSupported("plugin/ping routing not configured".to_string())),
        }
    }

    async fn plugin_call(&self, request: PluginCallRequest) -> Result<PluginCallResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_call(request).await,
            None => Err(ControlError::NotSupported("plugin/call routing not configured".to_string())),
        }
    }

    async fn plugin_search(&self, request: PluginSearchRequest) -> Result<PluginSearchResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_search(request).await,
            None => Err(ControlError::NotSupported("plugin/search routing not configured".to_string())),
        }
    }

    async fn plugin_browse(&self, request: PluginBrowseRequest) -> Result<PluginSearchResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_browse(request).await,
            None => Err(ControlError::NotSupported("plugin/browse routing not configured".to_string())),
        }
    }

    async fn plugin_update(&self, request: PluginUpdateRequest) -> Result<PluginUpdateResponse, ControlError> {
        match &self.plugin_routing {
            Some(routing) => routing.plugin_update(request).await,
            None => Err(ControlError::NotSupported("plugin/update routing not configured".to_string())),
        }
    }

    // ----- Daemon -----------------------------------------------------

    async fn daemon_status(&self) -> Result<DaemonStatusResponse, ControlError> {
        if let Some(routing) = &self.daemon_ops_routing {
            return routing.daemon_status().await;
        }
        let uptime_seconds = self.started_at.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        Ok(DaemonStatusResponse {
            running: true,
            pid: Some(self.daemon_pid),
            uptime_seconds: Some(uptime_seconds),
            version: Some(self.daemon_version.clone()),
            project_root: Some(self.project_root.clone()),
            log_path: None,
        })
    }

    async fn daemon_health(&self) -> Result<DaemonHealthResponse, ControlError> {
        if let Some(routing) = &self.daemon_ops_routing {
            return routing.daemon_health().await;
        }
        // v0.4.10: live `health/check` RPC fan-out per plugin. For each
        // discovered plugin we spawn the binary, run the `initialize` +
        // `health/check` handshake under a tight (3s) per-plugin deadline,
        // then shut it down. The probe is per-call (no long-lived pool yet)
        // but bounded — health is not a hot endpoint, and concurrent
        // probes overlap so the wall time is roughly one probe regardless
        // of plugin count. A probe failure reports the plugin as
        // `Unhealthy` with the error string in `last_error`; the daemon's
        // own status stays `Healthy` because plugin-side trouble is an
        // observability concern, not a daemon-liveness one.
        let discovered = orchestrator_plugin_host::discover_plugins(&self.project_root).unwrap_or_default();
        let project_root = self.project_root.clone();
        let probes = discovered.into_iter().map(|p| {
            let project_root = project_root.clone();
            async move { probe_plugin_health(&p, &project_root).await }
        });
        let plugins: Vec<PluginHealth> = futures_util::future::join_all(probes).await;
        Ok(DaemonHealthResponse { status: DaemonHealthStatus::Healthy, plugins, last_error: None })
    }

    async fn daemon_start(&self) -> Result<Unit, ControlError> {
        // The daemon serving this request is already running; start is a
        // no-op per the protocol spec.
        Ok(Unit::default())
    }

    async fn daemon_stop(&self) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("daemon/stop must be issued via the CLI/MCP for now (C6/C7)".to_string()))
    }

    async fn daemon_restart(&self) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("daemon/restart must be issued via the CLI/MCP for now (C6/C7)".to_string()))
    }

    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError> {
        if let Some(routing) = &self.daemon_ops_routing {
            return routing.daemon_agents().await;
        }
        // No clean Arc reference to the live AgentPool yet; daemon-side
        // agent tracking lands as part of C7. Returning an empty list
        // matches the historical "no agents currently active" shape.
        Ok(DaemonAgentsResponse { agents: Vec::new() })
    }

    async fn daemon_events(&self, _request: DaemonEventsRequest) -> Result<DaemonEventStream, ControlError> {
        let bus = self
            .event_bus
            .as_ref()
            .ok_or_else(|| ControlError::Unavailable("daemon event bus not initialized".to_string()))?;
        let rx = bus.subscribe();
        let stream = broadcast_to_stream(rx)
            .filter_map(|value| async move { serde_json::from_value::<WireDaemonRunEvent>(value).ok() });
        Ok(Box::pin(stream))
    }

    async fn daemon_logs(&self, request: DaemonLogsRequest) -> Result<DaemonLogStream, ControlError> {
        // Route through the active log_storage backend. When a
        // `log_storage_backend` plugin is installed and supervised at
        // daemon startup, this calls the plugin's `log_storage/query`
        // RPC and returns its decoded entries. Otherwise we read the
        // in-tree `events.jsonl` file directly.
        //
        // Streaming `follow=true` is not yet wired through the plugin —
        // the plugin returns a one-shot batch via `log_storage/query`
        // and we close. The in-tree path tacks live notifications onto
        // the tail via the [`DaemonLogBus`] when one is attached.
        // A future revision can subscribe to the plugin's
        // `log_storage/event` notifications via
        // [`PluginHost::subscribe_notifications`] to extend follow-mode
        // to plugin-backed sinks.
        let project_root = self.project_root.clone();
        let limit = log_request_limit(&request);
        let level_floor = request.level;
        let plugin_filter = request.plugin.clone();
        let since_filter = request.since;

        let historical: Vec<DaemonLogEntry> = if let Some(handle) = crate::log_storage::current_log_storage_handle() {
            if handle.is_plugin() {
                // Plugin returns daemon-events (DaemonEventLog::append).
                // Merge with the in-tree file so Logger-only entries
                // (daemon startup messages, `Logger::for_project`
                // tracing, etc.) — which still write to events.jsonl —
                // remain visible in `animus logs tail`. Policy (B) tee
                // keeps the file authoritative for those entries until
                // the logging crate is updated to forward them too.
                //
                // When the plugin's query call fails (write-only sink,
                // METHOD_NOT_SUPPORTED, transient error), we degrade to
                // the file-only view rather than failing the request —
                // the tee guarantees the same daemon events are still
                // present in `events.jsonl`.
                let plugin_entries = match tail_plugin_log_entries(&handle, &request, limit).await {
                    Ok(entries) => entries,
                    Err(error) => {
                        tracing::warn!(
                            target: "daemon_event_log",
                            plugin = %handle.plugin_name().unwrap_or("<unknown>"),
                            error = %format!("{error:#}"),
                            "log_storage plugin query failed; serving daemon/logs from in-tree events.jsonl only",
                        );
                        Vec::new()
                    }
                };
                let plugin_entries =
                    filter_log_entries_locally(plugin_entries, level_floor, since_filter, plugin_filter.as_deref());
                let logger_entries = read_in_tree_log_entries(
                    handle.project_root(),
                    limit,
                    level_floor,
                    since_filter,
                    plugin_filter.as_deref(),
                );
                // Also surface the daemon-events tee file
                // (`daemon-events.jsonl` under the global config dir).
                // `DaemonEventLog::append` writes there in addition to
                // dispatching the plugin call, so on the plugin
                // failure path those records would otherwise be invisible
                // to `animus logs tail`.
                let daemon_event_entries = read_daemon_events_log_entries(limit, level_floor, since_filter);
                let mut combined = merge_and_cap_log_entries(plugin_entries, logger_entries, limit.saturating_mul(2));
                combined = merge_and_cap_log_entries(combined, daemon_event_entries, limit);
                combined
            } else {
                read_in_tree_log_entries(
                    handle.project_root(),
                    limit,
                    level_floor,
                    since_filter,
                    plugin_filter.as_deref(),
                )
            }
        } else {
            // No handle installed (CLI one-shot / surface bootstrap
            // race) → fall back to resolver + file read.
            let resolution = resolve_log_storage_dispatch(&project_root)
                .map_err(|e| ControlError::Internal(format!("log dispatch resolution: {e}")))?;
            match resolution.selected.as_ref() {
                LogStorageDispatch::InTree { project_root: pr }
                | LogStorageDispatch::Plugin { project_root: pr, .. } => {
                    read_in_tree_log_entries(pr, limit, level_floor, since_filter, plugin_filter.as_deref())
                }
            }
        };

        let historical_stream = futures_util::stream::iter(historical.into_iter().map(Ok));

        if request.follow {
            if let Some(bus) = self.log_bus.as_ref() {
                let rx = bus.subscribe();
                let live_stream = broadcast_to_stream(rx).map(|value| {
                    serde_json::from_value(value)
                        .map_err(|e| ControlError::Internal(format!("daemon/logs decode: {e}")))
                });
                return Ok(Box::pin(historical_stream.chain(live_stream)));
            }
            // follow=true but no bus: still send historical and close.
        }

        Ok(Box::pin(historical_stream))
    }

    // ----- Workflow ---------------------------------------------------

    async fn workflow_list(&self, request: WorkflowListRequest) -> Result<WorkflowListResponse, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_list(request).await,
            None => Err(ControlError::NotSupported("workflow/list routing not configured".to_string())),
        }
    }

    async fn workflow_get(&self, request: WorkflowGetRequest) -> Result<WorkflowRun, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_get(request).await,
            None => Err(ControlError::NotSupported("workflow/get routing not configured".to_string())),
        }
    }

    async fn workflow_run(&self, request: WorkflowRunRequest) -> Result<WorkflowRunStart, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_run(request).await,
            None => Err(ControlError::NotSupported("workflow/run routing not configured".to_string())),
        }
    }

    async fn workflow_execute(&self, request: WorkflowExecuteRequest) -> Result<WorkflowRunStart, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_execute(request).await,
            None => Err(ControlError::NotSupported("workflow/execute routing not configured".to_string())),
        }
    }

    async fn workflow_pause(&self, request: WorkflowPauseRequest) -> Result<Unit, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_pause(request).await,
            None => Err(ControlError::NotSupported("workflow/pause routing not configured".to_string())),
        }
    }

    async fn workflow_resume(&self, request: WorkflowResumeRequest) -> Result<Unit, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_resume(request).await,
            None => Err(ControlError::NotSupported("workflow/resume routing not configured".to_string())),
        }
    }

    async fn workflow_cancel(&self, request: WorkflowCancelRequest) -> Result<Unit, ControlError> {
        match &self.workflow_routing {
            Some(routing) => routing.workflow_cancel(request).await,
            None => Err(ControlError::NotSupported("workflow/cancel routing not configured".to_string())),
        }
    }

    // ----- Agent ------------------------------------------------------
    //
    // The control-wire `agent/*` surface has no in-tree implementation:
    // there is no daemon-side agent pool, and the live `animus agent`
    // CLI + MCP path runs entirely in-process via `SessionBackendResolver`
    // and provider plugins (not over the control socket). These methods
    // exist only to satisfy the external `ControlSurface` trait contract
    // and always report `NotSupported`.

    async fn agent_run(&self, _request: AgentRunRequest) -> Result<AgentRunResult, ControlError> {
        Err(ControlError::NotSupported("agent/run is not served over the control wire".to_string()))
    }

    async fn agent_status(&self, _request: AgentStatusRequest) -> Result<AgentStatus, ControlError> {
        Err(ControlError::NotSupported("agent/status is not served over the control wire".to_string()))
    }

    async fn agent_cancel(&self, _request: AgentCancelRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("agent/cancel is not served over the control wire".to_string()))
    }

    // ----- Queue ------------------------------------------------------

    async fn queue_list(&self, request: QueueListRequest) -> Result<QueueListResponse, ControlError> {
        match &self.queue_routing {
            Some(routing) => routing.queue_list(request).await,
            None => Err(ControlError::NotSupported("queue/list routing not configured".to_string())),
        }
    }

    async fn queue_enqueue(&self, request: QueueEnqueueRequest) -> Result<QueueEntry, ControlError> {
        match &self.queue_routing {
            Some(routing) => {
                let entry = routing.queue_enqueue(request).await?;
                // Control-socket enqueue lands in the daemon process —
                // wake the scheduler loop so the entry drains immediately.
                crate::daemon::nudge_scheduler_local();
                Ok(entry)
            }
            None => Err(ControlError::NotSupported("queue/enqueue routing not configured".to_string())),
        }
    }

    async fn queue_drop(&self, request: QueueDropRequest) -> Result<Unit, ControlError> {
        match &self.queue_routing {
            Some(routing) => routing.queue_drop(request).await,
            None => Err(ControlError::NotSupported("queue/drop routing not configured".to_string())),
        }
    }

    async fn queue_hold(&self, request: QueueHoldRequest) -> Result<Unit, ControlError> {
        match &self.queue_routing {
            Some(routing) => routing.queue_hold(request).await,
            None => Err(ControlError::NotSupported("queue/hold routing not configured".to_string())),
        }
    }

    async fn queue_release(&self, request: QueueReleaseRequest) -> Result<Unit, ControlError> {
        match &self.queue_routing {
            Some(routing) => {
                let result = routing.queue_release(request).await?;
                crate::daemon::nudge_scheduler_local();
                Ok(result)
            }
            None => Err(ControlError::NotSupported("queue/release routing not configured".to_string())),
        }
    }

    async fn queue_reorder(&self, request: QueueReorderRequest) -> Result<Unit, ControlError> {
        match &self.queue_routing {
            Some(routing) => routing.queue_reorder(request).await,
            None => Err(ControlError::NotSupported("queue/reorder routing not configured".to_string())),
        }
    }

    async fn queue_stats(&self) -> Result<QueueStats, ControlError> {
        match &self.queue_routing {
            Some(routing) => routing.queue_stats().await,
            None => Err(ControlError::NotSupported("queue/stats routing not configured".to_string())),
        }
    }

    // ----- Project ----------------------------------------------------

    async fn project_init(&self, _request: ProjectInitRequest) -> Result<ProjectInfo, ControlError> {
        Err(ControlError::NotSupported("project/init will be wired in C6 (CLI cutover)".to_string()))
    }

    async fn project_setup(&self, _request: ProjectSetupRequest) -> Result<ProjectInfo, ControlError> {
        Err(ControlError::NotSupported("project/setup will be wired in C6 (CLI cutover)".to_string()))
    }

    async fn project_status(&self) -> Result<ProjectStatusResponse, ControlError> {
        Err(ControlError::NotSupported("project/status will be wired in C6 (CLI cutover)".to_string()))
    }
}

/// Permissive intermediate shape for `subject/list` plugin responses.
///
/// Plugin backends generally return `{"subjects": [...], "next_cursor": "..."}`
/// without the `fetched_at` field the control protocol adds. We add the
/// timestamp on the daemon side and re-pack into [`SubjectListResponse`].
#[derive(serde::Deserialize)]
struct SubjectListResponseRaw {
    #[serde(default)]
    subjects: Vec<WireSubject>,
    #[serde(default)]
    next_cursor: Option<String>,
}

use futures_util::StreamExt;

/// Adapt a [`tokio::sync::broadcast::Receiver`] into a `Stream`.
///
/// Slow subscribers may surface `Lagged` errors when the broadcast buffer
/// overflows. We drop the lagged batch and resume; the control protocol
/// has no in-band "you skipped events" signal yet, so a brief gap is the
/// best we can do.
fn broadcast_to_stream(rx: broadcast::Receiver<Value>) -> impl Stream<Item = Value> + Send + 'static {
    tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|res| async move { res.ok() })
}

/// Default cap on historical entries served by `daemon/logs`.
///
/// The wire request shape lacks a `limit` field, so we cap server-side to
/// keep a single CLI invocation bounded. CLI callers stream the historical
/// batch and stop after they've collected the limit they want.
const DAEMON_LOGS_DEFAULT_LIMIT: usize = 200;

/// Pluck the effective per-call limit from a [`DaemonLogsRequest`]. Today
/// the wire request has no explicit limit; reserve the function so a
/// future protocol bump that adds one needs to update exactly one place.
fn log_request_limit(_request: &DaemonLogsRequest) -> usize {
    DAEMON_LOGS_DEFAULT_LIMIT
}

/// Read the daemon-events tee file (`daemon-events.jsonl` under the
/// global config dir, populated by [`crate::DaemonEventLog::append`])
/// and project each row into the wire [`DaemonLogEntry`] shape so it
/// merges cleanly with plugin / per-project Logger entries.
///
/// Returns entries chronologically (oldest first) capped at `limit`.
/// Failures to read the file degrade to an empty batch — the daemon
/// must continue serving `daemon/logs` even if the tee file is
/// momentarily missing or rotated.
fn read_daemon_events_log_entries(
    limit: usize,
    level_floor: Option<animus_log_storage_protocol::LogLevel>,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<DaemonLogEntry> {
    use animus_log_storage_protocol::{LogLevel, LogSource};

    let records = crate::DaemonEventLog::read_records(Some(limit.saturating_mul(2)), None).unwrap_or_default();
    let mut entries: Vec<DaemonLogEntry> = records
        .into_iter()
        .filter_map(|record| {
            let ts = chrono::DateTime::parse_from_rfc3339(&record.timestamp)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .ok()?;
            if since.is_some_and(|threshold| ts < threshold) {
                return None;
            }
            let target = format!("daemon.events.{}", record.event_type);
            Some(DaemonLogEntry {
                id: record.id,
                ts,
                level: LogLevel::Info,
                source: LogSource::Daemon,
                source_name: None,
                target,
                message: record.event_type,
                fields: record.data,
            })
        })
        .filter(|entry| level_floor.is_none_or(|floor| entry.level >= floor))
        .collect();
    entries.sort_by_key(|a| a.ts);
    if entries.len() > limit {
        let drop_count = entries.len() - limit;
        entries.drain(0..drop_count);
    }
    entries
}

/// Apply the daemon-side `daemon/logs` filters to a plugin-supplied
/// batch.
///
/// The wire response from a plugin's `log_storage/query` is trusted to
/// produce the right entries, but plugins are free to advertise partial
/// filter support via `LogStorageSchema::supports_filtering` (the host
/// is responsible for re-filtering anything the plugin can't honor). To
/// avoid leaking entries outside the caller's filter regardless of what
/// the backend supports, we re-apply `level_floor`, `since`, and
/// `plugin` (matched against `source_name`) here before merging with the
/// in-tree file batch.
fn filter_log_entries_locally(
    entries: Vec<DaemonLogEntry>,
    level_floor: Option<animus_log_storage_protocol::LogLevel>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    plugin_filter: Option<&str>,
) -> Vec<DaemonLogEntry> {
    entries
        .into_iter()
        .filter(|entry| level_floor.is_none_or(|floor| entry.level >= floor))
        .filter(|entry| since.is_none_or(|ts| entry.ts >= ts))
        .filter(|entry| plugin_filter.is_none_or(|name| entry.source_name.as_deref() == Some(name)))
        .collect()
}

/// Merge two log-entry batches (typically plugin + in-tree file) into a
/// single chronologically-ordered batch capped at `limit` entries.
///
/// Used by `daemon_logs` to combine the plugin's `log_storage/query`
/// response with the in-tree `events.jsonl` reader so Logger-only
/// entries (which still write to the file regardless of the plugin) stay
/// visible to `animus logs tail`.
///
/// Dedup is keyed on `DaemonLogEntry::id`: if the plugin already mirrors
/// a file entry under the same id we drop the duplicate. The returned
/// batch is ordered by `ts` ascending and trimmed to the most-recent
/// `limit` after sorting.
fn merge_and_cap_log_entries(
    plugin_entries: Vec<DaemonLogEntry>,
    file_entries: Vec<DaemonLogEntry>,
    limit: usize,
) -> Vec<DaemonLogEntry> {
    use std::collections::HashSet;

    let mut seen: HashSet<String> = HashSet::with_capacity(plugin_entries.len() + file_entries.len());
    let mut merged: Vec<DaemonLogEntry> = Vec::with_capacity(plugin_entries.len() + file_entries.len());
    for entry in plugin_entries.into_iter().chain(file_entries) {
        if seen.insert(entry.id.clone()) {
            merged.push(entry);
        }
    }
    merged.sort_by_key(|a| a.ts);
    if merged.len() > limit {
        let drop_count = merged.len() - limit;
        merged.drain(0..drop_count);
    }
    merged
}

/// Request a one-shot historical batch from the active
/// `log_storage_backend` plugin and decode the response into the wire
/// [`DaemonLogEntry`] shape.
///
/// Sends a synchronous `log_storage/query` request — the streaming
/// `log_storage/tail` method would require subscribing to
/// `log_storage/event` notifications which the daemon's `daemon/logs`
/// historical path doesn't need. The plugin is expected to respond with
/// `{"entries": [...]}` (the
/// [`animus_log_storage_protocol::LogQueryResult`] shape) or a bare JSON
/// array of entries. Unknown response shapes degrade to an empty result
/// so a malformed plugin response surfaces as "no entries" rather than
/// panicking the daemon.
async fn tail_plugin_log_entries(
    handle: &std::sync::Arc<crate::LogStorageHandle>,
    request: &DaemonLogsRequest,
    limit: usize,
) -> anyhow::Result<Vec<DaemonLogEntry>> {
    use std::time::Duration;

    let mut params = serde_json::Map::new();
    if let Some(level) = request.level {
        params.insert("min_level".to_string(), serde_json::to_value(level)?);
    }
    if let Some(ts) = request.since {
        params.insert("since".to_string(), serde_json::to_value(ts)?);
    }
    if let Some(name) = request.plugin.as_ref() {
        params.insert("source_name".to_string(), serde_json::Value::String(name.clone()));
    }
    params.insert("limit".to_string(), serde_json::Value::Number(limit.into()));

    let response = handle.tail(Some(serde_json::Value::Object(params)), Duration::from_secs(10)).await?;
    let Some(value) = response else { return Ok(Vec::new()) };

    // Accept either `{ entries: [...] }` (LogQueryResult) or a bare array.
    let entries_value = match value {
        serde_json::Value::Object(ref obj) => {
            obj.get("entries").cloned().unwrap_or(serde_json::Value::Array(Vec::new()))
        }
        serde_json::Value::Array(_) => value,
        _ => serde_json::Value::Array(Vec::new()),
    };
    let entries: Vec<DaemonLogEntry> = serde_json::from_value(entries_value).unwrap_or_default();
    Ok(entries)
}

/// Read historical entries from the project's in-tree `events.jsonl` and
/// convert each row from [`orchestrator_logging::LogEntry`] into the
/// protocol [`DaemonLogEntry`] shape. Filters mirror
/// [`DaemonLogsRequest`].
///
/// Returns entries in chronological order (oldest first) so the streamed
/// view matches how a human reads a tail. Plugin-backed dispatch falls
/// through to the same file today because the long-lived log-storage
/// plugin host is deferred — the file is the source of truth until a
/// supervisor exists to relay `log_storage/tail` against the plugin.
fn read_in_tree_log_entries(
    project_root: &std::path::Path,
    limit: usize,
    level_floor: Option<animus_log_storage_protocol::LogLevel>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    plugin_filter: Option<&str>,
) -> Vec<DaemonLogEntry> {
    use orchestrator_logging::Logger;
    let logger = Logger::for_project(project_root);
    // Map the wire level floor onto the in-tree Level enum. The wire has
    // a Trace level that the in-tree reader does not — Trace requests
    // degrade to Debug (the lowest in-tree level).
    let level_filter = level_floor.map(wire_level_to_local);
    let since_str = since.map(|ts| ts.to_rfc3339());

    // Overscan and trim, matching the CLI reader's strategy so the
    // returned set is the most recent `limit` matches even when many
    // entries fail the plugin filter.
    let pool = limit.saturating_mul(4).max(limit);
    let raw = logger.read_entries_since(pool, None, level_filter, since_str.as_deref());

    let mut filtered: Vec<orchestrator_logging::LogEntry> =
        raw.into_iter().filter(|entry| plugin_matches(entry, plugin_filter)).collect();
    if filtered.len() > limit {
        let drop = filtered.len() - limit;
        filtered.drain(0..drop);
    }

    filtered.into_iter().map(local_entry_to_wire).collect()
}

fn plugin_matches(entry: &orchestrator_logging::LogEntry, plugin_filter: Option<&str>) -> bool {
    let Some(name) = plugin_filter else {
        return true;
    };
    if entry.provider.as_deref() == Some(name) {
        return true;
    }
    entry.meta.as_ref().and_then(|v| v.get("plugin")).and_then(|v| v.as_str()).map(|p| p == name).unwrap_or(false)
}

fn wire_level_to_local(level: animus_log_storage_protocol::LogLevel) -> orchestrator_logging::Level {
    use animus_log_storage_protocol::LogLevel;
    match level {
        // The in-tree reader does not distinguish Trace from Debug; map
        // Trace down to Debug so a `--level=trace` request still returns
        // every Debug entry.
        LogLevel::Trace | LogLevel::Debug => orchestrator_logging::Level::Debug,
        LogLevel::Info => orchestrator_logging::Level::Info,
        LogLevel::Warn => orchestrator_logging::Level::Warn,
        LogLevel::Error => orchestrator_logging::Level::Error,
    }
}

fn local_level_to_wire(level: orchestrator_logging::Level) -> animus_log_storage_protocol::LogLevel {
    use animus_log_storage_protocol::LogLevel;
    match level {
        orchestrator_logging::Level::Debug => LogLevel::Debug,
        orchestrator_logging::Level::Info => LogLevel::Info,
        orchestrator_logging::Level::Warn => LogLevel::Warn,
        orchestrator_logging::Level::Error => LogLevel::Error,
    }
}

fn local_entry_to_wire(entry: orchestrator_logging::LogEntry) -> DaemonLogEntry {
    use animus_log_storage_protocol::LogSource;
    use chrono::DateTime;

    let ts = DateTime::parse_from_rfc3339(&entry.ts)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());

    // `provider` carries the plugin name in the in-tree schema, so a
    // provider-tagged entry classifies as Plugin source with the plugin
    // name in `source_name`.
    let (source, source_name) = match entry.provider.as_ref() {
        Some(name) => (LogSource::Plugin, Some(name.clone())),
        None => (LogSource::Daemon, None),
    };

    // Synthesize a stable id from (ts, cat, msg) so the stream consumer
    // can dedupe across overlapping historical+follow batches. UUIDs
    // would also work; the v0.4.7 stream is short-lived enough that the
    // hash is sufficient.
    let id = format!("{}|{}|{}", entry.ts, entry.cat, short_hash(&entry.msg));

    let fields = serde_json::to_value(&entry).unwrap_or(serde_json::Value::Null);

    DaemonLogEntry {
        id,
        ts,
        level: local_level_to_wire(entry.level),
        source,
        source_name,
        target: entry.cat,
        message: entry.msg,
        fields,
    }
}

fn short_hash(msg: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    msg.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Build the [`PluginSpawnOptions`] used by [`probe_plugin_health`].
///
/// Forwards the plugin manifest's declared `env_required` allowlist (plus
/// the base allowlist) into the spawned plugin process so probes see the
/// same secrets the production spawn paths see. If any `required = true`
/// vars are missing from the daemon environment the function still
/// returns options (the plugin host warns at spawn time), but we also
/// emit a `daemon_health.probe` warn so operators can correlate the
/// probe-shape degradation with the unhealthy row.
fn build_probe_spawn_options(
    plugin: &orchestrator_plugin_host::DiscoveredPlugin,
    project_root: &std::path::Path,
) -> orchestrator_plugin_host::PluginSpawnOptions {
    use orchestrator_plugin_host::PluginSpawnOptions;

    let options = PluginSpawnOptions::for_manifest(
        plugin.name.clone(),
        &plugin.manifest.env_required,
        std::iter::empty::<String>(),
        None,
    )
    .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
    .with_working_dir(project_root);
    if !options.missing_required_env.is_empty() {
        tracing::warn!(
            target: "daemon_health.probe",
            plugin = %plugin.name,
            missing_env = ?options.missing_required_env,
            "plugin health probe spawning with missing required env vars; probe will likely report unhealthy"
        );
    }
    options
}

/// Per-plugin live `health/check` probe used by `daemon_health`.
///
/// Spawns the plugin one-shot, runs the `initialize` handshake, calls
/// `health/check`, and shuts the plugin down. The whole probe is bounded
/// by [`PLUGIN_HEALTH_PROBE_TIMEOUT`]; any timeout / spawn / RPC failure
/// turns into an `Unhealthy` row with `last_error` set.
///
/// Probes are designed to be fired concurrently from `join_all`. The
/// daemon's own status stays `Healthy` because plugin-side trouble is an
/// observability concern, not a daemon-liveness one.
async fn probe_plugin_health(
    plugin: &orchestrator_plugin_host::DiscoveredPlugin,
    project_root: &std::path::Path,
) -> PluginHealth {
    use orchestrator_plugin_host::PluginHost;
    use std::time::Duration;

    const PLUGIN_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

    let name = plugin.name.clone();
    let kind = plugin.manifest.plugin_kind.clone();
    let path = plugin.path.clone();

    // v0.4.x P2 fix: apply the plugin's manifest `env_required` allowlist to
    // the probe spawn so provider plugins (and other backends) that need API
    // keys (e.g. `OPENAI_API_KEY`) actually see them. Previously the probe
    // used `PluginSpawnOptions::default()`, which scrubs env down to the base
    // allowlist and reports false-unhealthy whenever a required secret is
    // present in the daemon environment but the plugin can't see it.
    //
    // Matches the spawn shape used by subject_dispatch.rs and the trigger
    // supervisor so all live plugin spawns share one env contract.
    let options = build_probe_spawn_options(plugin, project_root);

    let outcome = tokio::time::timeout(PLUGIN_HEALTH_PROBE_TIMEOUT, async move {
        let host = PluginHost::spawn_with_options(&path, &[], options).await?;
        host.handshake().await?;
        let result = host.health_check().await?;
        // Best-effort shutdown — we still report the probe result if shutdown trips.
        let _ = host.shutdown().await;
        Ok::<_, anyhow::Error>(result)
    })
    .await;

    match outcome {
        Ok(Ok(result)) => {
            let status = match result.status {
                animus_plugin_protocol::HealthStatus::Healthy => DaemonHealthStatus::Healthy,
                animus_plugin_protocol::HealthStatus::Degraded => DaemonHealthStatus::Degraded,
                animus_plugin_protocol::HealthStatus::Unhealthy => DaemonHealthStatus::Unhealthy,
            };
            PluginHealth { name, kind, status, uptime_ms: result.uptime_ms, last_error: result.last_error }
        }
        Ok(Err(err)) => PluginHealth {
            name,
            kind,
            status: DaemonHealthStatus::Unhealthy,
            uptime_ms: None,
            last_error: Some(format!("{err}")),
        },
        Err(_) => PluginHealth {
            name,
            kind,
            status: DaemonHealthStatus::Unhealthy,
            uptime_ms: None,
            last_error: Some(format!("health/check timed out after {PLUGIN_HEALTH_PROBE_TIMEOUT:?}")),
        },
    }
}

#[cfg(test)]
mod log_dispatch_tests {
    //! v0.4.7 Item 1: prove the daemon's `daemon/logs` historical reader
    //! projects in-tree `events.jsonl` rows into the protocol's
    //! `DaemonLogEntry` shape with level + plugin filtering preserved.

    use super::*;
    use animus_log_storage_protocol::{LogLevel, LogSource};
    use orchestrator_logging::Logger;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build a unique tempdir that doubles as a synthetic `project_root`.
    /// Both `Logger::for_project` (writer) and `read_in_tree_log_entries`
    /// (reader) resolve through `protocol::scoped_state_root(project_root)`
    /// against the live `$HOME`. Different tests get distinct scope
    /// hashes (the tempdir basename is unique), so they never collide on
    /// the same events.jsonl. Pinning the crate-wide stable test HOME
    /// first guarantees the writer and reader resolve the same scope even
    /// when sibling tests trigger the one-time HOME pin mid-run.
    fn unique_project_root() -> (TempDir, PathBuf) {
        crate::test_env::stable_test_home();
        let temp = TempDir::new().expect("tempdir");
        let repo = temp.path().to_path_buf();
        (temp, repo)
    }

    /// Resolve the file path Logger::for_project would write to, and
    /// delete any prior contents so the test starts clean. The scope is
    /// derived from the unique tempdir basename, so distinct tests never
    /// share a file under `~/.animus/<scope>/logs/`.
    fn reset_scoped_events(repo: &std::path::Path) {
        let path = Logger::logs_dir(repo).join("events.jsonl");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_in_tree_log_entries_projects_into_wire_shape() {
        let (_temp, repo) = unique_project_root();
        reset_scoped_events(&repo);
        let logger = Logger::for_project(&repo);
        logger.info("test", "info-line").emit();
        logger.warn("test", "warn-line").provider("kimi-code").emit();
        logger.error("test", "error-line").emit();

        let entries = read_in_tree_log_entries(&repo, 10, Some(LogLevel::Warn), None, None);
        assert_eq!(entries.len(), 2, "expected warn+error, got {entries:?}");
        // Chronological order: warn first, then error.
        assert_eq!(entries[0].message, "warn-line");
        assert_eq!(entries[0].level, LogLevel::Warn);
        assert_eq!(entries[0].source, LogSource::Plugin);
        assert_eq!(entries[0].source_name.as_deref(), Some("kimi-code"));
        assert_eq!(entries[1].message, "error-line");
        assert_eq!(entries[1].level, LogLevel::Error);
        assert_eq!(entries[1].source, LogSource::Daemon);
    }

    #[test]
    fn read_in_tree_log_entries_plugin_filter_narrows_set() {
        let (_temp, repo) = unique_project_root();
        reset_scoped_events(&repo);
        let logger = Logger::for_project(&repo);
        logger.warn("test", "no-plugin").emit();
        logger.warn("test", "kimi").provider("kimi-code").emit();
        logger.warn("test", "gpt").provider("gpt-code").emit();

        let entries = read_in_tree_log_entries(&repo, 10, Some(LogLevel::Warn), None, Some("kimi-code"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "kimi");
    }

    #[test]
    fn read_in_tree_log_entries_synthesizes_unique_ids_per_row() {
        let (_temp, repo) = unique_project_root();
        reset_scoped_events(&repo);
        let logger = Logger::for_project(&repo);
        logger.info("test", "msg-a").emit();
        logger.info("test", "msg-b").emit();
        let entries = read_in_tree_log_entries(&repo, 10, Some(LogLevel::Info), None, None);
        assert_eq!(entries.len(), 2);
        assert_ne!(entries[0].id, entries[1].id, "ids must disambiguate rows");
    }

    fn fixture_entry(id: &str, ts_rfc3339: &str, message: &str) -> animus_control_protocol::types::DaemonLogEntry {
        use animus_log_storage_protocol::{LogLevel, LogSource};
        animus_control_protocol::types::DaemonLogEntry {
            id: id.to_string(),
            ts: chrono::DateTime::parse_from_rfc3339(ts_rfc3339).expect("parse ts").with_timezone(&chrono::Utc),
            level: LogLevel::Info,
            source: LogSource::Daemon,
            source_name: None,
            target: "test".to_string(),
            message: message.to_string(),
            fields: serde_json::Value::Null,
        }
    }

    #[test]
    fn merge_and_cap_log_entries_dedupes_by_id_and_orders_by_ts() {
        let plugin = vec![
            fixture_entry("evt-2", "2026-05-28T00:00:02Z", "plugin-2"),
            fixture_entry("evt-1", "2026-05-28T00:00:01Z", "plugin-1"),
        ];
        let file = vec![
            fixture_entry("evt-1", "2026-05-28T00:00:01Z", "file-1"),
            fixture_entry("evt-3", "2026-05-28T00:00:03Z", "file-3"),
        ];
        let merged = super::merge_and_cap_log_entries(plugin, file, 10);
        // Three unique ids in chronological order, plugin won the
        // collision so evt-1's message is "plugin-1".
        assert_eq!(merged.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec!["evt-1", "evt-2", "evt-3"]);
        assert_eq!(merged[0].message, "plugin-1", "plugin entry wins on duplicate id");
    }

    #[test]
    fn merge_and_cap_log_entries_trims_oldest_first_to_limit() {
        let plugin = vec![
            fixture_entry("a", "2026-05-28T00:00:01Z", "1"),
            fixture_entry("b", "2026-05-28T00:00:02Z", "2"),
            fixture_entry("c", "2026-05-28T00:00:03Z", "3"),
        ];
        let file: Vec<animus_control_protocol::types::DaemonLogEntry> = Vec::new();
        let merged = super::merge_and_cap_log_entries(plugin, file, 2);
        assert_eq!(merged.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec!["b", "c"]);
    }
}

#[cfg(test)]
mod control_daemon_logs_plugin_route_tests {
    //! Audit P2: when a `log_storage_backend` plugin is installed (and
    //! its handle is in the process-global slot), the `daemon_logs`
    //! control endpoint must call the plugin's `log_storage/query`
    //! method instead of reading the in-tree `events.jsonl` file.

    use super::*;
    use crate::log_storage::{
        clear_log_storage_handle, install_log_storage_handle, LogStorageHandle, LOG_STORAGE_TEST_SLOT_LOCK,
    };
    use animus_control_protocol::types::DaemonLogsRequest;
    use animus_control_protocol::ControlSurface;
    use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use orchestrator_plugin_host::PluginHost;
    use std::sync::Arc;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct SlotGuard;
    impl Drop for SlotGuard {
        fn drop(&mut self) {
            clear_log_storage_handle();
        }
    }

    /// RAII guard restoring `ANIMUS_CONFIG_DIR` after the test. Wraps the
    /// protocol crate's `EnvVarGuard` so the mutation holds the process-wide
    /// env lock shared with every other env-mutating test in this binary.
    struct ConfigDirGuard {
        _inner: protocol::test_utils::EnvVarGuard,
    }
    impl ConfigDirGuard {
        fn set(value: &std::path::Path) -> Self {
            Self { _inner: protocol::test_utils::EnvVarGuard::set_os("ANIMUS_CONFIG_DIR", Some(value.as_os_str())) }
        }
    }

    async fn fake_tail_host(
        name: &str,
        canned: serde_json::Value,
        recorded_methods: Arc<tokio::sync::Mutex<Vec<String>>>,
    ) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let recorded = recorded_methods.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if value.get("id").is_none() || value.get("id") == Some(&serde_json::Value::Null) {
                    continue;
                }
                let request: RpcRequest = match serde_json::from_value(value) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                recorded.lock().await.push(request.method.clone());
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: animus_plugin_protocol::PLUGIN_KIND_LOG_STORAGE_BACKEND.to_string(),
                                description: None,
                            },
                            capabilities: PluginCapabilities::default(),
                        }),
                    ),
                    "log_storage/query" => RpcResponse::ok(request.id, canned.clone()),
                    _ => RpcResponse::ok(request.id, serde_json::json!({})),
                };
                let mut encoded = serde_json::to_string(&response).unwrap();
                encoded.push('\n');
                if plugin_writer.write_all(encoded.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn daemon_logs_routes_through_plugin_query_when_installed() {
        use futures_util::StreamExt;
        let _slot = LOG_STORAGE_TEST_SLOT_LOCK.lock().await;
        let _guard = SlotGuard;
        // Isolate daemon-events.jsonl so we don't pick up records
        // written by other tests in the same process.
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = ConfigDirGuard::set(temp.path());

        let canned = serde_json::json!({
            "entries": [
                {
                    "id": "evt-1",
                    "ts": "2026-05-28T00:00:00Z",
                    "level": "warn",
                    "source": "plugin",
                    "source_name": "test-log-sink",
                    "target": "from-plugin",
                    "message": "wired-up",
                }
            ]
        });
        let recorded: Arc<tokio::sync::Mutex<Vec<String>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let host = fake_tail_host("test-log-sink", canned, recorded.clone()).await;
        host.handshake().await.expect("handshake");
        let handle = Arc::new(LogStorageHandle::from_handshaked_host(
            "test-log-sink",
            host,
            PathBuf::from("/tmp/fake-project-route"),
        ));
        install_log_storage_handle(handle.clone());

        let surface = InProcessSurface::builder(PathBuf::from("/tmp/fake-project-route")).build();
        let request = DaemonLogsRequest::default();
        let mut stream = surface.daemon_logs(request).await.expect("daemon_logs ok");

        let mut collected: Vec<animus_control_protocol::types::DaemonLogEntry> = Vec::new();
        while let Some(item) = stream.next().await {
            collected.push(item.expect("entry decode"));
        }
        assert_eq!(collected.len(), 1, "expected one plugin-supplied entry, got {:?}", collected);
        assert_eq!(collected[0].message, "wired-up");
        assert_eq!(collected[0].source_name.as_deref(), Some("test-log-sink"));

        let methods = recorded.lock().await;
        assert!(
            methods.iter().any(|m| m == "log_storage/query"),
            "plugin must receive log_storage/query request, saw: {:?}",
            *methods
        );
        drop(methods);
        handle.shutdown().await;
    }
}

#[cfg(test)]
mod health_probe_env_tests {
    //! v0.4.x P2: prove that `daemon_health` plugin probes spawn with the
    //! manifest's declared `env_required` allowlist applied, so provider
    //! plugins that need `OPENAI_API_KEY` (etc.) actually see the var
    //! during the probe instead of false-reporting unhealthy.
    use super::build_probe_spawn_options;
    use animus_plugin_protocol::{EnvRequirement, PluginManifest};
    use orchestrator_plugin_host::{DiscoveredPlugin, DiscoverySource};
    use std::path::{Path, PathBuf};

    fn manifest_with_env(required: Vec<EnvRequirement>) -> PluginManifest {
        PluginManifest {
            name: "fake-provider".to_string(),
            version: "0.0.0".to_string(),
            plugin_kind: "provider".to_string(),
            description: "test fixture".to_string(),
            protocol_version: "0.1".to_string(),
            capabilities: Vec::new(),
            env_required: required,
            notification_buffer_size: None,
        }
    }

    fn discovered(manifest: PluginManifest) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: manifest.name.clone(),
            path: PathBuf::from("/tmp/fake-probe-plugin"),
            manifest,
            source: DiscoverySource::SystemPath,
        }
    }

    fn project_root() -> &'static Path {
        Path::new("/tmp/fake-project-root")
    }

    /// Probe spawn options must carry the manifest's declared env names into
    /// the allowlist so the plugin process can read its required secret.
    #[test]
    fn probe_options_forward_declared_env_when_set() {
        // Scope the env var to the test; the previous probe behavior used
        // `default()` and would have produced an empty allowlist.
        let _present = protocol::test_utils::EnvVarGuard::set("FAKE_PROBE_VAR_PRESENT", Some("expected-value"));

        let plugin = discovered(manifest_with_env(vec![EnvRequirement {
            name: "FAKE_PROBE_VAR_PRESENT".to_string(),
            description: None,
            sensitive: false,
            required: true,
        }]));

        let opts = build_probe_spawn_options(&plugin, project_root());

        assert!(
            opts.env_allowlist.iter().any(|n| n == "FAKE_PROBE_VAR_PRESENT"),
            "manifest env_required must propagate into the probe allowlist; got {:?}",
            opts.env_allowlist
        );
        assert!(
            opts.missing_required_env.is_empty(),
            "env var is present, so missing_required_env should be empty; got {:?}",
            opts.missing_required_env
        );
    }

    /// When the var is unset, the probe still forwards the name (so the
    /// allowlist is correct), but it surfaces the missing-required-env
    /// list so the operator-visible warn fires and the plugin's own
    /// "missing key" error path can run instead of a generic unhealthy.
    #[test]
    fn probe_options_report_missing_required_env_when_unset() {
        let _absent = protocol::test_utils::EnvVarGuard::set("FAKE_PROBE_VAR_ABSENT", None);

        let plugin = discovered(manifest_with_env(vec![EnvRequirement {
            name: "FAKE_PROBE_VAR_ABSENT".to_string(),
            description: None,
            sensitive: true,
            required: true,
        }]));

        let opts = build_probe_spawn_options(&plugin, project_root());

        assert!(
            opts.env_allowlist.iter().any(|n| n == "FAKE_PROBE_VAR_ABSENT"),
            "the allowlist still includes the declared name even when unset"
        );
        assert_eq!(
            opts.missing_required_env,
            vec!["FAKE_PROBE_VAR_ABSENT".to_string()],
            "required=true with var unset must populate missing_required_env"
        );
    }

    /// A manifest with no declared env still produces valid options — this
    /// is the no-secret subject_backend / web_ui case; the probe must not
    /// regress to behaving worse than the previous `default()` path for
    /// plugins that legitimately need no env.
    #[test]
    fn probe_options_handle_empty_env_required() {
        let plugin = discovered(manifest_with_env(Vec::new()));
        let opts = build_probe_spawn_options(&plugin, project_root());

        assert!(
            opts.env_allowlist.is_empty(),
            "no declared env => empty allowlist (base allowlist is added by the host)"
        );
        assert!(opts.missing_required_env.is_empty());
        assert_eq!(opts.plugin_label.as_deref(), Some("fake-provider"));
        assert_eq!(opts.working_dir.as_deref(), Some(project_root()));
    }
}
