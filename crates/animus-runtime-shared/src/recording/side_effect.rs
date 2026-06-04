//! Out-of-boundary tool side-effect re-assertion on replay.
//!
//! ## Architectural decision (Option B)
//!
//! Three options were on the table for handling tools that have side
//! effects outside the recording boundary (filesystem writes, network
//! calls, shell commands):
//!
//! - **Option A**: at replay time, wrap and re-execute side-effecting tools.
//!   Defeats determinism — replay becomes a second real run with all the
//!   bills (tokens, side-effects-on-prod) that implies.
//! - **Option B** (chosen): replay returns the recorded result without
//!   re-executing. Where the producer recorded a structured assertion
//!   describing the side effect, replay checks that the assertion holds.
//!   On mismatch, raise [`ReplayDivergenceError`] and surface to the
//!   operator. Determinism preserved; divergence is caught instead of
//!   silently smearing.
//! - **Option C**: classify tools at recording time and refuse to replay
//!   any session that touched an out-of-boundary tool. Honest, but adds
//!   a classification surface AND blocks the common useful case of
//!   "the file the agent wrote is still there, so the replay is sound."
//!
//! Option B keeps the smallest production surface: no new replay path,
//! no new tool classifier, just an assertion check on entries the
//! producer already opted in to. Net new public API: one decision-event
//! variant + one assertion enum + one check function.
//!
//! ## Producer contract
//!
//! When a tool that mutates state outside the recording boundary
//! completes, the producer (agent-runner provider integration) records
//! a [`super::DecisionEvent::ToolSideEffect`] with a structured
//! [`SideEffectAssertion`] alongside the existing `ToolResult`. The
//! assertion captures the observable post-condition (file exists, file
//! absent, etc.). Producers that don't yet know how to classify a tool
//! omit the event entirely; replay simply doesn't get an assertion to
//! check and behaves as before.
//!
//! ## Consumer contract (replay)
//!
//! [`assert_side_effect`] returns `Ok(())` when the recorded
//! post-condition still holds at replay time. A failure means the
//! filesystem (or other side-effect surface) is in a different state
//! than when the original run wrote the recording — typically because
//! the agent was about to read the file it just wrote, and that file
//! is now missing. `drive_replay` surfaces the error as a terminal
//! Error event so the operator sees the divergence rather than getting
//! garbage replay output.
//!
//! ## Path scoping
//!
//! Assertions use absolute paths. The producer is responsible for
//! resolving relative paths against the working directory at record
//! time. Replay does not attempt cwd-relative resolution because the
//! cwd at replay can differ from the cwd at record (operator running
//! `animus replay --session ...` from anywhere).
//!
//! ## v0.6+ deferral
//!
//! Side-effect kinds beyond filesystem presence checks (process exit
//! code, network endpoint reachability, env-var equality, etc.) extend
//! [`SideEffectAssertion`] as new variants. No replay-side breaking
//! change is required because the decoder ignores unknown variants via
//! `serde(other)`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, thiserror::Error)]
pub enum ReplayDivergenceError {
    #[error("replay divergence: recorded tool `{tool}` asserted file `{path}` exists, but it does not at replay time")]
    FileMissing { tool: String, path: PathBuf },
    #[error("replay divergence: recorded tool `{tool}` asserted file `{path}` was deleted, but it still exists at replay time")]
    FileStillPresent { tool: String, path: PathBuf },
    #[error("replay divergence: recorded assertion for tool `{tool}` is malformed: {detail}")]
    Malformed { tool: String, detail: String },
}

/// Structured post-condition captured at record time. Producers emit
/// this alongside `ToolResult` for tools whose effects sit outside the
/// recording boundary so replay can detect divergence.
///
/// `#[serde(tag = "kind")]` makes the on-disk shape
/// `{"kind":"file_exists","path":"..."}`; unknown variants land in the
/// `Unknown` arm so older readers don't refuse to parse forward-extended
/// decision logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SideEffectAssertion {
    /// The producer wrote (or otherwise ensured) a file at `path`.
    /// Replay asserts `path` still exists.
    FileExists { path: PathBuf },
    /// The producer deleted a file at `path`. Replay asserts `path`
    /// no longer exists.
    FileAbsent { path: PathBuf },
    /// Unrecognized variant (forward-compat). Replay treats this as
    /// "no assertion to check" and proceeds.
    #[serde(other)]
    Unknown,
}

