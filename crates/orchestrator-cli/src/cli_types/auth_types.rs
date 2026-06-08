use clap::Subcommand;

/// `animus auth ...` subcommands. v0.5.8 ships `whoami` only — the
/// design doc reserves further subcommands (`check <permission>`,
/// `principal list`) for v0.6 when the per-principal scope + role
/// table land.
#[derive(Debug, Subcommand)]
pub(crate) enum AuthCommand {
    /// Print the currently resolved principal (id + kind + peer OS user).
    Whoami,
}
