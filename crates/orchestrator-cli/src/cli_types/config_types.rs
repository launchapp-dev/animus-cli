use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Display all effective configuration resolved from all config layers.
    List,
}
