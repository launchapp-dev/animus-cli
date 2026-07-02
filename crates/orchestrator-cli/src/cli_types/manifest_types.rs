use clap::Args;

/// `animus install` — resolve `animus.toml` into the lockfile and install the
/// declared plugins + packs.
#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    #[arg(
        long,
        help = "Reproduce EXACTLY the set pinned in `.animus/plugins.lock` (npm-ci style). Fails if the manifest declares a plugin the lockfile does not pin. Use in CI / containers."
    )]
    pub(crate) locked: bool,
    #[arg(long, help = "Reinstall declared dependencies even when already present.")]
    pub(crate) force: bool,
    #[arg(
        long,
        value_name = "OWNER",
        help = "Pre-trust an additional GitHub owner (repeatable). Required to install a manifest git dependency from a non-`launchapp-dev` org in a non-interactive / CI / server context, where trust-on-first-use fails closed instead of auto-trusting."
    )]
    pub(crate) allow_org: Vec<String>,
}

/// `animus add <spec>` — add a plugin (or pack) to `animus.toml` and install it.
#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    #[arg(
        value_name = "SPEC",
        help = "Dependency spec: `name[@version]` (curated), `OWNER/REPO[@tag]` (explicit git), or a bare `name` (latest)."
    )]
    pub(crate) spec: String,
    #[arg(long, help = "Add to the `[packs]` table instead of `[plugins]`.")]
    pub(crate) pack: bool,
    #[arg(
        long,
        value_name = "PATH",
        help = "Add as a local `path` dependency pointing at PATH. The SPEC is used as the dependency name."
    )]
    pub(crate) path: Option<String>,
    #[arg(long, help = "Reinstall even when already present.")]
    pub(crate) force: bool,
}

/// `animus remove <name>` — drop a plugin (or pack) from `animus.toml` and
/// uninstall it.
#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
    #[arg(value_name = "NAME", help = "Plugin slug (or pack id with --pack) to remove.")]
    pub(crate) name: String,
    #[arg(long, help = "Remove from the `[packs]` table instead of `[plugins]`.")]
    pub(crate) pack: bool,
}
