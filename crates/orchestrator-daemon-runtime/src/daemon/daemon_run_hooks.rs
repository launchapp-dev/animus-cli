use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{DaemonStatus, PluginInstaller, PluginPreflightSpec};

use crate::control::{AgentRouting, DaemonOpsRouting, PluginRouting, QueueRouting, WorkflowRouting};
use crate::DaemonRunEvent;

#[async_trait::async_trait(?Send)]
pub trait DaemonRunHooks {
    fn handle_event(&mut self, event: DaemonRunEvent) -> Result<()>;

    async fn daemon_status(&mut self, project_root: &str) -> Result<DaemonStatus> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide daemon_status")
    }

    async fn start_daemon(&mut self, project_root: &str) -> Result<()> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide start_daemon")
    }

    async fn stop_daemon(&mut self, project_root: &str) -> Result<()> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide stop_daemon")
    }

    async fn recover_startup_orphans(&mut self, project_root: &str) -> Result<usize> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide recover_startup_orphans")
    }

    async fn flush_notifications(&mut self, _project_root: &str) -> Result<()> {
        Ok(())
    }

    /// Earliest future deferred-entry `run_at` reported by the installed
    /// queue plugin (`queue/next_deadline`), used by the wake loop to sleep
    /// precisely until a deferred entry is due instead of waiting for the
    /// heartbeat. Default `None` (no queue plugin / no future-dated entries);
    /// the CLI binary overrides this to query the queue plugin. Errors are
    /// the impl's concern — it should swallow them to `None` so a transient
    /// plugin hiccup degrades to heartbeat pacing rather than killing the loop.
    async fn queue_next_deadline(&mut self, _project_root: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        None
    }

    /// Called from every daemon exit path (`--once`, `GracefulShutdown`,
    /// SIGINT, SIGTERM) AFTER the loop breaks but BEFORE the runtime
    /// drops. Default is no-op. The CLI binary's notifier dispatcher
    /// overrides this to await every in-flight `notifier/notify` task so
    /// the final status events reach the plugin before the daemon exits.
    /// Closes codex v0.5.3 Task D round-5 P2.
    async fn shutdown_drain_notifications(&mut self, _project_root: &str) -> Result<()> {
        Ok(())
    }

    /// Provide a `plugin/*` routing handle for the control-RPC surface.
    /// Defaults to `None`, which leaves the daemon's `InProcessSurface`
    /// returning `NotSupported` for plugin/* methods. The CLI binary
    /// builds an implementation that delegates back to its in-tree
    /// `run_plugin_*` helpers.
    fn plugin_routing(&self) -> Option<Arc<dyn PluginRouting>> {
        None
    }

    /// Provide a `daemon/*` ops routing handle for the control-RPC
    /// surface. Defaults to `None`, which leaves the daemon's
    /// `InProcessSurface` returning its stub responses.
    fn daemon_ops_routing(&self) -> Option<Arc<dyn DaemonOpsRouting>> {
        None
    }

    /// Provide a `workflow/*` routing handle for the control-RPC
    /// surface. Defaults to `None`, which leaves the daemon's
    /// `InProcessSurface` returning `NotSupported` for workflow/*
    /// methods. The CLI binary builds an implementation that delegates
    /// back to its in-tree `WorkflowServiceApi` helpers.
    fn workflow_routing(&self) -> Option<Arc<dyn WorkflowRouting>> {
        None
    }

    /// Provide a `queue/*` routing handle for the control-RPC surface.
    /// Defaults to `None`, which leaves the daemon's
    /// `InProcessSurface` returning `NotSupported` for queue/* methods.
    /// The CLI binary builds an implementation that delegates to the
    /// installed `queue` plugin via `animus-queue-protocol` RPCs.
    fn queue_routing(&self) -> Option<Arc<dyn QueueRouting>> {
        None
    }

    /// Provide an `agent/*` routing handle for the control-RPC surface.
    /// Defaults to `None`, which leaves the daemon's
    /// `InProcessSurface` returning `NotSupported` for agent/* methods.
    /// C6.7 wires a pass-through implementation from the CLI binary
    /// whose runtime impl still degrades to `NotSupported` (AgentPool
    /// is currently dead-coded); the wire surface exists so MCP (C7)
    /// and WebAPI (C8) can mount a real implementation without changing
    /// the control contract.
    fn agent_routing(&self) -> Option<Arc<dyn AgentRouting>> {
        None
    }

    fn plugin_preflight_spec(&self) -> PluginPreflightSpec {
        PluginPreflightSpec::daemon_default()
    }

    fn plugin_installer(&self) -> Option<Arc<dyn PluginInstaller>> {
        None
    }
}
