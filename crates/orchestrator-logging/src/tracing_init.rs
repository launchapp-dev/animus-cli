use std::io;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Initialize tracing subscriber with unified configuration.
///
/// # Configuration
/// - `AO_LOG_JSON=1` enables JSON output format for machine-parseable logging
/// - `RUST_LOG` env var controls log levels (defaults to provided default_filter)
///
/// # Log Level Discipline
/// - ERROR: System broken, immediate attention required
/// - WARN:  System degraded, operation continuing with limitations
/// - INFO:  Lifecycle events (startup, shutdown, major state transitions)
/// - DEBUG: Decision points, diagnostic context for troubleshooting
/// - TRACE: Loop noise, high-frequency events (tick iterations, polls)
pub fn init_tracing(default_filter: &str) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_filter));

    let json_format = std::env::var("AO_LOG_JSON")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if json_format {
        let json_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(io::stderr)
            .with_target(true)
            .with_thread_ids(false)
            .with_current_span(true)
            .flatten_event(true)
            .with_filter(env_filter);

        let _ = tracing_subscriber::registry().with(json_layer).try_init();
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_target(false)
            .with_ansi(true)
            .compact()
            .with_filter(env_filter);

        let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
    }
}

/// Initialize tracing with a custom filter that includes crate-specific defaults.
///
/// This is a convenience wrapper for daemon components that want consistent
/// logging across the orchestrator ecosystem.
pub fn init_daemon_tracing() {
    init_tracing("info,orchestrator_daemon_runtime=debug,orchestrator_core=debug")
}

/// Initialize tracing for workflow runner components.
pub fn init_workflow_tracing() {
    init_tracing("info,workflow_runner_v2=debug,orchestrator_providers=debug")
}

/// Initialize tracing for agent runner components.
pub fn init_agent_tracing() {
    init_tracing("info,agent_runner=debug")
}
