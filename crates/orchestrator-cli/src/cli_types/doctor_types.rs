use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    #[arg(long, help = "Apply safe local remediations for doctor findings.")]
    pub(crate) fix: bool,
    #[arg(
        long,
        requires = "fix",
        help = "Apply fixes that require explicit consent (e.g. orphan worktree removal). Must be combined with --fix; clap rejects --yes on its own."
    )]
    pub(crate) yes: bool,
    #[arg(
        long,
        value_name = "NAME",
        help = "Run only checks whose id contains the given substring (repeatable, case-insensitive)."
    )]
    pub(crate) filter: Vec<String>,
    #[arg(
        long = "check",
        value_name = "NAME",
        help = "Run only checks whose id or category matches NAME exactly (repeatable). Use --filter for substring matching."
    )]
    pub(crate) check: Vec<String>,
    #[arg(long, help = "Skip checks that spawn external subprocesses (cosign verify, plugin --manifest probes).")]
    pub(crate) skip_subprocess: bool,
}
