//! CLI Wrapper - A standalone tool for testing and managing different agent CLIs
//!
//! This library provides a unified interface for working with various AI coding CLIs
//! including Claude Code, Codex, Gemini CLI, and others.
//!
//! # Features
//!
//! - **CLI Discovery**: Automatically detect installed CLIs
//! - **Testing Framework**: Verify CLI functionality and compatibility
//! - **Output Parsing**: Parse and validate CLI outputs
//! - **Configuration**: Manage CLI preferences and settings
//!
//! # Example
//!
//! ```no_run
//! use cli_wrapper::{CliRegistry, CliTester, TestSuite};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let mut registry = CliRegistry::new();
//!     registry.discover_clis().await?;
//!
//!     let tester = CliTester::new();
//!     let suite = TestSuite::basic_verification();
//!     let results = tester.test_all_clis(&registry, &suite).await?;
//!
//!     for result in results {
//!         println!("{}: {}", result.cli_type.display_name(), result.passed);
//!     }
//!
//!     Ok(())
//! }
//! ```

pub mod cli;
pub mod config;
pub mod error;
pub mod mcp_manager;
pub mod parser;
pub mod tester;
pub mod validator;

pub use cli::{
    codex_exec_insert_index_json, ensure_codex_config_override, ensure_codex_config_override_json,
    ensure_flag, ensure_flag_value, ensure_flag_value_json, ensure_machine_json_output,
    is_ai_cli_tool, is_binary_on_path, launch_prompt_insert_index_json, lookup_binary_in_path,
    parse_cli_type, parse_launch_from_runtime_contract, CliCapability, CliCommand, CliInterface,
    CliOutput, CliRegistry, CliStatus, CliType, LaunchInvocation,
};
pub use config::Config;
pub use error::{Error, Result};
pub use mcp_manager::McpServerManager;
pub use parser::{OutputParser, ParsedOutput};
pub use tester::{CliTester, TestResult, TestSuite};
pub use validator::{CliValidator, ValidationResult};
