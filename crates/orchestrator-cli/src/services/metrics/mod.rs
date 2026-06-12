//! Opt-in anonymous usage metrics.
//!
//! Telemetry surface for Animus. Disabled by default; enabled only after the
//! user explicitly opts in via the first-run prompt (or `animus daemon
//! metrics enable`). Emits counter-only events with bounded tag enums — no free-form
//! strings, no file paths, no repo/branch identifiers, no prompts, no
//! credentials reach the payload. The whole feature is short-circuited by
//! `ANIMUS_METRICS_DISABLE=1`.
//!
//! Public API:
//!   - [`Event`], [`EventName`], and the tag enums describe the closed event
//!     surface.
//!   - [`MetricsRecorder`] accumulates events into scoped-state JSONL.
//!   - [`flush_pending`] drains and POSTs the queue with bounded retry.
//!   - [`maybe_prompt_first_run`] handles the consent prompt.

pub(crate) mod events;
pub(crate) mod prompt;
pub(crate) mod recorder;
pub(crate) mod sender;

#[cfg(test)]
mod tests;

pub(crate) use events::{CommandGroup, EventTags, PluginRole};
pub(crate) use prompt::maybe_prompt_first_run;
pub(crate) use recorder::record_event;
pub(crate) use sender::{flush_pending, maybe_flush_if_due, FlushOutcome};

use std::path::{Path, PathBuf};

/// Returns the scoped-state directory that holds pending event batches.
pub(crate) fn metrics_state_dir(project_root: &Path) -> Option<PathBuf> {
    protocol::repository_scope::scoped_state_root(project_root).map(|root| root.join("metrics"))
}

/// Returns the host OS name used in event payloads. Closed set: linux /
/// darwin / windows / unknown.
pub(crate) fn host_os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        _ => "unknown",
    }
}

/// Returns the host architecture used in event payloads. Closed set:
/// x86_64 / aarch64 / unknown.
pub(crate) fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => "unknown",
    }
}
