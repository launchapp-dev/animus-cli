//! CLI interface and registry for managing different AI coding CLIs

pub mod interface;
pub mod launch;
pub mod registry;
pub mod types;

// Specific CLI implementations
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod oai_runner;
pub mod opencode;

pub use interface::{CliCommand, CliInterface, CliOutput};
pub use launch::{
    codex_exec_insert_index_json, ensure_codex_config_override, ensure_codex_config_override_json, ensure_flag,
    ensure_flag_value, ensure_flag_value_json, ensure_machine_json_output, is_ai_cli_tool, is_binary_on_path,
    launch_prompt_insert_index_json, lookup_binary_in_path, parse_cli_type, parse_launch_from_runtime_contract,
    LaunchInvocation,
};
pub use registry::CliRegistry;
pub use types::{CliCapability, CliMetadata, CliStatus, CliType};
