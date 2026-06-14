//! Routing traits that the daemon's [`super::InProcessSurface`] uses to
//! delegate plugin/* and daemon/* operations back to in-tree CLI logic.
//!
//! Architectural background
//! ------------------------
//!
//! The daemon and the CLI live in the same Rust binary today (`animus`).
//! The CLI's `ops_plugin::run_plugin_*` and `runtime_daemon::*` helpers
//! already implement every operation the control wire needs. C6 wires
//! the daemon's [`super::InProcessSurface`] to *call those same helpers*
//! when a control request arrives over the socket — rather than
//! duplicating the logic.
//!
//! To keep the daemon-runtime crate free of a circular dependency on the
//! orchestrator-cli crate, the wiring goes through these `Arc<dyn Trait>`
//! handles. The CLI binary builds one of each at daemon startup, hands
//! them to the [`super::InProcessSurfaceBuilder`], and the surface
//! delegates blindly. Tests can substitute mock implementations for both
//! routing-via-wire and routing-via-local paths.
//!
//! Anti-deadlock rules
//! -------------------
//!
//! - All methods are `&self` + `Send`. Implementors must not hold a lock
//!   across `.await`; the trait surface awaits each method exactly once.
//! - Handles are `Arc`-cloned at injection, never wrapped in
//!   `tokio::sync::Mutex` by the surface. Implementations are responsible
//!   for any internal synchronization they need.

use async_trait::async_trait;
use serde_json::Value;

use animus_control_protocol::{
    types::{
        DaemonAgentsResponse, DaemonHealthResponse, DaemonStatusResponse, PluginBrowseRequest, PluginCallRequest,
        PluginCallResponse, PluginInfo, PluginInfoRequest, PluginInstallRequest, PluginInstallResponse,
        PluginListRequest, PluginListResponse, PluginPingRequest, PluginPingResponse, PluginSearchRequest,
        PluginSearchResponse, PluginUninstallRequest, PluginUpdateRequest, PluginUpdateResponse, QueueDropRequest,
        QueueEnqueueRequest, QueueEntry, QueueHoldRequest, QueueListRequest, QueueListResponse, QueueReleaseRequest,
        QueueReorderRequest, QueueStats, Unit, WorkflowCancelRequest, WorkflowExecuteRequest, WorkflowGetRequest,
        WorkflowListRequest, WorkflowListResponse, WorkflowPauseRequest, WorkflowResumeRequest, WorkflowRun,
        WorkflowRunRequest, WorkflowRunStart,
    },
    ControlError,
};

/// Plugin/* dispatcher used by [`super::InProcessSurface`].
///
/// Wraps the CLI's existing `run_plugin_*` helpers behind a transport-
/// agnostic interface. The daemon owns one Arc'd instance for its lifetime;
/// the CLI binary constructs it at startup and hands it in via
/// [`super::InProcessSurfaceBuilder::plugin_routing`].
#[async_trait]
pub trait PluginRouting: Send + Sync {
    /// `plugin/list` — enumerate installed plugins.
    async fn plugin_list(&self, request: PluginListRequest) -> Result<PluginListResponse, ControlError>;

    /// `plugin/info` — describe one installed plugin.
    async fn plugin_info(&self, request: PluginInfoRequest) -> Result<PluginInfo, ControlError>;

    /// `plugin/install` — fetch + install a plugin.
    async fn plugin_install(&self, request: PluginInstallRequest) -> Result<PluginInstallResponse, ControlError>;

    /// `plugin/uninstall` — remove an installed plugin.
    async fn plugin_uninstall(&self, request: PluginUninstallRequest) -> Result<Unit, ControlError>;

    /// `plugin/ping` — lifecycle-ping into a plugin.
    async fn plugin_ping(&self, request: PluginPingRequest) -> Result<PluginPingResponse, ControlError>;

    /// `plugin/call` — opaque pass-through to a plugin domain method.
    async fn plugin_call(&self, request: PluginCallRequest) -> Result<PluginCallResponse, ControlError>;

    /// `plugin/search` — registry free-text search.
    async fn plugin_search(&self, request: PluginSearchRequest) -> Result<PluginSearchResponse, ControlError>;

    /// `plugin/browse` — registry listing by kind / install state.
    async fn plugin_browse(&self, request: PluginBrowseRequest) -> Result<PluginSearchResponse, ControlError>;

    /// `plugin/update` — check / apply plugin upgrades.
    async fn plugin_update(&self, request: PluginUpdateRequest) -> Result<PluginUpdateResponse, ControlError>;
}

/// `daemon/*` dispatcher used by [`super::InProcessSurface`] for the
/// observability surface (`status`, `health`, `agents`).
///
/// `daemon/start`, `daemon/stop`, and `daemon/restart` remain CLI-local —
/// the daemon controlling itself over its own socket is the wrong model.
#[async_trait]
pub trait DaemonOpsRouting: Send + Sync {
    /// `daemon/status` — live process status snapshot.
    async fn daemon_status(&self) -> Result<DaemonStatusResponse, ControlError>;

