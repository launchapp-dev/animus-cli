use clap::{Args, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Print the available version vs. installed and exit without
    /// touching the binary. Exit code 0 when an update is available,
    /// 1 when already on the latest release (scriptable for CI hooks).
    #[arg(long, default_value_t = false)]
    pub(crate) check: bool,
    /// Skip the interactive `[y/N]` confirmation. Required when piping
    /// or running under CI.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
    /// Re-install the resolved release even when it matches the installed
    /// version (useful to repair a broken install).
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Consider prereleases regardless of the selected `--channel`. Folded
    /// in from the retired `self update --prerelease` flag.
    #[arg(long, default_value_t = false)]
    pub(crate) prerelease: bool,
    /// Release channel to poll. `stable` follows the latest non-prerelease
    /// GitHub release; `nightly` follows the most recent prerelease
    /// (mapped to the existing `AutoUpdateChannel::Prerelease` admission
    /// rule — there is no separate `*-nightly` tag stream today).
    #[arg(long, value_enum, default_value_t = UpdateChannelArg::Stable)]
    pub(crate) channel: UpdateChannelArg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "lower")]
pub(crate) enum UpdateChannelArg {
    Stable,
    Nightly,
}
