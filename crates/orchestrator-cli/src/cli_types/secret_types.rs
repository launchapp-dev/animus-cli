use clap::{Args, Subcommand};

/// `animus secret ...` — project-scoped secrets stored in the OS
/// keychain. The keychain account string includes the current
/// `repo-scope`, so two projects with the same KEY do not collide.
#[derive(Debug, Subcommand)]
pub(crate) enum SecretCommand {
    /// Store a secret. With `--value` the value is taken from the
    /// flag; without it the value is read from stdin (pipe-safe).
    Set(SecretSetArgs),
    /// Print a stored value. Warns if stdout is a TTY so values do
    /// not accidentally land in shell scrollback.
    Get(SecretGetArgs),
    /// List stored KEY names for the current project. Values are
    /// never returned by this command.
    List(SecretListArgs),
    /// Remove a stored secret from the keychain and the per-scope
    /// index.
    Rm(SecretRmArgs),
    /// Bulk migrate from a `.env` file into the keychain. Each
    /// non-comment `KEY=VALUE` line becomes one stored entry.
    ImportEnv(SecretImportEnvArgs),
    /// Export stored secrets back to a `.env` file. Loud warning:
    /// this writes plaintext to disk.
    ExportEnv(SecretExportEnvArgs),
    /// Move every stored secret between backends (OS keyring <-> the
    /// device-encrypted store). Non-destructive: the source is left intact
    /// unless `--remove-source` is passed.
    Migrate(SecretMigrateArgs),
    /// Read-only probe of the device-encrypted store: reports whether the
    /// configured key source unlocks it (ok / missing / unlock-failed /
    /// corrupt) and NEVER creates, initializes, or rewrites anything. Intended
    /// as a deploy preflight so a bad ANIMUS_SECRET_KEY fails loudly at boot
    /// instead of bricking the daemon at first secret read.
    Verify(SecretVerifyArgs),
    /// Re-wrap the store's master key from the current operator key to the key
    /// staged in ANIMUS_SECRET_KEY_NEXT (hex or base64, 32 bytes). Only the
    /// wrap changes — no secret is re-encrypted. Idempotent. Operator flow:
    /// stage the new key in ANIMUS_SECRET_KEY_NEXT, run this command, run
    /// `animus secret verify` with the new key, swap ANIMUS_SECRET_KEY to the
    /// new value, then unset ANIMUS_SECRET_KEY_NEXT.
    RewrapKey(SecretRewrapArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SecretVerifyArgs {
    /// Emit machine-readable JSON output on the `animus.cli.v1` envelope.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretRewrapArgs {
    /// Emit machine-readable JSON output on the `animus.cli.v1` envelope.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretMigrateArgs {
    /// Target backend to copy every secret into.
    #[arg(long, value_parser = ["device", "keyring"])]
    pub(crate) to: String,
    /// After copying (and verifying) each secret, delete it from the source
    /// backend. Off by default so a failed migration never loses secrets.
    #[arg(long)]
    pub(crate) remove_source: bool,
    /// Emit machine-readable JSON output on the `animus.cli.v1` envelope.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretSetArgs {
    /// KEY name (e.g. `LINEAR_API_TOKEN`).
    pub(crate) key: String,
    /// Explicit value. When omitted, the value is read from stdin
    /// (allowing `cat secret.txt | animus secret set KEY`).
    #[arg(long)]
    pub(crate) value: Option<String>,
    /// Emit machine-readable JSON output on the `animus.cli.v1`
    /// envelope.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretGetArgs {
    /// KEY name.
    pub(crate) key: String,
    /// Emit JSON output. Off by default — `get` is shaped for
    /// piping the raw value into other tools.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretListArgs {
    /// Emit JSON output.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretRmArgs {
    /// KEY name.
    pub(crate) key: String,
    /// Emit JSON output.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretImportEnvArgs {
    /// Source `.env` file. Defaults to `<project-root>/.env`.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<String>,
    /// Overwrite existing keychain entries on collision. By default
    /// import skips KEYs that already have a stored value.
    #[arg(long)]
    pub(crate) overwrite: bool,
    /// Emit JSON output.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SecretExportEnvArgs {
    /// Destination `.env` file. Defaults to
    /// `<project-root>/.env.exported`. The command never writes to
    /// `<project-root>/.env` unless this flag is passed explicitly.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<String>,
    /// Emit JSON output.
    #[arg(long)]
    pub(crate) json: bool,
}
