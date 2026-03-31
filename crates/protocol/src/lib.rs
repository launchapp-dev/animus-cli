//! Shared wire protocol between the AO service layer and the standalone agent runner.
//!
//! Compatibility assumptions:
//! - Serde field names and enum tags are part of the wire contract and should remain stable.
//! - `PROTOCOL_VERSION` represents protocol compatibility and must only change with deliberate
//!   coordinated migrations across all producers and consumers.

pub mod agent_runner;
pub mod common;
pub mod config;
pub mod credentials;
pub mod daemon;
pub mod daemon_event_record;
pub mod error_classification;
pub mod errors;
pub mod model_routing;
pub mod orchestrator;
pub mod output;
pub mod process;
pub mod repository_scope;
pub mod sync_config;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

pub use agent_runner::*;
pub use common::*;
// Explicit re-exports for config helpers used across crates
pub use config::{
    cli_tracker_path, daemon_events_log_path, default_allowed_tool_prefixes, parse_env_bool, parse_env_bool_opt,
    ClaudeProfileEntry, Config, ProjectMcpServerEntry, ProjectMcpServerTransport,
};
pub use daemon::*;
pub use daemon_event_record::*;
pub use error_classification::*;
pub use errors::*;
pub use model_routing::*;
pub use orchestrator::{
    RunnerEvent, SubjectDispatch, SubjectExecutionFact, SubjectRef, SUBJECT_KIND_CUSTOM, SUBJECT_KIND_REQUIREMENT,
    SUBJECT_KIND_TASK,
};
pub use output::*;
pub use process::*;
pub use repository_scope::*;
pub use sync_config::SyncConfig;
pub const PROTOCOL_VERSION: &str = "1.0.0";

pub const CLI_SCHEMA_ID: &str = "ao.cli.v1";
pub const MAX_UNIX_SOCKET_PATH_LEN: usize = 100;
pub const ACTOR_CLI: &str = "ao-cli";
pub const ACTOR_DAEMON: &str = "ao-daemon";
pub const ACTOR_CORE: &str = "ao-core";
