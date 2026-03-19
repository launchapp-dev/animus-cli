use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    #[arg(long, help = "Check for the latest version without downloading or installing.")]
    pub(crate) check: bool,
    #[arg(long, short = 'y', help = "Skip the confirmation prompt and install automatically.")]
    pub(crate) yes: bool,
}
