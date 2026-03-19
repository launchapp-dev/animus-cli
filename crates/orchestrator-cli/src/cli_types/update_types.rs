use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    #[arg(long, help = "Check for updates without downloading or installing.")]
    pub(crate) check: bool,

    #[arg(long, help = "Force update even if already at the latest version.")]
    pub(crate) force: bool,

    #[arg(long, value_name = "VERSION", help = "Install a specific release version (e.g. 0.2.1).")]
    pub(crate) version: Option<String>,
}
