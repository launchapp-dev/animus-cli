//! Generic sink for workflow lifecycle events surfaced by
//! [`crate::workflow_execute::execute_workflow_with_hub`].
//!
//! Two delivery paths coexist:
//!
//! 1. **JSON-RPC return value.** When the plugin runs via stdio JSON-RPC the
//!    daemon collects `phase_events` directly from the `workflow/execute`
//!    response (see [`crate::phase_event_recorder`]). No side channel.
//! 2. **Subprocess back-channels.** When the plugin binary runs in
//!    direct-execute mode (`animus-workflow-runner-default execute ...`,
//!    spawned by the daemon scheduler), it streams events as they happen
//!    via [`SubprocessPipeEmitter`] (legacy daemon-binds path) and
//!    [`ReattachListenerEmitter`] (runner-binds reattach path). The runner
//!    uses [`FanoutEmitter`] to drive both from one emit call.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;

/// Kind discriminator for a [`RuntimeWorkflowEvent`].
///
/// Mirrors the `kind` string values the protocol layer emits on the wire
/// (`workflow_events`). Kept as an enum here so the runner cannot
/// mis-spell a kind; the emitter implementation maps to the wire string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeWorkflowEventKind {
    PhaseStarted,
    PhaseCompleted,
    PhaseFailed,
    WorkflowCompleted,
    WorkflowFailed,
}

impl RuntimeWorkflowEventKind {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::PhaseStarted => "phase_started",
            Self::PhaseCompleted => "phase_completed",
            Self::PhaseFailed => "phase_failed",
            Self::WorkflowCompleted => "workflow_completed",
            Self::WorkflowFailed => "workflow_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeWorkflowEvent {
    pub workflow_id: String,
    pub kind: RuntimeWorkflowEventKind,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
}

/// Wire form sent across the subprocess back-channel pipe and the reattach
/// listener. The runtime [`RuntimeWorkflowEventKind`] enum is serialized as
/// its protocol wire string so the daemon-side reader can deserialize
/// without a shared Rust dependency on the enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireWorkflowEvent {
    pub workflow_id: String,
    pub kind: String,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
}

impl From<&RuntimeWorkflowEvent> for WireWorkflowEvent {
    fn from(event: &RuntimeWorkflowEvent) -> Self {
        Self {
            workflow_id: event.workflow_id.clone(),
            kind: event.kind.as_wire_str().to_string(),
            payload: event.payload.clone(),
            occurred_at: event.occurred_at,
        }
    }
}

pub trait WorkflowEventEmitter: Send + Sync {
    fn emit(&self, event: RuntimeWorkflowEvent);
}

pub type SharedWorkflowEventEmitter = Arc<dyn WorkflowEventEmitter>;

/// Env var the daemon sets on workflow-runner spawn pointing at a
/// pre-bound Unix-domain socket. When present the runner constructs a
/// [`SubprocessPipeEmitter`] that streams [`WireWorkflowEvent`] lines back
/// to the daemon.
pub const ANIMUS_WORKFLOW_EVENT_PIPE_ENV: &str = "ANIMUS_WORKFLOW_EVENT_PIPE";

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopWorkflowEventEmitter;

impl WorkflowEventEmitter for NoopWorkflowEventEmitter {
    fn emit(&self, _event: RuntimeWorkflowEvent) {}
}

/// Subprocess-side emitter that serializes each event as a single JSON line
/// to a Unix domain socket the daemon prebinds. Used by the workflow
/// runner binary when [`ANIMUS_WORKFLOW_EVENT_PIPE_ENV`] is set.
///
/// The connection is established lazily on the first `emit` call and held
/// open for the lifetime of the emitter. If the daemon closes its end mid
/// stream we silently swallow subsequent write errors — losing a phase
/// boundary event is strictly preferable to crashing the runner.
///
/// Windows: not implemented; [`SubprocessPipeEmitter::new`] returns `None`
/// and callers fall back to [`NoopWorkflowEventEmitter`].
pub struct SubprocessPipeEmitter {
    #[cfg(unix)]
    inner: Mutex<Option<std::os::unix::net::UnixStream>>,
    #[cfg(unix)]
    socket_path: std::path::PathBuf,
}

impl SubprocessPipeEmitter {
    /// Construct from an explicit socket path. Returns `None` on platforms
    /// where the back-channel is not implemented (currently: non-Unix).
    #[cfg(unix)]
    pub fn new(socket_path: impl Into<std::path::PathBuf>) -> Option<Arc<Self>> {
        Some(Arc::new(Self { inner: Mutex::new(None), socket_path: socket_path.into() }))
    }

    #[cfg(not(unix))]
    pub fn new(_socket_path: impl Into<std::path::PathBuf>) -> Option<Arc<Self>> {
        None
    }

    /// Construct from the [`ANIMUS_WORKFLOW_EVENT_PIPE_ENV`] env var. Returns
    /// `None` if the env var is unset, empty, or the platform does not
    /// support the back-channel.
    pub fn from_env() -> Option<Arc<Self>> {
        let path = std::env::var(ANIMUS_WORKFLOW_EVENT_PIPE_ENV).ok()?;
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return None;
        }
        Self::new(trimmed)
    }
}

#[cfg(unix)]
impl WorkflowEventEmitter for SubprocessPipeEmitter {
    fn emit(&self, event: RuntimeWorkflowEvent) {
        use std::io::Write;
        let wire = WireWorkflowEvent::from(&event);
        let mut line = match serde_json::to_string(&wire) {
            Ok(s) => s,
            Err(_) => return,
        };
        line.push('\n');

        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if guard.is_none() {
            match std::os::unix::net::UnixStream::connect(&self.socket_path) {
                Ok(stream) => {
                    *guard = Some(stream);
                }
                Err(_) => return,
            }
        }
        if let Some(stream) = guard.as_mut() {
            if stream.write_all(line.as_bytes()).is_err() {
                *guard = None;
            }
        }
    }
}

#[cfg(not(unix))]
impl WorkflowEventEmitter for SubprocessPipeEmitter {
    fn emit(&self, _event: RuntimeWorkflowEvent) {}
}

/// Fan-out emitter that forwards every event to multiple underlying
/// emitters. Used by the workflow runner binary to drive both the legacy
/// daemon-bound [`SubprocessPipeEmitter`] and the v0.5.1 reattach listener
/// from a single phase-execution emit call.
pub struct FanoutEmitter {
    sinks: Vec<SharedWorkflowEventEmitter>,
}

impl FanoutEmitter {
    pub fn new(sinks: Vec<SharedWorkflowEventEmitter>) -> Arc<Self> {
        Arc::new(Self { sinks })
    }
}

impl WorkflowEventEmitter for FanoutEmitter {
    fn emit(&self, event: RuntimeWorkflowEvent) {
        for sink in &self.sinks {
            sink.emit(event.clone());
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct RecordingEmitter {
        events: Mutex<Vec<RuntimeWorkflowEvent>>,
    }

    impl RecordingEmitter {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        pub fn snapshot(&self) -> Vec<RuntimeWorkflowEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl WorkflowEventEmitter for RecordingEmitter {
        fn emit(&self, event: RuntimeWorkflowEvent) {
            self.events.lock().unwrap().push(event);
        }
    }
}
