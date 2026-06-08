use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum PluginCommand {
    /// Discover plugins via plugins.yaml, `.animus/plugins/`,
    /// `$ANIMUS_PLUGIN_DIR`, and `$ANIMUS_PLUGIN_PATH`.
    List(PluginListArgs),
    /// Print a plugin's manifest plus initialize-time capabilities.
    Info(PluginInfoArgs),
    /// Send a JSON-RPC request to a plugin and print its response.
    Call(PluginCallArgs),
    /// Health-check a plugin by spawning it, completing the handshake, and pinging.
    Ping(PluginPingArgs),
    /// Install a plugin binary from a public GitHub release (OWNER/REPO[@TAG]),
    /// a local path, or a direct URL into ~/.animus/plugins/ (override with
    /// --plugin-dir or $ANIMUS_PLUGIN_DIR).
    Install(PluginInstallArgs),
    /// Remove a previously installed plugin from ~/.animus/plugins/ (override
    /// with --plugin-dir or $ANIMUS_PLUGIN_DIR) and ~/.animus/plugins.yaml.
    Uninstall(PluginUninstallArgs),
    /// Scaffold a new plugin project from the launchapp-dev/animus-plugin-template scaffold.
    New(PluginNewArgs),
    /// Emit a minimal, offline starter Cargo project for a new plugin kind.
    /// Unlike `animus plugin new` (which clones the template repo), this
    /// subcommand writes a self-contained project from built-in templates
    /// so it works without network access. Currently scoped to `trigger`.
    #[command(subcommand)]
    Scaffold(PluginScaffoldCommand),
    /// Search the public Animus plugin registry by substring + filters.
    Search(PluginSearchArgs),
    /// Browse the public Animus plugin registry, grouped by kind.
    Browse(PluginBrowseArgs),
    /// Bulk-update installed release-source plugins to the recommended pins
    /// declared in `default-install.json`. Selectors: `--all`, `--kind <KIND>`,
    /// or `--name <NAME>` (exactly one required). `--check` previews the diff
    /// without writing; `--yes` skips the confirmation prompt.
    Update(PluginUpdateArgs),
    /// Install the standard set of provider plugins from public GitHub releases
    /// (claude, codex, gemini, opencode, oai). Skips plugins that are already
    /// installed. Optional flags pull in additional default plugins.
    InstallDefaults(PluginInstallDefaultsArgs),
    /// Inspect and verify the plugin lockfile (`.animus/plugins.lock`).
    /// The lockfile records sha256 + version for every installed plugin so an
    /// `install --force` or tampered-binary scenario is visible to operators.
    #[command(subcommand)]
    Lock(PluginLockCommand),
    /// Per-role view of installed plugins. Shows every preflight role with its
    /// installed plugins (by installed_kind + native_kind) and flags duplicates
    /// so collisions are visible without spelunking through the lockfile.
    Doctor(PluginDoctorArgs),
    /// Rename an installed plugin's `installed_kind` after install. Reuses the
    /// same collision check + auto-increment + invalid-character validation
    /// the install pipeline applies for `--as-kind`. Operates on the lockfile
    /// entry keyed by `<PLUGIN_NAME>`; the on-disk binary and manifest's
    /// `native_kind` are untouched. Only the user-facing `installed_kind` —
    /// the prefix the SubjectRouter dispatches against — changes.
    Rename(PluginRenameArgs),
    /// Per-plugin runtime status (pid, state, last RPC, restart count, last
    /// error). Answers "why does this plugin feel stuck?" by surfacing the
    /// supervisor's restart counter for every discovered plugin. As of
    /// v0.5.8 only provider plugin runtimes report live pid/last_rpc/restart
    /// fields; other kinds (subject_backend, trigger, log_storage,
    /// transport, queue, workflow_runner) appear as `discovered` until their
    /// spawners are wired through the same status registry. See
    /// TODO(codex-p2) markers in `daemon/run_daemon.rs`.
    Status(PluginStatusArgs),
    /// Inspect or wipe the on-disk plugin manifest cache. The cache stores
    /// serialized `--manifest` responses keyed by binary sha256 under
    /// `~/.animus/cache/manifests/<sha>.json` and is the reason
    /// `animus daemon status` returns in ~50ms instead of ~3s on a 30-plugin
    /// install. Use `clear` to wipe after a manual binary swap that didn't
    /// rewrite the lockfile, or `list` to debug cache contents.
    #[command(subcommand)]
    Cache(PluginCacheCommand),
}

