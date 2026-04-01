use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum TriggerCommand {
    /// List all configured event triggers for this project.
    List,
}