    /// `daemon/health` — health snapshot incl. per-plugin health.
    async fn daemon_health(&self) -> Result<DaemonHealthResponse, ControlError>;

    /// `daemon/agents` — currently active agent sessions.
    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError>;
}

/// `workflow/*` dispatcher used by [`super::InProcessSurface`].
///
/// Wraps the CLI's existing `WorkflowServiceApi` + `dispatch_workflow_event`
/// helpers behind a transport-agnostic interface, mirroring the pattern
/// established for [`PluginRouting`]. Implementations live in
/// `orchestrator-cli` so the daemon-runtime crate doesn't grow a
/// dependency on `orchestrator-core`'s service hub.
///
/// The detail payload on `workflow/get` is opaque `Value` — the
/// daemon-side `OrchestratorWorkflow` schema is rich (phases, decisions,
/// machine-state) and mirroring it exhaustively into the protocol crate
/// is deferred to a v0.4.x cleanup task.
#[async_trait]
pub trait WorkflowRouting: Send + Sync {
    /// `workflow/list` — page through workflow runs filtered by status.
    async fn workflow_list(&self, request: WorkflowListRequest) -> Result<WorkflowListResponse, ControlError>;

    /// `workflow/get` — fetch one workflow run, opaque detail included.
    async fn workflow_get(&self, request: WorkflowGetRequest) -> Result<WorkflowRun, ControlError>;

    /// `workflow/run` — start a workflow for a task subject.
    async fn workflow_run(&self, request: WorkflowRunRequest) -> Result<WorkflowRunStart, ControlError>;

    /// `workflow/execute` — start a workflow by definition name with params.
    async fn workflow_execute(&self, request: WorkflowExecuteRequest) -> Result<WorkflowRunStart, ControlError>;

    /// `workflow/pause` — pause a running workflow.
    async fn workflow_pause(&self, request: WorkflowPauseRequest) -> Result<Unit, ControlError>;

    /// `workflow/resume` — resume a paused workflow.
    async fn workflow_resume(&self, request: WorkflowResumeRequest) -> Result<Unit, ControlError>;

    /// `workflow/cancel` — cancel a workflow with an optional reason.
    async fn workflow_cancel(&self, request: WorkflowCancelRequest) -> Result<Unit, ControlError>;
}

/// `queue/*` dispatcher used by [`super::InProcessSurface`].
///
/// Forwards each control-protocol queue verb to the installed `queue`
/// plugin via `animus-queue-protocol` RPCs, mirroring the
/// [`PluginRouting`] and [`WorkflowRouting`] pattern. Implementations
/// live in `orchestrator-cli` so the daemon-runtime crate doesn't grow
/// a dependency on `orchestrator-core`'s service hub.
///
/// The wire-side [`QueueEntry`] schema is intentionally leaner than the
/// queue plugin's `animus_queue_protocol::QueueEntry` — wire callers
/// get a stable id + subject_id + status + priority + enqueued_at, with
/// the rich dispatch payload surfacing only on the CLI's local path.
/// Adapters map the plugin's entry shape into the wire shape best-effort.
#[async_trait]
pub trait QueueRouting: Send + Sync {
    /// `queue/list` — page through queue entries filtered by status.
    async fn queue_list(&self, request: QueueListRequest) -> Result<QueueListResponse, ControlError>;

    /// `queue/enqueue` — push a new entry onto the dispatch queue.
    async fn queue_enqueue(&self, request: QueueEnqueueRequest) -> Result<QueueEntry, ControlError>;

    /// `queue/drop` — remove an entry from the queue.
    async fn queue_drop(&self, request: QueueDropRequest) -> Result<Unit, ControlError>;

    /// `queue/hold` — pause an entry so the dispatcher won't pick it up.
    async fn queue_hold(&self, request: QueueHoldRequest) -> Result<Unit, ControlError>;

    /// `queue/release` — clear a held entry so it becomes dispatchable.
    async fn queue_release(&self, request: QueueReleaseRequest) -> Result<Unit, ControlError>;

    /// `queue/reorder` — change the relative order of queue entries.
    async fn queue_reorder(&self, request: QueueReorderRequest) -> Result<Unit, ControlError>;

    /// `queue/stats` — per-status counts and recent throughput.
    async fn queue_stats(&self) -> Result<QueueStats, ControlError>;
}

/// Marker used by integration tests that need to assert "the surface
/// received this exact JSON params payload" without spinning up a full
/// transport stack.
///
/// Not used in production code paths.
#[derive(Debug, Clone, Default)]
pub struct LastCallSpy {
    /// JSON-RPC method that was last invoked, if any.
    pub method: Option<String>,
    /// Params payload from the last invocation.
    pub params: Option<Value>,
}
