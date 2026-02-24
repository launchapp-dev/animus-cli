//! Shared wire protocol between the Tauri backend and the standalone agent runner.
//!
//! Compatibility assumptions:
//! - Serde field names and enum tags are part of the wire contract and should remain stable.
//! - `PROTOCOL_VERSION` represents protocol compatibility and must only change with deliberate
//!   coordinated migrations across all producers and consumers.

pub mod agent_runner;
pub mod common;
pub mod config;
pub mod daemon;
pub mod errors;
pub mod model_routing;
pub mod output;

pub use agent_runner::*;
pub use common::*;
pub use config::*;
pub use daemon::*;
pub use errors::*;
pub use model_routing::*;
pub use output::*;

pub const PROTOCOL_VERSION: &str = "1.0.0";