#[derive(Debug, Subcommand)]
pub(crate) enum PluginCacheCommand {
    /// Remove every cached manifest entry under
    /// `~/.animus/cache/manifests/`. Discovery will fall back to live
    /// `--manifest` probes (and repopulate the cache) on the next call.
    Clear(PluginCacheClearArgs),
    /// List every cached manifest entry with its sha256 key, byte size, and
    /// last-modified timestamp.
    List(PluginCacheListArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PluginCacheClearArgs {
    /// Emit the result envelope as JSON instead of human-readable text.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginCacheListArgs {
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginRenameArgs {
    /// Logical plugin name (matches the lockfile / `plugins.yaml` entry key).
    #[arg(value_name = "PLUGIN_NAME")]
    pub(crate) name: String,
    /// New `installed_kind` to assign. Validated against the same rules
    /// as `--as-kind`: rejects `/`, `*`, `:`, and whitespace.
    #[arg(long = "to", value_name = "NEW_KIND")]
    pub(crate) to: String,
    /// When the requested kind already collides with another installed
    /// plugin, auto-increment (`task` -> `task-2` -> ...) instead of
    /// failing. Without this flag a collision is a hard error so the
    /// operator picks the suffix explicitly.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Emit the result envelope as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginStatusArgs {
    /// Limit the report to one plugin by name. When omitted, every known
    /// plugin is listed.
    #[arg(value_name = "NAME")]
    pub(crate) name: Option<String>,
    /// Emit results as JSON (animus.cli.v1 envelope when paired with --json
    /// at the root).
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginDoctorArgs {
    /// Emit results as JSON (animus.cli.v1 envelope when paired with --json
    /// at the root).
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PluginLockCommand {
    /// List every entry currently recorded in the plugin lockfile.
    List(PluginLockListArgs),
    /// Re-hash every installed plugin binary and report mismatches against the lockfile.
    Verify(PluginLockVerifyArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PluginLockListArgs {
    /// Override the lockfile path. Defaults to `<project>/.animus/plugins.lock`
    /// when set, otherwise `~/.animus/plugins.lock`.
    #[arg(long, value_name = "PATH")]
    pub(crate) lockfile: Option<PathBuf>,
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginLockVerifyArgs {
    /// Override the lockfile path.
    #[arg(long, value_name = "PATH")]
    pub(crate) lockfile: Option<PathBuf>,
    /// Override the plugin install directory.
    #[arg(long, value_name = "PATH")]
    pub(crate) plugin_dir: Option<String>,
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}

/// Default URL for the public Animus plugin registry index.
pub(crate) const DEFAULT_PLUGIN_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/launchapp-dev/animus-plugin-registry/main/plugins.json";

#[derive(Debug, Args)]
pub(crate) struct PluginSearchArgs {
    /// Optional substring query matched against plugin name and description (case-insensitive).
    #[arg(value_name = "QUERY")]
    pub(crate) query: Option<String>,
    /// Filter by plugin kind (e.g. `provider`, `subject_backend`, `trigger`).
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: Option<String>,
    /// Filter by tag (repeatable, ANDed).
    #[arg(long, value_name = "TAG")]
    pub(crate) tag: Vec<String>,
    /// Filter by the repo owner (e.g. `launchapp-dev`).
    #[arg(long, value_name = "ORG")]
    pub(crate) org: Option<String>,
    /// Filter by stability marker (e.g. `alpha`, `beta`, `stable`).
    #[arg(long, value_name = "STABILITY")]
    pub(crate) stability: Option<String>,
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
    /// Override the registry URL. Defaults to launchapp-dev/animus-plugin-registry main.
    #[arg(long, value_name = "URL", default_value = DEFAULT_PLUGIN_REGISTRY_URL)]
    pub(crate) registry_url: String,
    /// Bypass the local registry cache and force a fresh fetch.
    #[arg(long, default_value_t = false)]
    pub(crate) no_cache: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginBrowseArgs {
    /// Filter by plugin kind (e.g. `provider`, `subject_backend`, `trigger`).
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: Option<String>,
    /// Only show plugins that are currently installed locally.
    #[arg(long, default_value_t = false, conflicts_with = "available")]
    pub(crate) installed: bool,
    /// Only show plugins that are NOT yet installed locally.
    #[arg(long, default_value_t = false, conflicts_with = "installed")]
    pub(crate) available: bool,
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
    /// Override the registry URL.
    #[arg(long, value_name = "URL", default_value = DEFAULT_PLUGIN_REGISTRY_URL)]
    pub(crate) registry_url: String,
    /// Bypass the local registry cache and force a fresh fetch.
    #[arg(long, default_value_t = false)]
    pub(crate) no_cache: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginUpdateArgs {
    /// Legacy positional plugin name. Equivalent to `--name <NAME>`. Retained
    /// so v0.5.7 scripts (`animus plugin update <NAME>`) still work.
    #[arg(value_name = "NAME", conflicts_with_all = ["all", "kind", "name_flag"])]
    pub(crate) name_positional: Option<String>,
    /// Update every installed release-source plugin to its recommended pin
    /// (from `default-install.json`).
    #[arg(long, default_value_t = false, conflicts_with_all = ["kind", "name_flag"])]
    pub(crate) all: bool,
    /// Update every installed release-source plugin whose recommended pin lives
    /// under the named kind in `default-install.json`. Accepts the
    /// canonical-plural section names (`providers`, `subjects`,
    /// `workflow_runners`, `queues`, `notifiers`, `transports`, `oai_agent`)
    /// or the singular plugin_kind values (`provider`, `subject_backend`,
    /// `workflow_runner`, `queue`, `notifier`, `transport_backend`).
    #[arg(long, value_name = "KIND", conflicts_with_all = ["all", "name_flag"])]
    pub(crate) kind: Option<String>,
    /// Update a single installed plugin by name (matches the lockfile /
    /// `plugins.yaml` entry key — i.e. the `name_override` if one was set at
    /// install, otherwise the manifest name).
    #[arg(long = "name", value_name = "NAME", conflicts_with_all = ["all", "kind"])]
    pub(crate) name_flag: Option<String>,
    /// Print the diff (would update X v1 -> v2) and exit 0 without writing
    /// anything. Mutually exclusive with `--yes`.
    #[arg(long, default_value_t = false, conflicts_with = "yes")]
    pub(crate) check: bool,
    /// Skip the confirmation prompt and proceed with the install.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
    /// Pin to a specific tag instead of resolving the recommended pin from
    /// `default-install.json`. Only valid with `--name`.
    #[arg(long, value_name = "TAG")]
    pub(crate) tag: Option<String>,
    /// Legacy alias for `--check`. Retained so v0.5.7 scripts still work.
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
    /// Reinstall even if the installed tag already matches the recommended pin.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// After updating, attempt to restart the daemon (best-effort) so the new
    /// binaries are picked up. Default off — the operator usually wants to
    /// schedule the restart themselves.
    #[arg(long, default_value_t = false)]
    pub(crate) restart_daemon: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginInstallDefaultsArgs {
    /// Override the plugin install directory. Takes precedence over
    /// `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/`.
    #[arg(long, value_name = "PATH")]
    pub(crate) plugin_dir: Option<String>,
    /// Reinstall plugins even if they are already present.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Auto-confirm the trust-on-first-use prompt for the launchapp-dev org.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
    /// Also install `animus-provider-oai-agent` (curated tag in
    /// `orchestrator-core::plugin_registry::DEFAULT_OAI_AGENT_PLUGINS`).
    #[arg(long, default_value_t = false)]
    pub(crate) include_oai_agent: bool,
    /// Also install the default subject_backend plugins (subject-default,
    /// subject-requirements, subject-linear, subject-sqlite, subject-markdown).
    #[arg(long, default_value_t = false)]
    pub(crate) include_subjects: bool,
    /// Also install the default transport_backend + web_ui plugins
    /// (transport-http, transport-graphql, web-ui) that back `animus web`.
    #[arg(long, default_value_t = false)]
    pub(crate) include_transports: bool,
    /// Emit results as JSON.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
    /// Discard a corrupt or incompatible `.animus/plugins.lock` and start a
    /// fresh in-memory lockfile for the batch install. SECURITY: this drops
    /// the existing integrity history; only use it after confirming the
    /// lockfile damage was not the result of tampering.
    #[arg(long, default_value_t = false)]
    pub(crate) force_rewrite_lockfile: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginInstallArgs {
    /// Public GitHub repo slug to install from (e.g.
    /// `launchapp-dev/animus-provider-claude` or
    /// `launchapp-dev/animus-provider-claude@v0.1.0`). Resolves the matching
    /// release asset for the current platform. Mutually exclusive with
    /// `--path` and `--url`.
    #[arg(value_name = "OWNER/REPO[@TAG]", group = "install_source")]
    pub(crate) source: Option<String>,
    /// Local path to the plugin binary to install.
    #[arg(long, value_name = "PATH", group = "install_source")]
    pub(crate) path: Option<String>,
    /// URL to download the plugin binary from (https only).
    #[arg(long, value_name = "URL", group = "install_source")]
    pub(crate) url: Option<String>,
    /// Release tag to install when using the positional OWNER/REPO. Defaults
    /// to the latest release. Conflicts with `@tag` syntax on the positional.
    #[arg(long, value_name = "TAG")]
    pub(crate) tag: Option<String>,
    /// Explicitly opt in to the latest release (this is the default behavior).
    /// Conflicts with `--tag`.
    #[arg(long, default_value_t = false, conflicts_with = "tag")]
    pub(crate) latest: bool,
    /// Optional logical plugin name. Defaults to the binary file name.
    #[arg(long, value_name = "NAME")]
    pub(crate) name: Option<String>,
    /// Expected SHA256 hex digest. Required when installing from `--url`;
    /// optional otherwise. The install fails if the downloaded/copied binary's
    /// checksum does not match.
    #[arg(long, value_name = "HEX")]
    pub(crate) sha256: Option<String>,
    /// Overwrite an existing installed plugin with the same name.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Skip running `--manifest` against the installed binary to verify it.
    #[arg(long, default_value_t = false)]
    pub(crate) skip_manifest_check: bool,
    /// Override the plugin install directory. Takes precedence over
    /// `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/`.
    #[arg(long, value_name = "PATH")]
    pub(crate) plugin_dir: Option<String>,
    /// Signature enforcement mode. `strict` refuses installs whose cosign
    /// keyless bundle is missing, invalid, or signed by an identity outside
    /// the trusted-publisher list. `warn` (the current default) logs the
    /// failure and proceeds.
    /// `disabled` skips verification entirely (escape hatch). Keyless trust
    /// is anchored on Sigstore Fulcio + Rekor and the per-publisher
    /// identity regex; no PEM is required. See `docs/reference/security.md`.
    #[arg(long, value_name = "MODE", value_parser = ["strict", "warn", "disabled"])]
    pub(crate) signature_policy: Option<String>,
    /// **Deprecated as of v0.4.12.** Keyless cosign verification has no
    /// static public-key trust anchor; this flag is retained so existing
    /// scripts don't break and is logged + ignored. The flag will be
    /// removed in a future release. Use `--signature-policy` plus the
    /// built-in trusted-publisher list instead.
    #[arg(long, value_name = "PATH", hide = true)]
    pub(crate) trust_key: Option<PathBuf>,
    /// Convenience flag: equivalent to `--signature-policy warn`.
    /// Mutually exclusive with `--signature-policy` and `--require-signature`.
    #[arg(long, default_value_t = false, conflicts_with_all = ["signature_policy", "require_signature"])]
    pub(crate) allow_unsigned: bool,
    /// Legacy: refuse install when no cosign bundle is present or when
    /// verification fails. Equivalent to `--signature-policy strict`.
    /// Retained for backward compatibility; also the recommended opt-in when
    /// fail-closed install behavior is required.
    #[arg(long, default_value_t = false, conflicts_with = "skip_signature")]
    pub(crate) require_signature: bool,
    /// Legacy: skip cosign signature verification entirely. Equivalent to
    /// `--signature-policy disabled`. Retained for backward compatibility.
    #[arg(long, default_value_t = false)]
    pub(crate) skip_signature: bool,
    /// Path to a `trusted-signers.yaml` allowlist. Defaults to
    /// `~/.animus/trusted-signers.yaml`.
    #[arg(long, value_name = "PATH")]
    pub(crate) trusted_signers: Option<PathBuf>,
    /// Allow installing a provider plugin whose `provider_tool` collides with
    /// an in-tree backend (claude/codex/gemini/opencode/oai-runner). Without
    /// this flag the install pipeline refuses such plugins because they
    /// silently hijack all dispatch for the matching tool.
    #[arg(long, default_value_t = false)]
    pub(crate) allow_shadow_builtin: bool,
    /// Mark the supplied `OWNER` as trusted for future installs (TOFU). Equivalent
    /// to a one-shot append to `~/.animus/trusted-orgs.yaml`. Repeat for multiple
    /// owners.
    #[arg(long = "allow-org", value_name = "OWNER")]
    pub(crate) allow_org: Vec<String>,
    /// Auto-confirm the trust-on-first-use (TOFU) prompt when installing from
    /// an untrusted org. Equivalent to typing `yes` at the prompt and adding the
    /// org to `~/.animus/trusted-orgs.yaml`.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
    /// Discard a corrupt or incompatible `.animus/plugins.lock` and start a
    /// fresh in-memory lockfile for this install. SECURITY: this drops the
    /// existing integrity history; only use it after confirming the lockfile
    /// damage was not the result of tampering. Without this flag, an
    /// unreadable lockfile fails the install closed rather than silently
    /// overwriting it.
    #[arg(long, default_value_t = false)]
    pub(crate) force_rewrite_lockfile: bool,
    /// (v0.5.7) Override the installed-kind assigned to a subject_backend
    /// plugin at install time. The supplied KIND becomes the user-facing
    /// prefix the SubjectRouter dispatches against (e.g. `archive` for a
    /// second `subject_kind:task` backend). When omitted and the native
    /// kind collides with an existing install, the install pipeline
    /// auto-increments (`task` -> `task-2` -> `task-3`). When supplied and
    /// the explicit value also collides, the install fails with an
    /// actionable error. v0.5.7 only supports renaming subject_backend
    /// plugins; passing --as-kind on a provider, transport, workflow_runner,
    /// queue, or trigger plugin is an error.
    #[arg(long = "as-kind", value_name = "KIND")]
    pub(crate) as_kind: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PluginUninstallArgs {
    #[arg(long, value_name = "NAME", help = "Logical plugin name to uninstall.")]
    pub(crate) name: String,
    /// Override the plugin install directory. Takes precedence over
    /// `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/`.
    #[arg(long, value_name = "PATH")]
    pub(crate) plugin_dir: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PluginListArgs {
    #[arg(long, default_value_t = false, help = "Also scan $PATH for animus-provider-* and animus-plugin-* binaries.")]
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginInfoArgs {
    #[arg(long, value_name = "NAME", help = "Plugin name (matches the discovered manifest or filename).")]
    pub(crate) name: String,
    #[arg(long, default_value_t = false, help = "Also scan $PATH while resolving the plugin.")]
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginCallArgs {
    #[arg(long, value_name = "NAME", help = "Plugin name to dispatch the request to.")]
    pub(crate) name: String,
    #[arg(long, value_name = "METHOD", help = "JSON-RPC method, e.g. agent/run, mcp/tool_call, or task/list.")]
    pub(crate) method: String,
    #[arg(
        long,
        value_name = "JSON",
        help = "Optional JSON params object. When omitted, the request is sent without a params field."
    )]
    pub(crate) params: Option<String>,
    #[arg(long, default_value_t = false, help = "Also scan $PATH while resolving the plugin.")]
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginPingArgs {
    #[arg(long, value_name = "NAME", help = "Plugin name to spawn and ping.")]
    pub(crate) name: String,
    #[arg(long, default_value_t = false, help = "Also scan $PATH while resolving the plugin.")]
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PluginNewArgs {
    /// Plugin kind (subject, provider, trigger).
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: String,

    /// Plugin short name (kebab-case, e.g. jira).
    #[arg(long, value_name = "NAME")]
    pub(crate) name: String,

    /// GitHub org used in the generated project's repository field.
    #[arg(long, value_name = "ORG", default_value = "launchapp-dev")]
    pub(crate) org: String,

    /// Short description for the plugin. Defaults to "An Animus <kind> backend plugin".
    #[arg(long, value_name = "TEXT")]
    pub(crate) description: Option<String>,

    /// Output directory. Defaults to ./animus-<kind>-<name>.
    #[arg(long, value_name = "PATH")]
    pub(crate) out_dir: Option<PathBuf>,

    /// Template git ref (branch or tag) to clone.
    #[arg(long, value_name = "REF", default_value = "main")]
    pub(crate) template_version: String,

    /// Template git URL. Defaults to launchapp-dev/animus-plugin-template.
    #[arg(long, value_name = "URL", default_value = "https://github.com/launchapp-dev/animus-plugin-template")]
    pub(crate) template_repo: String,

    /// Use a local checkout of the template repo instead of running `git clone`.
    #[arg(long, value_name = "PATH")]
    pub(crate) template_path: Option<PathBuf>,

    /// Override the existing output directory if it already exists.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PluginScaffoldCommand {
    /// Scaffold an external trigger backend plugin.
    Trigger(PluginScaffoldTriggerArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PluginScaffoldTriggerArgs {
    /// Plugin short name in kebab-case (e.g. `fswatch`, `cron`, `slack-thread`).
    /// The generated crate is named `animus-trigger-<name>`.
    #[arg(value_name = "NAME")]
    pub(crate) name: String,

    /// GitHub user or org for the generated project's repository field.
    /// Defaults to `$USER`, then `launchapp-dev`.
    #[arg(long, value_name = "OWNER")]
    pub(crate) owner: Option<String>,

    /// Output directory. Defaults to `./animus-trigger-<name>`.
    #[arg(long, value_name = "PATH")]
    pub(crate) out_dir: Option<PathBuf>,

    /// SPDX license identifier embedded into the generated `Cargo.toml`.
    #[arg(long, value_name = "ID", default_value = "MIT")]
    pub(crate) license: String,

    /// Short description for the generated `Cargo.toml` + README.
    #[arg(long, value_name = "TEXT")]
    pub(crate) description: Option<String>,

    /// Tag of `launchapp-dev/animus-protocol` to pin the generated
    /// project's `animus-plugin-protocol` + `animus-plugin-runtime`
    /// dependencies to. Defaults to the protocol tag this CLI was
    /// built against.
    #[arg(long, value_name = "TAG", default_value = "v0.5.5")]
    pub(crate) protocol_tag: String,

    /// Overwrite the output directory if it already exists.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,

    /// Emit the result envelope as JSON instead of human-readable text.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}
