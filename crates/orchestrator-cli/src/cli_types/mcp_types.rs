use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Start the MCP server in the current process.
    Serve,
    /// Start the memory context MCP server for workflow phases.
    Memory,
}