/// Check the recorded post-condition against the current filesystem
/// state. `tool` is the recorded tool name (used only in error
/// messages).
pub fn assert_side_effect(tool: &str, assertion: &SideEffectAssertion) -> Result<(), ReplayDivergenceError> {
    match assertion {
        SideEffectAssertion::FileExists { path } => {
            if !path.is_absolute() {
                return Err(ReplayDivergenceError::Malformed {
                    tool: tool.to_string(),
                    detail: format!("file_exists assertion path `{}` must be absolute", path.display()),
                });
            }
            if !path.exists() {
                return Err(ReplayDivergenceError::FileMissing { tool: tool.to_string(), path: path.clone() });
            }
            Ok(())
        }
        SideEffectAssertion::FileAbsent { path } => {
            if !path.is_absolute() {
                return Err(ReplayDivergenceError::Malformed {
                    tool: tool.to_string(),
                    detail: format!("file_absent assertion path `{}` must be absolute", path.display()),
                });
            }
            if path.exists() {
                return Err(ReplayDivergenceError::FileStillPresent { tool: tool.to_string(), path: path.clone() });
            }
            Ok(())
        }
        SideEffectAssertion::Unknown => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn file_exists_passes_when_file_present() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("hello.txt");
        std::fs::write(&p, b"hi").unwrap();
        let assertion = SideEffectAssertion::FileExists { path: p };
        assert!(assert_side_effect("Write", &assertion).is_ok());
    }

    #[test]
    fn file_exists_diverges_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("never.txt");
        let assertion = SideEffectAssertion::FileExists { path: p.clone() };
        match assert_side_effect("Write", &assertion) {
            Err(ReplayDivergenceError::FileMissing { tool, path }) => {
                assert_eq!(tool, "Write");
                assert_eq!(path, p);
            }
            other => panic!("expected FileMissing, got {:?}", other),
        }
    }

    #[test]
    fn file_absent_passes_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("gone.txt");
        let assertion = SideEffectAssertion::FileAbsent { path: p };
        assert!(assert_side_effect("Bash:rm", &assertion).is_ok());
    }

    #[test]
    fn file_absent_diverges_when_file_present() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("still-here.txt");
        std::fs::write(&p, b"oops").unwrap();
        let assertion = SideEffectAssertion::FileAbsent { path: p.clone() };
        match assert_side_effect("Bash:rm", &assertion) {
            Err(ReplayDivergenceError::FileStillPresent { tool, path }) => {
                assert_eq!(tool, "Bash:rm");
                assert_eq!(path, p);
            }
            other => panic!("expected FileStillPresent, got {:?}", other),
        }
    }

    #[test]
    fn relative_path_is_rejected_as_malformed() {
        let assertion = SideEffectAssertion::FileExists { path: PathBuf::from("relative.txt") };
        match assert_side_effect("Write", &assertion) {
            Err(ReplayDivergenceError::Malformed { tool, detail }) => {
                assert_eq!(tool, "Write");
                assert!(detail.contains("must be absolute"), "unexpected detail: {detail}");
            }
            other => panic!("expected Malformed, got {:?}", other),
        }
    }

    #[test]
    fn unknown_variant_is_no_op() {
        let assertion = SideEffectAssertion::Unknown;
        assert!(assert_side_effect("FutureTool", &assertion).is_ok());
    }

    #[test]
    fn unknown_kind_round_trips_via_serde_other() {
        let raw = serde_json::json!({"kind": "something_v0_6", "extra": "data"});
        let assertion: SideEffectAssertion = serde_json::from_value(raw).expect("forward-compat decode");
        assert!(matches!(assertion, SideEffectAssertion::Unknown));
    }

    #[test]
    fn file_exists_round_trips_via_serde() {
        let original = SideEffectAssertion::FileExists { path: PathBuf::from("/abs/path/foo") };
        let json = serde_json::to_value(&original).unwrap();
        assert_eq!(json["kind"], "file_exists");
        let back: SideEffectAssertion = serde_json::from_value(json).unwrap();
        assert_eq!(back, original);
    }
}
