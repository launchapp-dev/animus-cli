//! Host-side glue that wires the upstream `SessionBackend` trait (re-exported from
//! `animus_session_backend::session`) to the Animus STDIO plugin protocol.
//!
//! This module exists to keep the published `animus-session-backend` crate
//! small. The pieces here — `PluginSessionBackend` (per-session map + cancel
//! routing + notification forwarding) and the plugin-discovery-aware
//! `SessionBackendResolver` — are daemon-side adapters that depend on the
//! `orchestrator-plugin-host` transport. They have no business shipping in
//! the published session-backend surface that provider plugins themselves
//! consume.
//!
//! As of v0.4.12 there are no in-tree provider backends — every provider
//! (Claude, Codex, Gemini, OpenCode, OAI-runner, plus any third-party) ships
//! as a standalone `launchapp-dev/animus-provider-*` STDIO plugin. The
//! resolver here discovers and dispatches to those plugins only; a missing
//! plugin is a hard error, not a silent fallback.

pub mod error;
pub mod plugin_backend;
pub mod plugin_supervisor;
pub mod session_backend_resolver;

pub use error::{Error, Result};
pub use plugin_backend::{
    discover_provider_plugins, DiscoveredProviderPlugin, PluginSessionBackend, ResumeAgentOutcome,
};
pub use plugin_supervisor::{
    classify, is_structured_jsonrpc_error, DispatchObserver, NoopDispatchObserver, PluginSupervisor, RetryDecision,
    SupervisorConfig, SupervisorError,
};
pub use session_backend_resolver::{
    canonical_tool_alias, is_reserved_provider_tool, provider_install_command, SessionBackendResolver,
    RESERVED_PROVIDER_TOOLS,
};
