use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum VisionCommand {
    /// Read the current project vision.
    Get,
}
