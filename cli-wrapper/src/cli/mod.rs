//! CLI interface and registry for managing different AI coding CLIs

pub mod interface;
pub mod launch;
pub mod registry;
pub mod types;

// Specific CLI implementations
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;

pub use interface::{CliCommand, CliInterface, CliOutput};
pub use launch::{
    ensure_machine_json_output, is_ai_cli_tool, parse_cli_type, parse_launch_from_runtime_contract,
    LaunchInvocation,
};
pub use registry::CliRegistry;
pub use types::{CliCapability, CliMetadata, CliStatus, CliType};
