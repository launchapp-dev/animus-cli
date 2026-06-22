mod control_routing;
mod marketplace;
mod new;
mod scaffold;
mod scope;
mod signing;
mod status;

pub(crate) use control_routing::build_plugin_routing;
pub(crate) use marketplace::{
    read_installed_index, run_plugin_browse, run_plugin_search, run_plugin_update, InstalledPlugin,
    PluginBrowseRequest, PluginSearchRequest, PluginUpdateRequest, PluginUpdateSelector,
};
#[allow(unused_imports)]
pub(crate) use signing::{
    cosign_available, load_trusted_signers, resolve_trusted_signers_path, verify_with_cosign, SignatureStatus,
    GITHUB_OIDC_ISSUER,
};

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use animus_plugin_protocol::PluginManifest;
use anyhow::{anyhow, Context, Result};
use orchestrator_daemon_runtime::{Audit, AuditActor, AuditEvent, AuditEventKind};
use orchestrator_plugin_host::session::is_reserved_provider_tool;
use orchestrator_plugin_host::{
    current_target_triple, discover_plugins, global_lockfile_path, legacy_plugins_registry_path, plugin_install_dir,
    plugins_registry_path, project_lockfile_path, project_plugin_install_dir, project_plugins_registry_path,
    registered_skip_manifest_check_at_install_scoped, sha256_of_file as plugin_host_sha256_of_file, DiscoveredPlugin,
    DiscoverySource, DiscoveryWarning, LockEntry, LockVerifyResult, PluginDiscovery, PluginHost, PluginLockfile,
    PluginSpawnOptions, PolicyMode as PluginPolicyMode, TargetIntegrity,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    invalid_input_error, not_found_error, print_value, unavailable_error, PluginCallArgs, PluginCommand,
    PluginDoctorArgs, PluginInfoArgs, PluginInstallArgs, PluginInstallDefaultsArgs, PluginListArgs, PluginLockCommand,
    PluginLockListArgs, PluginLockVerifyArgs, PluginPingArgs, PluginPruneArgs, PluginRenameArgs, PluginRevokeTrustArgs,
    PluginScaffoldCommand, PluginScopeCommand, PluginTrustCommand, PluginTrustListArgs, PluginUninstallArgs,
};

#[derive(Debug, Serialize)]
pub(crate) struct DiscoveredPluginRow {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) plugin_kind: String,
    pub(crate) description: String,
    pub(crate) protocol_version: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) source: &'static str,
    pub(crate) path: String,
    /// Install scope the discovered binary belongs to: `project` for
    /// `<project>/.animus/plugins/` hits, `global` otherwise.
    pub(crate) scope: &'static str,
}

/// A global install that exists on disk but is hidden by a project-local
/// install of the same name (discovery prefers the project tier and dedupes
/// by name, so the global binary never reaches `plugins`).
#[derive(Debug, Serialize)]
pub(crate) struct ShadowedPluginRow {
    pub(crate) name: String,
    /// Path of the hidden global binary.
    pub(crate) path: String,
    /// Path of the project-local binary that wins discovery.
    pub(crate) shadowed_by: String,
    /// Always `"shadowed by project install"` — stable marker for scripts.
    pub(crate) note: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginWarningRow {
    pub(crate) name: String,
    pub(crate) source: &'static str,
    pub(crate) path: String,
    pub(crate) reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginListOutput {
    pub(crate) plugins: Vec<DiscoveredPluginRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<PluginWarningRow>,
    /// Global installs hidden by a same-named project-local install.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) shadowed: Vec<ShadowedPluginRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginInfoOutput {
    pub(crate) name: String,
    pub(crate) source: &'static str,
    pub(crate) path: String,
    pub(crate) manifest: PluginManifest,
    pub(crate) initialize: Value,
    /// Audit-trail field: `true` when the plugin was installed with
    /// `--skip-manifest-check`. Surfaced so operators can see why discovery
    /// silently tolerates manifest probe failures for this plugin.
    pub(crate) skip_manifest_check_at_install: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginCallOutput {
    pub(crate) name: String,
    pub(crate) method: String,
    pub(crate) result: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginPingOutput {
    pub(crate) name: String,
    pub(crate) ok: bool,
    pub(crate) plugin_info: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginInstallOutput {
    pub(crate) name: String,
    pub(crate) installed_path: String,
    pub(crate) sha256: String,
    pub(crate) manifest: Option<PluginManifest>,
    pub(crate) plugins_yaml: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) release_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) asset_name: Option<String>,
    pub(crate) sha256_verified: bool,
    /// Cosign signature verification outcome. Stable strings:
    /// `verified` | `unsigned` | `invalid` | `untrusted_signer` | `skipped`.
    /// See `docs/architecture/plugin-signing.md`.
    pub(crate) signature_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature_detail: Option<SignatureStatus>,
    /// User-facing kind assigned at install time. For subject_backend
    /// plugins this is the prefix the SubjectRouter dispatches against
    /// (e.g. `archive` after auto-increment from `task`). For provider
    /// plugins this is the `provider_tool` users invoke. Absent when the
    /// plugin declares no rename-eligible capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) assigned_kind: Option<String>,
    /// Plugin-native kind as declared in the manifest capabilities. Paired
    /// with [`Self::assigned_kind`] so scripts can detect auto-incremented
    /// renames via the `animus.plugin.install.v1` envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_kind: Option<String>,
    /// Install scope: `global` (the historical default) or `project`
    /// (`--project`, landing under `<project_root>/.animus/`).
    pub(crate) scope: &'static str,
    /// TOFU trust provenance for the org that admitted this install. Answers
    /// "when did we trust this org, and why?" from the install/list JSON.
    /// Absent for non-release installs (`--path` / `--url`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) org_trust: Option<OrgTrustAudit>,
}

/// Per-install record of which TOFU trust grant admitted the plugin's org.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrgTrustAudit {
    /// GitHub owner/org the plugin was installed from.
    pub(crate) org: String,
    /// RFC3339 timestamp of when the org was trusted (absent for built-in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) trusted_at: Option<String>,
    /// How the trust decision was made: `interactive-prompt` | `yes` |
    /// `allow-org` | `built-in`.
    pub(crate) decided_by: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUninstallOutput {
    pub(crate) name: String,
    pub(crate) removed_path: Option<String>,
    pub(crate) plugins_yaml: String,
    /// Uninstall scope: `global` or `project` (mirrors the install flag).
    pub(crate) scope: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginPruneOutput {
    /// `name` + `scope` (`global`/`project`) for each stale entry handled.
    pub(crate) stale: Vec<PluginPruneEntry>,
    /// `true` when the entries were actually removed; `false` for a dry-run
    /// preview (no `--yes`).
    pub(crate) pruned: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginPruneEntry {
    pub(crate) name: String,
    pub(crate) scope: &'static str,
    pub(crate) plugins_yaml: String,
}

// ===== Typed request structs (shared between CLI and MCP) =====

/// Typed request for `plugin list`. Both CLI and MCP build one of these and
/// call [`run_plugin_list`]. The CLI handler additionally streams warnings to
/// stderr when in text mode; MCP returns warnings inside the structured payload.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginListRequest {
    pub(crate) project_root: String,
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginInfoRequest {
    pub(crate) project_root: String,
    pub(crate) name: String,
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginPingRequest {
    pub(crate) project_root: String,
    pub(crate) name: String,
    pub(crate) include_system_path: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginCallRequest {
    pub(crate) project_root: String,
    pub(crate) name: String,
    pub(crate) method: String,
    pub(crate) params: Option<Value>,
    pub(crate) include_system_path: bool,
}

/// Typed request for `plugin install`. Mirrors the CLI arg surface so MCP can
/// invoke the same install pipeline. Exactly one of `source` / `path` / `url`
/// must be supplied. When `url` is set, `sha256` is required. The `source`
/// (owner/repo[@tag]) input is forwarded to the CLI install pipeline; if the
/// underlying handler does not yet implement public-repo installs, a clear
/// error is returned.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginInstallRequest {
    pub(crate) source: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) sha256: Option<String>,
    pub(crate) force: bool,
    pub(crate) skip_manifest_check: bool,
    /// Override the plugin install directory. Takes precedence over
    /// `$ANIMUS_PLUGIN_DIR`. When `None`, falls back to env / default.
    pub(crate) plugin_dir: Option<String>,
    /// Explicit signature policy. When `Some`, takes precedence over the
    /// legacy `require_signature` / `skip_signature` booleans. When `None`,
    /// the legacy booleans are interpreted: `skip_signature` -> `Disabled`,
    /// `require_signature` -> `Strict`, neither -> the lib default
    /// (`PluginPolicyMode::default_for_install()`, which is `Warn` in
    /// v0.4.12 while the built-in launchapp-dev cosign key is a placeholder
    /// and `Strict` again starting v0.4.13).
    pub(crate) signature_policy: Option<PluginPolicyMode>,
    /// **Deprecated as of v0.4.12** — keyless verification has no static
    /// public-key trust anchor. The flag is retained so existing scripts
    /// don't break; when `Some`, the install pipeline logs a deprecation
    /// warning and ignores the value. Use `--signature-policy` plus the
    /// built-in `TrustedPublisher` list (`launchapp-dev` keyless) instead.
    pub(crate) trust_key: Option<PathBuf>,
    /// Refuse install when no cosign bundle is present or when verification
    /// fails. Default `false` — verify-if-present.
    pub(crate) require_signature: bool,
    /// Skip cosign verification entirely (escape hatch). Mutually exclusive
    /// with `require_signature` (enforced at the CLI layer).
    pub(crate) skip_signature: bool,
    /// Optional path to the trusted-signers YAML (overrides default
    /// `~/.animus/trusted-signers.yaml`).
    pub(crate) trusted_signers: Option<PathBuf>,
    /// Permit installs whose provider_tool collides with an in-tree backend.
    /// Required for plugins that legitimately replace claude / codex / gemini
    /// / opencode / oai-runner dispatch.
    pub(crate) allow_shadow_builtin: bool,
    /// Owners to pre-trust before this install (TOFU). Appended to
    /// `~/.animus/trusted-orgs.yaml` after a successful install.
    pub(crate) allow_org: Vec<String>,
    /// Auto-confirm the TOFU prompt for unknown orgs.
    pub(crate) yes: bool,
    /// Project root for lockfile + audit-log resolution. `None` falls back to
    /// the global `~/.animus/plugins.lock` and skips audit logging.
    pub(crate) project_root: Option<String>,
    /// When `true`, a corrupt or incompatible `.animus/plugins.lock` is
    /// discarded and replaced with a fresh in-memory lockfile (with a
    /// `warn!` log noting integrity history was reset). When `false`
    /// (the default), the install **fails closed** with an actionable
    /// error pointing at the corrupt path. This is the audit-boundary
    /// equivalent of `--force`: it lets operators recover from a
    /// genuinely broken file while refusing to silently paper over what
    /// could be tamper.
    pub(crate) force_rewrite_lockfile: bool,
    /// Operator-supplied override for the user-facing `installed_kind`
    /// recorded in `plugins.lock`. Used by `animus plugin install --as-kind`
    /// to give a second `subject_kind:task` plugin a distinct dispatch
    /// prefix (e.g. `archive`). When `None`, the install pipeline picks
    /// the native kind from the manifest and auto-increments
    /// (`task -> task-2 -> task-3`) when the native value collides with
    /// an existing install. Provider plugins follow the same logic against
    /// their `provider_tool` capability.
    pub(crate) as_kind: Option<String>,
    /// When `true`, install into the project-local plugin root instead of
    /// the global one: binary -> `<project_root>/.animus/plugins/`,
    /// registry -> `<project_root>/.animus/plugins.yaml`, lockfile ->
    /// `<project_root>/.animus/plugins.lock`. Requires `project_root` and
    /// is mutually exclusive with `plugin_dir` (the CLI enforces the
    /// conflict via clap; this pipeline re-validates for non-CLI callers).
    pub(crate) project: bool,
    /// Internal: for RELEASE sources, the expected sha256 of the downloaded
    /// **tarball** (not the extracted binary). When set, `resolve_release_install`
    /// hard-fails before extracting if the tarball drifts from it. Used by
    /// `--locked` to gate a release reinstall against the lockfile's portable
    /// per-target `archive_sha256` (the `req.sha256` field gates the extracted
    /// BINARY, which differs from the tarball hash). `None` for normal installs.
    pub(crate) expected_archive_sha256: Option<String>,
    /// Internal: for `--locked` RELEASE reinstalls of multi-binary plugins,
    /// the lockfile's expected per-SECONDARY tarball sha for the current target
    /// (`secondary binary name -> archive sha256`). The multi-binary install
    /// loop verifies each downloaded secondary tarball against this BEFORE
    /// extracting, so a drifted secondary asset fails the reproducibility gate
    /// even when the release's own SHA256SUMS was regenerated to match. Empty
    /// for normal installs.
    pub(crate) locked_secondary_archive_shas: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginUninstallRequest {
    pub(crate) name: String,
    pub(crate) plugin_dir: Option<String>,
    /// Project root for lockfile + audit-log resolution.
    pub(crate) project_root: Option<String>,
    /// When `true`, uninstall from the project-local plugin root (binary,
    /// registry, and lockfile under `<project_root>/.animus/`) instead of
    /// the global one. Mutually exclusive with `plugin_dir`.
    pub(crate) project: bool,
}

pub(crate) async fn handle_plugin(command: PluginCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        PluginCommand::List(args) => handle_plugin_list(args, project_root, json).await,
        PluginCommand::Info(args) => handle_plugin_info(args, project_root, json).await,
        PluginCommand::Call(args) => handle_plugin_call(args, project_root, json).await,
        PluginCommand::Ping(args) => handle_plugin_ping(args, project_root, json).await,
        // Install and uninstall stay strictly local — they have heavy
        // filesystem side-effects (binary copy, registry yaml write,
        // signature checks) that don't benefit from a wire round-trip.
        // The daemon-side `plugin/install` handler exists so MCP/WebAPI
        // (C7/C8) can call it via the wire, but the CLI's user-facing
        // path is intentionally direct.
        PluginCommand::Install(args) => handle_plugin_install(args, project_root, json).await,
        PluginCommand::Uninstall(args) => handle_plugin_uninstall(args, project_root, json),
        PluginCommand::Prune(args) => handle_plugin_prune(args, project_root, json),
        PluginCommand::New(args) => new::handle_plugin_new(args, json),
        PluginCommand::Scaffold(cmd) => match cmd {
            PluginScaffoldCommand::Trigger(mut args) => {
                args.json = args.json || json;
                scaffold::handle_plugin_scaffold_trigger(args)
            }
        },
        PluginCommand::Search(args) => marketplace::handle_plugin_search(args).await,
        PluginCommand::Browse(args) => marketplace::handle_plugin_browse(args).await,
        PluginCommand::Update(args) => marketplace::handle_plugin_update(args, project_root, json).await,
        PluginCommand::Outdated(args) => marketplace::handle_plugin_outdated(args, project_root, json).await,
        PluginCommand::InstallDefaults(mut args) => {
            args.json = args.json || json;
            handle_plugin_install_defaults(args, project_root).await
        }
        PluginCommand::Lock(cmd) => handle_plugin_lock(cmd, project_root).await,
        PluginCommand::Doctor(args) => handle_plugin_doctor(args, project_root, json).await,
        PluginCommand::Rename(args) => handle_plugin_rename(args, project_root, json),
        PluginCommand::Status(mut args) => {
            args.json = args.json || json;
            status::handle_plugin_status(args, project_root).await
        }
        PluginCommand::Cache(cmd) => handle_plugin_cache(cmd, json),
        PluginCommand::Scope(cmd) => match cmd {
            PluginScopeCommand::Show(mut args) => {
                args.json = args.json || json;
                scope::handle_plugin_scope_show(args, project_root).await
            }
            PluginScopeCommand::Set(mut args) => {
                args.json = args.json || json;
                scope::handle_plugin_scope_set(args, project_root).await
            }
            PluginScopeCommand::Reset(mut args) => {
                args.json = args.json || json;
                scope::handle_plugin_scope_reset(args, project_root).await
            }
        },
        PluginCommand::Trust(cmd) => match cmd {
            PluginTrustCommand::List(mut args) => {
                args.json = args.json || json;
                handle_plugin_trust_list(args)
            }
        },
        PluginCommand::RevokeTrust(mut args) => {
            args.json = args.json || json;
            handle_plugin_revoke_trust(args, project_root)
        }
    }
}

/// `animus plugin trust list` — render the TOFU org allowlist (current +
/// revoked tombstones) with timestamps and how each grant was decided.
fn handle_plugin_trust_list(args: PluginTrustListArgs) -> Result<()> {
    let config = load_trusted_orgs()?;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    // Built-in orgs always show first as permanent trust anchors.
    for builtin in BUILTIN_TRUSTED_ORGS {
        rows.push(serde_json::json!({
            "org": builtin,
            "state": "trusted",
            "decided_by": TrustDecision::BuiltIn.as_str(),
            "trusted_at": serde_json::Value::Null,
            "revoked_at": serde_json::Value::Null,
            "first_plugin": serde_json::Value::Null,
            "builtin": true,
        }));
    }
    for record in config.records() {
        rows.push(serde_json::json!({
            "org": record.org,
            "state": if record.is_active() { "trusted" } else { "revoked" },
            "decided_by": record.decided_by.map(|d| d.as_str().to_string()),
            "trusted_at": record.trusted_at,
            "revoked_at": record.revoked_at,
            "first_plugin": record.first_plugin,
            "builtin": false,
        }));
    }

    if args.json {
        return print_value(
            serde_json::json!({ "trusted_orgs": rows, "path": trusted_orgs_path().display().to_string() }),
            true,
        );
    }

    if rows.is_empty() {
        println!("No trusted orgs recorded.");
        return Ok(());
    }
    println!("{:<28} {:<9} {:<18} {:<25} REVOKED-AT", "ORG", "STATE", "DECIDED-BY", "TRUSTED-AT");
    for row in &rows {
        let org = row["org"].as_str().unwrap_or("");
        let state = row["state"].as_str().unwrap_or("");
        let decided = row["decided_by"].as_str().unwrap_or("-");
        let trusted_at = row["trusted_at"].as_str().unwrap_or("-");
        let revoked_at = row["revoked_at"].as_str().unwrap_or("-");
        println!("{org:<28} {state:<9} {decided:<18} {trusted_at:<25} {revoked_at}");
    }
    println!("\ntrusted-orgs.yaml: {}", trusted_orgs_path().display());
    Ok(())
}

/// `animus plugin revoke-trust <ORG>` — tombstone the org's trust grant.
fn handle_plugin_revoke_trust(args: PluginRevokeTrustArgs, project_root: &str) -> Result<()> {
    let record = revoke_trusted_org(&args.org)?;
    if let Some(scoped) = protocol::repository_scope::scoped_state_root(std::path::Path::new(project_root)) {
        let audit = Audit::at_scoped_root(&scoped);
        audit.log_event(AuditEvent::new(
            AuditActor::User,
            AuditEventKind::TrustOrgRevoked,
            serde_json::json!({
                "org": record.org,
                "revoked_at": record.revoked_at,
                "previously_trusted_at": record.trusted_at,
                "decided_by": record.decided_by.map(|d| d.as_str().to_string()),
            }),
        ));
    }
    if args.json {
        return print_value(
            serde_json::json!({
                "revoked": record.org,
                "revoked_at": record.revoked_at,
                "previously_trusted_at": record.trusted_at,
                "decided_by": record.decided_by.map(|d| d.as_str().to_string()),
                "path": trusted_orgs_path().display().to_string(),
            }),
            true,
        );
    } else {
        println!(
            "Revoked trust for '{}'. A tombstone (revoked_at={}) was recorded in {}.",
            record.org,
            record.revoked_at.as_deref().unwrap_or("-"),
            trusted_orgs_path().display()
        );
        println!("Future installs from '{}' will re-prompt for trust.", record.org);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct PluginCacheClearOutput {
    kind: &'static str,
    root: String,
    removed: usize,
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct PluginCacheListEntry {
    sha256: String,
    size_bytes: u64,
    mtime: Option<u64>,
    path: String,
}

#[derive(Debug, Serialize)]
struct PluginCacheListOutput {
    kind: &'static str,
    root: String,
    enabled: bool,
    entries: Vec<PluginCacheListEntry>,
}

fn handle_plugin_cache(cmd: crate::cli_types::PluginCacheCommand, json: bool) -> Result<()> {
    use crate::cli_types::PluginCacheCommand;
    use orchestrator_plugin_host::ManifestCache;
    let cache = ManifestCache::from_default();
    match cmd {
        PluginCacheCommand::Clear(args) => {
            let emit_json = args.json || json;
            let root = cache.root().display().to_string();
            let removed = cache.clear().with_context(|| format!("failed to clear manifest cache at {root}"))?;
            let payload = PluginCacheClearOutput {
                kind: "plugin.cache.clear",
                root: root.clone(),
                removed,
                enabled: cache.is_enabled(),
            };
            if emit_json {
                return print_value(payload, true);
            }
            println!("Cleared {removed} cached manifest entr{} from {root}.", if removed == 1 { "y" } else { "ies" });
            if !cache.is_enabled() {
                println!("Note: cache is disabled (ANIMUS_DISABLE_MANIFEST_CACHE=1).");
            }
            Ok(())
        }
        PluginCacheCommand::List(args) => {
            let emit_json = args.json || json;
            let root = cache.root().display().to_string();
            let entries = cache.list().with_context(|| format!("failed to list manifest cache at {root}"))?;
            let rendered: Vec<PluginCacheListEntry> = entries
                .iter()
                .map(|e| PluginCacheListEntry {
                    sha256: e.sha256.clone(),
                    size_bytes: e.size_bytes,
                    mtime: e.mtime.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())),
                    path: e.path.display().to_string(),
                })
                .collect();
            let payload = PluginCacheListOutput {
                kind: "plugin.cache.list",
                root: root.clone(),
                enabled: cache.is_enabled(),
                entries: rendered,
            };
            if emit_json {
                return print_value(payload, true);
            }
            println!("Cache root: {root}");
            println!("Enabled: {}", if cache.is_enabled() { "yes" } else { "no" });
            if payload.entries.is_empty() {
                println!("No cached manifests.");
            } else {
                println!("Entries ({}):", payload.entries.len());
                for entry in &payload.entries {
                    println!("  {} ({} bytes)", entry.sha256, entry.size_bytes);
                }
            }
            Ok(())
        }
    }
}

/// Default plugin tables installed by `animus plugin install-defaults`.
///
/// These re-exports point at `orchestrator_core::plugin_registry` so the
/// daemon preflight (`PluginPreflightSpec::daemon_default`) and this CLI
/// command resolve identical `(owner/repo, tag)` pairs. Bump tags in
/// `crates/orchestrator-core/src/plugin_registry.rs`, not here.
use orchestrator_core::plugin_registry::{
    DEFAULT_CONFIG_SOURCE_PLUGINS, DEFAULT_OAI_AGENT_PLUGINS as DEFAULT_OAI_AGENT_PLUGIN, DEFAULT_PROVIDER_PLUGINS,
    DEFAULT_QUEUE_PLUGINS, DEFAULT_SUBJECT_PLUGINS, DEFAULT_TRANSPORT_PLUGINS, DEFAULT_WORKFLOW_RUNNER_PLUGINS,
};

#[derive(Debug, Serialize)]
struct InstallDefaultsEntry {
    repo: String,
    tag: String,
    status: &'static str,
    installed_path: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct InstallDefaultsSummary {
    installed: usize,
    skipped: usize,
    failed: usize,
}

#[derive(Debug, Serialize)]
struct InstallDefaultsOutput {
    results: Vec<InstallDefaultsEntry>,
    summary: InstallDefaultsSummary,
}

/// Assemble the `(slug, tag)` install list from the flavor manifest named
/// by `--flavor` (default: `default`). The manifest is the source of
/// truth for *which* slugs to install: everything marked `required`
/// always installs; `--include-recommended` adds the full `recommended`
/// set; the legacy `--include-subjects` / `--include-transports` flags
/// add the recommended slice of just those sections (back-compat). Tags
/// still come from the curated constants in
/// `orchestrator_core::plugin_registry`. Slugs the manifest declares that
/// the curated registry hasn't pinned yet (e.g. `animus-provider-ollama`,
/// `animus-trigger-cron`) emit a warning and are skipped: the manifest is
/// a forward-looking declaration; the constants table is the
/// authoritative pin.
///
/// The `default` flavor always resolves (binary-bundled manifest
/// fallback). Other flavor names must exist on disk as
/// `flavors/<name>.toml` or the command errors. Only when the default
/// manifest is present but unreadable do the legacy hardcoded
/// `DEFAULT_PROVIDER_PLUGINS + ...` tables kick in, with an error log.
fn build_install_defaults_targets(
    args: &PluginInstallDefaultsArgs,
    project_root: &str,
) -> Result<Vec<(String, String)>> {
    use orchestrator_core::flavor::load_flavor_in;
    use orchestrator_core::resolve_tag_for_slug;
    use orchestrator_core::DEFAULT_FLAVOR_ID;

    let project_root_path = std::path::Path::new(project_root);
    let manifest = match load_flavor_in(project_root_path, &args.flavor) {
        Ok(Some(manifest)) => Some(manifest),
        Ok(None) => {
            anyhow::bail!(
                "flavor '{}' not found; expected a manifest at <repo>/flavors/{}.toml (or under $ANIMUS_FLAVORS_DIR)",
                args.flavor,
                args.flavor
            );
        }
        Err(error) if args.flavor == DEFAULT_FLAVOR_ID => {
            // Codex P2: when `flavors/default.toml` is present but fails to
            // parse or validate, silently falling back to the hardcoded
            // tables would hide schema drift or a bad `$ANIMUS_FLAVORS_DIR`
            // override. Surface the load error via `tracing::error!` then
            // still fall back so the install path stays usable.
            tracing::error!(
                flavor = DEFAULT_FLAVOR_ID,
                error = %error,
                "flavor manifest present on disk but failed to load; falling back to hardcoded plugin defaults"
            );
            None
        }
        Err(error) => {
            return Err(error.context(format!("failed to load flavor manifest '{}'", args.flavor)));
        }
    };

    if let Some(manifest) = manifest {
        // The selected flavor is persisted to `.animus/plugin-scope.yaml`
        // on a successful install (see `handle_plugin_install_defaults`),
        // and the daemon + CLI scope resolvers read it back via
        // `active_flavor_id_in`, so a non-default flavor's plugins are
        // admitted by scoped discovery rather than filtered out.
        //
        // The manifest's REQUIRED set is the canonical install plan — the
        // same set `animus flavor current` reports drift against.
        let mut slugs: Vec<String> = manifest.required_plugins().into_iter().map(|(_, slug)| slug).collect();
        if args.include_recommended {
            slugs.extend(manifest.recommended_plugins().into_iter().map(|(_, slug)| slug));
        }
        if args.include_subjects {
            slugs.extend(manifest.subjects.recommended.iter().cloned());
        }
        if args.include_transports {
            slugs.extend(manifest.transports.recommended.iter().cloned());
            slugs.extend(manifest.ui.recommended.iter().cloned());
        }
        if args.include_oai_agent {
            for (slug, _) in DEFAULT_OAI_AGENT_PLUGIN {
                slugs.push((*slug).to_string());
            }
        }
        let mut targets: Vec<(String, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for slug in slugs {
            if !seen.insert(slug.clone()) {
                continue;
            }
            match resolve_tag_for_slug(&slug) {
                Some(tag) => targets.push((slug, tag.to_string())),
                None => {
                    tracing::warn!(
                        slug = %slug,
                        "flavor manifest references plugin slug with no curated tag pin; skipping install. Add a pin in orchestrator-core::plugin_registry to enable installation."
                    );
                }
            }
        }
        return Ok(targets);
    }

    let mut targets: Vec<(String, String)> =
        DEFAULT_PROVIDER_PLUGINS.iter().map(|(s, t)| ((*s).to_string(), (*t).to_string())).collect();
    // v0.5: workflow_runner + queue plugins are required by daemon preflight
    // and ship as part of the curated default flavor. Install unconditionally
    // so `animus plugin install-defaults` actually unblocks `animus daemon
    // start` on the broken-manifest fallback path.
    for (s, t) in DEFAULT_WORKFLOW_RUNNER_PLUGINS {
        targets.push(((*s).to_string(), (*t).to_string()));
    }
    for (s, t) in DEFAULT_QUEUE_PLUGINS {
        targets.push(((*s).to_string(), (*t).to_string()));
    }
    // v0.6: config_source is required by daemon preflight — the kernel sources
    // its workflow/agent config from this plugin. Install unconditionally on the
    // broken-manifest fallback path so `install-defaults` unblocks `daemon start`.
    for (s, t) in DEFAULT_CONFIG_SOURCE_PLUGINS {
        targets.push(((*s).to_string(), (*t).to_string()));
    }
    if args.include_oai_agent {
        for (s, t) in DEFAULT_OAI_AGENT_PLUGIN {
            targets.push(((*s).to_string(), (*t).to_string()));
        }
    }
    if args.include_subjects || args.include_recommended {
        for (s, t) in DEFAULT_SUBJECT_PLUGINS {
            targets.push(((*s).to_string(), (*t).to_string()));
        }
    }
    if args.include_transports || args.include_recommended {
        for (s, t) in DEFAULT_TRANSPORT_PLUGINS {
            targets.push(((*s).to_string(), (*t).to_string()));
        }
    }
    Ok(targets)
}

async fn handle_plugin_install_defaults(args: PluginInstallDefaultsArgs, project_root: &str) -> Result<()> {
    let mut targets: Vec<(String, String)> = build_install_defaults_targets(&args, project_root)?;

    let install_dir = install_root(args.plugin_dir.as_deref())?;

    // ---- Batch-level lockfile pre-check ----
    //
    // Per codex review of the v0.4.13 P2 fix: the per-target loop below skips
    // already-installed defaults BEFORE constructing a `PluginInstallRequest`,
    // so a corrupt lockfile would otherwise let an all-skipped run report
    // success despite the documented fail-closed policy. Validate once here,
    // up front, so the install-defaults surface is fail-closed even when no
    // actual install work would have happened.
    //
    // When `--force-rewrite-lockfile` IS set and the lockfile is corrupt, we
    // must also persist the fresh empty lockfile to disk now — otherwise an
    // all-skipped run (every default already installed) would discard the
    // corrupt bytes in memory but leave them on disk, so the documented
    // remediation would silently fail and the next install would refuse
    // again. Saving here guarantees the user-visible remediation actually
    // happens.
    {
        let project_root_path = std::path::PathBuf::from(project_root);
        let project_root_for_lock: Option<&std::path::Path> = Some(&project_root_path);
        let lock_existed = PluginLockfile::default_path(project_root_for_lock).exists();
        let lock_parsed_clean = PluginLockfile::load_default(project_root_for_lock).is_ok();
        let mut lock = load_or_refuse_lockfile(project_root_for_lock, None, args.force_rewrite_lockfile)?;
        if args.force_rewrite_lockfile && lock_existed && !lock_parsed_clean {
            // Persist the freshly emptied lockfile so a no-op (all-skipped)
            // batch still completes the remediation. The per-install
            // pipeline below would otherwise leave the corrupt bytes in
            // place until the next non-skipped install.
            lock.save().with_context(|| format!("failed to rewrite plugin lockfile at {}", lock.path().display()))?;
            tracing::warn!(
                lockfile = %lock.path().display(),
                "SECURITY: install-defaults --force-rewrite-lockfile rewrote a corrupt lockfile to a fresh empty state",
            );
        }
        // Global-scope installs fail closed on a corrupt
        // `~/.animus/plugins.lock` too (see run_plugin_install's preflight),
        // so the batch pre-check must validate (and, with the flag,
        // rewrite) that file as well — otherwise an all-skipped run inside
        // an initialized project would mask the corruption (codex P2).
        let global_lock_path = global_lockfile_path();
        if global_lock_path != PluginLockfile::default_path(project_root_for_lock) {
            let global_existed = global_lock_path.exists();
            let global_parsed_clean = PluginLockfile::load_or_empty(&global_lock_path).is_ok();
            let mut global_lock =
                load_or_refuse_lockfile(project_root_for_lock, Some(&global_lock_path), args.force_rewrite_lockfile)?;
            if args.force_rewrite_lockfile && global_existed && !global_parsed_clean {
                global_lock
                    .save()
                    .with_context(|| format!("failed to rewrite plugin lockfile at {}", global_lock_path.display()))?;
                tracing::warn!(
                    lockfile = %global_lock_path.display(),
                    "SECURITY: install-defaults --force-rewrite-lockfile rewrote a corrupt global lockfile to a fresh empty state",
                );
            }
        }
    }

    let mut results: Vec<InstallDefaultsEntry> = Vec::with_capacity(targets.len());
    let mut installed = 0_usize;
    let mut skipped = 0_usize;
    let mut failed = 0_usize;

    for (slug, tag) in targets.drain(..) {
        let repo_basename = slug.rsplit('/').next().unwrap_or(&slug).to_string();
        let pre_existing = install_dir.join(&repo_basename);
        if pre_existing.exists() && !args.force {
            if !args.json {
                eprintln!("[skip] {slug}@{tag} (already installed at {})", pre_existing.display());
            }
            skipped += 1;
            results.push(InstallDefaultsEntry {
                repo: slug.clone(),
                tag: tag.clone(),
                status: "skipped",
                installed_path: Some(pre_existing.display().to_string()),
                message: Some("already installed".to_string()),
            });
            continue;
        }

        if !args.json {
            eprintln!("[install] {slug}@{tag} ...");
        }

        // Curated launchapp-dev provider repos (e.g. animus-provider-claude)
        // intentionally claim the reserved in-tree provider_tool names. After
        // v0.4.12 deleted the in-tree providers, this curated registry is the
        // only sanctioned path to install those names, so bypass the
        // reserved-name guard here. User-typed `animus plugin install ...`
        // still has to pass --allow-shadow-builtin explicitly.
        let req = PluginInstallRequest {
            source: Some(slug.clone()),
            tag: Some(tag.clone()),
            force: args.force,
            plugin_dir: args.plugin_dir.clone(),
            allow_org: vec!["launchapp-dev".to_string()],
            yes: args.yes,
            allow_shadow_builtin: true,
            project_root: Some(project_root.to_string()),
            force_rewrite_lockfile: args.force_rewrite_lockfile,
            as_kind: None,
            ..Default::default()
        };

        match run_plugin_install(req).await {
            Ok(output) => {
                if !args.json {
                    eprintln!("[ok]   {slug}@{tag} -> {}", output.installed_path);
                }
                installed += 1;
                results.push(InstallDefaultsEntry {
                    repo: slug,
                    tag,
                    status: "installed",
                    installed_path: Some(output.installed_path),
                    message: None,
                });
            }
            Err(err) => {
                if !args.json {
                    eprintln!("[fail] {slug}@{tag}: {err}");
                }
                failed += 1;
                results.push(InstallDefaultsEntry {
                    repo: slug,
                    tag,
                    status: "failed",
                    installed_path: None,
                    message: Some(err.to_string()),
                });
            }
        }
    }

    if !args.json {
        eprintln!("[summary] installed={installed} skipped={skipped} failed={failed}");
    }

    // Emit the JSON/text envelope unconditionally so operators see the
    // per-plugin result table even when one or more installs failed.
    let failed_specs: Vec<String> = results
        .iter()
        .filter(|entry| entry.status == "failed")
        .map(|entry| format!("{}@{}", entry.repo, entry.tag))
        .collect();

    print_value(
        InstallDefaultsOutput { results, summary: InstallDefaultsSummary { installed, skipped, failed } },
        args.json,
    )?;

    // Persist the active flavor selection so the daemon + CLI scope
    // resolvers admit THIS flavor's plugins (not always
    // `flavors/default.toml`). Only on a clean run (no install failures):
    // recording a selection whose plugins failed to install would scope
    // discovery to a set the operator does not actually have. Best-effort
    // — a scope-file write failure must not fail the install that already
    // succeeded; surface it as a warning.
    if failed == 0 {
        if let Err(err) = scope::persist_active_flavor(std::path::Path::new(project_root), &args.flavor) {
            tracing::warn!(
                flavor = %args.flavor,
                error = %format!("{err:#}"),
                "installed flavor plugins but failed to persist the active flavor selection to plugin-scope.yaml"
            );
        }
    }

    // Codex round-6 P2: partial-failure must propagate as a non-zero exit
    // code so installer scripts and CI jobs notice. Previously the
    // function always returned `Ok(())` and `failed` was only visible in
    // the JSON envelope.
    if failed > 0 {
        return Err(anyhow!(
            "animus plugin install-defaults completed with {failed} failure(s); failed plugins: {}",
            failed_specs.join(", ")
        ));
    }

    Ok(())
}

// ===== Reusable typed entry points (shared between CLI and MCP) =====

/// List discovered plugins. Identical surface as `animus plugin list`.
pub(crate) fn run_plugin_list(req: PluginListRequest) -> Result<PluginListOutput> {
    let (discovered, warnings) = discover_with_warnings(&req.project_root, req.include_system_path)?;
    let rows: Vec<DiscoveredPluginRow> = discovered
        .into_iter()
        .map(|plugin| DiscoveredPluginRow {
            name: plugin.name,
            version: plugin.manifest.version,
            plugin_kind: plugin.manifest.plugin_kind,
            description: plugin.manifest.description,
            protocol_version: plugin.manifest.protocol_version,
            capabilities: plugin.manifest.capabilities,
            source: source_label(plugin.source),
            scope: scope_label(plugin.source),
            path: plugin.path.display().to_string(),
        })
        .collect();
    let warning_rows: Vec<PluginWarningRow> = warnings
        .into_iter()
        .map(|warning| PluginWarningRow {
            name: warning.name,
            source: source_label(warning.source),
            path: warning.path.display().to_string(),
            reason: warning.reason,
        })
        .collect();

    // Surface global installs hidden by a same-named project-local install.
    // Discovery dedupes by name with the project tier winning, so the global
    // binary silently disappears from `plugins` — make the shadowing visible
    // instead of leaving operators to wonder where the global copy went.
    // Two probe locations per name: the resolved global install dir, and the
    // absolute binary path recorded in the global registry (covers global
    // installs placed elsewhere via `--plugin-dir`).
    let global_dir = plugin_install_dir();
    let registry_index = marketplace::read_installed_index().unwrap_or_default();
    let shadowed: Vec<ShadowedPluginRow> = rows
        .iter()
        .filter(|row| row.scope == "project")
        .filter_map(|row| {
            let registry_candidate = registry_index
                .get(&row.name)
                .and_then(|entry| entry.binary.as_deref())
                .map(PathBuf::from)
                .filter(|path| path.is_file());
            let global_candidate = Some(global_dir.join(&row.name)).filter(|path| path.is_file());
            let hidden = global_candidate.or(registry_candidate)?;
            if hidden.display().to_string() == row.path {
                return None;
            }
            Some(ShadowedPluginRow {
                name: row.name.clone(),
                path: hidden.display().to_string(),
                shadowed_by: row.path.clone(),
                note: "shadowed by project install",
            })
        })
        .collect();

    Ok(PluginListOutput { plugins: rows, warnings: warning_rows, shadowed })
}

fn spawn_options_for_discovered(plugin: &DiscoveredPlugin) -> PluginSpawnOptions {
    PluginSpawnOptions::for_manifest(
        plugin.name.clone(),
        &plugin.manifest.env_required,
        std::iter::empty::<String>(),
        None,
    )
    .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
}

/// Spawn the named plugin, complete the handshake, and return manifest +
/// initialize-time capabilities.
pub(crate) async fn run_plugin_info(req: PluginInfoRequest) -> Result<PluginInfoOutput> {
    let discovered = find_plugin(&req.project_root, &req.name, req.include_system_path)?;
    let options = spawn_options_for_discovered(&discovered);
    let host =
        PluginHost::spawn_with_options(&discovered.path, &[], options).await.context("failed to spawn plugin")?;
    let initialize = host.handshake().await.context("plugin initialize failed")?;
    let _ = host.shutdown().await;
    let skip_flag = registered_skip_manifest_check_at_install_scoped(
        Some(std::path::Path::new(&req.project_root)),
        &discovered.name,
    );
    Ok(PluginInfoOutput {
        name: discovered.name,
        source: source_label(discovered.source),
        path: discovered.path.display().to_string(),
        manifest: discovered.manifest,
        initialize: serde_json::to_value(initialize)?,
        skip_manifest_check_at_install: skip_flag,
    })
}

/// Health-check a plugin by spawning it, completing the handshake, and
/// dispatching `$/ping`.
pub(crate) async fn run_plugin_ping(req: PluginPingRequest) -> Result<PluginPingOutput> {
    let discovered = find_plugin(&req.project_root, &req.name, req.include_system_path)?;
    let options = spawn_options_for_discovered(&discovered);
    let host =
        PluginHost::spawn_with_options(&discovered.path, &[], options).await.context("failed to spawn plugin")?;
    let initialize = host.handshake().await.context("plugin initialize failed")?;
    host.ping().await.context("plugin ping failed")?;
    let _ = host.shutdown().await;
    Ok(PluginPingOutput { name: discovered.name, ok: true, plugin_info: serde_json::to_value(initialize.plugin_info)? })
}

/// Send a JSON-RPC request to a discovered plugin and return its response.
pub(crate) async fn run_plugin_call(req: PluginCallRequest) -> Result<PluginCallOutput> {
    let method = req.method.trim().to_string();
    if method.is_empty() {
        return Err(invalid_input_error("method must not be empty"));
    }
    let discovered = find_plugin(&req.project_root, &req.name, req.include_system_path)?;
    let options = spawn_options_for_discovered(&discovered);
    let host =
        PluginHost::spawn_with_options(&discovered.path, &[], options).await.context("failed to spawn plugin")?;
    let _ = host.handshake().await.context("plugin initialize failed")?;
    let result = host
        .request(method.clone(), req.params)
        .await
        .map_err(|err| anyhow!("plugin call failed ({}): {}", err.code, err.message))?;
    let _ = host.shutdown().await;
    Ok(PluginCallOutput { name: discovered.name, method, result })
}

/// Uninstall a plugin from the install dir and registry yaml.
pub(crate) fn run_plugin_uninstall(req: PluginUninstallRequest) -> Result<PluginUninstallOutput> {
    let plugin_name = req.name.trim().to_string();
    if plugin_name.is_empty() {
        return Err(invalid_input_error("name must not be empty"));
    }

    let scope_paths = resolve_install_scope(req.project, req.project_root.as_deref(), req.plugin_dir.as_deref())?;
    let yaml_path = scope_paths.registry_yaml.clone();
    let mut config = load_plugins_yaml(&yaml_path)?;
    let key = serde_yaml::Value::String(plugin_name.clone());
    let entry_for_binaries = config.plugins.get(&key).cloned().or_else(|| config.providers.get(&key).cloned());
    let removed_in_yaml = config.plugins.remove(&key).is_some() || config.providers.remove(&key).is_some();
    if removed_in_yaml {
        save_plugins_yaml(&yaml_path, &config)?;
    }

    let install_dir = scope_paths.install_dir.clone();
    let mut binary_names: Vec<String> = vec![plugin_name.clone()];
    if let Some(serde_yaml::Value::Mapping(entry_map)) = entry_for_binaries {
        let binaries_key = serde_yaml::Value::String("binaries".to_string());
        if let Some(serde_yaml::Value::Sequence(seq)) = entry_map.get(&binaries_key) {
            binary_names.clear();
            for v in seq {
                if let serde_yaml::Value::String(name) = v {
                    binary_names.push(name.clone());
                }
            }
            if binary_names.is_empty() {
                binary_names.push(plugin_name.clone());
            }
        }
    }

    let primary_install_path = install_dir.join(&plugin_name);
    let mut primary_removed: Option<String> = None;
    for name in &binary_names {
        let path = install_dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
            if path == primary_install_path {
                primary_removed = Some(path.to_string_lossy().to_string());
            }
        }
    }
    let removed = primary_removed;

    if !removed_in_yaml && removed.is_none() {
        return Err(not_found_error(format!("plugin '{plugin_name}' is not installed")));
    }

    // Remove the lockfile entries (best-effort; never blocks uninstall).
    let project_root_pb = req.project_root.as_deref().map(std::path::PathBuf::from);
    let project_root_for_lock: Option<&std::path::Path> = project_root_pb.as_deref();
    if scope_paths.scope == "project" {
        let project_lock_path = scope_paths
            .lockfile_override
            .clone()
            .unwrap_or_else(|| PluginLockfile::default_path(project_root_for_lock));
        if let Ok(mut lockfile) = PluginLockfile::load_or_empty(&project_lock_path) {
            let mut changed = false;
            for name in &binary_names {
                if lockfile.remove(name).is_some() {
                    changed = true;
                }
            }
            // A project-scoped uninstall can un-shadow a same-named GLOBAL
            // install whose lock entry (incl. installed_kind alias
            // metadata) lives in `~/.animus/plugins.lock` — a file project
            // runtime readers (`PluginLockfile::load_default`) never
            // consult. COPY (not migrate) the global entry into the project
            // lockfile so the re-exposed global plugin keeps its alias +
            // integrity claim while the global lockfile stays the
            // cross-project record (codex P2 rounds 10-12).
            // No-op unless a same-named GLOBAL lock entry actually exists:
            // `find` returns None for every name when there is nothing to
            // un-shadow (the common case — most project uninstalls have no
            // global twin), so `changed` stays as the project-removal set.
            if let Ok(global_lock) = PluginLockfile::load_or_empty(&global_lockfile_path()) {
                for name in &binary_names {
                    if let Some(global_entry) = global_lock.find(name) {
                        lockfile.upsert(global_entry.clone());
                        changed = true;
                    }
                }
            }
            if changed {
                if let Err(err) = lockfile.save() {
                    tracing::warn!(path = %lockfile.path().display(), %err, "failed to persist plugin lockfile after uninstall");
                }
            }
        }
    } else {
        // Global scope: split removals PER BINARY NAME. Names claimed by a
        // project-scoped install keep their project lock entry (it protects
        // the project binary); everything else is removed from both the
        // default lockfile (legacy-reader location) and the global mirror
        // (codex P2 rounds 14-15).
        let default_lock_path = PluginLockfile::default_path(project_root_for_lock);
        let global_lock_path = global_lockfile_path();
        let mut default_lock = PluginLockfile::load_or_empty(&default_lock_path).ok();
        let mut global_lock = if global_lock_path == default_lock_path {
            None
        } else {
            PluginLockfile::load_or_empty(&global_lock_path).ok()
        };
        let mut default_changed = false;
        let mut global_changed = false;
        for name in &binary_names {
            let project_claimed =
                project_root_for_lock.map(|root| project_scope_claims_name(root, name)).unwrap_or(false);
            if !project_claimed {
                if let Some(lock) = default_lock.as_mut() {
                    if lock.remove(name).is_some() {
                        default_changed = true;
                    }
                }
            }
            if let Some(lock) = global_lock.as_mut() {
                if lock.remove(name).is_some() {
                    global_changed = true;
                }
            }
        }
        if default_changed {
            if let Some(lock) = default_lock.as_mut() {
                if let Err(err) = lock.save() {
                    tracing::warn!(path = %lock.path().display(), %err, "failed to persist plugin lockfile after uninstall");
                }
            }
        }
        if global_changed {
            if let Some(lock) = global_lock.as_mut() {
                if let Err(err) = lock.save() {
                    tracing::warn!(path = %lock.path().display(), %err, "failed to persist global lockfile mirror after uninstall");
                }
            }
        }
    }

    // Audit log.
    if let Some(root) = project_root_for_lock {
        if let Some(scoped) = protocol::repository_scope::scoped_state_root(root) {
            Audit::at_scoped_root(&scoped).log_event(AuditEvent::new(
                AuditActor::User,
                AuditEventKind::PluginUninstall,
                serde_json::json!({
                    "plugin": plugin_name,
                    "removed_path": removed,
                }),
            ));
        }
    }

    Ok(PluginUninstallOutput {
        name: plugin_name,
        removed_path: removed,
        plugins_yaml: yaml_path.to_string_lossy().to_string(),
        scope: scope_paths.scope,
    })
}

/// Whether a `plugins.yaml` entry's binary still resolves on disk, using the
/// same resolution discovery applies (`~/` expansion, path resolution, `$PATH`
/// lookup). The entry's recorded `binary:` field is authoritative when present;
/// otherwise the conventional `<install_dir>/<name>` is checked.
fn entry_binary_resolves(entry: &serde_yaml::Value, name: &str, install_dir: &Path) -> bool {
    if let Some(binary) = entry
        .as_mapping()
        .and_then(|m| m.get(serde_yaml::Value::String("binary".to_string())).and_then(serde_yaml::Value::as_str))
    {
        let trimmed = binary.trim();
        if !trimmed.is_empty() {
            return orchestrator_plugin_host::resolve_configured_binary(trimmed).is_some();
        }
    }
    install_dir.join(name).is_file()
}

/// Collect the names of `plugins.yaml` entries whose binary is gone for one
/// registry scope, removing them from `config` when `apply` is set.
fn collect_stale_entries(
    config: &mut PluginsYamlConfig,
    install_dir: &Path,
    scope: &'static str,
    yaml_path: &Path,
    apply: bool,
    out: &mut Vec<PluginPruneEntry>,
) -> bool {
    let mut removed_any = false;
    for table in [&mut config.plugins, &mut config.providers] {
        let stale_keys: Vec<serde_yaml::Value> = table
            .iter()
            .filter_map(|(key, value)| {
                let name = key.as_str()?;
                if entry_binary_resolves(value, name, install_dir) {
                    None
                } else {
                    Some(key.clone())
                }
            })
            .collect();
        for key in stale_keys {
            if let Some(name) = key.as_str() {
                out.push(PluginPruneEntry {
                    name: name.to_string(),
                    scope,
                    plugins_yaml: yaml_path.to_string_lossy().to_string(),
                });
            }
            if apply {
                table.remove(&key);
                removed_any = true;
            }
        }
    }
    removed_any
}

/// Resolve the global `plugins.yaml` path WITHOUT side effects. Unlike
/// [`plugins_yaml_path`], this never creates the config dir or migrates the
/// legacy registry — a read-only preview (`prune` without `--yes`) must write
/// nothing. Prefers the canonical path when it exists, else the legacy one,
/// else the canonical path as the nominal location for an empty scan.
fn plugins_yaml_path_readonly() -> PathBuf {
    let canonical = plugins_registry_path();
    if canonical.exists() {
        return canonical;
    }
    let legacy = legacy_plugins_registry_path();
    if legacy.exists() {
        return legacy;
    }
    canonical
}

/// Remove the supplied names from the lockfile at `lock_path` so a pruned
/// stale entry does not linger as a `missing_binary` record in `lock verify`.
/// Best-effort: a missing/unparseable lockfile is ignored.
fn prune_lock_entries(lock_path: &Path, names: &[String]) {
    if names.is_empty() {
        return;
    }
    if let Ok(mut lockfile) = PluginLockfile::load_or_empty(lock_path) {
        let mut changed = false;
        for name in names {
            if lockfile.remove(name).is_some() {
                changed = true;
            }
        }
        if changed {
            if let Err(err) = lockfile.save() {
                tracing::warn!(path = %lockfile.path().display(), %err, "failed to persist lockfile after prune");
            }
        }
    }
}

/// Remove `plugins.yaml` entries whose binaries are gone (stale registry
/// entries left behind by an out-of-band binary deletion). Scans both the
/// global registry and — when a project root resolves — the project-local one.
/// With `apply = false` it previews the stale set without writing. When
/// applying, matching lockfile records are removed too so `lock verify` stays
/// consistent with the pruned registry (parity with `uninstall`).
///
// TODO(codex-p2): the lockfile cleanup here keys off the registry entry name
// for the single-binary common case. Two uninstall behaviors are not yet
// mirrored: (1) a plugins.yaml entry that registered MULTIPLE lock binaries
// only has its name-matched lock record pruned; (2) pruning a project entry
// does not re-copy a shadowed same-named GLOBAL lock entry's alias/integrity
// metadata back into the project lockfile. Both require the full uninstall
// mirror/shadow algorithm and are out of scope for stale-entry pruning.
pub(crate) fn run_plugin_prune(project_root: Option<&str>, apply: bool) -> Result<PluginPruneOutput> {
    let mut stale: Vec<PluginPruneEntry> = Vec::new();
    let project_root_pb = project_root.map(str::trim).filter(|r| !r.is_empty()).map(Path::new);

    let global_yaml = if apply { plugins_yaml_path()? } else { plugins_yaml_path_readonly() };
    let mut global_config = load_plugins_yaml(&global_yaml)?;
    let before = stale.len();
    let global_removed =
        collect_stale_entries(&mut global_config, &plugin_install_dir(), "global", &global_yaml, apply, &mut stale);
    if apply && global_removed {
        save_plugins_yaml(&global_yaml, &global_config)?;
        // Global registry entries map to global lockfile records. Prune the
        // global lockfile, and the default-path lockfile only when it IS the
        // global one (no project root) — inside a project `default_path`
        // resolves to the PROJECT lockfile, whose same-named entry belongs to a
        // project-scoped install and must not be touched by a global prune.
        let names: Vec<String> = stale[before..].iter().map(|e| e.name.clone()).collect();
        let global_lock_path = global_lockfile_path();
        prune_lock_entries(&global_lock_path, &names);
        let default_lock_path = PluginLockfile::default_path(project_root_pb);
        if default_lock_path == global_lock_path {
            prune_lock_entries(&default_lock_path, &names);
        }
    }

    if let Some(root) = project_root_pb {
        let project_yaml = project_plugins_registry_path(root);
        if project_yaml.exists() {
            let mut project_config = load_plugins_yaml(&project_yaml)?;
            let before = stale.len();
            let project_removed = collect_stale_entries(
                &mut project_config,
                &project_plugin_install_dir(root),
                "project",
                &project_yaml,
                apply,
                &mut stale,
            );
            if apply && project_removed {
                save_plugins_yaml(&project_yaml, &project_config)?;
                let names: Vec<String> = stale[before..].iter().map(|e| e.name.clone()).collect();
                prune_lock_entries(&PluginLockfile::default_path(Some(root)), &names);
            }
        }
    }

    let pruned = apply && !stale.is_empty();
    Ok(PluginPruneOutput { stale, pruned })
}

fn handle_plugin_prune(args: PluginPruneArgs, project_root: &str, json: bool) -> Result<()> {
    let want_json = json || args.json;
    let output = run_plugin_prune(Some(project_root), args.yes)?;
    if want_json {
        return print_value(output, true);
    }
    if output.stale.is_empty() {
        println!("no stale plugins.yaml entries found");
        return Ok(());
    }
    if output.pruned {
        println!("pruned {} stale plugins.yaml entr{}:", output.stale.len(), plural_y(output.stale.len()));
    } else {
        println!(
            "{} stale plugins.yaml entr{} (run `animus plugin prune --yes` to remove):",
            output.stale.len(),
            plural_y(output.stale.len())
        );
    }
    for entry in &output.stale {
        println!("  {} ({})", entry.name, entry.scope);
    }
    Ok(())
}

fn plural_y(count: usize) -> &'static str {
    if count == 1 {
        "y"
    } else {
        "ies"
    }
}

/// Load the plugin lockfile for `project_root`, refusing the install when the
/// file is unparseable / schema-incompatible **unless** `force_rewrite_lockfile`
/// is set. The fail-closed behavior is part of the tamper/audit boundary: an
/// unreadable lockfile MUST be surfaced before any source resolution, network
/// fetch, or manifest probe so a corrupted lock can't trigger network work or
/// candidate-binary execution as a side effect.
///
/// On `force_rewrite_lockfile = true`, the unreadable file is discarded with
/// a `warn!` and an empty in-memory lockfile is returned; the eventual `save()`
/// at the end of the install pipeline rewrites it from scratch.
fn load_or_refuse_lockfile(
    project_root: Option<&Path>,
    explicit_path: Option<&Path>,
    force_rewrite_lockfile: bool,
) -> Result<PluginLockfile> {
    let lock_path = explicit_path.map(Path::to_path_buf).unwrap_or_else(|| PluginLockfile::default_path(project_root));
    match PluginLockfile::load_or_empty(&lock_path) {
        Ok(lock) => Ok(lock),
        Err(err) => {
            if force_rewrite_lockfile {
                tracing::warn!(
                    lockfile = %lock_path.display(),
                    error = %err,
                    "SECURITY: --force-rewrite-lockfile discarded the existing plugin lockfile; \
                     integrity history was reset and prior sha256 entries are no longer recorded. \
                     Audit the install context before trusting subsequent verifications.",
                );
                Ok(PluginLockfile::empty_at(&lock_path))
            } else {
                let chain = err.chain().map(|cause| cause.to_string()).collect::<Vec<_>>().join(": ");
                Err(invalid_input_error(format!(
                    "plugin lockfile at {lockfile} is unreadable: {chain}. \
                     The install was REFUSED to preserve the integrity audit trail. \
                     Remediation: \
                     (1) restore {lockfile} from version control or a backup, or \
                     (2) re-run with --force-rewrite-lockfile to discard the file and \
                     start a fresh lockfile (SECURITY WARNING: this drops the recorded \
                     sha256 history, so subsequent --force installs will not detect \
                     pre-existing tamper). Inspect the file at {lockfile} before \
                     choosing option (2).",
                    lockfile = lock_path.display(),
                )))
            }
        }
    }
}

/// Install a plugin binary from a public GitHub release (`owner/repo[@tag]`),
/// a local path, or an HTTPS URL. Wired into both the CLI
/// (`handle_plugin_install`) and the MCP install tool.
pub(crate) async fn run_plugin_install(req: PluginInstallRequest) -> Result<PluginInstallOutput> {
    let provided = [req.source.is_some(), req.path.is_some(), req.url.is_some()].iter().filter(|b| **b).count();
    if provided == 0 {
        return Err(invalid_input_error("one of `source` (owner/repo[@tag]), `path`, or `url` must be provided"));
    }
    if provided > 1 {
        return Err(invalid_input_error("`source`, `path`, and `url` are mutually exclusive"));
    }
    if req.tag.is_some() && req.source.is_none() {
        return Err(invalid_input_error("`tag` only applies when installing from a public repo (`source`)"));
    }
    if req.url.is_some() && req.sha256.is_none() {
        return Err(invalid_input_error(
            "--sha256 is required when installing from a URL; compute via `shasum -a 256 <plugin>`",
        ));
    }
    if req.require_signature && req.skip_signature {
        return Err(invalid_input_error("--require-signature and --skip-signature are mutually exclusive"));
    }
    if let Some(policy) = req.signature_policy {
        if matches!(policy, PluginPolicyMode::Strict) && req.skip_signature {
            return Err(invalid_input_error("--signature-policy strict and --skip-signature are mutually exclusive"));
        }
        if matches!(policy, PluginPolicyMode::Disabled) && req.require_signature {
            return Err(invalid_input_error(
                "--signature-policy disabled and --require-signature are mutually exclusive",
            ));
        }
    }

    // ---- Lockfile pre-load (runs BEFORE any source resolution / network /
    // manifest probe) ----------------------------------------------------
    //
    // Per codex review of the v0.4.13 P2 fix: if the lockfile is corrupt we
    // must refuse the install before downloading anything or running the
    // candidate binary with `--manifest`. Otherwise an attacker who
    // corrupted the lock could still trigger network fetch and untrusted
    // process execution as a side effect of the refusal path. The
    // returned lockfile is discarded here — the integrity check is the
    // contract; we reload (or rewrite) the lockfile right before the
    // verify_installed/upsert step below so a concurrent install that
    // committed during source download / manifest probe is not silently
    // erased on save.
    let scope_paths = resolve_install_scope(req.project, req.project_root.as_deref(), req.plugin_dir.as_deref())?;
    let project_root_pb_pre = req.project_root.as_deref().map(std::path::PathBuf::from);
    let project_root_for_lock_pre: Option<&std::path::Path> = project_root_pb_pre.as_deref();
    let _ = load_or_refuse_lockfile(
        project_root_for_lock_pre,
        scope_paths.lockfile_override.as_deref(),
        req.force_rewrite_lockfile,
    )?;
    // Global-scope installs may be re-routed to `~/.animus/plugins.lock`
    // later (when the resolved plugin name turns out to be project-scope
    // installed — see `global_scope_lockfile_override`). The plugin name is
    // not known yet at this point, so validate the global lockfile too;
    // otherwise a corrupt global lock would only fail AFTER the source
    // download + `--manifest` probe, breaking the fail-closed invariant.
    if scope_paths.scope == "global" {
        let global_lock_path = global_lockfile_path();
        if global_lock_path != PluginLockfile::default_path(project_root_for_lock_pre) {
            let parsed_clean = PluginLockfile::load_or_empty(&global_lock_path).is_ok();
            let mut global_lock = load_or_refuse_lockfile(
                project_root_for_lock_pre,
                Some(&global_lock_path),
                req.force_rewrite_lockfile,
            )?;
            // Complete the --force-rewrite-lockfile remediation for the
            // prechecked global lock: the eventual install usually writes
            // the project-default lockfile, so without this save a corrupt
            // global lock would stay corrupt on disk and refuse the NEXT
            // install at this same precheck (codex P2).
            if req.force_rewrite_lockfile && global_lock_path.exists() && !parsed_clean {
                global_lock
                    .save()
                    .with_context(|| format!("failed to rewrite plugin lockfile at {}", global_lock_path.display()))?;
                tracing::warn!(
                    lockfile = %global_lock_path.display(),
                    "SECURITY: --force-rewrite-lockfile rewrote a corrupt global lockfile to a fresh empty state",
                );
            }
        }
    }

    // `_install_temp` keeps the install-staging directory alive for the
    // remainder of this function. It drops at the end (RAII) so the
    // tempdir is reliably cleaned up whether install succeeds, errors,
    // or returns early — closing the GBs-of-`animus-plugin-install-*`
    // accumulation the old `std::env::temp_dir().join(uuid)` left behind.
    let (source_path, default_name, provenance, _install_temp, release_assets_opt): (
        PathBuf,
        String,
        InstallProvenance,
        Option<tempfile::TempDir>,
        Option<Vec<GithubReleaseAsset>>,
    ) = if let Some(slug) = req.source.as_deref() {
        let spec = parse_repo_spec(slug)?;
        let release = resolve_release_install(spec, req.tag.clone(), req.expected_archive_sha256.clone()).await?;
        let provenance = InstallProvenance {
            source_kind: Some("release"),
            origin: Some(release.origin.clone()),
            release_tag: Some(release.release_tag.clone()),
            asset_name: Some(release.asset_name.clone()),
            sha256_verified: Some(release.sha256_verified),
            asset_archive_path: release.asset_archive_path.clone(),
            bundle_path: release.bundle_path.clone(),
            owner: Some(release.owner.clone()),
            repo: Some(release.repo.clone()),
            source_repo: Some(format!("{}/{}", release.owner, release.repo)),
            resolved_commit: release.resolved_commit.clone(),
            sha256sums_targets: release.sha256sums_targets.clone(),
            sha256sums_body: release.sha256sums_body.clone(),
        };
        let release_assets = release.release_assets.clone();
        (release.binary_path, release.plugin_name_hint, provenance, Some(release._temp_dir), Some(release_assets))
    } else if let Some(p) = req.path.as_deref() {
        (
            PathBuf::from(p),
            String::new(),
            InstallProvenance {
                source_kind: Some("path"),
                source_repo: Some(format!("path:{p}")),
                ..Default::default()
            },
            None,
            None,
        )
    } else if let Some(u) = req.url.as_deref() {
        let expected = req
            .sha256
            .as_deref()
            .ok_or_else(|| invalid_input_error("sha256 is required when installing from a URL"))?;
        let (path, temp_dir) = fetch_url_to_temp(u, expected).await?;
        let provenance = InstallProvenance {
            source_kind: Some("url"),
            origin: Some(u.to_string()),
            sha256_verified: Some(true),
            source_repo: Some(u.to_string()),
            ..Default::default()
        };
        (path, String::new(), provenance, Some(temp_dir), None)
    } else {
        unreachable!("validated above");
    };

    if !source_path.exists() {
        return Err(not_found_error(format!("plugin source not found: {}", source_path.display())));
    }
    if !source_path.is_file() {
        return Err(invalid_input_error(format!("plugin source is not a file: {}", source_path.display())));
    }

    let computed_sha = sha256_of_file(&source_path)?;
    if let Some(expected) = req.sha256.as_deref() {
        if !expected.eq_ignore_ascii_case(&computed_sha) {
            return Err(invalid_input_error(format!("sha256 mismatch: expected {expected}, computed {computed_sha}")));
        }
    }

    // Manifest probe runs against the source binary BEFORE copying into the
    // install dir so we can refuse name-spoofed installs, reserved-name
    // shadows, and untrusted-org installs without leaving stale files behind.
    let source_manifest = if req.skip_manifest_check {
        None
    } else {
        // Only chmod the source for sources we downloaded ourselves (the
        // tarball-extracted release binary, or a `--url` blob in our temp
        // dir). For `--path` we leave the user's original file alone — the
        // post-copy `ensure_executable(&installed_path)` covers the install
        // location.
        if matches!(provenance.source_kind, Some("release") | Some("url")) {
            ensure_executable(&source_path)?;
        }
        Some(probe_manifest(&source_path)?)
    };

    if let Some(manifest_for_check) = source_manifest.as_ref() {
        enforce_provider_tool_policy(manifest_for_check, req.allow_shadow_builtin)?;
        if let (Some(owner), Some(repo)) = (provenance.owner.as_deref(), provenance.repo.as_deref()) {
            enforce_manifest_name_matches_repo(manifest_for_check, owner, repo, req.force)?;
        }
    }

    let mut org_trust_decision: Option<TrustDecision> = None;
    if provenance.source_kind == Some("release") {
        if let Some(owner) = provenance.owner.as_deref() {
            org_trust_decision = enforce_org_trust(owner, &req)?;
        }
    }

    let signature_detail = resolve_signature_status(&req, &provenance)?;
    let policy_mode = effective_policy_mode(&req);
    match evaluate_signature_policy(&signature_detail, policy_mode, req.require_signature) {
        SignaturePolicyOutcome::Block { reason } => return Err(invalid_input_error(reason)),
        SignaturePolicyOutcome::ProceedWithWarning { reason } => {
            tracing::warn!(reason = %reason, "plugin install proceeding under warn policy");
            eprintln!("warning: {reason}");
        }
        SignaturePolicyOutcome::Proceed => {}
    }

    let (plugin_name, name_override_for_yaml) = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => (name.to_string(), Some(name.to_string())),
        None => {
            let derived = if !default_name.is_empty() {
                default_name
            } else {
                source_path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .ok_or_else(|| invalid_input_error("could not derive plugin name from source path"))?
                    .to_string()
            };
            (derived, None)
        }
    };

    let install_dir = scope_paths.install_dir.clone();
    let installed_path = install_dir.join(&plugin_name);

    // ---- Lockfile pre-check (runs BEFORE the already-installed gate) ----
    //
    // When the installed binary's sha256 disagrees with the recorded lock
    // entry, treat this as a tampered-binary scenario and refuse — even
    // when the operator passes the equivalent of --force. The only way to
    // proceed is to either `animus plugin uninstall` first (clears the lock
    // entry) or to re-run with `--force`, which is the supported escape
    // hatch. Without this gate, an unattended `plugin install --force`
    // could silently paper over the on-disk tamper.
    //
    // The lockfile is RELOADED here (rather than reusing the pre-load
    // above) so a concurrent install that committed an entry while this
    // install was downloading / probing the source is not erased on
    // `save()`. The fail-closed validation contract from the pre-load
    // still holds — the same `load_or_refuse_lockfile` helper applies.
    let project_root_pb = req.project_root.as_deref().map(std::path::PathBuf::from);
    let project_root_for_lock: Option<&std::path::Path> = project_root_pb.as_deref();
    // Global-scope installs must not overwrite a project-scoped lock entry
    // for the same name (the project install shadows the global one); pin
    // the write to the global lockfile in that case.
    let global_shadow_override = if scope_paths.scope == "global" {
        global_scope_lockfile_override(project_root_for_lock, &plugin_name)
    } else {
        None
    };
    let effective_lock_override = scope_paths.lockfile_override.clone().or(global_shadow_override);
    let mut lockfile =
        load_or_refuse_lockfile(project_root_for_lock, effective_lock_override.as_deref(), req.force_rewrite_lockfile)?;
    let lockfile_path_for_log = lockfile.path().to_path_buf();
    let is_upgrade = installed_path.exists();
    if is_upgrade && lockfile.find(&plugin_name).is_some() {
        match lockfile.verify_installed(&plugin_name, &installed_path) {
            // `Missing` (no entry) and `MissingTarget` (no host-only
            // binary claim for this platform — a 1.0-migrated entry or a
            // lock generated elsewhere) both mean "nothing to compare
            // against here", so the upgrade proceeds and re-records.
            Ok(LockVerifyResult::Match)
            | Ok(LockVerifyResult::Missing)
            | Ok(LockVerifyResult::MissingTarget { .. }) => {}
            Ok(LockVerifyResult::Mismatch { expected, actual }) => {
                if let Some(root) = project_root_for_lock {
                    if let Some(scoped) = protocol::repository_scope::scoped_state_root(root) {
                        Audit::at_scoped_root(&scoped).log_event(AuditEvent::new(
                            AuditActor::User,
                            AuditEventKind::LockfileMismatch,
                            serde_json::json!({
                                "plugin": plugin_name,
                                "expected_sha256": expected,
                                "actual_sha256": actual,
                                "force": req.force,
                                "lockfile": lockfile_path_for_log.display().to_string(),
                            }),
                        ));
                    }
                }
                if !req.force {
                    return Err(invalid_input_error(format!(
                            "lockfile mismatch for plugin '{plugin_name}': recorded sha256 {expected} but on-disk binary hashes to {actual}. \
                             The installed binary appears to have been modified or replaced out of band. \
                             Re-run with --force to overwrite (and update the lockfile), or `animus plugin lock verify` to inspect.",
                        )));
                }
            }
            Err(err) => {
                tracing::warn!(plugin = %plugin_name, %err, "failed to hash existing installed plugin during lockfile pre-check");
            }
        }
    }

    if installed_path.exists() && !req.force {
        return Err(invalid_input_error(format!(
            "plugin '{plugin_name}' already installed at {} (pass force=true to overwrite)",
            installed_path.display()
        )));
    }

    // ---- v0.5.7: validate the installed_kind assignment BEFORE any
    // ----        filesystem mutation (copy / plugins.yaml save).
    //
    // Codex P1 round-2: returning Err after the binary copy + yaml save
    // leaves the plugin discoverable on disk without a matching lockfile
    // entry, which makes subsequent discovery + preflight disagree with
    // the install pipeline. Compute the assignment now so a bad
    // `--as-kind` (collision, or supplied for a non-rename-eligible
    // plugin) errors out before we touch any on-disk state.
    //
    // Live discovery feeds `currently_claimed_kinds` so pre-v0.5.7
    // lockfile rows still participate in collision detection — each
    // installed subject_backend's manifest is consulted directly rather
    // than blindly inferring its kind from the lockfile name (codex P1
    // round-3 v0.5.7).
    let currently_claimed_kinds = current_subject_kinds_for_collision_check(
        req.project_root.as_deref(),
        req.plugin_dir.as_deref(),
        &lockfile,
        &plugin_name,
    );
    let (assigned_kind, native_kind_for_lock) = compute_kind_assignment(
        source_manifest.as_ref(),
        &lockfile,
        &currently_claimed_kinds,
        &plugin_name,
        req.as_kind.as_deref(),
    )?;

    std::fs::copy(&source_path, &installed_path)
        .with_context(|| format!("failed to copy {} → {}", source_path.display(), installed_path.display()))?;
    ensure_executable(&installed_path)?;

    // Manifest was probed against the source binary above; nothing to do here.
    let manifest = source_manifest;

    // Multi-binary install: when the plugin's `plugin.toml` declares
    // `[[binaries]]` entries beyond the primary, fetch + extract + install
    // each as a sibling binary in the same `install_dir`. Driven entirely
    // by the plugin.toml convention; plugins that don't declare the
    // section keep the legacy single-binary behavior.
    let mut installed_binary_names: Vec<String> = vec![plugin_name.clone()];
    let mut secondary_installed: Vec<(String, PathBuf, String, Option<tempfile::TempDir>)> = Vec::new();
    if provenance.source_kind == Some("release") {
        if let (Some(release_assets), Some(owner), Some(repo), Some(tag)) = (
            release_assets_opt.as_ref(),
            provenance.owner.as_deref(),
            provenance.repo.as_deref(),
            provenance.release_tag.as_deref(),
        ) {
            let plugin_toml_text = match fetch_plugin_toml_for_release(owner, repo, tag).await {
                Ok(t) => t,
                Err(err) => {
                    tracing::warn!(owner, repo, tag, %err, "failed to fetch plugin.toml; skipping multi-binary install");
                    None
                }
            };
            if let Some(text) = plugin_toml_text {
                let binaries = parse_plugin_toml_binaries(&text)?;
                let platform_tokens = current_platform_tokens();
                for descriptor in binaries.iter().filter(|b| !b.primary) {
                    if descriptor.name == plugin_name {
                        continue;
                    }
                    let secondary_install_path = install_dir.join(&descriptor.name);
                    if secondary_install_path.exists() && !req.force {
                        return Err(invalid_input_error(format!(
                            "secondary binary '{}' from plugin '{plugin_name}' already installed at {} (pass force=true to overwrite)",
                            descriptor.name,
                            secondary_install_path.display()
                        )));
                    }
                    let asset = pick_release_asset_for_binary(release_assets, &descriptor.name, platform_tokens)
                        .ok_or_else(|| {
                            invalid_input_error(format!(
                                "no release asset matched secondary binary '{}' for current platform '{}'. Available assets in {tag}: [{}]",
                                descriptor.name,
                                current_platform_label(),
                                release_assets.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(", ")
                            ))
                        })?;
                    let secondary_temp = create_install_staging_dir()?;
                    let secondary_temp_path = secondary_temp.path().to_path_buf();
                    let secondary_asset_path = secondary_temp_path.join(&asset.name);
                    download_to_path(&asset.browser_download_url, &secondary_asset_path).await?;

                    let mut expected_sha: Option<String> = None;
                    if let Some(sidecar_asset) = find_sha256_sidecar(release_assets, &asset.name) {
                        match download_text(&sidecar_asset.browser_download_url).await {
                            Ok(body) => {
                                if let Some(hex) = parse_sha256_sidecar(&body) {
                                    expected_sha = Some(hex);
                                }
                            }
                            Err(err) => {
                                tracing::warn!(
                                    asset = %sidecar_asset.name,
                                    %err,
                                    "failed to download secondary sha256 sidecar"
                                );
                            }
                        }
                    }
                    if expected_sha.is_none() {
                        if let Some(digest) = asset.digest.as_deref() {
                            if let Some(hex) = parse_release_digest(digest) {
                                expected_sha = Some(hex);
                            }
                        }
                    }
                    let computed_secondary = sha256_of_file(&secondary_asset_path)?;
                    // `--locked` cross-platform gate: when the lockfile pins this
                    // secondary's tarball for the current target, the downloaded
                    // archive MUST match the LOCK (not just the release's own
                    // SHA256SUMS) — closes the drifted-secondary-with-regenerated-
                    // SHA256SUMS hole before we extract/execute it (codex P1).
                    if let Some(locked) = req.locked_secondary_archive_shas.get(&descriptor.name) {
                        if !locked.eq_ignore_ascii_case(&computed_secondary) {
                            return Err(invalid_input_error(format!(
                                "lockfile mismatch for secondary asset '{}': lock pins {locked} but the downloaded \
                                 tarball hashes to {computed_secondary} — the published release changed under the pin",
                                asset.name
                            )));
                        }
                    }
                    if let Some(expected) = expected_sha.as_ref() {
                        if !expected.eq_ignore_ascii_case(&computed_secondary) {
                            return Err(invalid_input_error(format!(
                                "sha256 mismatch for secondary asset '{}': expected {expected}, computed {computed_secondary}",
                                asset.name
                            )));
                        }
                    } else {
                        eprintln!(
                            "warning: no sha256 sidecar or digest for secondary asset '{}'; install proceeding without checksum verification",
                            asset.name
                        );
                    }

                    let lower = asset.name.to_ascii_lowercase();
                    #[allow(clippy::case_sensitive_file_extension_comparisons)]
                    let secondary_binary_path = if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
                        let extract_dir = secondary_temp_path.join("extracted");
                        extract_tarball(&secondary_asset_path, &extract_dir, &descriptor.name)?
                    } else {
                        secondary_asset_path.clone()
                    };
                    std::fs::copy(&secondary_binary_path, &secondary_install_path).with_context(|| {
                        format!(
                            "failed to copy {} → {}",
                            secondary_binary_path.display(),
                            secondary_install_path.display()
                        )
                    })?;
                    ensure_executable(&secondary_install_path)?;
                    // Record the EXTRACTED BINARY's sha256 (not the archive's
                    // `computed_secondary` checked against the release sidecar
                    // above) so the lockfile pin matches the on-disk binary.
                    // Every consumer — `plugin lock verify`, `install --locked`,
                    // and the daemon lock-drift warning — hashes the installed
                    // binary, so the pin must be the binary hash to stay
                    // verifiable for archived secondary assets.
                    let secondary_binary_sha = sha256_of_file(&secondary_install_path)?;
                    secondary_installed.push((
                        descriptor.name.clone(),
                        secondary_install_path,
                        secondary_binary_sha,
                        Some(secondary_temp),
                    ));
                    installed_binary_names.push(descriptor.name.clone());
                }
            }
        }
    }

    let yaml_path = scope_paths.registry_yaml.clone();
    let mut config = load_plugins_yaml(&yaml_path)?;
    let entry: serde_yaml::Mapping = {
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            serde_yaml::Value::String("binary".to_string()),
            serde_yaml::Value::String(installed_path.to_string_lossy().to_string()),
        );
        if installed_binary_names.len() > 1 {
            let mut binaries_seq = serde_yaml::Sequence::new();
            for name in &installed_binary_names {
                binaries_seq.push(serde_yaml::Value::String(name.clone()));
            }
            map.insert(serde_yaml::Value::String("binaries".to_string()), serde_yaml::Value::Sequence(binaries_seq));
        }
        if let Some(m) = manifest.as_ref() {
            map.insert(serde_yaml::Value::String("name".to_string()), serde_yaml::Value::String(m.name.clone()));
        }
        if let Some(override_name) = name_override_for_yaml.as_deref() {
            // v0.5.8+: persist the install-time `--name <NAME>` override so
            // discovery uses the same logical name the lockfile and the
            // daemon SubjectRouter alias map were keyed under. Without this
            // field, a plugin installed with `--name task-archive` would
            // discover as its manifest name and drop the lockfile-keyed
            // alias on next daemon start (codex P2 round-4 v0.5.7).
            map.insert(
                serde_yaml::Value::String("name_override".to_string()),
                serde_yaml::Value::String(override_name.to_string()),
            );
        }
        if let Some(kind) = provenance.source_kind {
            map.insert(
                serde_yaml::Value::String("source_kind".to_string()),
                serde_yaml::Value::String(kind.to_string()),
            );
        }
        if let Some(origin) = provenance.origin.as_ref() {
            map.insert(serde_yaml::Value::String("origin".to_string()), serde_yaml::Value::String(origin.clone()));
        }
        if let Some(tag) = provenance.release_tag.as_ref() {
            map.insert(serde_yaml::Value::String("release_tag".to_string()), serde_yaml::Value::String(tag.clone()));
        }
        if let Some(asset) = provenance.asset_name.as_ref() {
            map.insert(serde_yaml::Value::String("asset".to_string()), serde_yaml::Value::String(asset.clone()));
        }
        map.insert(serde_yaml::Value::String("sha256".to_string()), serde_yaml::Value::String(computed_sha.clone()));
        map.insert(
            serde_yaml::Value::String("installed_at".to_string()),
            serde_yaml::Value::String(chrono::Utc::now().to_rfc3339()),
        );
        // Persist an audit trail when the operator bypassed the manifest
        // probe at install time. Discovery emits a warn! on every subsequent
        // probe for plugins flagged this way so the silent tolerance of
        // probe failures stays visible. We only write the field when set to
        // keep the registry quiet for the common case.
        if req.skip_manifest_check {
            map.insert(
                serde_yaml::Value::String("skip_manifest_check_at_install".to_string()),
                serde_yaml::Value::Bool(true),
            );
        }
        map.insert(
            serde_yaml::Value::String("signature_status".to_string()),
            serde_yaml::Value::String(signature_detail.label().to_string()),
        );
        if let SignatureStatus::Verified { identity, bundle_path } = &signature_detail {
            map.insert(
                serde_yaml::Value::String("signature_identity".to_string()),
                serde_yaml::Value::String(identity.clone()),
            );
            map.insert(
                serde_yaml::Value::String("signature_bundle".to_string()),
                serde_yaml::Value::String(bundle_path.clone()),
            );
        }
        map
    };
    let table = match manifest.as_ref().map(|m| m.plugin_kind.as_str()) {
        Some("provider") => &mut config.providers,
        _ => &mut config.plugins,
    };
    table.insert(serde_yaml::Value::String(plugin_name.clone()), serde_yaml::Value::Mapping(entry));
    save_plugins_yaml(&yaml_path, &config)?;

    // TOFU: persist trust for the org we just installed from with rich audit
    // metadata (trusted_at / decided_by / first_plugin). Pre-trusted orgs and
    // orgs the user explicitly listed via `--allow-org` get written to
    // `~/.animus/trusted-orgs.yaml` so a follow-up install skips the prompt.
    let first_plugin_slug = match (provenance.owner.as_deref(), provenance.repo.as_deref()) {
        (Some(o), Some(r)) => Some(format!("{o}/{r}")),
        _ => None,
    };
    if let Some(owner) = provenance.owner.as_deref() {
        // `enforce_org_trust` returns the decision when a fresh grant happened;
        // when `None`, the org was already trusted and we leave its record as-is.
        if let Some(decision) = org_trust_decision {
            if let Err(error) = add_trusted_org(owner, decision, first_plugin_slug.as_deref()) {
                tracing::warn!(owner, %error, "failed to persist trusted org after install");
            }
        }
    }
    for explicit in &req.allow_org {
        // Only attribute `first_plugin` when the pre-trusted org actually owns
        // the plugin being installed; an unrelated `--allow-org` entry was not
        // "first triggered" by this install.
        let attributed = match provenance.owner.as_deref() {
            Some(owner) if owner.eq_ignore_ascii_case(explicit) => first_plugin_slug.as_deref(),
            _ => None,
        };
        if let Err(error) = add_trusted_org(explicit, TrustDecision::AllowOrg, attributed) {
            tracing::warn!(org = %explicit, %error, "failed to persist --allow-org");
        }
    }

    // Surface the resolved org-trust provenance (org + trusted_at + decided_by)
    // so the install JSON envelope can answer "when did we trust this org?".
    let org_trust_audit: Option<OrgTrustAudit> =
        provenance.owner.as_deref().and_then(|owner| match trusted_org_record(owner) {
            Ok(Some(record)) => Some(OrgTrustAudit {
                org: record.org,
                trusted_at: record.trusted_at,
                decided_by: record.decided_by.map(|d| d.as_str().to_string()).unwrap_or_else(|| "unknown".to_string()),
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(owner, %error, "failed to read trusted-org record for install audit");
                None
            }
        });

    let sha256_verified = match provenance.sha256_verified {
        Some(verified) => verified,
        None => req.sha256.is_some(),
    };

    let signature_status = signature_detail.label().to_string();

    // ---- Lockfile: persist this install ----
    let bundle_sha = provenance.bundle_path.as_deref().and_then(|p| plugin_host_sha256_of_file(p).ok());
    let recorded_at = chrono::Utc::now().to_rfc3339();
    let current_triple = current_target_triple();

    // Build the PRIMARY entry's per-target claim. Start from the release
    // `SHA256SUMS.txt` (every published platform's TARBALL sha → portable) and
    // overlay the install platform with the cosign-bundle sha + the extracted
    // BINARY sha (host-only tamper-check). For `--path`/`--url` sources the map
    // is empty, so we record only the current target's verified archive sha.
    let is_release_source = provenance.source_kind == Some("release");
    let mut primary_targets: BTreeMap<String, TargetIntegrity> = BTreeMap::new();
    for (triple, archive_sha) in &provenance.sha256sums_targets {
        primary_targets
            .insert(triple.clone(), TargetIntegrity { archive_sha256: archive_sha.clone(), ..Default::default() });
    }
    if let Some(triple) = current_triple {
        // Attach the host-only fields (installed-binary sha + cosign bundle) to
        // the current build target. For a RELEASE the archive sha for each
        // target comes from `sha256sums_targets` (the TARBALL hash); `computed_sha`
        // is the EXTRACTED-BINARY hash, so it must NOT seed a fresh
        // `archive_sha256`. When the build triple has no recorded target —
        // e.g. a `*-linux-musl` asset selected as a fallback on a `*-gnu` host —
        // skip the host overlay rather than write a binary hash as a bogus
        // tarball pin (codex P2); `lock verify` then reports MissingTarget for
        // this platform until a native-asset reinstall.
        let entry_for_target = if is_release_source {
            primary_targets.get_mut(triple)
        } else {
            // `--path`/`--url`: the fetched artifact IS the archive, so its sha
            // is both the archive and the installed-binary hash.
            Some(
                primary_targets
                    .entry(triple.to_string())
                    .or_insert_with(|| TargetIntegrity { archive_sha256: computed_sha.clone(), ..Default::default() }),
            )
        };
        if let Some(integrity) = entry_for_target {
            integrity.installed_binary_sha256 = Some(computed_sha.clone());
            integrity.signature_bundle_sha256 = bundle_sha;
        }
    }

    let mut new_lock_entries: Vec<LockEntry> = vec![LockEntry {
        name: plugin_name.clone(),
        version: provenance.release_tag.clone().unwrap_or_default(),
        targets: primary_targets,
        installed_at: recorded_at.clone(),
        installed_kind: assigned_kind.clone(),
        native_kind: native_kind_for_lock.clone(),
        source_repo: provenance.source_repo.clone(),
        resolved_commit: provenance.resolved_commit.clone(),
        legacy_artifact_sha256: None,
        legacy_signature_bundle_sha256: None,
    }];
    for (secondary_name, _path, secondary_sha, _temp) in &secondary_installed {
        // Secondary (multi-binary) entries ship in the same release, so they
        // inherit the primary's source + resolved commit. Derive their per-target
        // TARBALL shas from the SAME `SHA256SUMS.txt` (keyed by the secondary's
        // own `<name>-<triple>.tar.gz` archives) so the secondary entry is just
        // as portable as the primary — `--locked` on a foreign platform can pin
        // its tarball before extract. `secondary_sha` is the EXTRACTED-BINARY
        // hash (NOT a tarball sha), so it only seeds the current target's
        // host-only `installed_binary_sha256`.
        let mut secondary_targets: BTreeMap<String, TargetIntegrity> = BTreeMap::new();
        if let Some(body) = provenance.sha256sums_body.as_deref() {
            for (triple, archive_sha) in parse_sha256sums_for_targets(body, secondary_name) {
                secondary_targets.insert(triple, TargetIntegrity { archive_sha256: archive_sha, ..Default::default() });
            }
        }
        if let Some(triple) = current_triple {
            // Overlay the host-only binary hash on the current target. When the
            // release has no SHA256SUMS, the secondary's archive sha is UNKNOWN
            // (we only have the extracted-binary hash, which is NOT a tarball
            // sha) — leave `archive_sha256` empty so the `--locked` secondary
            // gate skips it rather than comparing a binary hash to a tarball
            // and failing a valid reinstall (codex P2).
            let integrity = secondary_targets.entry(triple.to_string()).or_default();
            integrity.installed_binary_sha256 = Some(secondary_sha.clone());
        }
        new_lock_entries.push(LockEntry {
            name: secondary_name.clone(),
            version: provenance.release_tag.clone().unwrap_or_default(),
            targets: secondary_targets,
            installed_at: recorded_at.clone(),
            installed_kind: None,
            native_kind: None,
            source_repo: provenance.source_repo.clone(),
            resolved_commit: provenance.resolved_commit.clone(),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        });
    }
    // Multi-binary follow-up to the primary-name routing above: secondary
    // binary names only become known after the multi-binary fetch, so
    // re-check the full entry set and re-route the write to the global
    // lockfile when ANY name is claimed by a project-scoped install
    // (codex P2 round 14).
    if scope_paths.scope == "global" && lockfile.path() != global_lockfile_path() {
        if let Some(root) = project_root_for_lock {
            if new_lock_entries.iter().any(|entry| project_scope_claims_name(root, &entry.name)) {
                lockfile = load_or_refuse_lockfile(
                    project_root_for_lock,
                    Some(&global_lockfile_path()),
                    req.force_rewrite_lockfile,
                )?;
            }
        }
    }
    for entry in &new_lock_entries {
        lockfile.upsert(entry.clone());
    }
    let alias_required = match (assigned_kind.as_deref(), native_kind_for_lock.as_deref()) {
        (Some(installed), Some(native)) => installed != native,
        _ => false,
    };
    if let Err(err) = lockfile.save() {
        if alias_required {
            // Codex P2 round-4 v0.5.7: when this install records a
            // non-identity installed_kind, the lockfile is the ONLY
            // place the daemon-side SubjectRouter learns about it.
            //
            // For a FRESH install we roll back the binary + plugins.yaml
            // entry so the partial state is not silently picked up by
            // the next daemon start. For an UPGRADE we cannot roll back
            // to the pre-upgrade state without a snapshot of the prior
            // binary + entry, so we fail closed but leave on-disk state
            // untouched — the old version stays installed under its
            // existing kind, and the user is told why the alias swap
            // failed. Codex P2 round-5 (don't wipe a working install
            // because of a transient lockfile failure during upgrade).
            let (action, abort_clause) = if is_upgrade {
                // The binary was already overwritten by std::fs::copy and
                // plugins.yaml has already been re-saved with the new
                // entry, so we cannot honestly claim the previous install
                // was preserved. We DO leave the new binary + entry in
                // place (don't wipe a working binary because of a
                // transient lockfile failure) but tell the user the
                // lockfile is now inconsistent so they know to retry.
                // Codex P2 round-6 (honest error message on aliased
                // upgrade failure).
                (
                    "upgrade left lockfile inconsistent (new binary + plugins.yaml saved, lockfile still records prior state)",
                    "rerun the install with the same args once the lockfile path is writable",
                )
            } else {
                let yaml_key = serde_yaml::Value::String(plugin_name.clone());
                config.providers.remove(&yaml_key);
                config.plugins.remove(&yaml_key);
                if let Err(rollback_err) = save_plugins_yaml(&yaml_path, &config) {
                    tracing::warn!(path = %yaml_path.display(), error = %rollback_err, "failed to roll back plugins.yaml after aliased install lockfile save failure");
                }
                if let Err(rollback_err) = std::fs::remove_file(&installed_path) {
                    tracing::warn!(path = %installed_path.display(), error = %rollback_err, "failed to remove installed binary during rollback");
                }
                ("install aborted", "binary + plugins.yaml entry rolled back")
            };
            return Err(invalid_input_error(format!(
                "failed to persist plugin lockfile at {path} after assigning installed_kind '{installed}' \
                 (native '{native}'): {err:#}. The {action} ({abort_clause}) because the daemon would \
                 otherwise lose this alias on next startup. Check the lockfile path's permissions and retry.",
                path = lockfile.path().display(),
                installed = assigned_kind.as_deref().unwrap_or(""),
                native = native_kind_for_lock.as_deref().unwrap_or(""),
            )));
        }
        tracing::warn!(path = %lockfile.path().display(), %err, "failed to persist plugin lockfile");
    }

    // Dual-write: when a global-scope install was recorded in the
    // project-default lockfile (the legacy-reader location), mirror the
    // entries into `~/.animus/plugins.lock` too so other projects and
    // out-of-project commands keep the integrity + alias record for the
    // global binary (codex P2 rounds 10-12). Best-effort, like the primary
    // non-aliased save.
    if scope_paths.scope == "global" && lockfile.path() != global_lockfile_path() {
        match PluginLockfile::load_or_empty(&global_lockfile_path()) {
            Ok(mut global_lock) => {
                for entry in &new_lock_entries {
                    global_lock.upsert(entry.clone());
                }
                if let Err(err) = global_lock.save() {
                    tracing::warn!(path = %global_lock.path().display(), %err, "failed to persist global lockfile mirror after install");
                }
            }
            Err(err) => {
                tracing::warn!(%err, "failed to load global lockfile for post-install mirror");
            }
        }
    }

    // ---- Audit log ----
    if let Some(root) = project_root_for_lock {
        if let Some(scoped) = protocol::repository_scope::scoped_state_root(root) {
            let audit = Audit::at_scoped_root(&scoped);
            let event_kind = if is_upgrade { AuditEventKind::PluginUpgrade } else { AuditEventKind::PluginInstall };
            let repo_label = provenance
                .origin
                .clone()
                .or_else(|| match (provenance.owner.as_deref(), provenance.repo.as_deref()) {
                    (Some(o), Some(r)) => Some(format!("{o}/{r}")),
                    _ => None,
                })
                .unwrap_or_else(|| plugin_name.clone());
            audit.log_event(AuditEvent::new(
                AuditActor::User,
                event_kind,
                serde_json::json!({
                    "repo": repo_label,
                    "plugin": plugin_name,
                    "version": provenance.release_tag.clone().unwrap_or_default(),
                    "sha256": computed_sha,
                    "signature_status": signature_status,
                    "force": req.force,
                    "source_kind": provenance.source_kind.unwrap_or("unknown"),
                    "binaries": installed_binary_names.clone(),
                    "org_trust": org_trust_audit.as_ref().map(|t| serde_json::json!({
                        "org": t.org,
                        "trusted_at": t.trusted_at,
                        "decided_by": t.decided_by,
                    })),
                }),
            ));
            match &signature_detail {
                SignatureStatus::Invalid { identity_pattern, message } => {
                    audit.log_event(AuditEvent::new(
                        AuditActor::User,
                        AuditEventKind::SignatureInvalid,
                        serde_json::json!({
                            "plugin": plugin_name,
                            "identity_pattern": identity_pattern,
                            "message": message,
                        }),
                    ));
                }
                SignatureStatus::Unsigned { reason }
                    if !matches!(effective_policy_mode(&req), PluginPolicyMode::Strict) =>
                {
                    audit.log_event(AuditEvent::new(
                        AuditActor::User,
                        AuditEventKind::SignatureSkipped,
                        serde_json::json!({
                            "plugin": plugin_name,
                            "reason": reason,
                            "policy": effective_policy_mode(&req).as_str(),
                        }),
                    ));
                }
                SignatureStatus::UntrustedSigner { identity_pattern }
                    if !matches!(effective_policy_mode(&req), PluginPolicyMode::Strict) =>
                {
                    audit.log_event(AuditEvent::new(
                        AuditActor::User,
                        AuditEventKind::SignatureSkipped,
                        serde_json::json!({
                            "plugin": plugin_name,
                            "reason": format!("untrusted signer ({identity_pattern})"),
                            "policy": effective_policy_mode(&req).as_str(),
                        }),
                    ));
                }
                _ => {}
            }
            if req.force {
                audit.log_event(AuditEvent::new(
                    AuditActor::User,
                    AuditEventKind::PolicyOverride,
                    serde_json::json!({
                        "flag": "--force",
                        "plugin": plugin_name,
                    }),
                ));
            }
            if req.skip_signature || matches!(effective_policy_mode(&req), PluginPolicyMode::Disabled) {
                audit.log_event(AuditEvent::new(
                    AuditActor::User,
                    AuditEventKind::PolicyOverride,
                    serde_json::json!({
                        "flag": "--skip-signature/--signature-policy=disabled",
                        "plugin": plugin_name,
                    }),
                ));
            }
            for org in &req.allow_org {
                audit.log_event(AuditEvent::new(
                    AuditActor::User,
                    AuditEventKind::TrustPublisherAdded,
                    serde_json::json!({"owner": org, "via": "--allow-org"}),
                ));
            }
            if req.trust_key.is_some() {
                audit.log_event(AuditEvent::new(
                    AuditActor::User,
                    AuditEventKind::TrustKeyAdded,
                    serde_json::json!({"deprecated": true}),
                ));
            }
        }
    }

    let signature_detail = Some(signature_detail);

    // Project-scope installs drop binaries under `.animus/plugins/`; keep
    // them out of version control while leaving the lockfile committable.
    if scope_paths.scope == "project" {
        if let Some(root) = project_root_for_lock {
            if let Err(err) = ensure_project_plugins_gitignore(root) {
                tracing::warn!(%err, "failed to update .animus/.gitignore after project-scoped install");
            }
        }
    }

    Ok(PluginInstallOutput {
        name: plugin_name,
        installed_path: installed_path.to_string_lossy().to_string(),
        sha256: computed_sha,
        manifest,
        plugins_yaml: yaml_path.to_string_lossy().to_string(),
        source_kind: provenance.source_kind,
        origin: provenance.origin,
        release_tag: provenance.release_tag,
        asset_name: provenance.asset_name,
        sha256_verified,
        signature_status,
        signature_detail,
        assigned_kind,
        native_kind: native_kind_for_lock,
        scope: scope_paths.scope,
        org_trust: org_trust_audit,
    })
}

/// Extract the manifest capability the v0.5.7 kind translator can rename.
///
/// Only `subject_backend` plugins are eligible in v0.5.7: their kind
/// dispatch flows through `SubjectRouter::route_call`, which translates
/// `<installed_kind>/<verb>` -> `<native_kind>/<verb>` at the wire
/// boundary. `provider` plugins are intentionally NOT renamed here — the
/// provider dispatch path derives `provider_tool` from the plugin binary
/// name and does not consult `plugins.lock`. Recording a provider alias
/// would mint an `installed_kind` the runtime cannot honor; that promise
/// is deferred to a future release. Plugins with no `subject_kind:*`
/// capability — and providers, transports, workflow_runners, queues,
/// triggers — return `None` here and skip the rename pipeline.
///
/// Returns the FIRST exact (non-glob) `subject_kind:*` capability — the
/// "primary" kind whose `installed_kind` slot is auto-incremented /
/// renamed by `--as-kind`. See [`all_rename_eligible_native_kinds`] for
/// the full set: install-time collision detection must check every kind
/// the plugin declares, not just the primary, so a multi-kind subject
/// backend (a single binary declaring both `task` and `requirement`) is
/// blocked when its secondary kind collides too.
#[cfg(test)]
fn rename_eligible_native_kind(manifest: &PluginManifest) -> Option<String> {
    all_rename_eligible_native_kinds(manifest).into_iter().next()
}

/// Every exact `subject_kind:*` capability eligible for the v0.5.7+ kind
/// translator. Order matches the manifest's declaration order, so the
/// first element is the "primary" kind returned by
/// [`rename_eligible_native_kind`]. Glob captures (e.g. `subject_kind:task.*`)
/// are excluded — the SubjectRouter passes them through unrenamed, and an
/// alias against a glob would mint an `installed_kind` the router never
/// registers.
///
/// v0.5.8 fold-in (closes codex P2 round-4 v0.5.7): install-time
/// collision detection now iterates every entry returned here. A
/// multi-kind subject backend (a plugin declaring `task` and
/// `requirement` in the same binary) is refused when ANY of its
/// declared kinds collides with an existing install, not just the
/// primary.
fn all_rename_eligible_native_kinds(manifest: &PluginManifest) -> Vec<String> {
    if manifest.plugin_kind != "subject_backend" {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for cap in &manifest.capabilities {
        let Some(rest) = cap.strip_prefix("subject_kind:") else {
            continue;
        };
        let trimmed = rest.trim();
        if trimmed.is_empty() || trimmed.ends_with(".*") {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Inventory every `installed_kind` currently claimed by a
/// `subject_backend` plugin discoverable on disk. Used at install time
/// so collision detection sees pre-v0.5.7 plugins through their
/// manifest capabilities (translated to the lock's `installed_kind` when
/// present, falling back to the native capability when absent). The
/// `current_plugin_name` argument is excluded so an `--force` upgrade of
/// the same plugin never collides with itself.
///
/// Discovery is best-effort: filesystem errors are swallowed and an
/// empty list is returned. The downstream collision check is purely a
/// safety net — the daemon's `SubjectRouter` also rejects duplicate
/// exact kinds at startup, so a stale discovery here at most leaves the
/// operator with a clearer pre-install error message than the daemon
/// would otherwise produce.
fn current_subject_kinds_for_collision_check(
    project_root: Option<&str>,
    plugin_dir: Option<&str>,
    lockfile: &PluginLockfile,
    current_plugin_name: &str,
) -> Vec<String> {
    let mut discovery = PluginDiscovery::new();
    if let Some(root) = project_root {
        discovery = discovery.with_project_root(std::path::Path::new(root));
    }
    let _ = plugin_dir;
    let discovered = match discovery.discover() {
        Ok(plugins) => plugins,
        Err(error) => {
            tracing::warn!(%error, "plugin discovery failed during install collision pre-check; assuming none claim a kind yet");
            return Vec::new();
        }
    };
    let mut out: Vec<String> = Vec::new();
    for plugin in discovered {
        if plugin.name == current_plugin_name || plugin.manifest.plugin_kind != "subject_backend" {
            continue;
        }
        let lock_entry = lockfile.find(&plugin.name);
        let (native_to_installed, _installed_kind_from_lock) = match lock_entry {
            Some(entry) => match (entry.effective_installed_kind(), entry.effective_native_kind()) {
                (Some(installed), Some(native)) if installed != native => {
                    (Some((native.to_string(), installed.to_string())), Some(installed.to_string()))
                }
                (Some(installed), _) => (None, Some(installed.to_string())),
                _ => (None, None),
            },
            None => (None, None),
        };
        for cap in &plugin.manifest.capabilities {
            let Some(rest) = cap.strip_prefix("subject_kind:") else {
                continue;
            };
            let trimmed = rest.trim();
            if trimmed.is_empty() || trimmed.ends_with(".*") {
                // Glob captures aren't renamed by the SubjectRouter, so
                // they neither block nor get auto-incremented in v0.5.7.
                continue;
            }
            let effective = match &native_to_installed {
                Some((native, installed)) if native == trimmed => installed.clone(),
                _ => trimmed.to_string(),
            };
            if !out.contains(&effective) {
                out.push(effective);
            }
        }
    }
    out
}

/// Compute the `(installed_kind, native_kind)` pair the v0.5.7 install
/// pipeline records in `plugins.lock` for `plugin_name`. Runs BEFORE
/// any on-disk install state is mutated so a bad `--as-kind` or a
/// collision aborts the install without leaving partially-installed
/// files behind. Returns `(None, None)` when the plugin declares no
/// rename-eligible capability (transports, workflow runners, queues,
/// providers in v0.5.7, etc.) — and surfaces an error in that case if
/// the operator still passed `--as-kind`.
fn compute_kind_assignment(
    manifest: Option<&PluginManifest>,
    lockfile: &PluginLockfile,
    currently_claimed_kinds: &[String],
    plugin_name: &str,
    requested_as_kind: Option<&str>,
) -> Result<(Option<String>, Option<String>)> {
    let native_kinds = manifest.map(all_rename_eligible_native_kinds).unwrap_or_default();
    let Some(primary_native) = native_kinds.first().cloned() else {
        if let Some(explicit) = requested_as_kind.map(str::trim).filter(|s| !s.is_empty()) {
            return Err(invalid_input_error(format!(
                "--as-kind '{explicit}' supplied but plugin '{plugin_name}' declares no \
                 rename-eligible capability (no subject_kind:* entry). v0.5.7 only renames \
                 subject_backend plugins with exact (non-glob) subject_kind capabilities; \
                 drop --as-kind to install this plugin."
            )));
        }
        return Ok((None, None));
    };

    // v0.5.8 (closes codex P2 round-4 v0.5.7): check secondary kinds for
    // collisions BEFORE assigning the primary slot. The lockfile records
    // a single `installed_kind` per plugin, so a multi-kind subject
    // backend (a plugin declaring both `task` and `requirement`) cannot
    // auto-increment a secondary collision via the rename slot — the
    // only safe action is to refuse the install and tell the operator
    // which kind clashes. Pre-v0.5.7 single-kind plugins are unaffected:
    // `native_kinds.len() == 1` short-circuits the loop.
    for secondary in native_kinds.iter().skip(1) {
        let collider = lockfile_collider(lockfile, currently_claimed_kinds, plugin_name, secondary);
        if let Some(collider_name) = collider {
            let collider_label = if collider_name.is_empty() {
                "an installed plugin's manifest capability".to_string()
            } else {
                format!("installed plugin '{collider_name}'")
            };
            return Err(invalid_input_error(format!(
                "plugin '{plugin_name}' declares secondary subject_kind '{secondary}' which is \
                 already claimed by {collider_label}. Multi-kind subject backends cannot \
                 auto-increment a secondary kind in v0.5.8 (the lockfile records one \
                 installed_kind per plugin). Uninstall the colliding plugin first or pick a \
                 different binary."
            )));
        }
    }

    // Codex round-2 v0.5.8 P2: pass the plugin's own secondary native
    // kinds so the auto-increment loop skips values that would collide
    // with capabilities this same binary will register at startup.
    // Without this guard, a manifest declaring `subject_kind:task` +
    // `subject_kind:task-2` could be assigned primary `task-2` after an
    // existing `task` install, and the SubjectRouter would reject the
    // duplicate at next boot.
    let own_secondary_kinds: Vec<String> = native_kinds.iter().skip(1).cloned().collect();
    let assigned = pick_installed_kind_for_install(
        lockfile,
        currently_claimed_kinds,
        plugin_name,
        &primary_native,
        requested_as_kind,
        &own_secondary_kinds,
    )?;
    if assigned != primary_native {
        eprintln!(
            "animus.plugin.install.v1: plugin '{plugin_name}' assigned installed_kind \
             '{assigned}' (native '{primary_native}'); a previously-installed plugin already \
             claimed '{primary_native}'. Pass --as-kind on a future install to override."
        );
        tracing::info!(
            plugin = %plugin_name,
            assigned_kind = %assigned,
            native_kind = %primary_native,
            "plugin install auto-incremented installed_kind (v0.5.7 kind translator)",
        );
    }
    Ok((Some(assigned), Some(primary_native)))
}

/// Look up which currently-installed plugin (if any) already claims `kind`.
///
/// Returns `Some(name)` with the colliding plugin's name when a lockfile
/// entry or live capability already records that kind, `None` otherwise.
/// Excludes `current_plugin_name` so an upgrade of the same plugin never
/// reports itself as a collider. An empty-string name indicates the
/// collision came from `currently_claimed_kinds` (live capability scan)
/// where the discovery side intentionally drops the source plugin name.
fn lockfile_collider(
    lockfile: &PluginLockfile,
    currently_claimed_kinds: &[String],
    current_plugin_name: &str,
    kind: &str,
) -> Option<String> {
    for entry in &lockfile.plugins {
        if entry.name == current_plugin_name {
            continue;
        }
        if let Some(installed) = entry.effective_installed_kind() {
            if installed == kind {
                return Some(entry.name.clone());
            }
        }
    }
    if currently_claimed_kinds.iter().any(|k| k == kind) {
        return Some(String::new());
    }
    None
}

/// Resolve the user-facing `installed_kind` for a fresh install. When the
/// operator passed `--as-kind <NEW>`, the value is validated against
/// other entries and either accepted or returned as an error. Otherwise
/// the function auto-increments from `native_kind` (`task -> task-2 ->
/// task-3 -> ...`) until a free slot is found.
///
/// `current_plugin_name` is excluded from collision detection so a
/// re-install / upgrade of the same plugin keeps its prior installed_kind
/// instead of being bumped to a new suffix.
fn pick_installed_kind_for_install(
    lockfile: &PluginLockfile,
    currently_claimed_kinds: &[String],
    current_plugin_name: &str,
    native_kind: &str,
    requested_as_kind: Option<&str>,
    own_secondary_kinds: &[String],
) -> Result<String> {
    // The collision set is built from two sources:
    //
    // 1. v0.5.7+ lockfile entries — `effective_installed_kind()` reads
    //    the recorded `installed_kind`. Pre-v0.5.7 rows have both fields
    //    unset and drop out of this filter.
    // 2. Live `currently_claimed_kinds` — the union of `subject_kind:*`
    //    capabilities declared by every currently-installed
    //    subject_backend plugin, computed before the install touches
    //    disk. Pre-v0.5.7 lockfile entries are covered here via their
    //    binary's manifest, so a legacy `subject_kind:task` install
    //    still blocks a second `task` install while a legacy provider
    //    row does NOT spuriously block the first `subject_kind:task`
    //    install (codex P1 round-3 v0.5.7).
    let mut existing: Vec<(String, String)> = lockfile
        .plugins
        .iter()
        .filter(|entry| entry.name != current_plugin_name)
        .filter_map(|entry| entry.effective_installed_kind().map(|k| (entry.name.clone(), k.to_string())))
        .collect();
    for kind in currently_claimed_kinds {
        if !existing.iter().any(|(_, k)| k == kind) {
            existing.push((String::new(), kind.clone()));
        }
    }

    if let Some(explicit) = requested_as_kind.map(str::trim).filter(|s| !s.is_empty()) {
        if explicit.contains('/')
            || explicit.contains('*')
            || explicit.contains(':')
            || explicit.contains(char::is_whitespace)
        {
            return Err(invalid_input_error(format!(
                "--as-kind '{explicit}' is not a valid subject kind. \
                 Kinds must be exact identifiers with no '/', '*', ':', or whitespace; \
                 the ':' separator is reserved for subject id encoding (`<kind>:<local-id>`) \
                 and glob/prefix-routed kinds are not supported by the v0.5.7 translator."
            )));
        }
        if let Some((collider_name, _)) = existing.iter().find(|(_, k)| k == explicit) {
            return Err(invalid_input_error(format!(
                "--as-kind '{explicit}' is already claimed by installed plugin '{collider_name}'. \
                 Pick a different kind or uninstall the colliding plugin first."
            )));
        }
        if own_secondary_kinds.iter().any(|k| k == explicit) {
            return Err(invalid_input_error(format!(
                "--as-kind '{explicit}' is one of plugin '{current_plugin_name}'s own native \
                 subject kinds. Aliasing the primary slot to a value the plugin will also \
                 register natively would cause the SubjectRouter to refuse the duplicate at \
                 startup. Pick a different kind."
            )));
        }
        return Ok(explicit.to_string());
    }

    // Re-install / upgrade: preserve the previously-assigned
    // installed_kind so a routine `animus plugin install --force` does
    // not silently move a plugin from `archive` back to `task`. Codex P2
    // round-1 v0.5.7. Only applies when no `--as-kind` override was
    // supplied (the explicit-override branch above already handled that
    // case).
    if let Some(prior) = lockfile.find(current_plugin_name).and_then(|e| e.effective_installed_kind()) {
        if !existing.iter().any(|(_, k)| k == prior) && !own_secondary_kinds.iter().any(|k| k == prior) {
            return Ok(prior.to_string());
        }
        // Prior alias now collides with another install (or the plugin's
        // own newly-declared secondary kind after an upgrade) — fall
        // through to the auto-increment branch so the upgrade gets a
        // fresh slot.
    }

    let conflicts_self = |candidate: &str| own_secondary_kinds.iter().any(|k| k == candidate);
    let conflicts_others = |candidate: &str| existing.iter().any(|(_, k)| k == candidate);

    if !conflicts_others(native_kind) && !conflicts_self(native_kind) {
        return Ok(native_kind.to_string());
    }
    let mut suffix: u32 = 2;
    loop {
        let candidate = format!("{native_kind}-{suffix}");
        if !conflicts_others(&candidate) && !conflicts_self(&candidate) {
            return Ok(candidate);
        }
        suffix = suffix.checked_add(1).ok_or_else(|| {
            invalid_input_error(format!(
                "exhausted u32 auto-increment range for installed_kind '{native_kind}'; this is a bug"
            ))
        })?;
    }
}

fn discover(project_root: &str, include_system_path: bool) -> Result<Vec<DiscoveredPlugin>> {
    let root = Path::new(project_root);
    let scope = scope::load_project_scope(root);
    PluginDiscovery::new()
        .with_project_root(root)
        .include_system_path(include_system_path)
        .with_scope(scope)
        .discover()
        .context("plugin discovery failed")
}

fn discover_with_warnings(
    project_root: &str,
    include_system_path: bool,
) -> Result<(Vec<DiscoveredPlugin>, Vec<DiscoveryWarning>)> {
    let root = Path::new(project_root);
    let scope = scope::load_project_scope(root);
    PluginDiscovery::new()
        .with_project_root(root)
        .include_system_path(include_system_path)
        .with_scope(scope)
        .discover_with_warnings()
        .context("plugin discovery failed")
}

fn source_label(source: DiscoverySource) -> &'static str {
    match source {
        DiscoverySource::ExplicitConfig => "explicit_config",
        DiscoverySource::ProjectLocal => "project_local",
        DiscoverySource::PluginPath => "plugin_path",
        DiscoverySource::SystemPath => "system_path",
    }
}

/// Map a discovery source onto the install scope shown in `plugin list`.
/// Only the project-local tier (`<project>/.animus/plugins/`) counts as
/// `project`; everything else (registry config, global install dir,
/// `$ANIMUS_PLUGIN_PATH`, `$PATH`) is `global`.
fn scope_label(source: DiscoverySource) -> &'static str {
    match source {
        DiscoverySource::ProjectLocal => "project",
        _ => "global",
    }
}

async fn handle_plugin_list(args: PluginListArgs, project_root: &str, json: bool) -> Result<()> {
    // C6: prefer the control wire when the daemon is running so the
    // daemon's view of installed plugins is authoritative. For CLI text
    // output we still drive the local in-process render path because it
    // pulls richer rows from the on-disk install index. JSON mode
    // round-trips through the wire's PluginListResponse shape (which is
    // the same shape MCP/WebAPI will surface in C7/C8).
    use animus_control_protocol::types::PluginListRequest as WirePluginListRequest;
    use orchestrator_daemon_runtime::control::ControlClient;

    // Skip the wire route when `--include-system-path` is set: the
    // control protocol's PluginListRequest has no slot for that flag
    // (the daemon hardcodes false in routing.rs), so honoring the flag
    // requires the local discovery path. Without this guard the daemon
    // wire would silently drop the user-supplied flag.
    // TODO(codex-p2): the wire PluginListResponse does not yet carry the
    // `scope` / `shadowed` fields the local JSON shape exposes; extending
    // the control protocol needs a coordinated animus-control-protocol
    // bump. Documented as a caveat in docs/reference/cli/index.md.
    if json && !args.include_system_path {
        let project_root_path = std::path::Path::new(project_root);
        if let Some(client) = ControlClient::try_connect(project_root_path).await? {
            let request = WirePluginListRequest { include_warnings: true, kind: None };
            match client.plugin_list(request).await {
                Ok(response) => return print_value(response, true),
                Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                    tracing::debug!(error = %err, "plugin/list wire returned unavailable; falling back to local");
                }
                Err(err) => return Err(err),
            }
        }
    }

    let output = run_plugin_list(PluginListRequest {
        project_root: project_root.to_string(),
        include_system_path: args.include_system_path,
    })?;

    if json {
        return print_value(output, true);
    }

    render_plugin_list_warnings(&output.warnings, args.verbose);

    print_plugin_list_table(&output, project_root)
}

/// Stream `plugin list` discovery warnings to stderr.
///
/// Stale `explicit_config` entries (a `plugins.yaml` key whose binary
/// vanished) used to print one `warning: ...` line apiece, burying the
/// shadowed-install notes below the table under a wall of repetition. Those
/// are collapsed to a single summary line that points at the prune remedy
/// (`animus plugin uninstall <name>`, which drops the registry key; see also
/// `animus doctor --check plugins` for a per-entry report). Warnings from
/// other discovery tiers (a genuinely broken installed plugin) still print
/// per-line — they each name a distinct, individually-actionable fault.
///
/// `--verbose` restores the full per-entry detail for every tier, and the
/// `--json` envelope always carries the complete `warnings` array regardless
/// of this rendering.
fn render_plugin_list_warnings(warnings: &[PluginWarningRow], verbose: bool) {
    for line in plugin_list_warning_lines(warnings, verbose) {
        eprintln!("{line}");
    }
}

/// Build the operator-facing warning lines for `plugin list` (pure so it is
/// unit-testable). See [`render_plugin_list_warnings`] for the policy: in the
/// non-verbose path stale `explicit_config` entries collapse to one summary
/// line; every other tier keeps a per-entry line.
fn plugin_list_warning_lines(warnings: &[PluginWarningRow], verbose: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if verbose {
        for warning in warnings {
            lines.push(format!(
                "warning: plugin '{}' was discovered ({}) but could not be loaded: {} ({})",
                warning.name, warning.source, warning.reason, warning.path
            ));
        }
        return lines;
    }
    let mut stale_config = 0usize;
    for warning in warnings {
        // Collapse only the stale not-found entries; an existing binary whose
        // manifest probe failed is a real load error and stays per-entry.
        if matches!(warning.source, "explicit_config" | "project_local")
            && warning.reason.starts_with("configured binary not found")
        {
            stale_config += 1;
            continue;
        }
        lines.push(format!(
            "warning: plugin '{}' was discovered ({}) but could not be loaded: {} ({})",
            warning.name, warning.source, warning.reason, warning.path
        ));
    }
    if stale_config > 0 {
        let noun = if stale_config == 1 { "entry" } else { "entries" };
        lines.push(format!(
            "warning: {stale_config} stale plugins.yaml {noun} (binary missing); run `animus plugin prune`",
        ));
    }
    lines
}

/// Normalize a recorded `installed_at` timestamp to a single `YYYY-MM-DD HH:MM`
/// form. `plugins.yaml` carries two historical shapes — RFC3339 (`2026-05-30T20:25:20...`)
/// and a space-separated chrono debug form (`2026-05-29 17:32:43.172858+00:00`).
/// Both reduce to date + minutes so the INSTALLED column reads uniformly.
fn normalize_installed_at(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "--".to_string();
    }
    // Split the date from the time on either `T` or a space.
    let (date, rest) = match trimmed.split_once(['T', ' ']) {
        Some((d, r)) => (d, r),
        None => return trimmed.to_string(),
    };
    // Keep only HH:MM from the time component (drop seconds, fractions, zone).
    let time = rest.split([':', '.', '+', 'Z']).take(2).collect::<Vec<_>>();
    let hh = time.first().copied().unwrap_or("");
    let mm = time.get(1).copied().unwrap_or("");
    if hh.is_empty() {
        date.to_string()
    } else if mm.is_empty() {
        format!("{date} {hh}")
    } else {
        format!("{date} {hh}:{mm}")
    }
}

/// Compute the VERSION cell for `plugin list`. The recorded lockfile/registry
/// `release_tag` is authoritative; the binary's manifest version is advisory.
/// When the two disagree, surface a `(binary <v>)` mismatch hint so operators
/// see the divergence instead of a single misleading number.
fn list_version_cell(recorded_tag: Option<&str>, manifest_version: &str) -> String {
    let tag = recorded_tag.map(str::trim).filter(|t| !t.is_empty());
    let manifest = manifest_version.trim();
    match tag {
        Some(tag) => {
            if manifest.is_empty() || tags_describe_same_version(tag, manifest) {
                tag.to_string()
            } else {
                format!("{tag} (binary {manifest})")
            }
        }
        None => {
            if manifest.is_empty() {
                "--".to_string()
            } else {
                manifest.to_string()
            }
        }
    }
}

/// True when a `vX.Y.Z` release tag and a bare `X.Y.Z` manifest version name the
/// same release (tolerates the conventional leading `v` on tags).
fn tags_describe_same_version(tag: &str, manifest: &str) -> bool {
    let strip = |s: &str| s.trim().trim_start_matches('v').to_string();
    strip(tag) == strip(manifest)
}

/// Render `plugin list` results as a table with source-of-truth columns:
/// `NAME  KIND  VERSION  SOURCE  INSTALLED  PATH`.
fn print_plugin_list_table(output: &PluginListOutput, project_root: &str) -> Result<()> {
    if output.plugins.is_empty() {
        println!("no plugins discovered");
        return Ok(());
    }
    let installed = marketplace::read_installed_index().unwrap_or_default();
    // Project-scoped rows resolve their install metadata (source +
    // installed-at) from the PROJECT registry; falling through to the
    // global index would show `--` (or a same-named global install's
    // metadata) for project installs.
    let project_installed =
        marketplace::read_installed_index_at(&project_plugins_registry_path(Path::new(project_root)))
            .unwrap_or_default();
    struct Row {
        name: String,
        kind: String,
        version: String,
        scope: String,
        source: String,
        installed: String,
        path: String,
    }
    let rows: Vec<Row> = output
        .plugins
        .iter()
        .map(|p| {
            let installed_entry =
                if p.scope == "project" { project_installed.get(&p.name) } else { installed.get(&p.name) };
            let source = installed_entry.map(marketplace::format_installed_source).unwrap_or_else(|| "--".to_string());
            let installed_at = installed_entry
                .and_then(|e| e.installed_at.as_deref())
                .map(normalize_installed_at)
                .unwrap_or_else(|| "--".to_string());
            Row {
                name: p.name.clone(),
                kind: p.plugin_kind.clone(),
                // VERSION is the authoritative lockfile/registry release tag.
                // The binary's self-reported manifest version is advisory; when
                // it disagrees with the recorded tag we flag a mismatch so the
                // operator knows the on-disk binary and the lockfile diverged.
                version: list_version_cell(installed_entry.and_then(|e| e.release_tag.as_deref()), &p.version),
                scope: p.scope.to_string(),
                source,
                installed: installed_at,
                path: p.path.clone(),
            }
        })
        .collect();
    let widths = [
        rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4),
        rows.iter().map(|r| r.kind.len()).max().unwrap_or(4).max(4),
        rows.iter().map(|r| r.version.len()).max().unwrap_or(7).max(7),
        rows.iter().map(|r| r.scope.len()).max().unwrap_or(5).max(5),
        rows.iter().map(|r| r.source.len()).max().unwrap_or(6).max(6),
        rows.iter().map(|r| r.installed.len()).max().unwrap_or(9).max(9),
    ];
    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}  PATH",
        "NAME",
        "KIND",
        "VERSION",
        "SCOPE",
        "SOURCE",
        "INSTALLED",
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
        w5 = widths[5],
    );
    for row in &rows {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}  {}",
            row.name,
            row.kind,
            row.version,
            row.scope,
            row.source,
            row.installed,
            row.path,
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
            w5 = widths[5],
        );
    }
    for shadow in &output.shadowed {
        println!(
            "note: global install of '{}' at {} is shadowed by the project install at {}",
            shadow.name, shadow.path, shadow.shadowed_by
        );
    }
    Ok(())
}

fn find_plugin(project_root: &str, name: &str, include_system_path: bool) -> Result<DiscoveredPlugin> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("--name must not be empty"));
    }
    let mut matches =
        if include_system_path { discover(project_root, true)? } else { discover_plugins(Path::new(project_root))? };
    matches.retain(|plugin| plugin.name == trimmed);
    matches.pop().ok_or_else(|| not_found_error(format!("plugin not found: {trimmed}")))
}

async fn handle_plugin_info(args: PluginInfoArgs, project_root: &str, json: bool) -> Result<()> {
    use animus_control_protocol::types::PluginInfoRequest as WirePluginInfoRequest;
    use orchestrator_daemon_runtime::control::ControlClient;

    // Skip the wire route when `--include-system-path` is set; see
    // `handle_plugin_list` for the rationale.
    if json && !args.include_system_path {
        let project_root_path = std::path::Path::new(project_root);
        if let Some(client) = ControlClient::try_connect(project_root_path).await? {
            let request = WirePluginInfoRequest { name: args.name.clone() };
            match client.plugin_info(request).await {
                Ok(response) => return print_value(response, true),
                Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                    tracing::debug!(error = %err, "plugin/info wire returned unavailable; falling back to local");
                }
                Err(err) => return Err(err),
            }
        }
    }

    let output = run_plugin_info(PluginInfoRequest {
        project_root: project_root.to_string(),
        name: args.name,
        include_system_path: args.include_system_path,
    })
    .await?;
    // Surface the audit flag in human-readable mode so operators see at a
    // glance that the manifest probe was skipped at install time. JSON mode
    // carries the same signal via `skip_manifest_check_at_install`.
    if !json && output.skip_manifest_check_at_install {
        println!("SKIP_MANIFEST_CHECK: true");
    }
    print_value(output, json)
}

async fn handle_plugin_call(args: PluginCallArgs, project_root: &str, json: bool) -> Result<()> {
    use animus_control_protocol::types::PluginCallRequest as WirePluginCallRequest;
    use orchestrator_daemon_runtime::control::ControlClient;

    let params = match args.params {
        Some(raw) => Some(serde_json::from_str::<Value>(&raw).context("--params must be valid JSON")?),
        None => None,
    };

    // Skip the wire route when `--include-system-path` is set; see
    // `handle_plugin_list` for the rationale.
    if json && !args.include_system_path {
        let project_root_path = std::path::Path::new(project_root);
        if let Some(client) = ControlClient::try_connect(project_root_path).await? {
            let request = WirePluginCallRequest {
                name: args.name.clone(),
                method: args.method.clone(),
                params: params.clone().unwrap_or(Value::Null),
            };
            match client.plugin_call(request).await {
                Ok(response) => return print_value(response, true),
                Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                    tracing::debug!(error = %err, "plugin/call wire returned unavailable; falling back to local");
                }
                Err(err) => return Err(err),
            }
        }
    }

    let output = run_plugin_call(PluginCallRequest {
        project_root: project_root.to_string(),
        name: args.name,
        method: args.method,
        params,
        include_system_path: args.include_system_path,
    })
    .await?;
    print_value(output, json)
}

async fn handle_plugin_ping(args: PluginPingArgs, project_root: &str, json: bool) -> Result<()> {
    use animus_control_protocol::types::PluginPingRequest as WirePluginPingRequest;
    use orchestrator_daemon_runtime::control::ControlClient;

    // Skip the wire route when `--include-system-path` is set; see
    // `handle_plugin_list` for the rationale.
    if json && !args.include_system_path {
        let project_root_path = std::path::Path::new(project_root);
        if let Some(client) = ControlClient::try_connect(project_root_path).await? {
            let request = WirePluginPingRequest { name: args.name.clone() };
            match client.plugin_ping(request).await {
                Ok(response) => return print_value(response, true),
                Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                    tracing::debug!(error = %err, "plugin/ping wire returned unavailable; falling back to local");
                }
                Err(err) => return Err(err),
            }
        }
    }

    let output = run_plugin_ping(PluginPingRequest {
        project_root: project_root.to_string(),
        name: args.name,
        include_system_path: args.include_system_path,
    })
    .await?;
    print_value(output, json)
}

/// Resolves the plugin install directory.
///
/// Resolution order:
/// 1. `--plugin-dir <path>` CLI arg (when provided)
/// 2. `$ANIMUS_PLUGIN_DIR` env var (via [`plugin_install_dir`])
/// 3. Default `~/.animus/plugins/`
fn install_root(cli_override: Option<&str>) -> Result<PathBuf> {
    let dir = match cli_override.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => plugin_install_dir(),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create install dir {}", dir.display()))?;
    Ok(dir)
}

/// The (install dir, registry yaml, lockfile) triple for one install scope.
///
/// Global scope mirrors the historical behavior exactly: install dir from
/// `--plugin-dir` / `$ANIMUS_PLUGIN_DIR` / `~/.animus/plugins/`, registry at
/// `~/.animus/plugins.yaml`, and lockfile via
/// [`PluginLockfile::default_path`] (which prefers
/// `<project>/.animus/plugins.lock` when the project has opted into Animus).
/// Project scope pins all three under `<project_root>/.animus/`.
#[derive(Debug, Clone)]
struct InstallScopePaths {
    /// `"global"` or `"project"` — surfaced in output envelopes.
    scope: &'static str,
    install_dir: PathBuf,
    registry_yaml: PathBuf,
    /// `Some(path)` pins the lockfile explicitly (project scope);
    /// `None` keeps the legacy [`PluginLockfile::default_path`] resolution.
    lockfile_override: Option<PathBuf>,
}

/// Resolve the scope triple for an install/uninstall request. `project=true`
/// requires `project_root` and refuses an explicit `plugin_dir` (defense in
/// depth behind the clap `conflicts_with`).
fn resolve_install_scope(
    project: bool,
    project_root: Option<&str>,
    plugin_dir: Option<&str>,
) -> Result<InstallScopePaths> {
    if !project {
        return Ok(InstallScopePaths {
            scope: "global",
            install_dir: install_root(plugin_dir)?,
            registry_yaml: plugins_yaml_path()?,
            lockfile_override: None,
        });
    }
    if plugin_dir.is_some() {
        return Err(invalid_input_error("--project and --plugin-dir are mutually exclusive"));
    }
    let root = project_root
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_input_error("--project requires a resolvable project root"))?;
    let root = Path::new(root);
    let install_dir = project_plugin_install_dir(root);
    std::fs::create_dir_all(&install_dir)
        .with_context(|| format!("failed to create project plugin dir {}", install_dir.display()))?;
    Ok(InstallScopePaths {
        scope: "project",
        install_dir,
        registry_yaml: project_plugins_registry_path(root),
        lockfile_override: Some(project_lockfile_path(root)),
    })
}

/// Decide the lockfile a GLOBAL-scope install/uninstall of `plugin_name`
/// should write. Legacy behavior: `PluginLockfile::default_path`, which
/// prefers `<project>/.animus/plugins.lock` once a project has opted into
/// Animus. That is fine until the same name is ALSO project-scope
/// installed — then a global op routed at the project lockfile would
/// overwrite or delete the entry that protects the project binary. In that
/// shadowed case, pin global-scope lock writes to the global
/// `~/.animus/plugins.lock` instead (codex P2, project-scoped installs).
fn global_scope_lockfile_override(project_root: Option<&Path>, plugin_name: &str) -> Option<PathBuf> {
    let root = project_root?;
    let default = PluginLockfile::default_path(Some(root));
    let global = global_lockfile_path();
    if default == global {
        return None;
    }
    if project_scope_claims_name(root, plugin_name) {
        return Some(global);
    }
    None
}

/// Whether `plugin_name` belongs to a project-scoped install of
/// `project_root`: either the binary sits in `<project>/.animus/plugins/`
/// or the project registry (`<project>/.animus/plugins.yaml`) records it.
/// The registry check matters when the binary was deleted out of band —
/// the project lock entry still describes the project install and must not
/// be reassigned to global-scope semantics. Project lockfile entries alone
/// are NOT consulted: pre-`--project` global installs recorded their
/// entries there via `PluginLockfile::default_path`, and those legacy
/// entries are exactly the ones global ops should keep maintaining.
fn project_scope_claims_name(project_root: &Path, plugin_name: &str) -> bool {
    if project_plugin_install_dir(project_root).join(plugin_name).exists() {
        return true;
    }
    project_registry_claimed_names(project_root).contains(plugin_name)
}

/// Every plugin/binary name a project-scoped install of `project_root`
/// claims: the basenames present in `<project>/.animus/plugins/` UNIONED
/// with every name the project registry (`<project>/.animus/plugins.yaml`)
/// records. This is the project-scope counterpart to a scan of the global
/// [`plugin_install_dir`]; drift / inventory callers that only look at the
/// global dir miss `--project` installs entirely. Empty when neither the
/// project install dir nor the registry exists or is readable.
pub(super) fn project_scope_installed_names(project_root: &Path) -> BTreeSet<String> {
    let install_dir = project_plugin_install_dir(project_root);
    let mut names = BTreeSet::new();
    // A registry-claimed name counts as installed only while its recorded
    // binary (or the default project install path) still exists — a stale
    // registry entry whose binary was deleted must not satisfy flavor drift.
    let path_key = serde_yaml::Value::String("binary".to_string());
    let binaries_key = serde_yaml::Value::String("binaries".to_string());
    if let Ok(config) = load_plugins_yaml(&project_plugins_registry_path(project_root)) {
        for table in [&config.plugins, &config.providers] {
            for (key, value) in table {
                let serde_yaml::Value::String(name) = key else { continue };
                let recorded_path = match value {
                    serde_yaml::Value::String(path) => Some(PathBuf::from(path)),
                    serde_yaml::Value::Mapping(entry) => {
                        entry.get(&path_key).and_then(|v| v.as_str()).map(PathBuf::from)
                    }
                    _ => None,
                };
                let present = recorded_path.map(|p| p.exists()).unwrap_or(false) || install_dir.join(name).exists();
                if !present {
                    continue;
                }
                names.insert(name.clone());
                if let serde_yaml::Value::Mapping(entry) = value {
                    if let Some(serde_yaml::Value::Sequence(seq)) = entry.get(&binaries_key) {
                        for item in seq {
                            if let serde_yaml::Value::String(secondary) = item {
                                names.insert(secondary.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    if let Ok(iter) = std::fs::read_dir(install_dir) {
        for entry in iter.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                names.insert(name.to_string());
            }
        }
    }
    names
}

/// Every binary name the project registry claims: the table keys of
/// `<project>/.animus/plugins.yaml` PLUS any secondary names recorded in an
/// entry's `binaries:` list (multi-binary release installs write one lock
/// entry per secondary but only one registry key). Empty when the registry
/// is missing or unreadable.
fn project_registry_claimed_names(project_root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let path = project_plugins_registry_path(project_root);
    let Ok(config) = load_plugins_yaml(&path) else {
        return names;
    };
    let binaries_key = serde_yaml::Value::String("binaries".to_string());
    for table in [&config.plugins, &config.providers] {
        for (key, value) in table {
            if let serde_yaml::Value::String(name) = key {
                names.insert(name.clone());
            }
            if let serde_yaml::Value::Mapping(entry) = value {
                if let Some(serde_yaml::Value::Sequence(seq)) = entry.get(&binaries_key) {
                    for item in seq {
                        if let serde_yaml::Value::String(name) = item {
                            names.insert(name.clone());
                        }
                    }
                }
            }
        }
    }
    names
}

/// Ensure `<project_root>/.animus/.gitignore` ignores the `plugins/`
/// directory so project-local plugin BINARIES are never committed. The
/// project lockfile (`plugins.lock`) and registry (`plugins.yaml`) are
/// intentionally NOT ignored — committing them is how a repo pins its own
/// plugin set. Idempotent: appends the pattern only when missing; never
/// rewrites operator-managed lines.
pub(crate) fn ensure_project_plugins_gitignore(project_root: &Path) -> Result<()> {
    let animus_dir = project_root.join(".animus");
    std::fs::create_dir_all(&animus_dir).with_context(|| format!("failed to create {}", animus_dir.display()))?;
    let gitignore = animus_dir.join(".gitignore");
    let existing = match std::fs::read_to_string(&gitignore) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(anyhow::Error::new(err).context(format!("failed to read {}", gitignore.display())));
        }
    };
    // An operator-managed wildcard already covers the binaries, but it ALSO
    // ignores the committable lockfile/registry that pin the project's
    // plugin set. Respect the operator's file (no rewrite), but say so
    // loudly instead of silently leaving the pin uncommittable (codex P2).
    if existing.lines().map(str::trim).any(|line| line == "*") {
        eprintln!(
            "note: {} ignores everything in .animus/ — to commit the project plugin pin, add \
             `!plugins.lock` (and optionally `!plugins.yaml`) below the `*` line",
            gitignore.display()
        );
        return Ok(());
    }
    let already_covered =
        existing.lines().map(str::trim).any(|line| matches!(line, "plugins/" | "plugins" | "/plugins/" | "/plugins"));
    if already_covered {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("# Project-local plugin binaries (commit plugins.lock, not the binaries).\nplugins/\n");
    std::fs::write(&gitignore, updated).with_context(|| format!("failed to write {}", gitignore.display()))
}

/// Resolves the plugin registry yaml path, performing a one-shot migration from
/// the legacy `~/.config/animus/plugins.yaml` location when needed.
fn plugins_yaml_path() -> Result<PathBuf> {
    let canonical = plugins_registry_path();
    if let Some(parent) = canonical.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create config dir {}", parent.display()))?;
    }

    if !canonical.exists() {
        let legacy = legacy_plugins_registry_path();
        if legacy.exists() {
            std::fs::copy(&legacy, &canonical).with_context(|| {
                format!("failed to migrate plugin registry from {} to {}", legacy.display(), canonical.display())
            })?;
            tracing::info!(
                from = %legacy.display(),
                to = %canonical.display(),
                "migrated plugin registry to canonical location",
            );
        }
    }

    Ok(canonical)
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct PluginsYamlConfig {
    #[serde(default)]
    plugins: serde_yaml::Mapping,
    #[serde(default)]
    providers: serde_yaml::Mapping,
}

fn load_plugins_yaml(path: &Path) -> Result<PluginsYamlConfig> {
    if !path.exists() {
        return Ok(PluginsYamlConfig::default());
    }
    let contents = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_yaml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_plugins_yaml(path: &Path, config: &PluginsYamlConfig) -> Result<()> {
    let serialized = serde_yaml::to_string(config).context("failed to serialize plugins.yaml")?;
    std::fs::write(path, serialized).with_context(|| format!("failed to write {}", path.display()))
}

fn sha256_of_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Create a fresh staging directory for a plugin install under
/// `$TMPDIR/animus-plugin-install-<random>`.
///
/// Returns a [`tempfile::TempDir`] guard — when it drops, the directory
/// is removed recursively via RAII. The pre-fix code used
/// `std::env::temp_dir().join(uuid)` and never cleaned up, accumulating
/// GBs of orphaned install dirs over time on power-user machines.
fn create_install_staging_dir() -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("animus-plugin-install-")
        .tempdir()
        .context("failed to create plugin install staging dir")
}

/// Download a plugin asset from `url` to a freshly created staging
/// directory and verify its sha256.
///
/// Returns the on-disk path of the downloaded file **and** the
/// [`tempfile::TempDir`] guard that owns the staging directory. The
/// caller MUST keep the `TempDir` alive until it has copied the binary
/// to its final home — when the guard drops, the staging dir (and the
/// returned path inside it) are deleted via RAII. This closes the GB-of-
/// orphaned-`animus-plugin-install-*` dirs leak that accumulated under
/// `$TMPDIR` over time.
async fn fetch_url_to_temp(url: &str, expected_sha256: &str) -> Result<(PathBuf, tempfile::TempDir)> {
    if !url.starts_with("https://") {
        return Err(invalid_input_error("--url must use https://"));
    }
    let response = reqwest::get(url)
        .await
        .map_err(|err| unavailable_error(format!("failed to download {url}: {err}")))?
        .error_for_status()
        .map_err(|err| unavailable_error(format!("download from {url} returned non-success status: {err}")))?;
    let bytes =
        response.bytes().await.map_err(|err| unavailable_error(format!("failed to read body from {url}: {err}")))?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let computed_sha = format!("{:x}", hasher.finalize());
    if !expected_sha256.eq_ignore_ascii_case(&computed_sha) {
        return Err(invalid_input_error(format!(
            "sha256 mismatch for {url}: expected {expected_sha256}, computed {computed_sha}"
        )));
    }

    let temp_dir = create_install_staging_dir()?;
    let filename = url.rsplit('/').next().unwrap_or("plugin");
    let dest = temp_dir.path().join(filename);
    std::fs::write(&dest, &bytes)
        .with_context(|| format!("failed to write downloaded plugin to {}", dest.display()))?;
    Ok((dest, temp_dir))
}

// ===== Public-repo (GitHub release) install support =====

/// Parsed `owner/repo[@tag]` positional source.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoSpec {
    owner: String,
    repo: String,
    tag: Option<String>,
}

/// Parse an `owner/repo` or `owner/repo@tag` slug. Whitespace is trimmed.
fn parse_repo_spec(raw: &str) -> Result<RepoSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("repo source must not be empty"));
    }
    let (slug, tag) = match trimmed.split_once('@') {
        Some((slug, tag)) => {
            let tag = tag.trim();
            if tag.is_empty() {
                return Err(invalid_input_error(format!("repo source '{trimmed}' has an empty tag after '@'")));
            }
            (slug.trim(), Some(tag.to_string()))
        }
        None => (trimmed, None),
    };
    let (owner, repo) = slug.split_once('/').ok_or_else(|| {
        invalid_input_error(format!("repo source '{trimmed}' must be in the form 'owner/repo[@tag]'"))
    })?;
    let owner = owner.trim();
    let repo = repo.trim();
    if owner.is_empty() || repo.is_empty() {
        return Err(invalid_input_error(format!("repo source '{trimmed}' must be in the form 'owner/repo[@tag]'")));
    }
    Ok(RepoSpec { owner: owner.to_string(), repo: repo.to_string(), tag })
}

/// Returns the list of platform-target substrings to match against asset names,
/// in priority order. The first asset whose name contains any of these
/// substrings (case-insensitive) is selected.
fn current_platform_tokens() -> &'static [&'static str] {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        &["aarch64-apple-darwin", "macos-aarch64", "darwin-arm64", "darwin-aarch64", "macos-arm64"]
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        &["x86_64-apple-darwin", "macos-x86_64", "darwin-amd64", "darwin-x86_64", "macos-amd64"]
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        &["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl", "linux-x86_64", "linux-amd64"]
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        &["aarch64-unknown-linux-gnu", "aarch64-unknown-linux-musl", "linux-aarch64", "linux-arm64"]
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        &["x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu", "windows-x86_64", "windows-amd64"]
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        &[]
    }
}

/// Human-readable label for the current build target.
fn current_platform_label() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    /// The commitish the release tag points at. For a tag created directly on
    /// a commit this is the 40-hex sha; for a tag created against a branch the
    /// GitHub API returns the branch name. Only recorded as `resolved_commit`
    /// when it is a real sha (see [`is_commit_sha`]).
    #[serde(default)]
    target_commitish: Option<String>,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

/// Returns `true` when `s` is a 40-char lowercase-hex git commit sha. GitHub's
/// release `target_commitish` is a sha only when the tag was created on a
/// commit; tags created against a branch return the branch name instead, which
/// must NOT be recorded as a resolved commit.
fn is_commit_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// Pure asset-selection helper. Picks the first asset whose name contains any
/// of `platform_tokens` (case-insensitive). Sidecar `.sha256` assets are
/// excluded from candidate matching.
fn pick_release_asset<'a>(
    assets: &'a [GithubReleaseAsset],
    platform_tokens: &[&str],
) -> Option<&'a GithubReleaseAsset> {
    for token in platform_tokens {
        let needle = token.to_ascii_lowercase();
        for asset in assets {
            let lower = asset.name.to_ascii_lowercase();
            if lower.ends_with(".sha256") || lower.ends_with(".sha256sum") {
                continue;
            }
            if lower.contains(&needle) {
                return Some(asset);
            }
        }
    }
    None
}

/// Look up the sidecar `<asset_name>.sha256` in the same release, if present.
fn find_sha256_sidecar<'a>(assets: &'a [GithubReleaseAsset], asset_name: &str) -> Option<&'a GithubReleaseAsset> {
    let sidecar = format!("{asset_name}.sha256");
    assets.iter().find(|a| a.name.eq_ignore_ascii_case(&sidecar))
}

/// Build the GitHub releases API URL for either `latest` or a specific tag.
fn github_release_api_url(owner: &str, repo: &str, tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}"),
        None => format!("https://api.github.com/repos/{owner}/{repo}/releases/latest"),
    }
}

fn release_user_agent() -> String {
    format!("animus-cli/{}", env!("CARGO_PKG_VERSION"))
}

async fn fetch_github_release(owner: &str, repo: &str, tag: Option<&str>) -> Result<GithubRelease> {
    let url = github_release_api_url(owner, repo, tag);
    let client =
        reqwest::Client::builder().user_agent(release_user_agent()).build().context("failed to build HTTP client")?;
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| unavailable_error(format!("failed to GET {url}: {err}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(not_found_error(format!(
            "no release found at {url} (check the repo slug, tag, or whether a release has been published yet)"
        )));
    }
    let response = response
        .error_for_status()
        .map_err(|err| unavailable_error(format!("GET {url} returned non-success status: {err}")))?;
    let release: GithubRelease =
        response.json().await.with_context(|| format!("failed to parse GitHub release JSON from {url}"))?;
    Ok(release)
}

/// Parse an `algo:hex` digest string (as returned by the GitHub API's `digest`
/// field on release assets), returning the lowercased hex if the algorithm is
/// `sha256`. Returns `None` for unsupported algorithms.
fn parse_release_digest(digest: &str) -> Option<String> {
    let trimmed = digest.trim();
    let (algo, hex) = trimmed.split_once(':')?;
    if !algo.eq_ignore_ascii_case("sha256") {
        return None;
    }
    let hex = hex.trim();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex.to_ascii_lowercase())
}

/// Parse a sidecar file body (commonly `<hex>  <filename>\n`), returning the
/// leading hex digest if present.
fn parse_sha256_sidecar(body: &str) -> Option<String> {
    let line = body.lines().next()?.trim();
    let token = line.split_whitespace().next()?;
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token.to_ascii_lowercase())
    } else {
        None
    }
}

/// Every Rust target triple Animus releases publish assets for. Used to derive
/// a per-asset target triple from a `SHA256SUMS.txt` filename so the lockfile
/// can record a portable, per-platform integrity claim.
const KNOWN_TARGET_TRIPLES: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "x86_64-pc-windows-gnu",
];

/// Derive the target triple a release asset filename targets, by scanning for a
/// known triple substring (case-insensitive). `None` for non-archive assets
/// (e.g. `SHA256SUMS.txt`, `.bundle`, `.sha256` sidecars) and any name with no
/// recognizable triple.
fn target_triple_from_asset_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    // `lower` is already ASCII-lowercased, so these `ends_with` checks are
    // case-insensitive by construction; clippy's heuristic doesn't see it.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    let is_archive = lower.ends_with(".tar.gz") || lower.ends_with(".tgz");
    if !is_archive {
        return None;
    }
    KNOWN_TARGET_TRIPLES.iter().copied().find(|triple| lower.contains(&triple.to_ascii_lowercase()))
}

/// Parse a release `SHA256SUMS.txt` body into per-target archive shas for the
/// archives belonging to `plugin`. Each line is `<hex>␠␠<filename>`; only
/// archive filenames carrying a recognizable target triple contribute. When
/// several archives map to the same triple (e.g. multi-binary releases
/// publishing several `<bin>-<triple>.tar.gz`), the EXACT
/// `<plugin>-<triple>.{tar.gz,tgz}` archive is preferred — an exact base match,
/// not a prefix test, so a sibling like `<plugin>-helper-<triple>.tar.gz` is
/// not mistaken for `<plugin>`'s archive. Otherwise the first seen is kept.
fn parse_sha256sums_for_targets(body: &str, plugin: &str) -> BTreeMap<String, String> {
    let plugin_lower = plugin.to_ascii_lowercase();
    let mut out: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(hex) = parts.next() else { continue };
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let Some(filename) = parts.next() else { continue };
        // Strip a leading `*` (binary-mode marker some sha256sum tools emit).
        let filename = filename.strip_prefix('*').unwrap_or(filename);
        let Some(triple) = target_triple_from_asset_name(filename) else { continue };
        // The archive belongs to `plugin` exactly when its name is
        // `<plugin>-<triple>.<ext>`. Compared case-insensitively against the
        // canonical archive names for this triple.
        let lower = filename.to_ascii_lowercase();
        let is_preferred =
            lower == format!("{plugin_lower}-{triple}.tar.gz") || lower == format!("{plugin_lower}-{triple}.tgz");
        match out.get(triple) {
            Some((_, existing_preferred)) if *existing_preferred || !is_preferred => {}
            _ => {
                out.insert(triple.to_string(), (hex.to_ascii_lowercase(), is_preferred));
            }
        }
    }
    out.into_iter().map(|(triple, (hex, _))| (triple, hex)).collect()
}

/// Locate the release's `SHA256SUMS.txt` asset (case-insensitive; also accepts
/// `SHA256SUMS`).
fn find_sha256sums_asset(assets: &[GithubReleaseAsset]) -> Option<&GithubReleaseAsset> {
    assets.iter().find(|a| {
        let lower = a.name.to_ascii_lowercase();
        lower == "sha256sums.txt" || lower == "sha256sums"
    })
}

async fn download_to_path(url: &str, dest: &Path) -> Result<()> {
    let client =
        reqwest::Client::builder().user_agent(release_user_agent()).build().context("failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| unavailable_error(format!("failed to download {url}: {err}")))?
        .error_for_status()
        .map_err(|err| unavailable_error(format!("download from {url} returned non-success status: {err}")))?;
    let bytes =
        response.bytes().await.map_err(|err| unavailable_error(format!("failed to read body from {url}: {err}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create download dir {}", parent.display()))?;
    }
    std::fs::write(dest, &bytes).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(())
}

async fn download_text(url: &str) -> Result<String> {
    let client =
        reqwest::Client::builder().user_agent(release_user_agent()).build().context("failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| unavailable_error(format!("failed to download {url}: {err}")))?
        .error_for_status()
        .map_err(|err| unavailable_error(format!("download from {url} returned non-success status: {err}")))?;
    response.text().await.map_err(|err| unavailable_error(format!("failed to read body from {url}: {err}")))
}

/// Extract a `.tar.gz` archive into `dest_dir` and pick the plugin binary
/// out of the extracted tree.
///
/// Selection priority (deterministic — `first_file()` was order-dependent
/// and silently installed READMEs):
///
/// 1. A regular file whose basename matches `expected_name` exactly (with
///    or without a `.exe` suffix).
/// 2. If exactly one extracted file has any execute bit set, use it.
/// 3. Otherwise, error with a list of every extracted file so the operator
///    can see why the install was rejected.
///
/// `expected_name` is the plugin name derived from the install source —
/// typically the GitHub repo basename (`animus-provider-claude`).
fn extract_tarball(archive: &Path, dest_dir: &Path, expected_name: &str) -> Result<PathBuf> {
    let file = std::fs::File::open(archive).with_context(|| format!("failed to open {}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar_reader = tar::Archive::new(gz);
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("failed to create extract dir {}", dest_dir.display()))?;
    tar_reader
        .unpack(dest_dir)
        .with_context(|| format!("failed to extract {} into {}", archive.display(), dest_dir.display()))?;

    fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                out.push(path);
            } else if metadata.is_dir() {
                collect_files(&path, out)?;
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect_files(dest_dir, &mut files)?;
    if files.is_empty() {
        return Err(invalid_input_error(format!("tarball {} contained no regular files", archive.display())));
    }

    // 1. Exact basename match (with or without `.exe`).
    if let Some(matched) = files.iter().find(|path| {
        let Some(base) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        base.eq_ignore_ascii_case(expected_name) || base.eq_ignore_ascii_case(&format!("{expected_name}.exe"))
    }) {
        return Ok(matched.clone());
    }

    // 2. Sole executable.
    let executables: Vec<&PathBuf> = files.iter().filter(|p| is_executable_file(p)).collect();
    if executables.len() == 1 {
        return Ok(executables[0].clone());
    }

    // 3. Ambiguous — list every file we extracted so operators can see why
    //    we refused to guess.
    let names: Vec<String> = files
        .iter()
        .filter_map(|p| p.strip_prefix(dest_dir).ok().and_then(|rel| rel.to_str()).map(str::to_string))
        .collect();
    Err(invalid_input_error(format!(
        "could not determine which file is the plugin binary in {}; expected one named '{}'. Extracted files: [{}]",
        archive.display(),
        expected_name,
        names.join(", ")
    )))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    // On non-unix targets the executable bit isn't a stable signal — fall
    // back to existence + regular-file. Selection then relies on the
    // basename-match path (which is the common case for our releases).
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Result of resolving a public-repo install source.
#[derive(Debug)]
struct ReleaseInstall {
    binary_path: PathBuf,
    plugin_name_hint: String,
    asset_name: String,
    release_tag: String,
    /// 40-hex commit sha the release resolved to, when its `target_commitish`
    /// was a sha (not a branch name). `None` otherwise.
    resolved_commit: Option<String>,
    origin: String,
    sha256_verified: bool,
    /// Downloaded asset archive (`.tar.gz` etc.) — what cosign signed.
    asset_archive_path: Option<PathBuf>,
    /// Local path of the `.bundle` sidecar, when one was published alongside the asset.
    bundle_path: Option<PathBuf>,
    /// `<owner>` of the GitHub repo, for identity matching.
    owner: String,
    /// `<repo>` of the GitHub repo, for identity matching.
    repo: String,
    /// Full set of release assets — retained so secondary-binary installs
    /// can pick sibling `<binary>-<target>.tar.gz` archives without a
    /// second API roundtrip. Populated only on the release source path.
    release_assets: Vec<GithubReleaseAsset>,
    /// Per-target archive sha256, derived from the release `SHA256SUMS.txt`
    /// (`triple -> tarball sha`). Records EVERY published platform so a lock
    /// generated on one platform drives a verified `--locked` install on
    /// another. Empty when the release publishes no `SHA256SUMS.txt`.
    sha256sums_targets: BTreeMap<String, String>,
    /// Raw `SHA256SUMS.txt` body, retained so secondary (multi-binary) installs
    /// can derive THEIR per-target archive shas without a second fetch. `None`
    /// when the release publishes no sums file.
    sha256sums_body: Option<String>,
    /// RAII guard for the staging directory the asset was downloaded into.
    /// All paths above point inside this guard's directory; the caller
    /// must keep `_temp_dir` alive until the binary has been copied to
    /// its final home. Dropping the guard recursively removes the staging
    /// dir — closes the `$TMPDIR/animus-plugin-install-*` leak.
    _temp_dir: tempfile::TempDir,
}

async fn resolve_release_install(
    spec: RepoSpec,
    explicit_tag: Option<String>,
    expected_archive_sha256: Option<String>,
) -> Result<ReleaseInstall> {
    let tag = match (spec.tag.clone(), explicit_tag) {
        (Some(spec_tag), Some(flag_tag)) => {
            if spec_tag != flag_tag {
                return Err(invalid_input_error(format!(
                    "conflicting tag: positional says '{spec_tag}', --tag says '{flag_tag}'"
                )));
            }
            Some(spec_tag)
        }
        (Some(tag), None) | (None, Some(tag)) => Some(tag),
        (None, None) => None,
    };

    let release = fetch_github_release(&spec.owner, &spec.repo, tag.as_deref()).await?;
    let platform_tokens = current_platform_tokens();
    if platform_tokens.is_empty() {
        return Err(invalid_input_error(format!(
            "current platform '{}' is not supported by `animus plugin install` (no asset selectors registered)",
            current_platform_label()
        )));
    }

    // Prefer assets whose name starts with `<repo>-` (the canonical plugin
    // archive naming) so multi-binary releases — which publish sibling
    // `<other-bin>-<target>.tar.gz` archives in the same release — don't
    // accidentally pick a secondary binary as the primary based on GitHub's
    // asset ordering. Fall through to the legacy prefix-agnostic picker for
    // back-compat with releases that use a different archive base name.
    let asset = pick_release_asset_for_binary(&release.assets, &spec.repo, platform_tokens)
        .or_else(|| pick_release_asset(&release.assets, platform_tokens))
        .ok_or_else(|| {
            let available: Vec<String> = release.assets.iter().map(|a| a.name.clone()).collect();
            invalid_input_error(format!(
                "no release asset matched current platform '{}' (looked for any of: {}). Available assets in {}: [{}]",
                current_platform_label(),
                platform_tokens.join(", "),
                release.tag_name,
                available.join(", ")
            ))
        })?;

    // RAII staging dir — drops when `ReleaseInstall` drops. Replaces the
    // pre-fix `std::env::temp_dir().join(uuid)` that was created and
    // never cleaned up, accumulating GBs of orphaned staging dirs under
    // `$TMPDIR` over time.
    let temp_dir = create_install_staging_dir()?;
    let temp_path = temp_dir.path().to_path_buf();

    let asset_path = temp_path.join(&asset.name);
    download_to_path(&asset.browser_download_url, &asset_path).await?;

    // Fetch the release `SHA256SUMS.txt` once and record the tarball sha for
    // EVERY published platform. This is what makes the lockfile portable: a
    // macOS install records the linux (etc.) tarball sha too, so `--locked`
    // can verify on a fresh linux container before extracting. Best-effort: a
    // release without `SHA256SUMS.txt` falls back to the per-asset
    // sidecar/digest path below (and records only the current target).
    let mut sha256sums_targets: BTreeMap<String, String> = BTreeMap::new();
    let mut sha256sums_body: Option<String> = None;
    if let Some(sums_asset) = find_sha256sums_asset(&release.assets) {
        match download_text(&sums_asset.browser_download_url).await {
            Ok(body) => {
                sha256sums_targets = parse_sha256sums_for_targets(&body, &spec.repo);
                sha256sums_body = Some(body);
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to download '{}': {}; recording only the current target",
                    sums_asset.name, err
                );
            }
        }
    }

    // The target triple the SELECTED asset actually targets. `current_platform_tokens`
    // allows compatible fallbacks (e.g. a musl archive on a gnu host), so the
    // selected asset's triple can differ from the build triple — key the
    // SHA256SUMS lookup + the recorded claim off the asset that was downloaded,
    // not the build triple, otherwise verification is skipped and the sha is
    // recorded under the wrong target (codex P2).
    let selected_target = target_triple_from_asset_name(&asset.name).or_else(current_target_triple);

    // Resolve expected SHA256 for the CURRENT asset (the TARBALL):
    // caller-pinned `expected_archive_sha256` (the `--locked` lock pin) >
    // SHA256SUMS (selected target) > sidecar asset > release `digest` field.
    // The caller pin wins so a `--locked` run aborts BEFORE extracting when
    // the published tarball drifted from the committed archive sha — even if
    // the release's own SHA256SUMS was regenerated to match the drift.
    let mut expected_sha: Option<String> = expected_archive_sha256
        .clone()
        .or_else(|| selected_target.and_then(|triple| sha256sums_targets.get(triple).cloned()));
    if expected_sha.is_none() {
        if let Some(sidecar_asset) = find_sha256_sidecar(&release.assets, &asset.name) {
            match download_text(&sidecar_asset.browser_download_url).await {
                Ok(body) => {
                    if let Some(hex) = parse_sha256_sidecar(&body) {
                        expected_sha = Some(hex);
                    } else {
                        eprintln!(
                            "warning: sha256 sidecar '{}' had unexpected format; skipping verification",
                            sidecar_asset.name
                        );
                    }
                }
                Err(err) => {
                    eprintln!("warning: failed to download sha256 sidecar '{}': {}", sidecar_asset.name, err);
                }
            }
        }
    }
    if expected_sha.is_none() {
        if let Some(digest) = asset.digest.as_deref() {
            if let Some(hex) = parse_release_digest(digest) {
                expected_sha = Some(hex);
            }
        }
    }

    let mut sha256_verified = false;
    let computed = sha256_of_file(&asset_path)?;
    if let Some(expected) = expected_sha.as_ref() {
        if !expected.eq_ignore_ascii_case(&computed) {
            return Err(invalid_input_error(format!(
                "sha256 mismatch for asset '{}': expected {expected}, computed {computed}",
                asset.name
            )));
        }
        sha256_verified = true;
    } else {
        eprintln!(
            "warning: no sha256 sidecar or digest for asset '{}'; install proceeding without checksum verification",
            asset.name
        );
    }

    // Guarantee the lockfile records at least the SELECTED asset's target
    // archive sha, even for a release without a `SHA256SUMS.txt` (the
    // per-target map is otherwise empty). The current asset is the tarball we
    // just verified (or hashed), so its `computed` sha IS that target's
    // archive sha. Keyed off the selected asset's triple so a fallback archive
    // (e.g. musl on a gnu host) is recorded under the triple it actually is.
    if let Some(triple) = selected_target {
        sha256sums_targets.entry(triple.to_string()).or_insert_with(|| computed.clone());
    }

    // Extract if tarball; otherwise treat as a bare binary.
    // The expected plugin binary basename is the GitHub repo name —
    // releases publish `animus-provider-foo-<target>.tar.gz` containing a
    // single binary named `animus-provider-foo`. Passing the repo name
    // lets `extract_tarball` deterministically reject tarballs that ship
    // README/LICENSE alongside the binary instead of installing whatever
    // happened to come back first in walk order.
    let lower = asset.name.to_ascii_lowercase();
    // `lower` is already ASCII-lowercased above, so the `ends_with` checks are case-insensitive
    // by construction; clippy's heuristic doesn't see the prior normalization.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    let binary_path = if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let extract_dir = temp_path.join("extracted");
        extract_tarball(&asset_path, &extract_dir, &spec.repo)?
    } else {
        asset_path.clone()
    };

    // Download the cosign signature bundle if one is published. Bundles match
    // the original archive (not the extracted binary).
    let bundle_path = match find_bundle_sidecar(&release.assets, &asset.name) {
        Some(bundle_asset) => {
            let local = temp_path.join(&bundle_asset.name);
            match download_to_path(&bundle_asset.browser_download_url, &local).await {
                Ok(()) => Some(local),
                Err(err) => {
                    eprintln!("warning: failed to download cosign bundle '{}': {}", bundle_asset.name, err);
                    None
                }
            }
        }
        None => None,
    };

    let plugin_name_hint = binary_path
        .file_name()
        .and_then(|f| f.to_str())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| spec.repo.clone());

    Ok(ReleaseInstall {
        binary_path,
        plugin_name_hint,
        asset_name: asset.name.clone(),
        release_tag: release.tag_name.clone(),
        resolved_commit: release.target_commitish.as_deref().filter(|c| is_commit_sha(c)).map(str::to_string),
        origin: format!("{}/{}@{}", spec.owner, spec.repo, release.tag_name),
        sha256_verified,
        asset_archive_path: Some(asset_path.clone()),
        bundle_path,
        owner: spec.owner.clone(),
        repo: spec.repo.clone(),
        release_assets: release.assets.clone(),
        sha256sums_targets,
        sha256sums_body,
        _temp_dir: temp_dir,
    })
}

/// Look up the cosign signature bundle (`<asset>.bundle`) in the release
/// assets, if present.
fn find_bundle_sidecar<'a>(assets: &'a [GithubReleaseAsset], asset_name: &str) -> Option<&'a GithubReleaseAsset> {
    let bundle_name = format!("{asset_name}.bundle");
    assets.iter().find(|a| a.name.eq_ignore_ascii_case(&bundle_name))
}

#[derive(Debug, Clone)]
pub(crate) struct BinaryDescriptor {
    pub(crate) name: String,
    pub(crate) primary: bool,
}

pub(crate) fn parse_plugin_toml_binaries(toml_text: &str) -> Result<Vec<BinaryDescriptor>> {
    #[derive(serde::Deserialize)]
    struct PluginTomlBinary {
        name: String,
        #[serde(default)]
        primary: bool,
    }

    #[derive(serde::Deserialize)]
    struct PluginTomlRoot {
        #[serde(default)]
        binaries: Option<Vec<PluginTomlBinary>>,
        #[serde(default)]
        name: Option<String>,
    }

    let root: PluginTomlRoot = toml::from_str(toml_text).context("failed to parse plugin.toml")?;
    let Some(binaries) = root.binaries else {
        return Ok(Vec::new());
    };
    if binaries.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<BinaryDescriptor> = Vec::with_capacity(binaries.len());
    let mut primary_count = 0usize;
    for b in binaries {
        let name = b.name.trim().to_string();
        if name.is_empty() {
            return Err(invalid_input_error("plugin.toml [[binaries]] entry has empty `name`"));
        }
        if !seen.insert(name.clone()) {
            return Err(invalid_input_error(format!("plugin.toml [[binaries]] entry '{name}' appears more than once")));
        }
        if b.primary {
            primary_count += 1;
        }
        out.push(BinaryDescriptor { name, primary: b.primary });
    }
    if primary_count == 0 {
        if let Some(plugin_name) = root.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            if let Some(pos) = out.iter().position(|b| b.name == plugin_name) {
                out[pos].primary = true;
                primary_count = 1;
            }
        }
    }
    if primary_count == 0 {
        out[0].primary = true;
    } else if primary_count > 1 {
        return Err(invalid_input_error("plugin.toml declares more than one `primary = true` binary"));
    }
    Ok(out)
}

async fn fetch_plugin_toml_for_release(owner: &str, repo: &str, tag: &str) -> Result<Option<String>> {
    let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/{tag}/plugin.toml");
    let client =
        reqwest::Client::builder().user_agent(release_user_agent()).build().context("failed to build HTTP client")?;
    let response =
        client.get(&url).send().await.map_err(|err| unavailable_error(format!("failed to GET {url}: {err}")))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .map_err(|err| unavailable_error(format!("GET {url} returned non-success status: {err}")))?;
    let body =
        response.text().await.map_err(|err| unavailable_error(format!("failed to read body from {url}: {err}")))?;
    Ok(Some(body))
}

fn pick_release_asset_for_binary<'a>(
    assets: &'a [GithubReleaseAsset],
    binary_name: &str,
    platform_tokens: &[&str],
) -> Option<&'a GithubReleaseAsset> {
    let prefix_lower = format!("{}-", binary_name.to_ascii_lowercase());
    for token in platform_tokens {
        let needle = token.to_ascii_lowercase();
        for asset in assets {
            let lower = asset.name.to_ascii_lowercase();
            if lower.ends_with(".sha256") || lower.ends_with(".sha256sum") || lower.ends_with(".bundle") {
                continue;
            }
            if !lower.starts_with(&prefix_lower) {
                continue;
            }
            if lower.contains(&needle) {
                return Some(asset);
            }
        }
    }
    None
}

/// Regex-escape a GitHub owner or repo segment so it can be embedded in
/// a `cosign --certificate-identity-regexp` pattern without leaking regex
/// metacharacters. GitHub slugs are restricted to `[A-Za-z0-9._-]`, all of
/// which are safe to pass through; this helper exists purely as a
/// defense-in-depth guard against a future slug rule change.
fn regex_escape_for_identity(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Bridge between [`orchestrator_plugin_host::VerificationResult`] (the
/// policy-aware result type) and the CLI-internal [`SignatureStatus`]
/// that's persisted in `plugins.yaml` and the install envelope. Used
/// when `resolve_signature_status` routes through the plugin-host's
/// strict `TrustedPublisher` keyless verifier (e.g. for `launchapp-dev`
/// owners).
fn map_host_result_to_status(
    result: orchestrator_plugin_host::VerificationResult,
    bundle_path: &Path,
) -> SignatureStatus {
    use orchestrator_plugin_host::VerificationResult as VR;
    match result {
        VR::Verified { identity, bundle_path: _ } => {
            SignatureStatus::Verified { identity, bundle_path: bundle_path.display().to_string() }
        }
        VR::Unsigned { reason } => SignatureStatus::Unsigned { reason },
        VR::Invalid { identity_pattern, message } => SignatureStatus::Invalid { identity_pattern, message },
        VR::UntrustedSigner { identity_pattern } => SignatureStatus::UntrustedSigner { identity_pattern },
        VR::Skipped => SignatureStatus::Skipped,
    }
}

/// Compute the effective [`PluginPolicyMode`] for an install request.
///
/// Precedence:
/// 1. `req.signature_policy` (the `--signature-policy` flag).
/// 2. `--skip-signature` -> `Disabled`.
/// 3. `--require-signature` -> `Strict`.
/// 4. Fallback: `Warn` (verify-if-present). Matches the library default in
///    [`PluginPolicyMode::default_for_install`]. The CLI handler flows
///    through the same value so direct callers (unit tests, MCP wire) and
///    CLI users agree. See `docs/reference/security.md` for the rationale
///    and the recommended `strict` opt-in for production.
fn effective_policy_mode(req: &PluginInstallRequest) -> PluginPolicyMode {
    if let Some(mode) = req.signature_policy {
        return mode;
    }
    if req.skip_signature {
        return PluginPolicyMode::Disabled;
    }
    if req.require_signature {
        return PluginPolicyMode::Strict;
    }
    PluginPolicyMode::Warn
}

/// Outcome of applying the signature policy gate to a resolved
/// `SignatureStatus`. The install pipeline maps `Block` -> install error,
/// `ProceedWithWarning` -> tracing warn + stderr line, and `Proceed` -> no-op.
#[derive(Debug, PartialEq, Eq)]
enum SignaturePolicyOutcome {
    Proceed,
    ProceedWithWarning { reason: String },
    Block { reason: String },
}

/// Apply the signature policy to a resolved [`SignatureStatus`].
///
/// The policy gate is centralized here so every failure mode (`Invalid`,
/// `UntrustedSigner`, `Unsigned`) routes through the SAME strict/warn/disabled
/// matrix. Prior to v0.4.12 codex round 5, `Invalid` and `UntrustedSigner`
/// bypassed `policy_mode` entirely — even `--signature-policy warn` failed
/// the install. With the launchapp-dev cosign key still a placeholder, that
/// turned every signed-but-unverifiable release into a hard block.
///
/// Strict (or legacy `require_signature=true`): every non-verified status
/// blocks.
/// Warn: log and proceed. Disabled / Skipped / Verified: silently proceed.
fn evaluate_signature_policy(
    status: &SignatureStatus,
    policy_mode: PluginPolicyMode,
    require_signature: bool,
) -> SignaturePolicyOutcome {
    let strict = matches!(policy_mode, PluginPolicyMode::Strict) || require_signature;
    match status {
        SignatureStatus::Invalid { message, .. } if strict => SignaturePolicyOutcome::Block {
            reason: format!("cosign signature verification FAILED; refusing install: {message}"),
        },
        SignatureStatus::Invalid { message, .. } if matches!(policy_mode, PluginPolicyMode::Warn) => {
            SignaturePolicyOutcome::ProceedWithWarning {
                reason: format!("plugin install proceeding with INVALID cosign signature ({message})"),
            }
        }
        SignatureStatus::UntrustedSigner { identity_pattern } if strict => SignaturePolicyOutcome::Block {
            reason: format!(
                "signature is valid but the signer is not in trusted-signers.yaml (identity pattern: {identity_pattern})"
            ),
        },
        SignatureStatus::UntrustedSigner { identity_pattern } if matches!(policy_mode, PluginPolicyMode::Warn) => {
            SignaturePolicyOutcome::ProceedWithWarning {
                reason: format!(
                    "plugin install proceeding with untrusted signer (identity pattern: {identity_pattern})"
                ),
            }
        }
        SignatureStatus::Unsigned { reason } if strict => SignaturePolicyOutcome::Block {
            reason: format!(
                "signature policy is strict but no cosign signature could be verified: {reason}\n\
                 To proceed anyway, pass --allow-unsigned (warn) or --signature-policy disabled."
            ),
        },
        SignatureStatus::Unsigned { reason } if matches!(policy_mode, PluginPolicyMode::Warn) => {
            SignaturePolicyOutcome::ProceedWithWarning {
                reason: format!("plugin install proceeding without verified signature ({reason})"),
            }
        }
        _ => SignaturePolicyOutcome::Proceed,
    }
}

/// Verify the cosign signature for the install source (if any), apply the
/// trusted-signers policy, and return the resulting [`SignatureStatus`]. The
/// caller is responsible for turning hard-fail statuses (`Invalid`,
/// `UntrustedSigner`, `Unsigned` under `Strict`) into install errors.
fn resolve_signature_status(req: &PluginInstallRequest, provenance: &InstallProvenance) -> Result<SignatureStatus> {
    if matches!(effective_policy_mode(req), PluginPolicyMode::Disabled) {
        return Ok(SignatureStatus::Skipped);
    }
    if req.skip_signature {
        return Ok(SignatureStatus::Skipped);
    }

    let (Some(asset_archive), Some(bundle_path)) =
        (provenance.asset_archive_path.as_deref(), provenance.bundle_path.as_deref())
    else {
        return Ok(SignatureStatus::Unsigned {
            reason: match provenance.source_kind {
                Some("release") => "no cosign signature bundle published in release".to_string(),
                Some("path") => "local --path install; cosign signatures only apply to release assets".to_string(),
                Some("url") => "direct --url install; cosign signatures only apply to release assets".to_string(),
                _ => "no signature context available for this install source".to_string(),
            },
        });
    };

    let signers_path = resolve_trusted_signers_path(req.trusted_signers.as_deref());
    let trusted = load_trusted_signers(&signers_path)?;
    let identity_regex = if let (Some(owner), Some(repo)) = (provenance.owner.as_deref(), provenance.repo.as_deref()) {
        let cfg = trusted.clone().unwrap_or_default();
        Some(cfg.identity_regexp_for(owner, repo))
    } else {
        None
    };

    if req.trust_key.is_some() {
        tracing::warn!(
            "--trust-key is deprecated as of v0.4.12 and has no effect: keyless cosign verification uses \
             --signature-policy plus the built-in TrustedPublisher list (launchapp-dev keyless). The flag \
             will be removed in a future release."
        );
    }

    // Trusted-publisher path: when the install owner is in the host's
    // `SignaturePolicy::default_install()` trusted-publisher list, delegate
    // to the host's strict keyless verifier. This is the ONLY path that
    // anchors verification to the `/.github/workflows/release.yml@refs/tags/v*`
    // identity regex; the legacy per-spec `verify_with_cosign` fallback below
    // uses a much weaker `^https://github\.com/<owner>/<repo>/.+` pattern
    // that would accept signatures from any workflow on any ref.
    if let (Some(owner), Some(repo)) = (provenance.owner.as_deref(), provenance.repo.as_deref()) {
        let org_publisher = orchestrator_plugin_host::TrustedPublisher::launchapp_dev();
        if org_publisher.owner == owner {
            // Narrow the org-wide TrustedPublisher regex to the SPECIFIC repo
            // the operator asked to install. The lib's launchapp-dev regex
            // accepts any `launchapp-dev/[^/]+/.../release.yml@refs/tags/v.*`,
            // which would let cosign verify a bundle signed by a different
            // launchapp-dev repo against the install for `animus-provider-claude`.
            // Pinning the repo segment here closes that hole while keeping the
            // workflow URI + tag anchors the lib enforces.
            let pinned_regex = format!(
                "^https://github\\.com/{}/{}/\\.github/workflows/release\\.yml@refs/tags/v.*",
                regex_escape_for_identity(owner),
                regex_escape_for_identity(repo)
            );
            let pinned_publisher = orchestrator_plugin_host::TrustedPublisher {
                owner: org_publisher.owner.clone(),
                identity_regex: pinned_regex,
                oidc_issuer: org_publisher.oidc_issuer.clone(),
            };
            let host_policy = orchestrator_plugin_host::SignaturePolicy {
                mode: match effective_policy_mode(req) {
                    PluginPolicyMode::Strict => orchestrator_plugin_host::PolicyMode::Strict,
                    PluginPolicyMode::Warn => orchestrator_plugin_host::PolicyMode::Warn,
                    PluginPolicyMode::Disabled => orchestrator_plugin_host::PolicyMode::Disabled,
                },
                trusted_publishers: vec![pinned_publisher],
                allow_unsigned_for: Vec::new(),
            };
            let repo_spec = format!("{owner}/{repo}");
            let host_result = orchestrator_plugin_host::verify_plugin_install(
                &repo_spec,
                asset_archive,
                Some(bundle_path),
                &host_policy,
            )?;
            let mapped = map_host_result_to_status(host_result, bundle_path);
            // Re-apply the operator's `trusted-signers.yaml` allowlist on top of
            // the host's TrustedPublisher verdict — even with the pinned regex,
            // an operator may have narrowed `trusted-signers.yaml` to a subset
            // of launchapp-dev repos and that allowlist must still bind.
            if let SignatureStatus::Verified { .. } = &mapped {
                if let Some(cfg) = trusted.as_ref() {
                    let slug = format!("{owner}/{repo}");
                    if !cfg.matches_repo(&slug) {
                        return Ok(SignatureStatus::UntrustedSigner {
                            identity_pattern: identity_regex.unwrap_or_else(|| ".*".to_string()),
                        });
                    }
                }
            }
            return Ok(mapped);
        }
    }

    if !cosign_available() {
        let mode = effective_policy_mode(req);
        let suffix = if matches!(mode, PluginPolicyMode::Strict) {
            " (signature policy is strict; install cosign or rerun with --signature-policy warn/disabled)"
        } else {
            ""
        };
        return Ok(SignatureStatus::Unsigned {
            reason: format!(
                "cosign binary not found on PATH; install cosign from https://github.com/sigstore/cosign to enable signature verification{suffix}"
            ),
        });
    }

    let status = verify_with_cosign(asset_archive, bundle_path, identity_regex.as_deref(), GITHUB_OIDC_ISSUER)?;
    if let SignatureStatus::Verified { .. } = &status {
        if let Some(cfg) = trusted.as_ref() {
            if let (Some(owner), Some(repo)) = (provenance.owner.as_deref(), provenance.repo.as_deref()) {
                let slug = format!("{owner}/{repo}");
                if !cfg.matches_repo(&slug) {
                    return Ok(SignatureStatus::UntrustedSigner {
                        identity_pattern: identity_regex.unwrap_or_else(|| ".*".to_string()),
                    });
                }
            }
        }
    }
    Ok(status)
}

/// Provenance attached to a resolved install source. Recorded in the registry
/// and surfaced in the install output.
#[derive(Debug, Default)]
struct InstallProvenance {
    source_kind: Option<&'static str>,
    origin: Option<String>,
    release_tag: Option<String>,
    asset_name: Option<String>,
    /// `Some(true)` if checksum verification ran and passed during resolution.
    sha256_verified: Option<bool>,
    /// Path to the archive that cosign signs (the `.tar.gz`, not the extracted binary).
    asset_archive_path: Option<PathBuf>,
    /// Local path to the cosign `.bundle`, when published.
    bundle_path: Option<PathBuf>,
    /// `<owner>` for identity-regex construction.
    owner: Option<String>,
    /// `<repo>` for identity-regex construction.
    repo: Option<String>,
    /// Where this install came from, recorded into the lockfile's
    /// `source_repo`: an `owner/repo` slug for release installs, the URL for
    /// `--url`, or `path:<...>` for `--path`. Drives `plugin install --locked`
    /// reproducibility.
    source_repo: Option<String>,
    /// 40-hex commit sha the release resolved to (release source only),
    /// recorded into the lockfile's `resolved_commit`.
    resolved_commit: Option<String>,
    /// Per-target archive sha256 from the release `SHA256SUMS.txt`
    /// (`triple -> tarball sha`), for the PRIMARY asset. Populated only on the
    /// release source path; drives the lockfile's portable `targets` claim.
    sha256sums_targets: BTreeMap<String, String>,
    /// Raw `SHA256SUMS.txt` body, so secondary (multi-binary) lock entries can
    /// derive their OWN per-target archive shas (keeping them portable too).
    /// `None` for non-release sources or releases without a sums file.
    sha256sums_body: Option<String>,
}

/// Probe a plugin binary's `--manifest` output without touching the install
/// directory. Used to validate identity (name vs repo) and policy
/// (reserved-provider-tool) before the install commits.
fn probe_manifest(binary_path: &Path) -> Result<PluginManifest> {
    let output = std::process::Command::new(binary_path)
        .arg("--manifest")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run {} --manifest", binary_path.display()))?;
    if !output.status.success() {
        return Err(invalid_input_error(format!(
            "binary failed --manifest probe (exit={:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    serde_json::from_slice::<PluginManifest>(&output.stdout).map_err(|err| {
        invalid_input_error(format!("plugin {} returned malformed --manifest JSON: {err}", binary_path.display()))
    })
}

/// Refuse provider plugins whose manifest name (or `animus-provider-*` suffix)
/// claims one of the in-tree `RESERVED_PROVIDER_TOOLS`. A misconfigured or
/// malicious plugin can otherwise replace the entire `claude` / `codex` /
/// `gemini` / `opencode` / `oai-runner` dispatch path without warning.
fn enforce_provider_tool_policy(manifest: &PluginManifest, allow_shadow_builtin: bool) -> Result<()> {
    if manifest.plugin_kind != animus_plugin_protocol::PLUGIN_KIND_PROVIDER {
        return Ok(());
    }
    let derived_tool = manifest.name.strip_prefix("animus-provider-").unwrap_or(manifest.name.as_str());
    if !is_reserved_provider_tool(derived_tool) {
        return Ok(());
    }
    if allow_shadow_builtin {
        tracing::warn!(
            plugin = %manifest.name,
            tool = %derived_tool,
            "installing plugin that shadows the in-tree '{}' backend (--allow-shadow-builtin)",
            derived_tool,
        );
        return Ok(());
    }
    Err(invalid_input_error(format!(
        "plugin '{}' resolves to provider_tool '{}', which is a reserved in-tree backend \
         (claude / codex / gemini / opencode / oai-runner). Installing it would silently \
         override the built-in dispatch for that tool. Pass --allow-shadow-builtin to proceed.",
        manifest.name, derived_tool
    )))
}

/// Refuse installs whose published manifest name disagrees with the GitHub
/// repo basename it was downloaded from. This is the most common shape of a
/// supply-chain typosquat (`evil-org/animus-provider-claude` shipping a binary
/// whose manifest is `animus-provider-claude` from `launchapp-dev`).
fn enforce_manifest_name_matches_repo(manifest: &PluginManifest, _owner: &str, repo: &str, force: bool) -> Result<()> {
    if manifest.name == repo {
        return Ok(());
    }
    let message = format!(
        "manifest name '{}' does not match repo basename '{}' — this may be a typosquat or supply-chain attack. \
         Pass --force to install anyway.",
        manifest.name, repo
    );
    if force {
        tracing::warn!(
            manifest_name = %manifest.name,
            repo = %repo,
            "installing plugin with manifest/repo basename mismatch (--force)"
        );
        return Ok(());
    }
    Err(invalid_input_error(message))
}

/// Path to the trusted-orgs allowlist used by `animus plugin install`.
///
/// Honors `$ANIMUS_TRUSTED_ORGS` first, then falls back to
/// `<animus_home>/trusted-orgs.yaml`.
fn trusted_orgs_path() -> PathBuf {
    if let Ok(value) = std::env::var("ANIMUS_TRUSTED_ORGS") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let base = match std::env::var("ANIMUS_CONFIG_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => home.join(".animus"),
    };
    base.join("trusted-orgs.yaml")
}

/// Built-in trusted orgs. Pre-populated with `launchapp-dev` so a fresh
/// install gets a safe default for the canonical animus plugins. The built-in
/// org cannot be revoked via `animus plugin revoke-trust`.
const BUILTIN_TRUSTED_ORGS: &[&str] = &["launchapp-dev"];

/// How a TOFU trust decision was made. Recorded per-entry in
/// `trusted-orgs.yaml` so the audit trail explains why an org is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TrustDecision {
    /// Operator typed `yes` at the interactive TOFU prompt.
    InteractivePrompt,
    /// `--yes` / `--force` auto-confirmed the prompt non-interactively.
    Yes,
    /// Pre-trusted via `--allow-org <OWNER>`.
    AllowOrg,
    /// Ships in `BUILTIN_TRUSTED_ORGS` (never persisted, surfaced in listings).
    BuiltIn,
}

impl TrustDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::InteractivePrompt => "interactive-prompt",
            Self::Yes => "yes",
            Self::AllowOrg => "allow-org",
            Self::BuiltIn => "built-in",
        }
    }
}

/// A single trusted-org audit record. New entries always serialize the rich
/// shape; the loader still accepts the legacy bare-string format for
/// back-compat (see [`OrgEntry`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TrustedOrgRecord {
    /// GitHub owner/org slug.
    org: String,
    /// RFC3339 timestamp of when trust was first granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trusted_at: Option<String>,
    /// How the trust decision was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decided_by: Option<TrustDecision>,
    /// The `owner/repo` whose install first triggered the trust prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_plugin: Option<String>,
    /// RFC3339 timestamp of revocation. When `Some`, the org is NOT trusted —
    /// the record survives as a tombstone so re-trusting re-prompts and the
    /// audit trail is preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<String>,
}

impl TrustedOrgRecord {
    fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// On-disk entry: either the legacy bare string (`- some-org`) or the rich
/// record. Untagged so serde tries the map shape first, then the string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum OrgEntry {
    Rich(TrustedOrgRecord),
    Legacy(String),
}

impl OrgEntry {
    fn into_record(self) -> TrustedOrgRecord {
        match self {
            Self::Rich(r) => r,
            Self::Legacy(org) => {
                TrustedOrgRecord { org, trusted_at: None, decided_by: None, first_plugin: None, revoked_at: None }
            }
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TrustedOrgsConfig {
    #[serde(default)]
    trusted_orgs: Vec<OrgEntry>,
}

impl TrustedOrgsConfig {
    /// Normalize on-disk entries into rich records.
    fn records(&self) -> Vec<TrustedOrgRecord> {
        self.trusted_orgs.iter().cloned().map(OrgEntry::into_record).collect()
    }

    /// Find the index of the (first) entry for `owner`, case-insensitive.
    fn position(&self, owner: &str) -> Option<usize> {
        self.trusted_orgs.iter().position(|e| {
            let org = match e {
                OrgEntry::Rich(r) => r.org.as_str(),
                OrgEntry::Legacy(s) => s.as_str(),
            };
            org.eq_ignore_ascii_case(owner)
        })
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn load_trusted_orgs() -> Result<TrustedOrgsConfig> {
    let path = trusted_orgs_path();
    if !path.exists() {
        return Ok(TrustedOrgsConfig::default());
    }
    let contents = std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: TrustedOrgsConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse {} as TrustedOrgsConfig", path.display()))?;
    Ok(parsed)
}

fn save_trusted_orgs(config: &TrustedOrgsConfig) -> Result<()> {
    let path = trusted_orgs_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create trusted-orgs dir {}", parent.display()))?;
    }
    let serialized = serde_yaml::to_string(config).context("failed to serialize trusted-orgs.yaml")?;
    std::fs::write(&path, serialized).with_context(|| format!("failed to write {}", path.display()))
}

/// Record trust for `owner` on disk with rich audit metadata. Idempotent for
/// already-active entries; re-grants a previously revoked org by clearing its
/// tombstone and stamping a fresh decision. Built-in orgs are never persisted.
fn add_trusted_org(owner: &str, decision: TrustDecision, first_plugin: Option<&str>) -> Result<()> {
    let trimmed = owner.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if BUILTIN_TRUSTED_ORGS.iter().any(|o| o.eq_ignore_ascii_case(trimmed)) {
        return Ok(());
    }
    let mut config = load_trusted_orgs()?;
    let fresh = TrustedOrgRecord {
        org: trimmed.to_string(),
        trusted_at: Some(now_rfc3339()),
        decided_by: Some(decision),
        first_plugin: first_plugin.map(str::to_string),
        revoked_at: None,
    };
    match config.position(trimmed) {
        Some(idx) => {
            // Upgrade legacy bare-string entries to rich records and clear any
            // tombstone (re-trust). Preserve the existing record otherwise so a
            // benign repeat install doesn't churn the timestamp.
            let existing = config.trusted_orgs[idx].clone().into_record();
            if existing.is_active() && existing.trusted_at.is_some() {
                return Ok(());
            }
            config.trusted_orgs[idx] = OrgEntry::Rich(fresh);
        }
        None => config.trusted_orgs.push(OrgEntry::Rich(fresh)),
    }
    save_trusted_orgs(&config)
}

/// Remove trust for `owner`, recording a tombstone (`revoked_at`) so the audit
/// trail survives and a re-trust re-prompts. Built-in orgs cannot be revoked.
/// Returns the revoked record on success.
fn revoke_trusted_org(owner: &str) -> Result<TrustedOrgRecord> {
    let trimmed = owner.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("organization name cannot be empty"));
    }
    if BUILTIN_TRUSTED_ORGS.iter().any(|o| o.eq_ignore_ascii_case(trimmed)) {
        return Err(invalid_input_error(format!(
            "'{trimmed}' is a built-in trusted org and cannot be revoked. It is the trust anchor for the \
             canonical Animus plugins; revoking it would break `animus plugin install-defaults`."
        )));
    }
    let mut config = load_trusted_orgs()?;
    let Some(idx) = config.position(trimmed) else {
        return Err(invalid_input_error(format!(
            "org '{trimmed}' is not in the trusted-orgs allowlist; nothing to revoke"
        )));
    };
    let mut record = config.trusted_orgs[idx].clone().into_record();
    if !record.is_active() {
        return Err(invalid_input_error(format!("org '{trimmed}' is already revoked")));
    }
    record.revoked_at = Some(now_rfc3339());
    config.trusted_orgs[idx] = OrgEntry::Rich(record.clone());
    save_trusted_orgs(&config)?;
    Ok(record)
}

/// Returns `Some(record)` describing the active trust grant for `owner`, or
/// `None` when the org is untrusted (never seen, or tombstoned).
fn trusted_org_record(owner: &str) -> Result<Option<TrustedOrgRecord>> {
    if BUILTIN_TRUSTED_ORGS.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return Ok(Some(TrustedOrgRecord {
            org: owner.to_string(),
            trusted_at: None,
            decided_by: Some(TrustDecision::BuiltIn),
            first_plugin: None,
            revoked_at: None,
        }));
    }
    let config = load_trusted_orgs()?;
    Ok(config.records().into_iter().find(|r| r.org.eq_ignore_ascii_case(owner) && r.is_active()))
}

fn org_is_trusted(owner: &str) -> Result<bool> {
    Ok(trusted_org_record(owner)?.is_some())
}

/// Implements the trust-on-first-use prompt for installs from public-repo
/// sources. Pre-trusted orgs (`launchapp-dev` plus anything active in
/// `~/.animus/trusted-orgs.yaml`) skip the prompt entirely. Operators can
/// pre-trust additional orgs via `--allow-org`, or auto-confirm via `--yes`.
///
/// Returns the [`TrustDecision`] that admitted the install so the caller can
/// persist it and surface it in the install audit line. `None` means the org
/// was already trusted (no fresh decision to record).
fn enforce_org_trust(owner: &str, req: &PluginInstallRequest) -> Result<Option<TrustDecision>> {
    if req.allow_org.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return Ok(Some(TrustDecision::AllowOrg));
    }
    if org_is_trusted(owner)? {
        return Ok(None);
    }
    if req.yes || req.force {
        tracing::warn!(owner, "installing plugin from untrusted org (--yes / --force); recording trust on first use");
        return Ok(Some(TrustDecision::Yes));
    }
    if !std::io::stdin().is_terminal() {
        return Err(invalid_input_error(format!(
            "installing plugin from untrusted org '{owner}'. Pass --allow-org {owner} (or --yes) to confirm \
             non-interactively. trusted-orgs.yaml lives at {}.",
            trusted_orgs_path().display()
        )));
    }
    eprintln!(
        "warning: you are installing a plugin from `{owner}`, which is not a trusted organization.\n\
         Verify this is the intended publisher before continuing. Type 'yes' to trust this org \
         for future installs, anything else to abort."
    );
    eprint!("> ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).with_context(|| "failed to read TOFU response from stdin")?;
    let normalized = answer.trim().to_ascii_lowercase();
    if normalized == "yes" || normalized == "y" {
        Ok(Some(TrustDecision::InteractivePrompt))
    } else {
        Err(invalid_input_error(format!("user declined to trust org '{owner}'; aborting install")))
    }
}

async fn handle_plugin_install(args: PluginInstallArgs, project_root: &str, json: bool) -> Result<()> {
    if args.locked {
        if args.source.is_some() || args.path.is_some() || args.url.is_some() || args.tag.is_some() || args.latest {
            return Err(invalid_input_error(
                "--locked installs the set pinned in `.animus/plugins.lock` and is mutually exclusive with a \
                 positional source, --path, --url, --tag, and --latest",
            ));
        }
        return run_locked_install(args, project_root, json).await;
    }
    if args.latest && args.tag.is_some() {
        return Err(invalid_input_error("--latest and --tag are mutually exclusive"));
    }
    if (args.tag.is_some() || args.latest) && args.source.is_none() {
        return Err(invalid_input_error(
            "--tag and --latest only apply when installing from a public repo (positional OWNER/REPO[@TAG])",
        ));
    }

    let signature_policy = resolve_cli_signature_policy(&args)?;
    // Keyless verification (cosign --certificate-identity-regexp +
    // --certificate-oidc-issuer) needs no PEM trust seed — the trust anchor
    // is Sigstore Fulcio + Rekor, both built into the cosign binary. The
    // pre-v0.4.12 seed step (LAUNCHAPP_DEV_COSIGN_PUBLIC_KEY_PEM into
    // ~/.animus/trusted-keys/launchapp-dev.pem) is intentionally gone.
    let output = run_plugin_install(PluginInstallRequest {
        source: args.source,
        path: args.path,
        url: args.url,
        tag: args.tag,
        name: args.name,
        sha256: args.sha256,
        force: args.force,
        skip_manifest_check: args.skip_manifest_check,
        plugin_dir: args.plugin_dir,
        signature_policy: Some(signature_policy),
        trust_key: args.trust_key,
        require_signature: args.require_signature,
        skip_signature: args.skip_signature,
        trusted_signers: args.trusted_signers,
        allow_shadow_builtin: args.allow_shadow_builtin,
        allow_org: args.allow_org,
        yes: args.yes,
        project_root: Some(project_root.to_string()),
        force_rewrite_lockfile: args.force_rewrite_lockfile,
        as_kind: args.as_kind,
        project: args.project,
        expected_archive_sha256: None,
        locked_secondary_archive_shas: BTreeMap::new(),
    })
    .await?;
    let role = output
        .manifest
        .as_ref()
        .map(|m| plugin_role_from_kind(&m.plugin_kind))
        .unwrap_or(crate::services::metrics::PluginRole::Other);
    crate::services::metrics::record_event(
        std::path::Path::new(project_root),
        crate::services::metrics::EventTags::PluginInstalled { plugin_kind: role },
    );
    print_value(output, json)
}

#[derive(Debug, Serialize)]
struct LockedInstallRow {
    name: String,
    source_repo: String,
    version: String,
    expected_sha256: String,
    /// `installed` | `verified` | `failed`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct LockedInstallOutput {
    schema: &'static str,
    lockfile: String,
    installed: usize,
    failed: usize,
    rows: Vec<LockedInstallRow>,
}

/// Reproducible install: reinstall EXACTLY the set pinned in
/// `.animus/plugins.lock`, then verify each freshly installed artifact's
/// sha256 against the lockfile. Used by the CI / fresh-machine path
/// (`animus plugin install --locked`). Fails the whole run if the lockfile is
/// missing/empty, an entry cannot be reconstructed, or any installed artifact
/// hash drifts from the pin (the published release changed under the pin).
async fn run_locked_install(args: PluginInstallArgs, project_root: &str, json: bool) -> Result<()> {
    let root = std::path::Path::new(project_root);
    // Carry the operator's install-security flags through to every reinstall
    // so e.g. `--locked --signature-policy strict` actually enforces strict
    // signatures in CI rather than silently falling back to the default warn
    // policy (codex P2). The legacy `require_signature` / `skip_signature` are
    // folded into the resolved policy; resolution also validates conflicting
    // flag combinations up front.
    let signature_policy = resolve_cli_signature_policy(&args)?;
    let trusted_signers = args.trusted_signers.clone();
    let allow_shadow_builtin = args.allow_shadow_builtin;
    let lock_path = PluginLockfile::default_path(Some(root));
    if !lock_path.exists() {
        return Err(invalid_input_error(format!(
            "no plugin lockfile at {} — `--locked` reproduces a previously recorded set, so the lockfile must \
             exist. Install plugins normally first (e.g. `animus plugin install-defaults`) to record it.",
            lock_path.display()
        )));
    }
    let lockfile = PluginLockfile::load_or_empty(&lock_path)?;
    if lockfile.plugins.is_empty() {
        return Err(invalid_input_error(format!(
            "plugin lockfile {} is empty — nothing to install",
            lock_path.display()
        )));
    }

    // `--locked` REPRODUCES the committed pin; it must never rewrite it. The
    // reinstalls below route through `run_plugin_install`, which re-saves the
    // lockfile (including new secondary-binary entries) on success — so if a
    // secondary asset changed under the same tag, the pin would be silently
    // overwritten before verification fails (codex P1). Capture the committed
    // entries now and re-assert them afterward through the locked
    // `PluginLockfile::save()` path so the restore is serialized + merged with
    // any concurrent installer instead of clobbering its entry with a raw
    // byte rewrite (codex P2).
    let original_entries = lockfile.plugins.clone();
    let lock_path_for_restore = lock_path.clone();
    let restore_lock = move |entries: &[LockEntry]| {
        // Reload under the save-lock and re-assert ONLY the entries this
        // locked run owns, reverting any drifted values the reinstalls wrote.
        // `save()` holds the fs2 sidecar lock and merges on-disk entries this
        // snapshot never saw, so a concurrent installer's unrelated new entry
        // survives.
        match PluginLockfile::load_or_empty(&lock_path_for_restore) {
            Ok(mut reloaded) => {
                for entry in entries {
                    reloaded.upsert(entry.clone());
                }
                if let Err(err) = reloaded.save() {
                    tracing::warn!(path = %lock_path_for_restore.display(), %err, "failed to restore lockfile pins after --locked install");
                }
            }
            Err(err) => {
                tracing::warn!(path = %lock_path_for_restore.display(), %err, "failed to reload lockfile to restore pins after --locked install");
            }
        }
    };

    // Infer project scope when reproducing a PROJECT lockfile: a
    // `.animus/plugins.lock` committed by `animus plugin install --project`
    // must reinstall into `<project>/.animus/plugins/`, otherwise the locked
    // binaries land in the global dir while the committed project
    // `plugins.yaml` still points at the project dir — leaving daemon preflight
    // unsatisfied after a "verified" run (codex P2). An explicit
    // `--plugin-dir` override opts out (the operator chose the dir). `--project`
    // forces it regardless.
    let resolves_project_lock = lock_path == project_lockfile_path(root);
    let effective_project = args.project || (resolves_project_lock && args.plugin_dir.is_none());

    // Install dir the reinstalled binaries land in — verification re-hashes
    // each locked binary here against its pin.
    let install_dir =
        if effective_project { project_plugin_install_dir(root) } else { install_root(args.plugin_dir.as_deref())? };

    // A release reinstall reproduces ALL of its sibling binaries (multi-binary
    // releases publish secondary archives in the same release), so installing
    // once per `(source_repo, version)` group reproduces every locked entry
    // from that release. Reinstalling each row independently would replay the
    // PRIMARY release asset under a secondary entry's name and clobber it
    // (codex P1). `--url` / `path:` sources install per-entry. We track which
    // groups were already installed so secondary rows don't reinstall.
    //
    // TODO(codex-p2): the `(source_repo, version)` group key cannot tell a
    // multi-binary release's SECONDARY entry apart from a SEPARATE primary
    // install of the same release under a different `--name`/`--as-kind`. In
    // the latter (rare) case the second row is skipped and then fails
    // verification as "missing". Distinguishing them needs the release's
    // declared binary set (`plugin.toml`), which is not recorded in the lock;
    // deferred.
    // The platform we are reproducing ON. Every entry's portable claim is
    // keyed by triple; without a known triple there is nothing to verify
    // against, so fail the whole run with an actionable message.
    let Some(current_triple) = current_target_triple() else {
        return Err(invalid_input_error(
            "current platform has no known target triple, so `--locked` cannot verify any pinned archive sha. \
             Install plugins normally on this platform instead.",
        ));
    };

    // Release groups (`source_repo`, `version`) that have at least one entry
    // with a current-target archive sha. A multi-binary release's SECONDARY
    // entries only record the install platform (we never had their foreign
    // tarball shas), so on a different platform they lack a current-target
    // claim — but the group's PRIMARY does, and a single release reinstall
    // reproduces every sibling binary. Such a covered secondary must NOT fail
    // the missing-archive-sha guard; it is verified by its installed binary
    // below instead (codex P2).
    // A usable archive pin for the current target: a recorded `archive_sha256`
    // that is non-empty (an empty one means "binary installed but tarball sha
    // unknown" — e.g. a no-SHA256SUMS secondary — which can't gate a download).
    let archive_pin = |e: &LockEntry| -> Option<String> {
        e.target(current_triple).map(|t| t.archive_sha256.clone()).filter(|s| !s.is_empty())
    };
    let mut release_groups_with_current_target: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in &lockfile.plugins {
        if let (Some(repo), true) = (entry.source_repo.as_deref(), archive_pin(entry).is_some()) {
            if !repo.starts_with("https://") && !repo.starts_with("path:") {
                release_groups_with_current_target.insert((repo.to_string(), entry.version.clone()));
            }
        }
    }

    let mut installed_release_groups: BTreeSet<(String, String)> = BTreeSet::new();
    let mut rows: Vec<LockedInstallRow> = Vec::with_capacity(lockfile.plugins.len());
    let mut failed = 0_usize;
    let mut installed = 0_usize;

    for entry in &lockfile.plugins {
        // Portable, cross-platform pin for THIS platform: the recorded
        // TARBALL sha for the current target. Its absence means the lock was
        // generated without this platform (or migrated from 1.0) — fail with
        // a regenerate/reinstall hint rather than installing unverified.
        // EXCEPTION: a release-group secondary covered by a primary that DOES
        // pin the current target (see above) is reproduced via the group, so
        // it carries no own archive sha and is verified by binary hash instead.
        let expected_archive_sha = archive_pin(entry);
        let covered_secondary = expected_archive_sha.is_none()
            && entry.source_repo.as_deref().is_some_and(|repo| {
                release_groups_with_current_target.contains(&(repo.to_string(), entry.version.clone()))
            });
        if expected_archive_sha.is_none() && !covered_secondary {
            failed += 1;
            let detail = if entry.is_legacy_unverifiable() {
                format!(
                    "lockfile entry has no per-target integrity claim (migrated from a 1.0 lockfile) — \
                     run `animus plugin install {}` once on this platform to record portable shas",
                    entry.source_repo.as_deref().unwrap_or(&entry.name)
                )
            } else {
                format!(
                    "lockfile records no archive sha for target '{current_triple}' (the lock was generated without this \
                     platform) — regenerate the lock on this platform, or `animus plugin install {}` once here",
                    entry.source_repo.as_deref().unwrap_or(&entry.name)
                )
            };
            rows.push(LockedInstallRow {
                name: entry.name.clone(),
                source_repo: entry.source_repo.clone().unwrap_or_default(),
                version: entry.version.clone(),
                expected_sha256: String::new(),
                status: "failed",
                detail: Some(detail),
            });
            continue;
        }
        // Empty for a covered secondary (reproduced via its release group); the
        // recorded tarball sha otherwise.
        let expected_archive_sha = expected_archive_sha.unwrap_or_default();

        let Some(source_repo) = entry.source_repo.as_deref().filter(|s| !s.is_empty()) else {
            failed += 1;
            rows.push(LockedInstallRow {
                name: entry.name.clone(),
                source_repo: String::new(),
                version: entry.version.clone(),
                expected_sha256: expected_archive_sha.clone(),
                status: "failed",
                detail: Some("lockfile entry records no source_repo — installed before source provenance was tracked; reinstall it once to record the source".to_string()),
            });
            continue;
        };

        let is_url = source_repo.starts_with("https://");
        let is_path = source_repo.starts_with("path:");
        let is_release = !is_url && !is_path;
        let group_key = (source_repo.to_string(), entry.version.clone());
        let already_installed = is_release && installed_release_groups.contains(&group_key);

        let install_err = if already_installed {
            // A sibling row from the same release already triggered the
            // reinstall; this row only needs verification below.
            None
        } else {
            // Carry the locked dispatch alias through so a fresh-machine
            // `--locked` reinstall keeps the recorded `installed_kind`. Without
            // this, a subject backend locked as e.g. `archive` would be
            // recomputed to its native/auto-incremented kind and silently break
            // workflows that dispatch to the locked alias (codex P2). Only set
            // it for a TRUE rename (installed_kind != native_kind) — passing
            // `--as-kind` on a non-renamed / non-subject plugin is an error.
            let locked_as_kind = match (entry.effective_installed_kind(), entry.effective_native_kind()) {
                (Some(installed), Some(native)) if installed != native => Some(installed.to_string()),
                _ => None,
            };
            // For a RELEASE reinstall, gather the lock's expected per-target
            // tarball sha for every OTHER entry in the same release group (the
            // multi-binary SECONDARIES) so the install loop pins each secondary
            // tarball against the lock before extracting (codex P1). Keyed by
            // the secondary's binary name (`entry.name`), matching the
            // `descriptor.name` the multi-binary loop installs under.
            let locked_secondary_archive_shas: BTreeMap<String, String> = if is_release {
                lockfile
                    .plugins
                    .iter()
                    .filter(|other| {
                        other.name != entry.name
                            && other.source_repo.as_deref() == Some(source_repo)
                            && other.version == entry.version
                    })
                    .filter_map(|other| {
                        other
                            .target(current_triple)
                            .filter(|t| !t.archive_sha256.is_empty())
                            .map(|t| (other.name.clone(), t.archive_sha256.clone()))
                    })
                    .collect()
            } else {
                BTreeMap::new()
            };
            let mut req = PluginInstallRequest {
                name: Some(entry.name.clone()),
                force: true,
                project_root: Some(project_root.to_string()),
                project: effective_project,
                plugin_dir: args.plugin_dir.clone(),
                yes: true,
                allow_org: vec![],
                allow_shadow_builtin,
                as_kind: locked_as_kind,
                signature_policy: Some(signature_policy),
                trusted_signers: trusted_signers.clone(),
                locked_secondary_archive_shas,
                ..Default::default()
            };
            // Pin the TARBALL sha for the current target so the install
            // pipeline aborts before extracting / executing a binary whose
            // archive drifted from the lock — the cross-platform verification
            // that makes a Mac-generated lock safe to reproduce on linux
            // (codex P1). For a RELEASE source the pin gates the downloaded
            // TARBALL via `expected_archive_sha256` (the extracted-binary hash
            // differs from the tarball hash, so `req.sha256` — which gates the
            // binary — would spuriously fail). For `--url`/`--path` the fetched
            // artifact IS the archive, so `req.sha256` is correct.
            if is_url {
                req.url = Some(source_repo.to_string());
                req.sha256 = Some(expected_archive_sha.clone());
            } else if let Some(path) = source_repo.strip_prefix("path:") {
                req.path = Some(path.to_string());
                req.sha256 = Some(expected_archive_sha.clone());
            } else {
                req.source = Some(source_repo.to_string());
                req.tag = if entry.version.is_empty() { None } else { Some(entry.version.clone()) };
                // The primary entry is written to the lockfile first, so it is
                // the first row of each release group and its current-target
                // archive sha matches the primary release tarball.
                //
                // TODO(codex-p2): only the PRIMARY tarball is pinned via
                // `expected_archive_sha256`; a multi-binary release's SECONDARY
                // tarballs are checked against the release SHA256SUMS/sidecar,
                // not the lock, so a drifted secondary is copied to disk and
                // only flagged when this `--locked` run verifies its installed
                // binary below. The lock pin is preserved (a re-run re-detects
                // the drift), but the drifted secondary binary is left
                // installed. Removing it would need per-binary rollback in the
                // install pipeline; deferred.
                //
                // Empty only for a covered secondary that somehow drives its
                // own install (its primary did not precede it) — leave the pin
                // unset and rely on the release's own SHA256SUMS check.
                if !expected_archive_sha.is_empty() {
                    req.expected_archive_sha256 = Some(expected_archive_sha.clone());
                }
            }
            match run_plugin_install(req).await {
                Ok(_) => {
                    if is_release {
                        installed_release_groups.insert(group_key);
                    }
                    None
                }
                Err(err) => Some(format!("{err:#}")),
            }
        };

        if let Some(err) = install_err {
            failed += 1;
            rows.push(LockedInstallRow {
                name: entry.name.clone(),
                source_repo: source_repo.to_string(),
                version: entry.version.clone(),
                expected_sha256: expected_archive_sha.clone(),
                status: "failed",
                detail: Some(format!("reinstall failed: {err}")),
            });
            continue;
        }

        // The TARBALL sha was already verified against `expected_archive_sha`
        // by the install pipeline's pre-extract gate (cross-platform). Here we
        // additionally confirm the EXTRACTED binary landed and matches the
        // sha the just-completed reinstall recorded for this platform — a
        // host-only tamper check. `verify_installed` reads the in-memory
        // `lockfile` (the committed pin), whose current-target host hash may be
        // absent for a foreign-generated lock; in that case `MissingTarget` is
        // not a failure (the portable archive gate already passed), so treat it
        // as verified.
        let binary = install_dir.join(&entry.name);
        if !binary.exists() {
            failed += 1;
            rows.push(LockedInstallRow {
                name: entry.name.clone(),
                source_repo: source_repo.to_string(),
                version: entry.version.clone(),
                expected_sha256: expected_archive_sha.clone(),
                status: "failed",
                detail: Some(format!("installed binary not found at {} after reinstall", binary.display())),
            });
            continue;
        }
        // Secondary entries now carry per-target `archive_sha256` from
        // SHA256SUMS (see the lock-build path), and the multi-binary install
        // gate (below, via `locked_secondary_archive_shas`) checks each
        // secondary TARBALL against the LOCK's recorded archive sha before
        // extracting — so a drifted secondary asset fails the gate even with a
        // regenerated SHA256SUMS. A foreign-generated lock has no current-target
        // `installed_binary_sha256` for secondaries (we never extracted their
        // binary on this platform), so `verify_installed` here returns
        // `MissingTarget`; that is not unpinned (the archive was gated above),
        // so it counts as verified.
        match lockfile.verify_installed(&entry.name, &binary) {
            Ok(LockVerifyResult::Match)
            | Ok(LockVerifyResult::MissingTarget { .. })
            | Ok(LockVerifyResult::Missing) => {
                installed += 1;
                rows.push(LockedInstallRow {
                    name: entry.name.clone(),
                    source_repo: source_repo.to_string(),
                    version: entry.version.clone(),
                    expected_sha256: expected_archive_sha.clone(),
                    status: "verified",
                    detail: None,
                });
            }
            Ok(LockVerifyResult::Mismatch { expected, actual }) => {
                failed += 1;
                rows.push(LockedInstallRow {
                    name: entry.name.clone(),
                    source_repo: source_repo.to_string(),
                    version: entry.version.clone(),
                    expected_sha256: expected_archive_sha.clone(),
                    status: "failed",
                    detail: Some(format!(
                        "installed binary sha256 drifted from the pin: lockfile expected {expected} but the binary \
                         at {} hashes to {actual}",
                        binary.display()
                    )),
                });
            }
            Err(err) => {
                failed += 1;
                rows.push(LockedInstallRow {
                    name: entry.name.clone(),
                    source_repo: source_repo.to_string(),
                    version: entry.version.clone(),
                    expected_sha256: expected_archive_sha.clone(),
                    status: "failed",
                    detail: Some(format!("failed to hash installed binary at {}: {err}", binary.display())),
                });
            }
        }
    }

    // Restore the committed pins: the reinstalls above may have re-saved the
    // lockfile with drifted secondary entries; `--locked` must leave the
    // entries it owns exactly as committed so a later CI run cannot pass
    // against drift.
    restore_lock(&original_entries);

    let output = LockedInstallOutput {
        schema: "animus.plugin.install-locked.v1",
        lockfile: lock_path.to_string_lossy().to_string(),
        installed,
        failed,
        rows,
    };

    if json {
        print_value(output, true)?;
    } else {
        println!("plugin install --locked (lockfile: {})", output.lockfile);
        for row in &output.rows {
            match row.status {
                "verified" => println!("  [ok] {} {} (sha verified)", row.name, row.version),
                _ => println!(
                    "  [FAIL] {} {} — {}",
                    row.name,
                    row.version,
                    row.detail.as_deref().unwrap_or("unknown error")
                ),
            }
        }
        println!("summary: {} verified, {} failed", output.installed, output.failed);
    }

    if failed > 0 {
        return Err(anyhow!(
            "plugin install --locked failed: {failed} of {} entries could not be reproduced",
            lockfile.plugins.len()
        ));
    }
    Ok(())
}

/// Maps a plugin manifest `plugin_kind` string into the bounded
/// [`crate::services::metrics::PluginRole`] enum. Unknown kinds collapse
/// to `Other` — payloads must never carry a free-form string.
fn plugin_role_from_kind(kind: &str) -> crate::services::metrics::PluginRole {
    use crate::services::metrics::PluginRole;
    match kind {
        "subject_backend" | "task_backend" => PluginRole::SubjectBackend,
        "provider" | "session_backend" => PluginRole::Provider,
        "transport" | "transport_backend" => PluginRole::Transport,
        "web_ui" => PluginRole::WebUi,
        "trigger" | "trigger_backend" => PluginRole::Trigger,
        "log_storage" | "log_storage_backend" => PluginRole::LogStorage,
        "queue" => PluginRole::Queue,
        "notifier" => PluginRole::Notifier,
        "workflow_runner" => PluginRole::WorkflowRunner,
        _ => PluginRole::Other,
    }
}

/// Translate CLI flag combinations into the canonical [`PluginPolicyMode`].
///
/// Precedence:
/// 1. `--signature-policy <strict|warn|disabled>` if set.
/// 2. `--allow-unsigned` -> `Warn`.
/// 3. `--skip-signature` -> `Disabled` (legacy).
/// 4. `--require-signature` -> `Strict` (legacy alias; explicit opt-in).
/// 5. Fallback: [`PluginPolicyMode::default_for_install`], which is
///    `Warn` for v0.4.12 as a one-release migration window — pre-v0.4.12
///    installs used the (now-removed) key-based PEM path and may not have
///    keyless bundles available yet. v0.4.13 flips that lib default back
///    to `Strict` now that keyless verification has a real Sigstore trust
///    anchor. See `docs/reference/security.md`.
fn resolve_cli_signature_policy(args: &PluginInstallArgs) -> Result<PluginPolicyMode> {
    if let Some(raw) = args.signature_policy.as_deref() {
        return raw
            .parse::<PluginPolicyMode>()
            .map_err(|msg| invalid_input_error(format!("invalid --signature-policy: {msg}")));
    }
    if args.allow_unsigned {
        return Ok(PluginPolicyMode::Warn);
    }
    if args.skip_signature {
        return Ok(PluginPolicyMode::Disabled);
    }
    if args.require_signature {
        return Ok(PluginPolicyMode::Strict);
    }
    Ok(PluginPolicyMode::default_for_install())
}

fn handle_plugin_uninstall(args: PluginUninstallArgs, project_root: &str, json: bool) -> Result<()> {
    let output = run_plugin_uninstall(PluginUninstallRequest {
        name: args.name,
        plugin_dir: args.plugin_dir,
        project_root: Some(project_root.to_string()),
        project: args.project,
    })?;
    print_value(output, json)
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginRenameOutput {
    pub(crate) schema: &'static str,
    pub(crate) plugin_name: String,
    pub(crate) old_kind: String,
    pub(crate) new_kind: String,
    /// The unmodified value the operator passed via `--to`. Distinct from
    /// `new_kind` when `--force` auto-incremented past a collision
    /// (e.g. requested `task`, assigned `task-2`).
    pub(crate) requested_kind: String,
    pub(crate) native_kind: String,
    pub(crate) lockfile: String,
    pub(crate) auto_incremented: bool,
}

fn handle_plugin_rename(args: PluginRenameArgs, project_root: &str, json: bool) -> Result<()> {
    let want_json = json || args.json;
    let output = run_plugin_rename(PluginRenameRequest {
        name: args.name,
        to: args.to,
        force: args.force,
        project_root: project_root.to_string(),
    })?;
    if output.auto_incremented {
        eprintln!(
            "animus.plugin.rename.v1: plugin '{plugin}' assigned installed_kind \
             '{assigned}' (requested '{requested}'); the requested value was already \
             claimed and --force auto-incremented to the next free slot.",
            plugin = output.plugin_name,
            assigned = output.new_kind,
            requested = output.requested_kind,
        );
    }
    print_value(output, want_json)
}

#[derive(Debug, Clone)]
pub(crate) struct PluginRenameRequest {
    pub(crate) name: String,
    pub(crate) to: String,
    pub(crate) force: bool,
    pub(crate) project_root: String,
}

pub(crate) fn run_plugin_rename(req: PluginRenameRequest) -> Result<PluginRenameOutput> {
    let plugin_name = req.name.trim().to_string();
    if plugin_name.is_empty() {
        return Err(invalid_input_error("PLUGIN_NAME must not be empty"));
    }
    let requested = req.to.trim().to_string();
    if requested.is_empty() {
        return Err(invalid_input_error("--to must not be empty"));
    }
    if requested.contains('/')
        || requested.contains('*')
        || requested.contains(':')
        || requested.contains(char::is_whitespace)
    {
        return Err(invalid_input_error(format!(
            "--to '{requested}' is not a valid subject kind. Kinds must be exact identifiers \
             with no '/', '*', ':', or whitespace; the ':' separator is reserved for subject id \
             encoding (`<kind>:<local-id>`) and glob/prefix-routed kinds are not supported by \
             the v0.5.7 translator."
        )));
    }

    let project_root_path = std::path::Path::new(&req.project_root);
    let mut lockfile =
        PluginLockfile::load_default(Some(project_root_path)).with_context(|| {
            format!(
                "failed to load plugin lockfile at {}",
                PluginLockfile::default_path(Some(project_root_path)).display(),
            )
        })?;
    let lockfile_path = lockfile.path().to_path_buf();

    // The collision check pulls live subject kinds from disk so a
    // pre-v0.5.7 plugin (no lockfile rename slot) still blocks the rename
    // target. Same helper as the install pipeline.
    let currently_claimed_kinds =
        current_subject_kinds_for_collision_check(Some(&req.project_root), None, &lockfile, &plugin_name);

    let entry = lockfile
        .find(&plugin_name)
        .ok_or_else(|| {
            not_found_error(format!(
                "plugin '{plugin_name}' has no entry in {}. Install it first with `animus plugin install` \
                 or run `animus plugin lock list` to see currently-tracked plugins.",
                lockfile_path.display(),
            ))
        })?
        .clone();

    let old_kind = entry
        .effective_installed_kind()
        .map(str::to_string)
        .or_else(|| entry.effective_native_kind().map(str::to_string))
        .ok_or_else(|| {
            invalid_input_error(format!(
                "plugin '{plugin_name}' has no installed_kind or native_kind recorded in {}. \
                 Reinstall the plugin to populate the rename surface before calling \
                 `animus plugin rename`.",
                lockfile_path.display(),
            ))
        })?;
    let native_kind = entry.effective_native_kind().map(str::to_string).unwrap_or_else(|| old_kind.clone());

    if old_kind == requested {
        // No-op rename — treated as success so scripted retries are idempotent.
        return Ok(PluginRenameOutput {
            schema: "animus.plugin.rename.v1",
            plugin_name,
            old_kind: old_kind.clone(),
            new_kind: requested.clone(),
            requested_kind: requested,
            native_kind,
            lockfile: lockfile_path.to_string_lossy().to_string(),
            auto_incremented: false,
        });
    }

    // Codex round-1 v0.5.8 P2: a multi-kind subject backend declaring both
    // `task` (primary) and `requirement` (secondary) must not be renamed
    // to its OWN secondary kind. The lockfile alias only renames the
    // primary slot, so the SubjectRouter would still register the
    // secondary native kind unaliased — a `--to requirement` rename on
    // such a plugin produces two registrations of `requirement` (the
    // primary, now aliased, plus the secondary, still native) and the
    // router rejects the duplicate at startup. Refuse the rename here so
    // the operator sees a clear error instead of a broken next boot.
    let own_secondary_native_kinds: Vec<String> = match PluginDiscovery::new()
        .with_project_root(std::path::Path::new(&req.project_root))
        .discover()
    {
        Ok(plugins) => plugins
            .into_iter()
            .find(|p| p.name == plugin_name)
            .map(|p| {
                let mut kinds = all_rename_eligible_native_kinds(&p.manifest);
                // Drop the primary native kind — it's the slot the
                // rename legitimately replaces.
                if let Some(idx) = kinds.iter().position(|k| k == &native_kind) {
                    kinds.swap_remove(idx);
                }
                kinds
            })
            .unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, "plugin discovery failed during rename self-collision pre-check; relying on lockfile collision detection only");
            Vec::new()
        }
    };
    if own_secondary_native_kinds.iter().any(|k| k == &requested) {
        return Err(invalid_input_error(format!(
            "--to '{requested}' is one of plugin '{plugin_name}'s own native subject kinds. \
             The lockfile records a single installed_kind per plugin, so the rename would \
             alias the primary slot to '{requested}' while the secondary capability remains \
             registered under its native value — the SubjectRouter would refuse the \
             resulting duplicate at startup. Pick a different `--to` value or uninstall the \
             plugin's secondary capability."
        )));
    }

    let collider = lockfile_collider(&lockfile, &currently_claimed_kinds, &plugin_name, &requested);
    let (assigned_kind, auto_incremented) = match (collider, req.force) {
        (Some(collider_name), false) => {
            let collider_label = if collider_name.is_empty() {
                "an installed plugin's manifest capability".to_string()
            } else {
                format!("installed plugin '{collider_name}'")
            };
            return Err(invalid_input_error(format!(
                "--to '{requested}' is already claimed by {collider_label}. Pass --force to \
                 auto-increment from '{requested}' (e.g. '{requested}-2'), or pick a different \
                 value and re-run."
            )));
        }
        (Some(_), true) => {
            // Auto-increment from the requested base, exactly like the install
            // pipeline's auto-increment behavior. Codex round-2 v0.5.8 P2:
            // also skip candidates that would alias to one of this plugin's
            // OWN secondary native kinds — otherwise the SubjectRouter would
            // see two registrations for the same kind at next boot.
            let mut suffix: u32 = 2;
            let chosen = loop {
                let candidate = format!("{requested}-{suffix}");
                let collides_other =
                    lockfile_collider(&lockfile, &currently_claimed_kinds, &plugin_name, &candidate).is_some();
                let collides_self = own_secondary_native_kinds.iter().any(|k| k == &candidate);
                if !collides_other && !collides_self {
                    break candidate;
                }
                suffix = suffix.checked_add(1).ok_or_else(|| {
                    invalid_input_error(format!(
                        "exhausted u32 auto-increment range for installed_kind '{requested}'; this is a bug"
                    ))
                })?;
            };
            (chosen, true)
        }
        (None, _) => (requested.clone(), false),
    };

    let mut updated = entry.clone();
    updated.installed_kind = Some(assigned_kind.clone());
    if updated.native_kind.is_none() {
        updated.native_kind = Some(native_kind.clone());
    }
    lockfile.upsert(updated);
    if let Err(err) = lockfile.save() {
        return Err(invalid_input_error(format!(
            "failed to persist plugin lockfile at {} after renaming '{plugin_name}' to '{assigned_kind}': {err:#}. \
             The lockfile may now be inconsistent — rerun the rename once the path is writable.",
            lockfile_path.display(),
        )));
    }

    if let Some(scoped) = protocol::repository_scope::scoped_state_root(project_root_path) {
        Audit::at_scoped_root(&scoped).log_event(AuditEvent::new(
            AuditActor::User,
            AuditEventKind::PluginInstall,
            serde_json::json!({
                "plugin": plugin_name,
                "action": "rename",
                "old_kind": old_kind,
                "new_kind": assigned_kind,
                "native_kind": native_kind,
                "auto_incremented": auto_incremented,
            }),
        ));
    }

    Ok(PluginRenameOutput {
        schema: "animus.plugin.rename.v1",
        plugin_name,
        old_kind,
        new_kind: assigned_kind,
        requested_kind: requested,
        native_kind,
        lockfile: lockfile_path.to_string_lossy().to_string(),
        auto_incremented,
    })
}

// ===== `plugin lock` subcommands =====

#[derive(Debug, Serialize)]
struct PluginLockTargetCoverage {
    name: String,
    /// Target triples this entry has a recorded archive sha for.
    targets: Vec<String>,
    /// `true` for a 1.0-migrated entry with no usable per-target claim.
    legacy_unverifiable: bool,
}

#[derive(Debug, Serialize)]
struct PluginLockListOutput {
    lockfile: String,
    schema_version: String,
    generated_at: String,
    /// Per-entry platform coverage summary (which targets each entry pins).
    coverage: Vec<PluginLockTargetCoverage>,
    plugins: Vec<LockEntry>,
}

#[derive(Debug, Serialize)]
struct PluginLockVerifyEntry {
    name: String,
    status: &'static str,
    /// The current build target the entry was verified against (the host-only
    /// `installed_binary_sha256` claim is keyed by it).
    target: String,
    expected_sha256: String,
    actual_sha256: Option<String>,
    installed_path: Option<String>,
    detail: Option<String>,
    /// Lockfile root the entry came from: `global`
    /// (`~/.animus/plugins.lock`), `project`
    /// (`<project>/.animus/plugins.lock`), or `explicit` (`--lockfile`).
    scope: &'static str,
    /// Path of the lockfile the entry was read from.
    lockfile: String,
}

#[derive(Debug, Serialize)]
struct PluginLockVerifyOutput {
    /// Primary lockfile path (back-compat field). Equals the `--lockfile`
    /// override when supplied, otherwise the default-resolved path.
    lockfile: String,
    /// The current build target triple all entries were verified against.
    target: String,
    /// Every lockfile root the verify swept (global + project when both
    /// exist as distinct files).
    lockfiles: Vec<String>,
    entries: Vec<PluginLockVerifyEntry>,
    matched: usize,
    mismatched: usize,
    missing_binary: usize,
    /// Entries that exist but have no host-only integrity claim for the
    /// current target (1.0-migrated, or a lock generated on another platform).
    /// Reported distinctly from `mismatch`; it is drift the operator must fix
    /// by reinstalling on this platform, so it fails the verify gate.
    missing_target: usize,
    /// Installed plugins discovered on disk that are absent from every swept
    /// lockfile. Like mismatch/missing this is drift and fails the verify gate.
    extra: usize,
}

// ===== `plugin doctor` =====

#[derive(Debug, Serialize)]
struct PluginDoctorClaim {
    plugin: String,
    installed_kind: String,
    native_kind: String,
    /// `true` when the install pipeline applied a rename (auto-increment
    /// or explicit `--as-kind`). Surfaced so doctor output makes the
    /// distinction visible without forcing operators to re-derive it
    /// from the side-by-side columns.
    renamed: bool,
}

#[derive(Debug, Serialize)]
struct PluginDoctorRole {
    role: String,
    /// One entry per installed plugin currently satisfying this role.
    /// Multiple entries for the same role can be legitimate (two
    /// distinct providers, or two subject backends with auto-incremented
    /// installed_kinds) — but the same `installed_kind` claimed twice is
    /// always a collision.
    claims: Vec<PluginDoctorClaim>,
    /// Set when two or more `claims` share the same `installed_kind`.
    /// The presence of this list — independent of `claims.len()` — is
    /// the doctor's collision signal.
    collisions: Vec<String>,
    /// `true` when the role has zero satisfying installs. Distinct from
    /// "no collisions": preflight will refuse daemon startup in this case.
    unsatisfied: bool,
}

#[derive(Debug, Serialize)]
struct PluginDoctorOutput {
    lockfile: String,
    roles: Vec<PluginDoctorRole>,
    /// Operator-facing summary so a single grep can flag broken setups
    /// without parsing the per-role rows.
    total_collisions: usize,
    total_unsatisfied: usize,
}

async fn handle_plugin_doctor(args: PluginDoctorArgs, project_root: &str, json: bool) -> Result<()> {
    use orchestrator_core::{summarize_discovered_plugins_with_lock, PluginPreflightSpec, RequiredRole};

    let project_root_path = std::path::Path::new(project_root);
    let discovered = discover_plugins(project_root_path).context("plugin discovery failed")?;
    let lockfile = PluginLockfile::load_default(Some(project_root_path)).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "plugin lockfile unreadable; doctor will treat every entry as identity-renamed");
        PluginLockfile::empty_at(&PluginLockfile::default_path(Some(project_root_path)))
    });
    // Read the lockfile-aware summary so role membership reflects the
    // same `installed_kind` view the daemon preflight uses. Without this,
    // doctor and preflight can disagree (codex P2 round-1 v0.5.7):
    // doctor would still report `subject_kind:task` satisfied by a plugin
    // installed as `archive`, while the daemon refuses startup because
    // its preflight maps the same plugin to `subject_kind:archive`.
    let summaries = summarize_discovered_plugins_with_lock(&discovered, Some(&lockfile));

    let spec = PluginPreflightSpec::daemon_default();
    let mut roles_output: Vec<PluginDoctorRole> = Vec::with_capacity(spec.required_roles.len());
    let mut total_collisions = 0_usize;
    let mut total_unsatisfied = 0_usize;

    for role in &spec.required_roles {
        let role_label = role.label();
        let mut claims: Vec<PluginDoctorClaim> = Vec::new();

        for summary in &summaries {
            let claim_kind = match (role, summary) {
                (RequiredRole::AtLeastOneProvider, s) if s.is_provider() => provider_native_kind(&discovered, &s.name),
                (RequiredRole::SubjectKind(target), s) => {
                    // `summary.subject_kinds` is already the lock-aware
                    // installed_kind view; matching against `target` lines
                    // doctor up with what the daemon's preflight checks.
                    if s.is_subject_backend() && s.subject_kinds.iter().any(|k| k == target) {
                        Some(target.clone())
                    } else {
                        None
                    }
                }
                (RequiredRole::WorkflowRunner, s) if s.is_workflow_runner() => Some(summary.plugin_kind.clone()),
                (RequiredRole::Queue, s) if s.is_queue() => Some(summary.plugin_kind.clone()),
                (RequiredRole::ConfigSource, s) if s.is_config_source() => Some(summary.plugin_kind.clone()),
                (RequiredRole::TransportEnabled, _) => None,
                _ => None,
            };
            let Some(claim_kind) = claim_kind else {
                continue;
            };
            let lock_entry = lockfile.find(&summary.name);
            let installed_kind = lock_entry
                .and_then(|e| e.effective_installed_kind())
                .map(str::to_string)
                .unwrap_or_else(|| claim_kind.clone());
            let native_kind =
                lock_entry.and_then(|e| e.effective_native_kind()).map(str::to_string).unwrap_or_else(|| claim_kind);
            let renamed = installed_kind != native_kind;
            claims.push(PluginDoctorClaim { plugin: summary.name.clone(), installed_kind, native_kind, renamed });
        }

        let mut by_installed: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for claim in &claims {
            by_installed.entry(claim.installed_kind.clone()).or_default().push(claim.plugin.clone());
        }
        let mut collisions: Vec<String> =
            by_installed.iter().filter(|(_, plugins)| plugins.len() > 1).map(|(kind, _)| kind.clone()).collect();
        collisions.sort();

        let unsatisfied = claims.is_empty() && !matches!(role, RequiredRole::TransportEnabled);
        if !collisions.is_empty() {
            total_collisions += 1;
        }
        if unsatisfied {
            total_unsatisfied += 1;
        }

        roles_output.push(PluginDoctorRole { role: role_label, claims, collisions, unsatisfied });
    }

    // Codex P2 round-4 v0.5.7: synthesize per-installed-kind rows for
    // every `subject_kind` outside the daemon default spec. Two plugins
    // both renamed to `archive` (or two plugins installed under any
    // other non-default subject kind) would otherwise pass `doctor`
    // without warning even though the SubjectRouter rejects the
    // duplicate exact-kind registration at startup. Synthetic rows are
    // appended AFTER the spec-driven roles so the standard preflight
    // view stays at the top of the output.
    let mut spec_subject_kinds: std::collections::HashSet<String> = std::collections::HashSet::new();
    for role in &spec.required_roles {
        if let RequiredRole::SubjectKind(k) = role {
            spec_subject_kinds.insert(k.clone());
        }
    }
    let mut extra_kinds: Vec<String> = Vec::new();
    for summary in &summaries {
        if !summary.is_subject_backend() {
            continue;
        }
        for kind in &summary.subject_kinds {
            if !spec_subject_kinds.contains(kind) && !extra_kinds.contains(kind) {
                extra_kinds.push(kind.clone());
            }
        }
    }
    extra_kinds.sort();
    for kind in extra_kinds {
        let role_label = format!("subject_kind:{kind}");
        let mut claims: Vec<PluginDoctorClaim> = Vec::new();
        for summary in &summaries {
            if !summary.is_subject_backend() || !summary.subject_kinds.contains(&kind) {
                continue;
            }
            let lock_entry = lockfile.find(&summary.name);
            let installed_kind = lock_entry
                .and_then(|e| e.effective_installed_kind())
                .map(str::to_string)
                .unwrap_or_else(|| kind.clone());
            let native_kind =
                lock_entry.and_then(|e| e.effective_native_kind()).map(str::to_string).unwrap_or_else(|| kind.clone());
            let renamed = installed_kind != native_kind;
            claims.push(PluginDoctorClaim { plugin: summary.name.clone(), installed_kind, native_kind, renamed });
        }
        let mut by_installed: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for claim in &claims {
            by_installed.entry(claim.installed_kind.clone()).or_default().push(claim.plugin.clone());
        }
        let mut collisions: Vec<String> =
            by_installed.iter().filter(|(_, plugins)| plugins.len() > 1).map(|(kind, _)| kind.clone()).collect();
        collisions.sort();
        if !collisions.is_empty() {
            total_collisions += 1;
        }
        // Synthetic subject_kind rows never count as `unsatisfied` —
        // they exist only because something IS installed under the kind.
        roles_output.push(PluginDoctorRole { role: role_label, claims, collisions, unsatisfied: false });
    }

    let lockfile_display = lockfile.path().display().to_string();
    let output = PluginDoctorOutput {
        lockfile: lockfile_display.clone(),
        roles: roles_output,
        total_collisions,
        total_unsatisfied,
    };

    if json || args.json {
        return print_value(output, true);
    }

    println!("plugin doctor (lockfile: {lockfile_display})");
    for role in &output.roles {
        let header = if !role.collisions.is_empty() {
            format!("[COLLISION] {}", role.role)
        } else if role.unsatisfied {
            format!("[UNSATISFIED] {}", role.role)
        } else {
            format!("[ok] {}", role.role)
        };
        println!("  {header}");
        if role.claims.is_empty() {
            println!("    (no installed plugin satisfies this role)");
            continue;
        }
        for claim in &role.claims {
            let rename_note = if claim.renamed { " (renamed)" } else { "" };
            println!(
                "    - {plugin} installed={installed} native={native}{rename_note}",
                plugin = claim.plugin,
                installed = claim.installed_kind,
                native = claim.native_kind,
            );
        }
        if !role.collisions.is_empty() {
            for kind in &role.collisions {
                println!("    ! duplicate installed_kind '{kind}' claimed by multiple plugins");
            }
        }
    }
    println!(
        "summary: {total_collisions} role(s) with collisions, {total_unsatisfied} role(s) unsatisfied",
        total_collisions = output.total_collisions,
        total_unsatisfied = output.total_unsatisfied,
    );
    Ok(())
}

/// Look up the `provider_tool:*` capability declared in a discovered
/// plugin's manifest. Returns the bare tool name (e.g. `claude`,
/// `codex`) or `None` when the plugin declares no `provider_tool:*`
/// capability (older provider plugins rely on `is_provider()` alone).
fn provider_native_kind(discovered: &[DiscoveredPlugin], plugin_name: &str) -> Option<String> {
    let plugin = discovered.iter().find(|p| p.name == plugin_name)?;
    for cap in &plugin.manifest.capabilities {
        if let Some(rest) = cap.strip_prefix("provider_tool:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    Some(plugin.manifest.name.clone())
}

async fn handle_plugin_lock(cmd: PluginLockCommand, project_root: &str) -> Result<()> {
    match cmd {
        PluginLockCommand::List(args) => run_lock_list(args, project_root),
        PluginLockCommand::Verify(args) => run_lock_verify(args, project_root),
    }
}

fn run_lock_list(args: PluginLockListArgs, project_root: &str) -> Result<()> {
    let path = args.lockfile.unwrap_or_else(|| PluginLockfile::default_path(Some(std::path::Path::new(project_root))));
    let lockfile = PluginLockfile::load_or_empty(&path)?;
    // Per-entry platform coverage: which target triples each entry has a
    // recorded archive sha for. Surfaces portability at a glance — an entry
    // covering only the generating platform won't drive `--locked` elsewhere.
    let coverage: Vec<PluginLockTargetCoverage> = lockfile
        .plugins
        .iter()
        .map(|e| PluginLockTargetCoverage {
            name: e.name.clone(),
            targets: e.target_triples().into_iter().map(str::to_string).collect(),
            legacy_unverifiable: e.is_legacy_unverifiable(),
        })
        .collect();
    let output = PluginLockListOutput {
        lockfile: path.to_string_lossy().to_string(),
        schema_version: lockfile.schema_version.clone(),
        generated_at: lockfile.generated_at.clone(),
        coverage,
        plugins: lockfile.plugins.clone(),
    };
    print_value(output, args.json)
}

fn run_lock_verify(args: PluginLockVerifyArgs, project_root: &str) -> Result<()> {
    let json = args.json;
    let output = compute_lock_verify(args, project_root)?;
    // `animus plugin lock verify` is meant to be wired into CI / cron as a
    // tamper-detection gate. A hash mismatch, a missing on-disk binary, a
    // missing per-target claim for this platform, AND an installed plugin
    // absent from the lockfile ("extra") all indicate the install state has
    // drifted from the lockfile, so any of them must exit non-zero.
    let exit_err = if output.mismatched > 0
        || output.missing_binary > 0
        || output.missing_target > 0
        || output.extra > 0
    {
        Some(anyhow!(
            "plugin lock verify failed: {} mismatched, {} missing binary, {} missing target ({}), {} extra (not in lockfile), {} matched",
            output.mismatched,
            output.missing_binary,
            output.missing_target,
            output.target,
            output.extra,
            output.matched
        ))
    } else {
        None
    };
    print_value(output, json)?;
    if let Some(err) = exit_err {
        return Err(err);
    }
    Ok(())
}

fn compute_lock_verify(args: PluginLockVerifyArgs, project_root: &str) -> Result<PluginLockVerifyOutput> {
    let project_root_path = std::path::Path::new(project_root);
    let global_dir = install_root(args.plugin_dir.as_deref())?;
    let project_dir = project_plugin_install_dir(project_root_path);
    // Extra-detection (installed-but-unlocked) only makes sense for the default
    // both-roots sweep against the normal discovery locations. An explicit
    // `--lockfile` or `--plugin-dir` scopes the verify to a subset, so the
    // unscoped `discover_plugins` set would falsely flag unrelated default /
    // global plugins as `extra` (codex P2) — skip it in those modes.
    let detect_extra = args.lockfile.is_none() && args.plugin_dir.is_none();

    // The verify targets: (lockfile path, scope, candidate install dirs in
    // probe order). With an explicit `--lockfile` override only that file is
    // verified (legacy behavior). Otherwise BOTH roots are swept: the global
    // lockfile against the global install dir, and the project lockfile with
    // the project dir probed first and the global dir as fallback (pre-flag
    // installs recorded global-dir binaries into the project lockfile via
    // `PluginLockfile::default_path`).
    let mut targets: Vec<(PathBuf, &'static str, Vec<PathBuf>)> = Vec::new();
    let primary_path: PathBuf;
    if let Some(explicit) = args.lockfile {
        // An explicit lockfile brings its own natural install dir: the
        // `plugins/` sibling next to the file (`<project>/.animus/plugins/`
        // for a committed project lockfile, `~/.animus/plugins/` for the
        // global one). Probe it alongside the resolved global dir so
        // `--lockfile <project>/.animus/plugins.lock` verifies project
        // binaries without also requiring `--plugin-dir` (codex P2). An
        // explicit `--plugin-dir` keeps first priority.
        let sibling_dir = explicit.parent().map(|parent| parent.join("plugins"));
        let mut candidate_dirs: Vec<PathBuf> = Vec::new();
        if args.plugin_dir.is_some() {
            candidate_dirs.push(global_dir.clone());
        }
        if let Some(sibling) = sibling_dir {
            if !candidate_dirs.contains(&sibling) {
                candidate_dirs.push(sibling);
            }
        }
        if !candidate_dirs.contains(&global_dir) {
            candidate_dirs.push(global_dir.clone());
        }
        primary_path = explicit.clone();
        targets.push((explicit, "explicit", candidate_dirs));
    } else {
        primary_path = PluginLockfile::default_path(Some(project_root_path));
        let global_path = global_lockfile_path();
        let project_path = project_lockfile_path(project_root_path);
        targets.push((global_path.clone(), "global", vec![global_dir.clone()]));
        if project_path != global_path {
            targets.push((project_path, "project", vec![project_dir.clone(), global_dir.clone()]));
        }
    }

    // Names recorded by a true project-scoped install. For those entries
    // the global-dir fallback below must NOT apply: when the project binary
    // is missing, falling back to a same-named global binary would report
    // `ok` and defeat the project tamper gate (codex P2). The fallback only
    // serves legacy project-lock entries written by global installs, which
    // never appear in the project registry.
    let project_registry_names: BTreeSet<String> = project_registry_claimed_names(project_root_path);

    // The platform we verify host-only binary claims against. An unknown
    // triple leaves every entry as `missing_target` (no claim can be matched).
    let current_target = current_target_triple().unwrap_or("<unknown>");

    let mut lockfiles: Vec<String> = Vec::with_capacity(targets.len());
    let mut entries = Vec::new();
    let mut matched = 0_usize;
    let mut mismatched = 0_usize;
    let mut missing_binary = 0_usize;
    let mut missing_target = 0_usize;
    // Every name claimed by any swept lockfile — used below to flag installed
    // plugins that are absent from the lockfile ("extra" drift).
    let mut locked_names: BTreeSet<String> = BTreeSet::new();
    for (path, scope, candidate_dirs) in &targets {
        let lockfile = PluginLockfile::load_or_empty(path)?;
        let lockfile_display = path.to_string_lossy().to_string();
        lockfiles.push(lockfile_display.clone());
        for entry in &lockfile.plugins {
            locked_names.insert(entry.name.clone());
        }
        // An explicit `--lockfile` pointing at the project lockfile carries
        // the same project-scope semantics (CI verifying the committed
        // lock) — pin its project-registry entries to the project dir too.
        // Canonicalize both sides so a relative invocation
        // (`--lockfile .animus/plugins.lock` from the project root) still
        // matches the absolute project path (codex P2).
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let target_is_project_lock = *scope == "project"
            || (*scope == "explicit" && canonical(path) == canonical(&project_lockfile_path(project_root_path)));
        for entry in &lockfile.plugins {
            let pin_to_project_dir = target_is_project_lock && project_registry_names.contains(&entry.name);
            let entry_dirs: &[PathBuf] =
                if pin_to_project_dir { std::slice::from_ref(&project_dir) } else { candidate_dirs.as_slice() };
            let installed_path = entry_dirs
                .iter()
                .map(|dir| dir.join(&entry.name))
                .find(|candidate| candidate.exists())
                .unwrap_or_else(|| entry_dirs[0].join(&entry.name));
            // The host-only claim recorded for the current platform, if any.
            let expected_for_target =
                entry.target(current_target).and_then(|t| t.installed_binary_sha256.clone()).unwrap_or_default();
            if !installed_path.exists() {
                missing_binary += 1;
                entries.push(PluginLockVerifyEntry {
                    name: entry.name.clone(),
                    status: "missing_binary",
                    target: current_target.to_string(),
                    expected_sha256: expected_for_target,
                    actual_sha256: None,
                    installed_path: Some(installed_path.to_string_lossy().to_string()),
                    detail: Some("installed binary not found at expected path".to_string()),
                    scope,
                    lockfile: lockfile_display.clone(),
                });
                continue;
            }
            match lockfile.verify_installed(&entry.name, &installed_path) {
                Ok(LockVerifyResult::Match) => {
                    matched += 1;
                    entries.push(PluginLockVerifyEntry {
                        name: entry.name.clone(),
                        status: "ok",
                        target: current_target.to_string(),
                        expected_sha256: expected_for_target.clone(),
                        actual_sha256: Some(expected_for_target),
                        installed_path: Some(installed_path.to_string_lossy().to_string()),
                        detail: None,
                        scope,
                        lockfile: lockfile_display.clone(),
                    });
                }
                Ok(LockVerifyResult::Mismatch { expected, actual }) => {
                    mismatched += 1;
                    if let Some(scoped) = protocol::repository_scope::scoped_state_root(project_root_path) {
                        Audit::at_scoped_root(&scoped).log_event(AuditEvent::new(
                            AuditActor::User,
                            AuditEventKind::LockfileMismatch,
                            serde_json::json!({
                                "plugin": entry.name,
                                "target": current_target,
                                "expected_sha256": expected,
                                "actual_sha256": actual,
                                "lockfile": lockfile_display.clone(),
                            }),
                        ));
                    }
                    entries.push(PluginLockVerifyEntry {
                        name: entry.name.clone(),
                        status: "mismatch",
                        target: current_target.to_string(),
                        expected_sha256: expected,
                        actual_sha256: Some(actual),
                        installed_path: Some(installed_path.to_string_lossy().to_string()),
                        detail: Some("sha256 of installed binary does not match lockfile".to_string()),
                        scope,
                        lockfile: lockfile_display.clone(),
                    });
                }
                Ok(LockVerifyResult::MissingTarget { target }) => {
                    // The plugin IS pinned, but there's no host-only binary
                    // claim for THIS platform (1.0-migrated, or a lock made
                    // elsewhere). Reported distinctly so the operator knows to
                    // reinstall on this platform rather than chase a "mismatch".
                    missing_target += 1;
                    entries.push(PluginLockVerifyEntry {
                        name: entry.name.clone(),
                        status: "missing_target",
                        target: target.clone(),
                        expected_sha256: String::new(),
                        actual_sha256: None,
                        installed_path: Some(installed_path.to_string_lossy().to_string()),
                        detail: Some(format!(
                            "lockfile has no integrity claim for target '{target}' (migrated from 1.0, or generated on \
                             another platform) — run `animus plugin install` on this platform to record it"
                        )),
                        scope,
                        lockfile: lockfile_display.clone(),
                    });
                }
                Ok(LockVerifyResult::Missing) => {
                    // Should not happen because we just iterated the lockfile entries.
                    entries.push(PluginLockVerifyEntry {
                        name: entry.name.clone(),
                        status: "missing_lock_entry",
                        target: current_target.to_string(),
                        expected_sha256: expected_for_target,
                        actual_sha256: None,
                        installed_path: Some(installed_path.to_string_lossy().to_string()),
                        detail: Some("entry vanished between read and verify".to_string()),
                        scope,
                        lockfile: lockfile_display.clone(),
                    });
                }
                Err(err) => {
                    entries.push(PluginLockVerifyEntry {
                        name: entry.name.clone(),
                        status: "error",
                        target: current_target.to_string(),
                        expected_sha256: expected_for_target,
                        actual_sha256: None,
                        installed_path: Some(installed_path.to_string_lossy().to_string()),
                        detail: Some(err.to_string()),
                        scope,
                        lockfile: lockfile_display.clone(),
                    });
                }
            }
        }
    }
    // Drift the other direction: installed plugins absent from every swept
    // lockfile. Discovery failure is non-fatal here — the integrity sweep
    // above already ran, and extra-detection is an additive signal.
    //
    // Discover UNRESTRICTED (bypassing the project's flavor/`plugin-scope.yaml`
    // filter): an installed plugin that the runtime scope excludes is still
    // on disk and unpinned, so the integrity gate must still flag it as extra
    // (codex P2) — matching the daemon lock-drift probe.
    //
    // TODO(codex-p2): scope/path-aware extra-detection. A project-local binary
    // that SHADOWS a same-named globally-locked plugin while having NO project
    // lock entry is not flagged here because `locked_names` (union of both
    // roots, keyed by name) contains the global name. Distinguishing this
    // needs path/scope matching against the existing project-shadow logic
    // above (see `pin_to_project_dir`); deferred as a rare edge case.
    let mut extra = 0_usize;
    if detect_extra {
        if let Ok(discovered) = PluginDiscovery::new()
            .with_project_root(project_root_path)
            .with_scope(orchestrator_plugin_host::PluginScope::unrestricted())
            .discover()
        {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for plugin in &discovered {
                if locked_names.contains(&plugin.name) || !seen.insert(plugin.name.clone()) {
                    continue;
                }
                extra += 1;
                entries.push(PluginLockVerifyEntry {
                    name: plugin.name.clone(),
                    status: "extra",
                    target: current_target.to_string(),
                    expected_sha256: String::new(),
                    actual_sha256: None,
                    installed_path: None,
                    detail: Some("installed plugin is not recorded in any lockfile".to_string()),
                    scope: "discovered",
                    lockfile: String::new(),
                });
            }
        }
    }

    Ok(PluginLockVerifyOutput {
        lockfile: primary_path.to_string_lossy().to_string(),
        target: current_target.to_string(),
        lockfiles,
        entries,
        matched,
        mismatched,
        missing_binary,
        missing_target,
        extra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a single-target integrity map for the CURRENT build target whose
    /// archive + installed-binary shas both equal `sha`. Mirrors how an install
    /// on this platform records its own target; lets the existing lock tests
    /// exercise `verify_installed` / drift against `install_dir/<name>`.
    fn lock_targets(sha: impl Into<String>) -> BTreeMap<String, TargetIntegrity> {
        let sha = sha.into();
        let mut targets = BTreeMap::new();
        if let Some(triple) = current_target_triple() {
            targets.insert(
                triple.to_string(),
                TargetIntegrity {
                    archive_sha256: sha.clone(),
                    signature_bundle_sha256: None,
                    installed_binary_sha256: Some(sha),
                },
            );
        }
        targets
    }

    fn asset(name: &str) -> GithubReleaseAsset {
        GithubReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
            digest: None,
        }
    }

    #[test]
    fn target_triple_from_asset_name_recognizes_archives_only() {
        assert_eq!(
            target_triple_from_asset_name("animus-provider-claude-x86_64-unknown-linux-gnu.tar.gz"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(target_triple_from_asset_name("ao-v0.6.5-aarch64-apple-darwin.tgz"), Some("aarch64-apple-darwin"));
        // Non-archive sidecars and the sums file itself contribute nothing.
        assert_eq!(target_triple_from_asset_name("SHA256SUMS.txt"), None);
        assert_eq!(target_triple_from_asset_name("foo-x86_64-unknown-linux-gnu.tar.gz.bundle"), None);
        assert_eq!(target_triple_from_asset_name("README.md"), None);
    }

    #[test]
    fn parse_sha256sums_populates_all_targets() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let c = "c".repeat(64);
        let body = format!(
            "{a}  animus-provider-claude-x86_64-unknown-linux-gnu.tar.gz\n\
             {b}  animus-provider-claude-aarch64-apple-darwin.tar.gz\n\
             {c} *animus-provider-claude-x86_64-pc-windows-msvc.tar.gz\n\
             deadbeef  not-a-valid-line\n"
        );
        let targets = parse_sha256sums_for_targets(&body, "animus-provider-claude");
        assert_eq!(targets.len(), 3);
        assert_eq!(targets.get("x86_64-unknown-linux-gnu"), Some(&a));
        assert_eq!(targets.get("aarch64-apple-darwin"), Some(&b));
        // The `*` binary-mode marker is stripped.
        assert_eq!(targets.get("x86_64-pc-windows-msvc"), Some(&c));
    }

    #[test]
    fn parse_sha256sums_prefers_plugin_prefixed_archive_per_target() {
        // A multi-binary release publishes a secondary archive for the SAME
        // target; the plugin-prefixed primary must win.
        let primary = "1".repeat(64);
        let secondary = "2".repeat(64);
        let body = format!(
            "{secondary}  other-bin-x86_64-unknown-linux-gnu.tar.gz\n\
             {primary}  animus-provider-claude-x86_64-unknown-linux-gnu.tar.gz\n"
        );
        let targets = parse_sha256sums_for_targets(&body, "animus-provider-claude");
        assert_eq!(targets.get("x86_64-unknown-linux-gnu"), Some(&primary));
    }

    fn warning_row(name: &str, source: &'static str) -> PluginWarningRow {
        PluginWarningRow {
            name: name.to_string(),
            source,
            path: format!("/gone/{name}"),
            reason: "configured binary not found: /tmp/missing".to_string(),
        }
    }

    #[test]
    fn stale_explicit_config_warnings_collapse_to_one_summary_line() {
        let warnings = vec![
            warning_row("animus-subject-default", "explicit_config"),
            warning_row("animus-subject-requirements", "explicit_config"),
            warning_row("animus-provider-broken", "project_local"),
        ];
        let lines = plugin_list_warning_lines(&warnings, false);
        // Stale not-found entries collapse across BOTH registry tiers
        // (explicit_config + project_local) into a single summary line.
        assert_eq!(lines.len(), 1, "{lines:?}");
        let summary = lines.iter().find(|l| l.contains("stale plugins.yaml")).expect("summary line");
        assert!(summary.contains("3 stale plugins.yaml entries"), "{summary}");
        assert!(summary.contains("animus plugin prune"), "summary must name the prune remedy: {summary}");
    }

    #[test]
    fn single_stale_entry_uses_singular_noun() {
        let warnings = vec![warning_row("animus-subject-default", "explicit_config")];
        let lines = plugin_list_warning_lines(&warnings, false);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("1 stale plugins.yaml entry"), "{}", lines[0]);
    }

    #[test]
    fn verbose_keeps_one_line_per_warning() {
        let warnings = vec![
            warning_row("animus-subject-default", "explicit_config"),
            warning_row("animus-subject-requirements", "explicit_config"),
        ];
        let lines = plugin_list_warning_lines(&warnings, true);
        assert_eq!(lines.len(), 2, "verbose prints one line per stale entry: {lines:?}");
        assert!(lines.iter().all(|l| l.contains("could not be loaded")), "{lines:?}");
    }

    #[test]
    fn normalize_installed_at_handles_both_formats() {
        // RFC3339 (`T` separator).
        assert_eq!(normalize_installed_at("2026-05-30T20:25:20.159924+00:00"), "2026-05-30 20:25");
        // Space-separated chrono debug form.
        assert_eq!(normalize_installed_at("2026-05-29 17:32:43.172858+00:00"), "2026-05-29 17:32");
        // Date only.
        assert_eq!(normalize_installed_at("2026-06-05"), "2026-06-05");
        // Empty -> placeholder.
        assert_eq!(normalize_installed_at("  "), "--");
    }

    #[test]
    fn list_version_cell_uses_lockfile_tag_and_flags_mismatch() {
        // Tag and manifest agree (v-prefix tolerated) -> just the tag.
        assert_eq!(list_version_cell(Some("v0.2.1"), "0.2.1"), "v0.2.1");
        // Disagreement -> tag wins, binary version flagged.
        assert_eq!(list_version_cell(Some("v0.2.1"), "0.1.0"), "v0.2.1 (binary 0.1.0)");
        // No recorded tag -> fall back to manifest version.
        assert_eq!(list_version_cell(None, "0.1.0"), "0.1.0");
        // Neither -> placeholder.
        assert_eq!(list_version_cell(None, ""), "--");
        // Tag present, manifest empty -> tag alone (no spurious mismatch).
        assert_eq!(list_version_cell(Some("v0.2.1"), ""), "v0.2.1");
    }

    #[test]
    fn collect_stale_entries_flags_missing_binaries() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path();
        // One present binary, one missing.
        let present = install_dir.join("animus-present");
        std::fs::write(&present, b"x").unwrap();

        let yaml = format!(
            "plugins:\n  animus-present:\n    binary: {}\n  animus-gone:\n    binary: {}\n",
            present.display(),
            install_dir.join("animus-gone").display()
        );
        let mut config: PluginsYamlConfig = serde_yaml::from_str(&yaml).unwrap();

        // Preview: collect without removing.
        let mut preview = Vec::new();
        let removed = collect_stale_entries(
            &mut config,
            install_dir,
            "global",
            Path::new("/tmp/plugins.yaml"),
            false,
            &mut preview,
        );
        assert!(!removed, "preview must not mutate");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].name, "animus-gone");
        assert!(config.plugins.contains_key(serde_yaml::Value::String("animus-gone".to_string())));

        // Apply: remove the stale entry.
        let mut applied = Vec::new();
        let removed = collect_stale_entries(
            &mut config,
            install_dir,
            "global",
            Path::new("/tmp/plugins.yaml"),
            true,
            &mut applied,
        );
        assert!(removed);
        assert_eq!(applied.len(), 1);
        assert!(!config.plugins.contains_key(serde_yaml::Value::String("animus-gone".to_string())));
        assert!(config.plugins.contains_key(serde_yaml::Value::String("animus-present".to_string())));
    }

    #[test]
    fn parses_repo_at_tag_syntax() {
        let spec = parse_repo_spec("launchapp-dev/animus-provider-claude@v0.1.0").unwrap();
        assert_eq!(spec.owner, "launchapp-dev");
        assert_eq!(spec.repo, "animus-provider-claude");
        assert_eq!(spec.tag.as_deref(), Some("v0.1.0"));
    }

    #[test]
    fn parses_repo_without_tag() {
        let spec = parse_repo_spec("launchapp-dev/animus-provider-claude").unwrap();
        assert_eq!(spec.owner, "launchapp-dev");
        assert_eq!(spec.repo, "animus-provider-claude");
        assert!(spec.tag.is_none());
    }

    #[test]
    fn parse_repo_spec_trims_whitespace() {
        let spec = parse_repo_spec("  launchapp-dev/foo @ v1.2.3  ").unwrap();
        assert_eq!(spec.owner, "launchapp-dev");
        assert_eq!(spec.repo, "foo");
        assert_eq!(spec.tag.as_deref(), Some("v1.2.3"));
    }

    #[test]
    fn parse_repo_spec_rejects_missing_slash() {
        let err = parse_repo_spec("animus-provider-claude").unwrap_err();
        assert!(format!("{err}").contains("owner/repo"));
    }

    #[test]
    fn parse_repo_spec_rejects_empty_tag() {
        let err = parse_repo_spec("launchapp-dev/foo@").unwrap_err();
        assert!(format!("{err}").contains("empty tag"));
    }

    #[test]
    fn parse_repo_spec_rejects_empty_owner_or_repo() {
        assert!(parse_repo_spec("/foo").is_err());
        assert!(parse_repo_spec("foo/").is_err());
        assert!(parse_repo_spec("").is_err());
    }

    #[test]
    fn selects_aarch64_apple_darwin_asset() {
        let assets = vec![
            asset("animus-provider-oai-aarch64-apple-darwin.tar.gz"),
            asset("animus-provider-oai-x86_64-apple-darwin.tar.gz"),
            asset("animus-provider-oai-x86_64-unknown-linux-gnu.tar.gz"),
        ];
        let tokens: &[&str] = &["aarch64-apple-darwin", "macos-aarch64", "darwin-arm64"];
        let picked = pick_release_asset(&assets, tokens).expect("expected an asset to match");
        assert_eq!(picked.name, "animus-provider-oai-aarch64-apple-darwin.tar.gz");
    }

    #[test]
    fn selects_x86_64_linux_asset() {
        let assets = vec![
            asset("animus-provider-oai-aarch64-apple-darwin.tar.gz"),
            asset("animus-provider-oai-x86_64-apple-darwin.tar.gz"),
            asset("animus-provider-oai-x86_64-unknown-linux-gnu.tar.gz"),
        ];
        let tokens: &[&str] = &["x86_64-unknown-linux-gnu", "linux-x86_64", "linux-amd64"];
        let picked = pick_release_asset(&assets, tokens).expect("expected an asset to match");
        assert_eq!(picked.name, "animus-provider-oai-x86_64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn errors_clearly_when_no_matching_asset() {
        let assets = vec![asset("animus-provider-oai-x86_64-unknown-linux-gnu.tar.gz")];
        let tokens: &[&str] = &["aarch64-apple-darwin", "macos-aarch64"];
        assert!(pick_release_asset(&assets, tokens).is_none());
    }

    #[test]
    fn current_platform_has_known_tokens() {
        let tokens = current_platform_tokens();
        assert!(!tokens.is_empty(), "no platform tokens registered for {}", current_platform_label());
    }

    #[test]
    fn excludes_sha256_sidecars_from_asset_picking() {
        let assets = vec![
            asset("animus-provider-oai-aarch64-apple-darwin.tar.gz.sha256"),
            asset("animus-provider-oai-aarch64-apple-darwin.tar.gz"),
        ];
        let tokens: &[&str] = &["aarch64-apple-darwin"];
        let picked = pick_release_asset(&assets, tokens).expect("expected the binary asset to win");
        assert_eq!(picked.name, "animus-provider-oai-aarch64-apple-darwin.tar.gz");
    }

    // ===== Multi-binary plugin install support (v0.5.3 Task A) =====

    /// Backward-compat: plugin.toml without `[[binaries]]` falls back to
    /// single-binary install (empty descriptor list signals legacy mode).
    #[test]
    fn parse_plugin_toml_binaries_returns_empty_when_section_absent() {
        let text = r#"
schema = "animus.plugin.v1"
name = "animus-provider-claude"
version = "0.2.2"
plugin_kind = "provider"

[binary]
default = "animus-provider-claude"
"#;
        let bins = parse_plugin_toml_binaries(text).unwrap();
        assert!(bins.is_empty(), "no [[binaries]] section must yield single-binary legacy mode");
    }

    /// Multi-binary declaration: parses two entries and respects the
    /// explicit `primary = true` marker.
    #[test]
    fn parse_plugin_toml_binaries_picks_explicit_primary() {
        let text = r#"
schema = "animus.plugin.v1"
name = "animus-provider-oai-agent"
version = "0.1.4"
plugin_kind = "provider"

[[binaries]]
name = "animus-provider-oai-agent"
primary = true

[[binaries]]
name = "animus-oai-runner"
"#;
        let bins = parse_plugin_toml_binaries(text).unwrap();
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].name, "animus-provider-oai-agent");
        assert!(bins[0].primary);
        assert_eq!(bins[1].name, "animus-oai-runner");
        assert!(!bins[1].primary);
    }

    /// When no entry is marked primary but one matches the plugin's `name`
    /// field, that entry is promoted to primary automatically.
    #[test]
    fn parse_plugin_toml_binaries_promotes_matching_name_when_no_primary_declared() {
        let text = r#"
schema = "animus.plugin.v1"
name = "animus-provider-oai-agent"
version = "0.1.4"

[[binaries]]
name = "animus-oai-runner"

[[binaries]]
name = "animus-provider-oai-agent"
"#;
        let bins = parse_plugin_toml_binaries(text).unwrap();
        let primaries: Vec<&str> = bins.iter().filter(|b| b.primary).map(|b| b.name.as_str()).collect();
        assert_eq!(primaries, vec!["animus-provider-oai-agent"]);
    }

    /// When nothing matches and nothing is marked, fall back to the first
    /// entry (keeps the install pipeline deterministic).
    #[test]
    fn parse_plugin_toml_binaries_falls_back_to_first_when_no_primary_marker() {
        let text = r#"
[[binaries]]
name = "binary-one"

[[binaries]]
name = "binary-two"
"#;
        let bins = parse_plugin_toml_binaries(text).unwrap();
        assert!(bins[0].primary);
        assert!(!bins[1].primary);
    }

    /// Refuse a malformed plugin.toml that declares two primary binaries.
    /// The install pipeline can't decide which to copy to the canonical
    /// `<plugin_name>` path.
    #[test]
    fn parse_plugin_toml_binaries_rejects_multiple_primaries() {
        let text = r#"
[[binaries]]
name = "one"
primary = true

[[binaries]]
name = "two"
primary = true
"#;
        let err = parse_plugin_toml_binaries(text).unwrap_err();
        assert!(format!("{err}").contains("more than one"));
    }

    /// Refuse duplicate binary names — uninstall would otherwise
    /// double-delete the same path.
    #[test]
    fn parse_plugin_toml_binaries_rejects_duplicates() {
        let text = r#"
[[binaries]]
name = "same"

[[binaries]]
name = "same"
"#;
        let err = parse_plugin_toml_binaries(text).unwrap_err();
        assert!(format!("{err}").contains("more than once"));
    }

    /// `pick_release_asset_for_binary` must constrain selection to assets
    /// whose name begins with the binary name. With both `animus-provider-
    /// oai-agent-*` and `animus-oai-runner-*` archives in the release, the
    /// runner pick must NOT match an `animus-provider-oai-agent-*` archive.
    #[test]
    fn pick_release_asset_for_binary_filters_to_matching_prefix() {
        let assets = vec![
            asset("animus-provider-oai-agent-aarch64-apple-darwin.tar.gz"),
            asset("animus-provider-oai-agent-aarch64-apple-darwin.tar.gz.sha256"),
            asset("animus-oai-runner-aarch64-apple-darwin.tar.gz"),
            asset("animus-oai-runner-aarch64-apple-darwin.tar.gz.sha256"),
        ];
        let tokens: &[&str] = &["aarch64-apple-darwin"];
        let agent = pick_release_asset_for_binary(&assets, "animus-provider-oai-agent", tokens).unwrap();
        assert_eq!(agent.name, "animus-provider-oai-agent-aarch64-apple-darwin.tar.gz");
        let runner = pick_release_asset_for_binary(&assets, "animus-oai-runner", tokens).unwrap();
        assert_eq!(runner.name, "animus-oai-runner-aarch64-apple-darwin.tar.gz");
    }

    /// Cosign `.bundle` sidecars must also be excluded from the binary
    /// selection — they are sibling artifacts, not installable binaries.
    #[test]
    fn pick_release_asset_for_binary_excludes_bundles_and_sidecars() {
        let assets = vec![
            asset("animus-oai-runner-x86_64-unknown-linux-gnu.tar.gz.sha256"),
            asset("animus-oai-runner-x86_64-unknown-linux-gnu.tar.gz.bundle"),
            asset("animus-oai-runner-x86_64-unknown-linux-gnu.tar.gz"),
        ];
        let tokens: &[&str] = &["x86_64-unknown-linux-gnu"];
        let runner = pick_release_asset_for_binary(&assets, "animus-oai-runner", tokens).unwrap();
        assert_eq!(runner.name, "animus-oai-runner-x86_64-unknown-linux-gnu.tar.gz");
    }

    /// No matching asset for a declared secondary binary returns None so
    /// the install pipeline can produce an actionable error.
    #[test]
    fn pick_release_asset_for_binary_returns_none_when_no_prefix_match() {
        let assets = vec![asset("animus-provider-oai-agent-aarch64-apple-darwin.tar.gz")];
        let tokens: &[&str] = &["aarch64-apple-darwin"];
        assert!(pick_release_asset_for_binary(&assets, "animus-oai-runner", tokens).is_none());
    }

    #[test]
    fn verifies_sha256_sidecar_when_present() {
        let assets = vec![
            asset("animus-provider-oai-aarch64-apple-darwin.tar.gz"),
            asset("animus-provider-oai-aarch64-apple-darwin.tar.gz.sha256"),
        ];
        let sidecar = find_sha256_sidecar(&assets, "animus-provider-oai-aarch64-apple-darwin.tar.gz")
            .expect("expected sidecar to be found");
        assert_eq!(sidecar.name, "animus-provider-oai-aarch64-apple-darwin.tar.gz.sha256");

        let body =
            "a10a3a505ca102bc4249d4e660f0622278abd319054a2e033b72988783ea7a48  animus-provider-oai-aarch64-apple-darwin.tar.gz\n";
        let hex = parse_sha256_sidecar(body).unwrap();
        assert_eq!(hex, "a10a3a505ca102bc4249d4e660f0622278abd319054a2e033b72988783ea7a48");
    }

    #[test]
    fn returns_none_when_no_sha256_sidecar() {
        let assets = vec![asset("animus-provider-oai-aarch64-apple-darwin.tar.gz")];
        assert!(find_sha256_sidecar(&assets, "animus-provider-oai-aarch64-apple-darwin.tar.gz").is_none());
    }

    #[test]
    fn parses_release_digest_field() {
        let hex =
            parse_release_digest("sha256:a10a3a505ca102bc4249d4e660f0622278abd319054a2e033b72988783ea7a48").unwrap();
        assert_eq!(hex, "a10a3a505ca102bc4249d4e660f0622278abd319054a2e033b72988783ea7a48");
        assert!(parse_release_digest("md5:deadbeef").is_none());
        assert!(parse_release_digest("sha256:").is_none());
        assert!(parse_release_digest("not-a-digest").is_none());
        assert!(parse_release_digest("sha256:deadbeef").is_none()); // too short
    }

    #[test]
    fn parses_hex_only_sidecar_body() {
        let body = "A10A3A505CA102BC4249D4E660F0622278ABD319054A2E033B72988783EA7A48\n";
        let hex = parse_sha256_sidecar(body).unwrap();
        assert_eq!(hex, "a10a3a505ca102bc4249d4e660f0622278abd319054a2e033b72988783ea7a48");
    }

    #[test]
    fn rejects_malformed_sidecar_body() {
        assert!(parse_sha256_sidecar("").is_none());
        assert!(parse_sha256_sidecar("not-a-hex-digest").is_none());
        assert!(parse_sha256_sidecar("deadbeef").is_none()); // too short
    }

    #[test]
    fn github_release_api_url_is_correct() {
        assert_eq!(
            github_release_api_url("launchapp-dev", "animus-provider-oai", None),
            "https://api.github.com/repos/launchapp-dev/animus-provider-oai/releases/latest"
        );
        assert_eq!(
            github_release_api_url("launchapp-dev", "animus-provider-oai", Some("v0.1.0")),
            "https://api.github.com/repos/launchapp-dev/animus-provider-oai/releases/tags/v0.1.0"
        );
    }

    #[test]
    fn extract_tarball_returns_named_binary() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("plugin.tar.gz");
        let bin_path = dir.path().join("animus-provider-foo");
        std::fs::File::create(&bin_path).unwrap().write_all(b"#!/bin/sh\necho ok\n").unwrap();
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            builder.append_path_with_name(&bin_path, "animus-provider-foo").unwrap();
            let enc = builder.into_inner().unwrap();
            enc.finish().unwrap();
        }

        let extract_dir = dir.path().join("extracted");
        let extracted = extract_tarball(&archive_path, &extract_dir, "animus-provider-foo").unwrap();
        assert_eq!(extracted.file_name().and_then(|n| n.to_str()), Some("animus-provider-foo"));
        assert!(extracted.exists());
    }

    /// Tarball containing README + LICENSE alongside the binary. Pre-fix
    /// behavior installed the README. With basename matching the binary
    /// always wins.
    #[test]
    fn extract_tarball_prefers_matching_basename() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("plugin.tar.gz");

        // Write three files into a staging dir, then archive all three.
        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let readme = staging.join("README.md");
        let license = staging.join("LICENSE");
        let binary = staging.join("animus-provider-foo");
        std::fs::File::create(&readme).unwrap().write_all(b"# Foo Plugin\n").unwrap();
        std::fs::File::create(&license).unwrap().write_all(b"MIT\n").unwrap();
        std::fs::File::create(&binary).unwrap().write_all(b"#!/bin/sh\necho ok\n").unwrap();

        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder.append_path_with_name(&readme, "README.md").unwrap();
        builder.append_path_with_name(&license, "LICENSE").unwrap();
        builder.append_path_with_name(&binary, "animus-provider-foo").unwrap();
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap();

        let extract_dir = dir.path().join("extracted");
        let extracted = extract_tarball(&archive_path, &extract_dir, "animus-provider-foo").unwrap();
        assert_eq!(
            extracted.file_name().and_then(|n| n.to_str()),
            Some("animus-provider-foo"),
            "basename match must win even when README/LICENSE come first in walk order"
        );
    }

    /// No file name-matches the expected plugin name, but exactly one is
    /// executable. The executable wins. Mirrors releases that publish
    /// `<plugin>-<version>` style binaries.
    #[cfg(unix)]
    #[test]
    fn extract_tarball_picks_only_executable_when_no_name_match() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("plugin.tar.gz");

        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let readme = staging.join("README.md");
        let binary = staging.join("animus-provider-foo-v0.1.0");
        std::fs::File::create(&readme).unwrap().write_all(b"# readme\n").unwrap();
        std::fs::File::create(&binary).unwrap().write_all(b"#!/bin/sh\necho ok\n").unwrap();
        // Mark the binary executable; README stays mode 0o644.
        let mut perms = std::fs::metadata(&binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms).unwrap();

        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder.append_path_with_name(&readme, "README.md").unwrap();
        builder.append_path_with_name(&binary, "animus-provider-foo-v0.1.0").unwrap();
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap();

        let extract_dir = dir.path().join("extracted");
        let extracted = extract_tarball(&archive_path, &extract_dir, "animus-provider-foo").unwrap();
        assert_eq!(
            extracted.file_name().and_then(|n| n.to_str()),
            Some("animus-provider-foo-v0.1.0"),
            "the sole executable must win when no basename matches"
        );
    }

    // =================== Tempdir cleanup tests (gap #13) ===================
    //
    // The pre-fix code created `std::env::temp_dir().join(uuid)` and
    // never removed it; the fix wraps creation in a [`tempfile::TempDir`]
    // RAII guard. These tests track the *specific* staging dir created
    // by `create_install_staging_dir()` (uuid-suffixed, can't collide
    // with parallel tests' tempdirs) and assert it disappears once the
    // guard drops — both on the happy path and when a downstream step
    // errors before the binary is copied to its final home.

    /// On success: the staging dir lives until the `TempDir` guard is
    /// dropped, then disappears. Mirrors the install pipeline's
    /// contract: caller holds the guard while copying out the binary,
    /// then drops it.
    #[test]
    fn install_staging_dir_cleaned_up_on_success() {
        let staging_path: PathBuf;
        {
            let staging = create_install_staging_dir().expect("create staging");
            staging_path = staging.path().to_path_buf();
            assert!(staging_path.exists(), "staging dir should exist while guard is held");
            // Sanity: the dir lives in the platform temp dir and uses
            // the documented `animus-plugin-install-` prefix so logs and
            // cleanup scripts can find it.
            let basename = staging_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            assert!(
                basename.starts_with("animus-plugin-install-"),
                "staging dir basename must start with `animus-plugin-install-`, got '{basename}'"
            );
            assert!(
                staging_path.starts_with(std::env::temp_dir()),
                "staging dir must live under the platform temp dir, got {staging_path:?}"
            );
        } // drop

        // After: RAII removed the staging dir.
        assert!(!staging_path.exists(), "staging dir must be removed when TempDir drops; leaked: {staging_path:?}");
    }

    /// On failure: even when the caller errors after creating the
    /// staging dir, the RAII guard drop still cleans it up. Simulates
    /// "download then sha256 mismatch" / "extraction failed" /
    /// "manifest probe rejected" paths.
    #[test]
    fn install_staging_dir_cleaned_up_on_failure() {
        let staging_path: PathBuf = (|| -> Result<PathBuf> {
            let staging = create_install_staging_dir()?;
            let path = staging.path().to_path_buf();
            // Mirror the real install pipeline: write a "download" into
            // the staging dir, then fail before copying it out.
            std::fs::write(path.join("downloaded.tar.gz"), b"bytes")?;
            assert!(path.exists());
            // Return the path — `staging` drops here as it leaves the
            // closure, simulating the install function returning Err.
            Ok(path)
        })()
        .expect("setup must not fail");

        assert!(
            !staging_path.exists(),
            "staging dir must be removed even when the install path errored before copy; leaked: {staging_path:?}"
        );
    }

    /// Tarball with two non-matching, non-executable files. We must error
    /// loudly and list every file rather than silently install whichever
    /// came back first.
    #[test]
    fn extract_tarball_errors_clearly_on_ambiguous_content() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("plugin.tar.gz");

        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let readme = staging.join("README.md");
        let license = staging.join("LICENSE");
        std::fs::File::create(&readme).unwrap().write_all(b"# readme\n").unwrap();
        std::fs::File::create(&license).unwrap().write_all(b"MIT\n").unwrap();

        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder.append_path_with_name(&readme, "README.md").unwrap();
        builder.append_path_with_name(&license, "LICENSE").unwrap();
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap();

        let extract_dir = dir.path().join("extracted");
        let err = extract_tarball(&archive_path, &extract_dir, "animus-provider-foo")
            .expect_err("ambiguous tarball must not silently install");
        let reason = format!("{err:#}");
        assert!(reason.contains("animus-provider-foo"), "error must name expected plugin, got: {reason}");
        assert!(reason.contains("README.md"), "error must list README.md, got: {reason}");
        assert!(reason.contains("LICENSE"), "error must list LICENSE, got: {reason}");
    }

    // =================== Signature verification tests ===================

    fn release_provenance_with_bundle(bundle: Option<std::path::PathBuf>) -> InstallProvenance {
        InstallProvenance {
            source_kind: Some("release"),
            origin: Some("launchapp-dev/animus-provider-claude@v0.1.2".to_string()),
            release_tag: Some("v0.1.2".to_string()),
            asset_name: Some("animus-provider-claude-aarch64-apple-darwin.tar.gz".to_string()),
            sha256_verified: Some(true),
            asset_archive_path: Some(std::path::PathBuf::from("/tmp/example.tar.gz")),
            bundle_path: bundle,
            owner: Some("launchapp-dev".to_string()),
            repo: Some("animus-provider-claude".to_string()),
            source_repo: Some("launchapp-dev/animus-provider-claude".to_string()),
            resolved_commit: None,
            sha256sums_targets: BTreeMap::new(),
            sha256sums_body: None,
        }
    }

    #[test]
    fn marks_unsigned_when_no_bundle_in_release() {
        let req = PluginInstallRequest::default();
        let prov = release_provenance_with_bundle(None);
        let status = resolve_signature_status(&req, &prov).expect("status should resolve");
        assert_eq!(status.label(), "unsigned");
    }

    #[test]
    fn refuses_install_when_require_signature_and_no_bundle() {
        let status = SignatureStatus::Unsigned { reason: "no bundle".to_string() };
        let require_signature = true;
        let blocked = matches!(&status, SignatureStatus::Unsigned { .. }) && require_signature;
        assert!(blocked);
    }

    #[test]
    fn skips_verification_when_skip_signature_flag() {
        let req = PluginInstallRequest { skip_signature: true, ..Default::default() };
        let prov = release_provenance_with_bundle(Some(std::path::PathBuf::from("/tmp/x.bundle")));
        let status = resolve_signature_status(&req, &prov).expect("status should resolve");
        assert_eq!(status, SignatureStatus::Skipped);
    }

    #[test]
    fn falls_back_to_unsigned_when_cosign_not_in_path() {
        let req = PluginInstallRequest::default();
        let tmp = tempfile::tempdir().unwrap();
        let fake_bundle = tmp.path().join("fake.bundle");
        std::fs::write(&fake_bundle, b"not a real bundle").unwrap();
        let prov = release_provenance_with_bundle(Some(fake_bundle));
        let status = resolve_signature_status(&req, &prov).expect("status should resolve");
        // Without cosign on PATH: Unsigned. With cosign: Invalid (fake bytes).
        assert!(matches!(&status, SignatureStatus::Unsigned { .. } | SignatureStatus::Invalid { .. }));
    }

    /// Wiring guard: a launchapp-dev install with a bundle present must
    /// route through `orchestrator_plugin_host::verify_plugin_install`,
    /// which anchors on the strict
    /// `^https://github\.com/launchapp-dev/[^/]+/\.github/workflows/release\.yml@refs/tags/v.*`
    /// regex (not the legacy per-spec `^https://github\.com/<owner>/<repo>/.+`
    /// pattern from signing.rs). We verify the wiring by checking the
    /// identity_pattern surfaced on the Invalid result.
    #[test]
    fn launchapp_dev_install_uses_strict_trusted_publisher_regex() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("fake.bundle");
        std::fs::write(&bundle, b"not a real bundle").unwrap();
        let archive = tmp.path().join("animus-provider-claude.tar.gz");
        std::fs::write(&archive, b"fake archive").unwrap();
        let mut prov = release_provenance_with_bundle(Some(bundle));
        prov.asset_archive_path = Some(archive);

        let req = PluginInstallRequest::default();
        let status = resolve_signature_status(&req, &prov).expect("resolves");

        match &status {
            SignatureStatus::Invalid { identity_pattern, .. } => {
                assert!(
                    identity_pattern.contains("\\.github/workflows/release\\.yml@refs/tags/v"),
                    "launchapp-dev path must use the strict TrustedPublisher regex anchored at \
                     `/.github/workflows/release.yml@refs/tags/v*`, got: {identity_pattern}"
                );
                assert!(
                    identity_pattern.contains("launchapp-dev/animus-provider-claude/"),
                    "identity pattern must be pinned to the requested repo, got: {identity_pattern}"
                );
                assert!(
                    !identity_pattern.contains("[^/]+"),
                    "identity pattern must NOT use the org-wide `[^/]+` wildcard, got: {identity_pattern}"
                );
            }
            SignatureStatus::Unsigned { reason } => {
                assert!(
                    reason.contains("cosign"),
                    "Unsigned reason must come from the host's cosign-missing path, got: {reason}"
                );
            }
            other => panic!("expected Invalid or Unsigned from host TrustedPublisher path, got: {other:?}"),
        }
    }

    /// Regression for codex round-2 P1: the regex helper preserves safe
    /// GitHub slug characters and escapes regex metacharacters. Even though
    /// real GitHub owner/repo slugs only contain `[A-Za-z0-9._-]`, this
    /// keeps the cosign command line safe if that ever changes.
    #[test]
    fn regex_escape_for_identity_passes_safe_chars_through() {
        assert_eq!(regex_escape_for_identity("launchapp-dev"), "launchapp-dev");
        assert_eq!(regex_escape_for_identity("animus-provider-claude"), "animus-provider-claude");
        assert_eq!(regex_escape_for_identity("animus_subject.linear"), "animus_subject\\.linear");
        assert_eq!(regex_escape_for_identity("a.b+c*d"), "a\\.b\\+c\\*d");
    }

    /// Strict mode + missing bundle on a launchapp-dev install must Block
    /// the install (Unsigned -> strict failure). Guards against a future
    /// regression that lets the launchapp-dev install path silently succeed
    /// when no signature bundle was published.
    #[test]
    fn launchapp_dev_strict_install_blocks_when_bundle_missing() {
        let req = PluginInstallRequest { signature_policy: Some(PluginPolicyMode::Strict), ..Default::default() };
        let prov = release_provenance_with_bundle(None);
        let status = resolve_signature_status(&req, &prov).expect("resolves");
        assert!(matches!(&status, SignatureStatus::Unsigned { .. }), "missing bundle must yield Unsigned");

        let outcome = evaluate_signature_policy(&status, PluginPolicyMode::Strict, false);
        assert!(
            matches!(outcome, SignaturePolicyOutcome::Block { .. }),
            "strict + missing bundle on launchapp-dev install must Block, got: {outcome:?}"
        );
    }

    /// Regression: when the host TrustedPublisher path verifies a
    /// launchapp-dev install but the operator has narrowed
    /// `trusted-signers.yaml` to a different repo, the verdict must
    /// still flip to `UntrustedSigner`. Without this gate, the host's
    /// owner-wide TrustedPublisher policy would bypass the operator's
    /// per-repo allowlist (codex round-1 P1).
    #[test]
    fn launchapp_dev_host_verify_respects_trusted_signers_repo_narrowing() {
        let tmp = tempfile::tempdir().unwrap();
        let signers_yaml = tmp.path().join("trusted-signers.yaml");
        std::fs::write(&signers_yaml, "trusted_signers:\n  - identity: \"launchapp-dev/animus-subject-linear\"\n")
            .unwrap();

        let mapped = SignatureStatus::Verified {
            identity: "^https://github\\.com/launchapp-dev/[^/]+/\\.github/workflows/release\\.yml@refs/tags/v.*"
                .to_string(),
            bundle_path: "/tmp/x.bundle".to_string(),
        };
        let cfg = load_trusted_signers(&signers_yaml).unwrap().expect("config loads");
        let owner = "launchapp-dev";
        let repo = "animus-provider-claude";
        let slug = format!("{owner}/{repo}");

        let allowlisted = cfg.matches_repo(&slug);
        assert!(!allowlisted, "non-allowlisted repo must NOT match the narrowed yaml");

        let identity_regex = cfg.identity_regexp_for(owner, repo);
        let gated = if let SignatureStatus::Verified { .. } = &mapped {
            if !cfg.matches_repo(&slug) {
                SignatureStatus::UntrustedSigner { identity_pattern: identity_regex }
            } else {
                mapped
            }
        } else {
            mapped
        };
        match gated {
            SignatureStatus::UntrustedSigner { identity_pattern } => {
                assert!(identity_pattern.contains("animus-provider-claude"));
            }
            other => panic!("narrowed allowlist must downgrade Verified -> UntrustedSigner, got: {other:?}"),
        }
    }

    /// Disabled mode (the `--signature-policy disabled` / `--skip-signature`
    /// escape hatch) must continue to short-circuit BEFORE the host
    /// TrustedPublisher path — proving the escape hatch survives the wiring.
    #[test]
    fn launchapp_dev_install_respects_disabled_escape_hatch() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("fake.bundle");
        std::fs::write(&bundle, b"not a real bundle").unwrap();
        let mut prov = release_provenance_with_bundle(Some(bundle));
        prov.asset_archive_path = Some(tmp.path().join("animus-provider-claude.tar.gz"));

        let req = PluginInstallRequest { signature_policy: Some(PluginPolicyMode::Disabled), ..Default::default() };
        let status = resolve_signature_status(&req, &prov).expect("resolves");
        assert_eq!(status, SignatureStatus::Skipped);
    }

    #[test]
    fn verifies_signature_when_bundle_present_and_cosign_works() {
        let verified = SignatureStatus::Verified {
            identity: "^https://github\\.com/launchapp-dev/animus-provider-claude/.+".to_string(),
            bundle_path: "/tmp/x.bundle".to_string(),
        };
        assert_eq!(verified.label(), "verified");
        let blocked = matches!(&verified, SignatureStatus::Unsigned { .. });
        assert!(!blocked, "Verified must never refuse install");
    }

    /// Codex round-5 P2 regression: `--signature-policy warn` (the v0.4.12
    /// default while the launchapp-dev cosign key is a placeholder) must NOT
    /// hard-fail a release whose cosign bundle reports `Invalid`. It should
    /// log a warning and proceed, matching the documented warn semantics and
    /// the existing `Unsigned` arm. Strict mode must still block.
    #[test]
    fn signature_invalid_with_warn_policy_proceeds_with_warning() {
        let status = SignatureStatus::Invalid {
            identity_pattern: "^https://github\\.com/launchapp-dev/animus-provider-claude/.+".to_string(),
            message: "no matching signatures found".to_string(),
        };

        let outcome = evaluate_signature_policy(&status, PluginPolicyMode::Warn, false);
        match outcome {
            SignaturePolicyOutcome::ProceedWithWarning { reason } => {
                assert!(
                    reason.contains("INVALID cosign signature"),
                    "warn message must call out invalid signature, got: {reason}"
                );
                assert!(reason.contains("no matching signatures found"), "warn message must include cosign reason");
            }
            other => panic!("warn policy must proceed with warning on Invalid, got: {other:?}"),
        }

        // Strict must still block — the warn-relaxation is scoped to warn mode.
        let strict_outcome = evaluate_signature_policy(&status, PluginPolicyMode::Strict, false);
        assert!(
            matches!(strict_outcome, SignaturePolicyOutcome::Block { .. }),
            "strict policy must block Invalid signatures"
        );

        // Legacy --require-signature must also block, regardless of mode.
        let legacy_outcome = evaluate_signature_policy(&status, PluginPolicyMode::Warn, true);
        assert!(
            matches!(legacy_outcome, SignaturePolicyOutcome::Block { .. }),
            "require_signature=true must override warn mode and block"
        );

        // Disabled mode silently proceeds (defense-in-depth: the resolver
        // returns Skipped under Disabled, but if some path leaks an Invalid
        // through, Disabled must NOT escalate it to a block).
        let disabled_outcome = evaluate_signature_policy(&status, PluginPolicyMode::Disabled, false);
        assert_eq!(disabled_outcome, SignaturePolicyOutcome::Proceed);
    }

    /// Codex round-5 P2 regression: `--signature-policy warn` must NOT
    /// hard-fail a release whose cosign signature is valid but signed by a
    /// signer not in `trusted-signers.yaml`. Warn mode logs and proceeds;
    /// strict mode blocks.
    #[test]
    fn untrusted_signer_with_warn_policy_proceeds_with_warning() {
        let status = SignatureStatus::UntrustedSigner {
            identity_pattern: "^https://github\\.com/unknown-org/animus-provider-foo/.+".to_string(),
        };

        let outcome = evaluate_signature_policy(&status, PluginPolicyMode::Warn, false);
        match outcome {
            SignaturePolicyOutcome::ProceedWithWarning { reason } => {
                assert!(
                    reason.contains("untrusted signer"),
                    "warn message must mention untrusted signer, got: {reason}"
                );
                assert!(reason.contains("unknown-org"), "warn message must include identity pattern");
            }
            other => panic!("warn policy must proceed with warning on UntrustedSigner, got: {other:?}"),
        }

        let strict_outcome = evaluate_signature_policy(&status, PluginPolicyMode::Strict, false);
        assert!(
            matches!(strict_outcome, SignaturePolicyOutcome::Block { .. }),
            "strict policy must block UntrustedSigner"
        );

        let legacy_outcome = evaluate_signature_policy(&status, PluginPolicyMode::Warn, true);
        assert!(
            matches!(legacy_outcome, SignaturePolicyOutcome::Block { .. }),
            "require_signature=true must override warn mode and block"
        );

        let disabled_outcome = evaluate_signature_policy(&status, PluginPolicyMode::Disabled, false);
        assert_eq!(disabled_outcome, SignaturePolicyOutcome::Proceed);
    }

    /// The Unsigned arm continues to honor the same policy matrix.
    #[test]
    fn unsigned_policy_matrix_unchanged() {
        let status =
            SignatureStatus::Unsigned { reason: "no cosign signature bundle published in release".to_string() };
        assert!(matches!(
            evaluate_signature_policy(&status, PluginPolicyMode::Warn, false),
            SignaturePolicyOutcome::ProceedWithWarning { .. }
        ));
        assert!(matches!(
            evaluate_signature_policy(&status, PluginPolicyMode::Strict, false),
            SignaturePolicyOutcome::Block { .. }
        ));
        assert_eq!(
            evaluate_signature_policy(&status, PluginPolicyMode::Disabled, false),
            SignaturePolicyOutcome::Proceed
        );
    }

    #[test]
    fn verified_and_skipped_always_proceed() {
        let verified = SignatureStatus::Verified {
            identity: "^https://github\\.com/launchapp-dev/animus-provider-claude/.+".to_string(),
            bundle_path: "/tmp/x.bundle".to_string(),
        };
        for mode in [PluginPolicyMode::Strict, PluginPolicyMode::Warn, PluginPolicyMode::Disabled] {
            assert_eq!(evaluate_signature_policy(&verified, mode, true), SignaturePolicyOutcome::Proceed);
        }
        for mode in [PluginPolicyMode::Strict, PluginPolicyMode::Warn, PluginPolicyMode::Disabled] {
            assert_eq!(
                evaluate_signature_policy(&SignatureStatus::Skipped, mode, true),
                SignaturePolicyOutcome::Proceed
            );
        }
    }

    fn provider_manifest(name: &str) -> PluginManifest {
        PluginManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            plugin_kind: animus_plugin_protocol::PLUGIN_KIND_PROVIDER.to_string(),
            description: "test".to_string(),
            protocol_version: "1.0.0".to_string(),
            capabilities: vec!["agent/run".to_string()],
            env_required: Vec::new(),
            notification_buffer_size: None,
        }
    }

    #[test]
    fn install_refuses_reserved_provider_tool_without_flag() {
        let manifest = provider_manifest("animus-provider-claude");
        let err = enforce_provider_tool_policy(&manifest, false).expect_err("must refuse claude provider plugin");
        assert!(format!("{err}").contains("reserved in-tree backend"));
    }

    #[test]
    fn install_allows_reserved_with_explicit_flag() {
        let manifest = provider_manifest("animus-provider-codex");
        let ok = enforce_provider_tool_policy(&manifest, true);
        assert!(ok.is_ok(), "--allow-shadow-builtin must let install through");
    }

    #[test]
    fn install_allows_non_reserved_provider_plugin() {
        let manifest = provider_manifest("animus-provider-mock");
        let ok = enforce_provider_tool_policy(&manifest, false);
        assert!(ok.is_ok(), "non-reserved provider tools must install without the override");
    }

    #[test]
    fn install_skips_provider_check_for_non_provider_plugins() {
        let mut manifest = provider_manifest("animus-provider-claude");
        manifest.plugin_kind = animus_plugin_protocol::PLUGIN_KIND_SUBJECT_BACKEND.to_string();
        let ok = enforce_provider_tool_policy(&manifest, false);
        assert!(ok.is_ok(), "subject backends are never gated by reserved provider tools");
    }

    #[test]
    fn install_defaults_succeeds_for_curated_providers_with_reserved_names() {
        let mut at_least_one_reserved = false;
        for (slug, _tag) in DEFAULT_PROVIDER_PLUGINS {
            let repo_basename = slug.rsplit('/').next().unwrap_or(slug);
            let manifest = provider_manifest(repo_basename);

            let curated_install = enforce_provider_tool_policy(&manifest, true);
            assert!(
                curated_install.is_ok(),
                "curated install-defaults (allow_shadow_builtin=true) must accept '{repo_basename}'"
            );

            let derived_tool = repo_basename.strip_prefix("animus-provider-").unwrap_or(repo_basename);
            if is_reserved_provider_tool(derived_tool) {
                at_least_one_reserved = true;
                let user_install = enforce_provider_tool_policy(&manifest, false);
                assert!(
                    user_install.is_err(),
                    "user-typed install MUST still be blocked for reserved name '{repo_basename}'"
                );
            }
        }
        assert!(
            at_least_one_reserved,
            "DEFAULT_PROVIDER_PLUGINS should contain at least one reserved-name provider (regression guard for P1)"
        );

        let (slug, tag) = DEFAULT_PROVIDER_PLUGINS[0];
        let req = PluginInstallRequest {
            source: Some(slug.to_string()),
            tag: Some(tag.to_string()),
            allow_org: vec!["launchapp-dev".to_string()],
            yes: true,
            allow_shadow_builtin: true,
            ..Default::default()
        };
        assert!(req.allow_shadow_builtin, "install-defaults request must opt into shadow-builtin bypass");
        assert!(req.yes, "install-defaults request must auto-confirm TOFU");
        assert_eq!(req.allow_org, vec!["launchapp-dev".to_string()]);
    }

    #[test]
    fn user_install_still_blocked_for_reserved_names_without_flag() {
        let manifest = provider_manifest("animus-provider-claude");
        let req =
            PluginInstallRequest { source: Some("attacker/animus-provider-claude".to_string()), ..Default::default() };
        assert!(!req.allow_shadow_builtin, "user-default request must NOT bypass shadow-builtin guard");
        let err = enforce_provider_tool_policy(&manifest, req.allow_shadow_builtin)
            .expect_err("user-typed install of reserved name must still be rejected");
        assert!(format!("{err}").contains("reserved in-tree backend"));
    }

    #[test]
    fn install_rejects_manifest_name_repo_mismatch() {
        let manifest = provider_manifest("animus-provider-claude");
        let err = enforce_manifest_name_matches_repo(&manifest, "evil-org", "animus-provider-oai", false)
            .expect_err("name vs repo basename mismatch must fail");
        let msg = format!("{err}");
        assert!(msg.contains("typosquat") || msg.contains("does not match"), "unexpected message: {msg}");
    }

    #[test]
    fn install_allows_manifest_name_repo_mismatch_with_force() {
        let manifest = provider_manifest("animus-provider-claude");
        let ok = enforce_manifest_name_matches_repo(&manifest, "evil-org", "animus-provider-oai", true);
        assert!(ok.is_ok(), "--force should bypass the manifest-name check");
    }

    #[test]
    fn install_accepts_matching_manifest_name() {
        let manifest = provider_manifest("animus-provider-mock");
        let ok = enforce_manifest_name_matches_repo(&manifest, "launchapp-dev", "animus-provider-mock", false);
        assert!(ok.is_ok(), "exact match must pass");
    }

    #[test]
    fn launchapp_dev_is_builtin_trusted() {
        // Don't read disk in this test — only the built-in list.
        assert!(BUILTIN_TRUSTED_ORGS.contains(&"launchapp-dev"));
    }

    /// v0.4.10: serializes the trusted-orgs tests below that all mutate the
    /// process-global `ANIMUS_TRUSTED_ORGS` env var. Cargo test runs in
    /// parallel by default; concurrent `set_var`/`remove_var` calls were
    /// the root cause of the documented `install_succeeds_after_org_added_to_trusted`
    /// flake. Held alongside [`protocol::test_utils::EnvVarGuard`] so the env
    /// var is restored on drop even when an assertion panics, and so the
    /// underlying `ENV_LOCK` serializes against every other env-mutating test
    /// in this binary (e.g. the plugin-tool MCP tests in
    /// `services::operations::ops_mcp::plugin_tools`). Cross-module env vars
    /// like `ANIMUS_CONFIG_DIR` and `ANIMUS_PLUGIN_DIR` used to race because
    /// the legacy `ScopedEnv` here mutated env state outside the crate-wide
    /// `ENV_LOCK`. Always go through `EnvVarGuard` for new tests.
    static TRUSTED_ORGS_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn install_warns_on_untrusted_org_first_time() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        // Direct unit test of the trusted-org policy.
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        // Untrusted, non-interactive, no --yes -> must error.
        let req = PluginInstallRequest::default();
        let err = enforce_org_trust("evil-org", &req).expect_err("untrusted org without --yes must fail");
        assert!(format!("{err}").contains("untrusted org"), "unexpected: {err}");
    }

    #[test]
    fn install_succeeds_after_org_added_to_trusted() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        // Pre-populate with someone-elses-org.
        std::fs::write(&trusted_orgs_yaml, "trusted_orgs:\n  - someone-elses-org\n").unwrap();
        let req = PluginInstallRequest::default();
        let ok = enforce_org_trust("someone-elses-org", &req);
        assert!(ok.is_ok(), "previously-trusted org must skip the TOFU prompt");
    }

    #[test]
    fn install_succeeds_when_org_passed_via_allow_org() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        let req = PluginInstallRequest { allow_org: vec!["new-friend-org".to_string()], ..Default::default() };
        let ok = enforce_org_trust("new-friend-org", &req);
        assert!(ok.is_ok(), "--allow-org should pre-trust the org for this install");
    }

    #[test]
    fn launchapp_dev_skips_tofu_prompt() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        let req = PluginInstallRequest::default();
        let ok = enforce_org_trust("launchapp-dev", &req);
        assert!(ok.is_ok(), "launchapp-dev is pre-trusted and must never trip TOFU");
    }

    #[test]
    fn add_trusted_org_persists_and_is_idempotent() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        add_trusted_org("first-org", TrustDecision::InteractivePrompt, Some("first-org/animus-foo")).expect("add 1st");
        add_trusted_org("first-org", TrustDecision::Yes, None).expect("idempotent 2nd");
        add_trusted_org("second-org", TrustDecision::AllowOrg, None).expect("add 2nd");
        let cfg = load_trusted_orgs().expect("load");
        let records = cfg.records();
        assert_eq!(records.len(), 2);
        let first = records.iter().find(|r| r.org == "first-org").expect("first-org present");
        // Idempotent: the second add did NOT churn the original decision/timestamp.
        assert_eq!(first.decided_by, Some(TrustDecision::InteractivePrompt));
        assert!(first.trusted_at.is_some(), "rich record carries trusted_at");
        assert_eq!(first.first_plugin.as_deref(), Some("first-org/animus-foo"));
        assert!(records.iter().any(|r| r.org == "second-org"));
        // Pre-trusted built-ins never get written.
        add_trusted_org("launchapp-dev", TrustDecision::Yes, None).expect("builtin add is no-op");
        let cfg2 = load_trusted_orgs().expect("reload");
        assert_eq!(cfg2.records().len(), 2, "launchapp-dev must not be appended to trusted-orgs.yaml");
    }

    #[test]
    fn load_trusted_orgs_accepts_legacy_bare_string_format() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        // Old format: bare string list.
        std::fs::write(&trusted_orgs_yaml, "trusted_orgs:\n  - legacy-org\n  - another-legacy\n").unwrap();
        let cfg = load_trusted_orgs().expect("legacy format must load");
        let records = cfg.records();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.is_active()), "legacy entries are active (no tombstone)");
        assert!(records.iter().all(|r| r.trusted_at.is_none()), "legacy entries carry no timestamp");
        assert!(org_is_trusted("legacy-org").expect("trusted lookup"));
        assert!(org_is_trusted("ANOTHER-LEGACY").expect("case-insensitive"));
    }

    #[test]
    fn new_entries_carry_rich_metadata_and_serialize() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        add_trusted_org("rich-org", TrustDecision::InteractivePrompt, Some("rich-org/animus-bar")).expect("add");
        // On-disk YAML must carry the rich fields.
        let raw = std::fs::read_to_string(&trusted_orgs_yaml).expect("read back");
        assert!(raw.contains("rich-org"), "org persisted: {raw}");
        assert!(raw.contains("trusted_at"), "trusted_at persisted: {raw}");
        assert!(raw.contains("interactive-prompt"), "decided_by persisted: {raw}");
        assert!(raw.contains("animus-bar"), "first_plugin persisted: {raw}");
        let record = trusted_org_record("rich-org").expect("lookup").expect("active record");
        assert_eq!(record.decided_by, Some(TrustDecision::InteractivePrompt));
        assert_eq!(record.first_plugin.as_deref(), Some("rich-org/animus-bar"));
    }

    #[test]
    fn revoke_records_tombstone_and_reprompts() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        add_trusted_org("tmp-org", TrustDecision::Yes, None).expect("add");
        assert!(org_is_trusted("tmp-org").expect("trusted before revoke"));

        let revoked = revoke_trusted_org("tmp-org").expect("revoke");
        assert!(revoked.revoked_at.is_some(), "tombstone carries revoked_at");
        // Org is no longer trusted, so a fresh install must re-prompt.
        assert!(!org_is_trusted("tmp-org").expect("untrusted after revoke"));

        // Tombstone survives in the store (audit trail preserved).
        let cfg = load_trusted_orgs().expect("reload");
        let records = cfg.records();
        let tombstone = records.iter().find(|r| r.org == "tmp-org").expect("tombstone present");
        assert!(!tombstone.is_active(), "tombstone is inactive");

        // Double-revoke errors.
        assert!(revoke_trusted_org("tmp-org").is_err(), "already-revoked must error");

        // Re-trusting clears the tombstone with a fresh decision.
        add_trusted_org("tmp-org", TrustDecision::InteractivePrompt, Some("tmp-org/animus-baz")).expect("re-trust");
        let record = trusted_org_record("tmp-org").expect("lookup").expect("active again");
        assert_eq!(record.decided_by, Some(TrustDecision::InteractivePrompt));
        assert!(record.revoked_at.is_none(), "re-trust clears tombstone");
    }

    #[test]
    fn revoke_builtin_org_is_refused() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        let err = revoke_trusted_org("launchapp-dev").expect_err("built-in must not be revocable");
        assert!(format!("{err}").contains("built-in"), "unexpected error: {err}");
        // launchapp-dev stays trusted.
        assert!(org_is_trusted("launchapp-dev").expect("still trusted"));
    }

    #[test]
    fn revoke_unknown_org_errors() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        let err = revoke_trusted_org("never-seen").expect_err("unknown org must error");
        assert!(format!("{err}").contains("not in the trusted-orgs allowlist"), "unexpected: {err}");
    }

    #[test]
    fn enforce_org_trust_returns_decision_for_fresh_grant() {
        let _guard = TRUSTED_ORGS_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let trusted_orgs_yaml = temp.path().join("trusted-orgs.yaml");
        let _env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_TRUSTED_ORGS",
            Some(trusted_orgs_yaml.to_str().expect("trusted-orgs path utf-8")),
        );
        // --yes path returns the Yes decision.
        let req = PluginInstallRequest { yes: true, ..Default::default() };
        assert_eq!(enforce_org_trust("fresh-org", &req).expect("ok"), Some(TrustDecision::Yes));
        // --allow-org path returns AllowOrg.
        let req2 = PluginInstallRequest { allow_org: vec!["friend-org".into()], ..Default::default() };
        assert_eq!(enforce_org_trust("friend-org", &req2).expect("ok"), Some(TrustDecision::AllowOrg));
        // launchapp-dev (built-in, already trusted) -> None (no fresh grant).
        let req3 = PluginInstallRequest::default();
        assert_eq!(enforce_org_trust("launchapp-dev", &req3).expect("ok"), None);
    }

    #[test]
    fn trusted_signers_yaml_matches_launchapp_dev() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("trusted.yaml");
        std::fs::write(
            &p,
            r#"
trusted_signers:
  - identity: "launchapp-dev/animus-*"
    issuer: "https://token.actions.githubusercontent.com"
"#,
        )
        .unwrap();
        let cfg = signing::load_trusted_signers(&p).unwrap().expect("config should load");
        assert!(cfg.matches_repo("launchapp-dev/animus-provider-claude"));
        assert!(!cfg.matches_repo("evil-org/animus-provider-claude"));
    }

    // ---- Gap #11: --skip-manifest-check audit trail ----------------------
    //
    // These tests drive the full `run_plugin_install` pipeline with a local
    // `--path` source, then read back the canonical plugins.yaml registry to
    // verify the `skip_manifest_check_at_install` field is persisted when the
    // flag is set, and absent otherwise.

    /// Mutex to serialize install-pipeline tests that mutate process-global
    /// env vars (ANIMUS_CONFIG_DIR, ANIMUS_PLUGIN_DIR, ANIMUS_TRUSTED_ORGS).
    /// Cargo runs tests on multiple threads; sharing these env vars across
    /// concurrent tests would otherwise race.
    static INSTALL_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    fn write_fake_plugin_binary(path: &std::path::Path, manifest_name: &str, plugin_kind: &str) {
        write_fake_plugin_binary_with_capabilities(path, manifest_name, plugin_kind, &[]);
    }

    #[cfg(unix)]
    fn write_fake_plugin_binary_with_capabilities(
        path: &std::path::Path,
        manifest_name: &str,
        plugin_kind: &str,
        capabilities: &[&str],
    ) {
        use std::os::unix::fs::PermissionsExt;
        let capabilities_value: Vec<serde_json::Value> =
            capabilities.iter().map(|c| serde_json::Value::String((*c).to_string())).collect();
        let manifest = serde_json::json!({
            "name": manifest_name,
            "version": "0.1.0",
            "plugin_kind": plugin_kind,
            "description": "fake plugin for install-pipeline tests",
            "protocol_version": "1.0.0",
            "capabilities": capabilities_value,
        });
        // The probe runs the binary with `--manifest`. A POSIX shell script that
        // prints the manifest JSON when `--manifest` is the first arg is enough
        // to satisfy the probe.
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--manifest\" ]; then\n  printf '%s\\n' '{manifest}'\nfi\n",
            manifest = manifest
        );
        std::fs::write(path, script).expect("write fake plugin binary");
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_with_skip_manifest_check_persists_flag() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        // `--path` install does not exercise the TOFU org-trust pipeline
        // (that only runs for `--source` release installs), so we deliberately
        // leave ANIMUS_TRUSTED_ORGS alone to avoid racing the existing
        // `install_succeeds_after_org_added_to_trusted` test which serialises
        // its own use of that variable.
        //
        // `EnvVarGuard` holds the crate-wide `ENV_LOCK` for the lifetime of
        // each guard, so the env vars stay stable across the `.await` below
        // *and* every other env-mutating test in this binary (including the
        // MCP `plugin_tools` tests that also depend on these two vars) is
        // blocked from racing the install pipeline. The previous raw
        // `std::env::set_var` / `remove_var` calls bypassed that lock and
        // were the documented root cause of the
        // `plugin_install_uninstall_round_trip` flake.
        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        let source = tmp.path().join("animus-provider-skipped");
        write_fake_plugin_binary(&source, "animus-provider-skipped", "subject_backend");

        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_manifest_check: true,
            skip_signature: true,
            yes: true,
            ..Default::default()
        };

        let result = run_plugin_install(req).await;

        let output = result.expect("install must succeed with --skip-manifest-check");
        let yaml_path = std::path::PathBuf::from(&output.plugins_yaml);
        let yaml = std::fs::read_to_string(&yaml_path).expect("read plugins.yaml");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse plugins.yaml");
        // No manifest was probed, so the plugin lands in the generic `plugins`
        // table (not `providers`) under its file-name-derived key.
        let entry =
            parsed.get("plugins").and_then(|p| p.get(&output.name)).expect("registry entry for installed plugin");
        let flag = entry
            .get("skip_manifest_check_at_install")
            .and_then(|v| v.as_bool())
            .expect("skip_manifest_check_at_install field must be persisted when flag is set");
        assert!(flag, "skip_manifest_check_at_install must be `true` when the install flag is set");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_without_skip_manifest_check_omits_flag() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        // `--path` install does not exercise the TOFU org-trust pipeline
        // (that only runs for `--source` release installs), so we deliberately
        // leave ANIMUS_TRUSTED_ORGS alone to avoid racing the existing
        // `install_succeeds_after_org_added_to_trusted` test which serialises
        // its own use of that variable.
        //
        // See `install_with_skip_manifest_check_persists_flag` for why these
        // env vars go through `EnvVarGuard` instead of raw `set_var`.
        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        // Note: when the manifest probe runs, the install pipeline insists the
        // manifest name match the install file basename for the `--path`
        // shape. Using the same basename here keeps the test focused on the
        // audit-flag persistence behavior rather than the unrelated name check.
        let source = tmp.path().join("animus-plugin-honest");
        write_fake_plugin_binary(&source, "honest", "subject_backend");

        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_manifest_check: false,
            skip_signature: true,
            yes: true,
            ..Default::default()
        };

        let result = run_plugin_install(req).await;

        let output = result.expect("install must succeed without --skip-manifest-check");
        let yaml_path = std::path::PathBuf::from(&output.plugins_yaml);
        let yaml = std::fs::read_to_string(&yaml_path).expect("read plugins.yaml");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse plugins.yaml");
        // The manifest was probed and accepted, so the entry lands under
        // `plugins` (the manifest's plugin_kind is `subject_backend`, not
        // `provider`).
        let entry =
            parsed.get("plugins").and_then(|p| p.get(&output.name)).expect("registry entry for installed plugin");
        let flag = entry.get("skip_manifest_check_at_install").and_then(|v| v.as_bool());
        assert!(
            flag.is_none() || flag == Some(false),
            "skip_manifest_check_at_install must be absent (or `false`) when the flag is not set; got {flag:?}"
        );
    }

    // ---- Fail-closed on lockfile parse failure ----------------------------
    //
    // Regression guard: a corrupt or schema-incompatible `.animus/plugins.lock`
    // must refuse the install rather than silently overwriting the lockfile
    // and losing the integrity audit trail. The escape hatch is the
    // `--force-rewrite-lockfile` flag, which discards the file with a
    // `warn!` log.

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_refuses_when_lockfile_is_corrupt_without_flag() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(&animus_dir).unwrap();

        // Corrupt project-local lockfile (invalid TOML AND wrong schema).
        let lock_path = animus_dir.join("plugins.lock");
        std::fs::write(&lock_path, b"this is not valid toml :::: !!!!").expect("write corrupt lockfile");

        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        let source = tmp.path().join("animus-plugin-corruptlock");
        write_fake_plugin_binary(&source, "animus-plugin-corruptlock", "subject_backend");

        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            // Default: force_rewrite_lockfile = false → fail closed.
            ..Default::default()
        };

        let err = run_plugin_install(req).await.expect_err("install must REFUSE corrupt lockfile by default");
        let msg = format!("{err}");
        assert!(
            msg.contains("plugin lockfile") && msg.contains("unreadable"),
            "error must mention 'plugin lockfile' and 'unreadable'; got: {msg}"
        );
        assert!(
            msg.contains(&lock_path.display().to_string()),
            "error must include the exact corrupt lockfile path; got: {msg}"
        );
        assert!(
            msg.contains("--force-rewrite-lockfile"),
            "error must point at the --force-rewrite-lockfile escape hatch; got: {msg}"
        );
        assert!(
            msg.contains("restore") || msg.contains("version control"),
            "error must mention the restore-from-VCS remediation; got: {msg}"
        );

        // Crucial: the corrupt file must NOT have been overwritten.
        let after = std::fs::read(&lock_path).expect("corrupt file must still exist");
        assert_eq!(after.as_slice(), b"this is not valid toml :::: !!!!", "corrupt lockfile must not be rewritten");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_succeeds_with_force_rewrite_lockfile_flag() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(&animus_dir).unwrap();

        // Corrupt lockfile in the same shape as the fail-closed test.
        let lock_path = animus_dir.join("plugins.lock");
        std::fs::write(&lock_path, b"garbage that cannot parse").expect("write corrupt lockfile");

        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        let source = tmp.path().join("animus-plugin-corruptlock-rewrite");
        write_fake_plugin_binary(&source, "animus-plugin-corruptlock-rewrite", "subject_backend");

        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            force_rewrite_lockfile: true,
            ..Default::default()
        };

        let output = run_plugin_install(req)
            .await
            .expect("install with --force-rewrite-lockfile must succeed past corrupt lock");
        assert!(!output.installed_path.is_empty(), "installed_path must be populated on success");

        // The on-disk lockfile must now be a valid parseable file with the
        // newly recorded entry, proving the rewrite happened intentionally.
        let after = std::fs::read_to_string(&lock_path).expect("rewritten lockfile must be readable");
        assert!(
            after.contains("schema_version"),
            "rewritten lockfile must contain a valid schema_version field; got: {after}"
        );
    }

    // Focused unit test on the `load_or_refuse_lockfile` helper to keep the
    // fail-closed contract regression-guarded even when the wider install
    // pipeline is refactored.
    #[test]
    fn load_or_refuse_lockfile_returns_fail_closed_error_with_corrupt_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().to_path_buf();
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let lock_path = animus_dir.join("plugins.lock");
        std::fs::write(&lock_path, b"definitely not toml").unwrap();

        let err = load_or_refuse_lockfile(Some(&project_root), None, false)
            .expect_err("default must refuse corrupt lockfile");
        let msg = format!("{err}");
        assert!(msg.contains("plugin lockfile"));
        assert!(msg.contains("unreadable"));
        assert!(msg.contains(&lock_path.display().to_string()));
        assert!(msg.contains("--force-rewrite-lockfile"));
    }

    #[test]
    fn load_or_refuse_lockfile_rewrites_with_flag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().to_path_buf();
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let lock_path = animus_dir.join("plugins.lock");
        std::fs::write(&lock_path, b"definitely not toml").unwrap();

        let lock = load_or_refuse_lockfile(Some(&project_root), None, true)
            .expect("--force-rewrite-lockfile must produce a fresh in-memory lock");
        assert_eq!(lock.path(), &lock_path, "lockfile path must point at the project-local file");
        // The in-memory lock starts empty; the on-disk corrupt bytes are
        // untouched until the install pipeline calls `save()`.
        let on_disk = std::fs::read(&lock_path).unwrap();
        assert_eq!(on_disk.as_slice(), b"definitely not toml", "helper must not touch disk until save()");
    }

    // Regression guard for codex review round-3 P2:
    // `install-defaults --force-rewrite-lockfile` must actually rewrite the
    // corrupt lockfile on disk even when every default is already installed
    // and the per-target loop skips them all. Otherwise the documented
    // remediation is a no-op and the next install fails closed again.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_defaults_force_rewrite_persists_when_all_skipped() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(&animus_dir).unwrap();

        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        // Pre-create the install-dir entries for every plugin the bundled
        // default flavor manifest (loaded from the binary via include_str!
        // after Wave 3 codex R6 [P2]) marks `required`, so the per-target
        // loop skips them all even when no `flavors/` directory is on disk
        // under the tempdir. This is the exact required set the
        // manifest-driven install resolves.
        let bundled = orchestrator_core::flavor::load_flavor_in(&project_root, "default")
            .expect("bundled default flavor must parse")
            .expect("bundled default flavor must resolve");
        for (_role, slug) in bundled.required_plugins() {
            let basename = slug.rsplit('/').next().unwrap_or(&slug);
            std::fs::write(install_dir.join(basename), b"placeholder").unwrap();
        }

        // Seed a corrupt lockfile.
        let lock_path = animus_dir.join("plugins.lock");
        std::fs::write(&lock_path, b"garbage that will not parse as TOML").unwrap();

        // Drive the install-defaults handler directly with the project root
        // set to our tempdir. This is the exact entry point that the CLI's
        // dispatcher routes `animus plugin install-defaults` through.
        let args = PluginInstallDefaultsArgs {
            plugin_dir: Some(install_dir.to_string_lossy().to_string()),
            force: false,
            yes: true,
            flavor: "default".to_string(),
            include_recommended: false,
            include_oai_agent: false,
            include_subjects: false,
            include_transports: false,
            json: true,
            force_rewrite_lockfile: true,
        };
        let result = handle_plugin_install_defaults(args, &project_root.to_string_lossy()).await;
        assert!(result.is_ok(), "install-defaults must succeed when force_rewrite_lockfile=true; got {result:?}");

        // The lockfile on disk MUST now be a fresh parseable file, not the
        // original garbage bytes. Without the new save() call, the corrupt
        // bytes would still be there.
        let after = std::fs::read_to_string(&lock_path).expect("lockfile must be readable after rewrite");
        assert_ne!(after.as_bytes(), b"garbage that will not parse as TOML");
        let reparsed = PluginLockfile::load_or_empty(&lock_path)
            .expect("rewritten lockfile must parse cleanly under the current schema");
        assert!(reparsed.plugins.is_empty(), "rewritten lockfile must start empty");
    }

    fn install_defaults_args(flavor: &str) -> PluginInstallDefaultsArgs {
        PluginInstallDefaultsArgs {
            plugin_dir: None,
            force: false,
            yes: true,
            flavor: flavor.to_string(),
            include_recommended: false,
            include_oai_agent: false,
            include_subjects: false,
            include_transports: false,
            json: true,
            force_rewrite_lockfile: false,
        }
    }

    /// Write a flavor manifest under `<root>/flavors/<name>.toml` for
    /// manifest-resolution tests. Slugs reference curated-pinned plugins so
    /// `resolve_tag_for_slug` keeps them in the target list.
    fn write_test_flavor(root: &std::path::Path, name: &str, body: &str) {
        let dir = root.join("flavors");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), body).unwrap();
    }

    const TEST_FLAVOR_TOML: &str = r#"
schema = "animus.flavor.v1"
id = "default"
version = "0.5.0"
title = "Test Flavor"
description = "Manifest-resolution test fixture."

[providers]
required = ["launchapp-dev/animus-provider-claude"]
recommended = ["launchapp-dev/animus-provider-codex", "launchapp-dev/animus-provider-ollama"]

[subjects]
required = ["launchapp-dev/animus-subject-default", "launchapp-dev/animus-subject-requirements"]
recommended = ["launchapp-dev/animus-subject-linear"]

[transports]
required = ["launchapp-dev/animus-transport-http"]
recommended = ["launchapp-dev/animus-transport-graphql"]

[ui]
recommended = ["launchapp-dev/animus-web-ui"]

[workflow_runner]
required = ["launchapp-dev/animus-workflow-runner-default"]

[queue]
required = ["launchapp-dev/animus-queue-default"]
"#;

    fn target_slugs(targets: &[(String, String)]) -> Vec<&str> {
        targets.iter().map(|(slug, _)| slug.as_str()).collect()
    }

    #[test]
    fn install_defaults_targets_install_everything_the_manifest_marks_required() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_flavor(tmp.path(), "default", TEST_FLAVOR_TOML);
        let args = install_defaults_args("default");
        let targets = build_install_defaults_targets(&args, &tmp.path().to_string_lossy()).unwrap();
        let slugs = target_slugs(&targets);
        for required in [
            "launchapp-dev/animus-provider-claude",
            "launchapp-dev/animus-subject-default",
            "launchapp-dev/animus-subject-requirements",
            "launchapp-dev/animus-transport-http",
            "launchapp-dev/animus-workflow-runner-default",
            "launchapp-dev/animus-queue-default",
        ] {
            assert!(slugs.contains(&required), "required slug {required} missing from {slugs:?}");
        }
        assert!(
            !slugs.iter().any(|s| s.contains("codex") || s.contains("linear") || s.contains("graphql")),
            "recommended slugs must not install without --include-recommended: {slugs:?}"
        );
        assert!(
            targets.iter().all(|(_, tag)| tag.starts_with('v')),
            "every target must carry a curated tag pin: {targets:?}"
        );
    }

    #[test]
    fn install_defaults_targets_include_recommended_adds_recommended_and_skips_unpinned() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_flavor(tmp.path(), "default", TEST_FLAVOR_TOML);
        let mut args = install_defaults_args("default");
        args.include_recommended = true;
        let targets = build_install_defaults_targets(&args, &tmp.path().to_string_lossy()).unwrap();
        let slugs = target_slugs(&targets);
        assert!(slugs.contains(&"launchapp-dev/animus-provider-codex"), "got: {slugs:?}");
        assert!(slugs.contains(&"launchapp-dev/animus-subject-linear"), "got: {slugs:?}");
        assert!(slugs.contains(&"launchapp-dev/animus-transport-graphql"), "got: {slugs:?}");
        assert!(slugs.contains(&"launchapp-dev/animus-web-ui"), "got: {slugs:?}");
        assert!(
            !slugs.contains(&"launchapp-dev/animus-provider-ollama"),
            "slugs without a curated tag pin must be skipped with a warning: {slugs:?}"
        );
    }

    #[test]
    fn install_defaults_targets_back_compat_include_flags_add_recommended_slices() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_flavor(tmp.path(), "default", TEST_FLAVOR_TOML);
        let mut args = install_defaults_args("default");
        args.include_subjects = true;
        args.include_transports = true;
        let targets = build_install_defaults_targets(&args, &tmp.path().to_string_lossy()).unwrap();
        let slugs = target_slugs(&targets);
        assert!(slugs.contains(&"launchapp-dev/animus-subject-linear"), "got: {slugs:?}");
        assert!(slugs.contains(&"launchapp-dev/animus-transport-graphql"), "got: {slugs:?}");
        assert!(slugs.contains(&"launchapp-dev/animus-web-ui"), "got: {slugs:?}");
        assert!(
            !slugs.contains(&"launchapp-dev/animus-provider-codex"),
            "--include-subjects/--include-transports must not pull in recommended providers: {slugs:?}"
        );
        let unique: std::collections::HashSet<&str> = slugs.iter().copied().collect();
        assert_eq!(unique.len(), slugs.len(), "targets must be deduplicated: {slugs:?}");
    }

    #[test]
    fn install_defaults_targets_unknown_flavor_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let args = install_defaults_args("does-not-exist");
        let err = build_install_defaults_targets(&args, &tmp.path().to_string_lossy())
            .expect_err("unknown flavor must error, not silently fall back");
        assert!(err.to_string().contains("does-not-exist"), "error must name the flavor: {err}");
    }

    #[test]
    fn install_defaults_targets_bundled_default_manifest_covers_daemon_preflight() {
        // No `flavors/` directory anywhere under the temp project root:
        // the binary-bundled default manifest must resolve and its
        // required set must cover every daemon-preflight role so the
        // canonical first run (`install-defaults` then `daemon start`)
        // works without a second command.
        let tmp = tempfile::tempdir().unwrap();
        let args = install_defaults_args("default");
        let targets = build_install_defaults_targets(&args, &tmp.path().to_string_lossy()).unwrap();
        let slugs = target_slugs(&targets);
        for required in [
            "launchapp-dev/animus-provider-claude",
            "launchapp-dev/animus-subject-default",
            "launchapp-dev/animus-subject-requirements",
            "launchapp-dev/animus-workflow-runner-default",
            "launchapp-dev/animus-queue-default",
        ] {
            assert!(slugs.contains(&required), "bundled required slug {required} missing from {slugs:?}");
        }
    }

    // Regression guard for codex review round-2 P2: a concurrent install
    // that completed and saved a lockfile entry between this install's
    // pre-load and its `save()` must NOT be erased. The fix reloads the
    // lockfile right before upsert/save so the new on-disk entry survives.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_preserves_concurrent_lockfile_entry_added_after_preload() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(&animus_dir).unwrap();

        // Seed the lockfile with a legitimate entry from a "concurrent
        // install B" that finished while install A was downloading.
        let lock_path = animus_dir.join("plugins.lock");
        let mut concurrent_lock = PluginLockfile::empty_at(&lock_path);
        concurrent_lock.upsert(LockEntry {
            name: "animus-plugin-other".to_string(),
            version: "v0.1.0".to_string(),
            targets: lock_targets("c".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: chrono::Utc::now().to_rfc3339(),
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
        });
        concurrent_lock.save().expect("seed concurrent entry");

        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        let source = tmp.path().join("animus-plugin-newcomer");
        write_fake_plugin_binary(&source, "animus-plugin-newcomer", "subject_backend");

        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };

        run_plugin_install(req).await.expect("install must succeed against a valid concurrent-write lockfile");

        // The previously-recorded "other" entry must still be present in
        // the saved lockfile alongside the new entry. Pre-fix code paths
        // would have erased it because they reused a stale preload.
        let reloaded = PluginLockfile::load_or_empty(&lock_path).expect("reload saved lockfile");
        let names: Vec<&str> = reloaded.plugins.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"animus-plugin-other"), "concurrent entry must survive; got {names:?}");
        assert!(names.contains(&"animus-plugin-newcomer"), "newly installed entry must be present; got {names:?}");
    }

    // Verify the lockfile is refused BEFORE source probing. We use a
    // non-existent `--path` to prove the corrupt-lockfile error wins over
    // the (later) source-not-found error.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_refuses_lockfile_before_touching_source() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(&animus_dir).unwrap();
        let lock_path = animus_dir.join("plugins.lock");
        std::fs::write(&lock_path, b"corrupted lockfile bytes").unwrap();

        let _config_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(config_dir.to_str().expect("config dir utf-8")),
        );
        let _plugin_env = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_DIR",
            Some(install_dir.to_str().expect("install dir utf-8")),
        );

        // Path that does NOT exist on disk. If lockfile pre-check ran AFTER
        // source resolution we would surface a not-found error here. With
        // pre-check first, the unreadable-lockfile error wins.
        let missing_source = tmp.path().join("does-not-exist-plugin");

        let req = PluginInstallRequest {
            path: Some(missing_source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };

        let err = run_plugin_install(req).await.expect_err("install must refuse on corrupt lockfile");
        let msg = format!("{err}");
        assert!(
            msg.contains("plugin lockfile") && msg.contains("unreadable"),
            "lockfile fail-closed must win over source-not-found; got: {msg}"
        );
        assert!(!msg.contains("plugin source not found"), "source resolution must not run; got: {msg}");
    }

    // =================== SignaturePolicy / effective_policy_mode tests ===================

    #[test]
    fn effective_policy_uses_explicit_signature_policy_first() {
        let req = PluginInstallRequest {
            signature_policy: Some(PluginPolicyMode::Strict),
            require_signature: false,
            skip_signature: true,
            ..Default::default()
        };
        assert_eq!(effective_policy_mode(&req), PluginPolicyMode::Strict);
    }

    #[test]
    fn effective_policy_maps_skip_signature_to_disabled() {
        let req = PluginInstallRequest { skip_signature: true, ..Default::default() };
        assert_eq!(effective_policy_mode(&req), PluginPolicyMode::Disabled);
    }

    #[test]
    fn effective_policy_maps_require_signature_to_strict() {
        let req = PluginInstallRequest { require_signature: true, ..Default::default() };
        assert_eq!(effective_policy_mode(&req), PluginPolicyMode::Strict);
    }

    #[test]
    fn effective_policy_default_is_warn_for_legacy_callers() {
        let req = PluginInstallRequest::default();
        assert_eq!(
            effective_policy_mode(&req),
            PluginPolicyMode::Warn,
            "callers that build PluginInstallRequest without setting signature_policy get the verify-if-present default; this matches the v0.4.12 lib default while the built-in launchapp-dev cosign key is still a placeholder"
        );
    }

    #[test]
    fn resolve_signature_status_returns_skipped_under_disabled_policy() {
        let req = PluginInstallRequest { signature_policy: Some(PluginPolicyMode::Disabled), ..Default::default() };
        let prov = release_provenance_with_bundle(Some(std::path::PathBuf::from("/tmp/x.bundle")));
        let status = resolve_signature_status(&req, &prov).expect("resolves");
        assert_eq!(status, SignatureStatus::Skipped);
    }

    #[test]
    fn strict_policy_rejects_install_when_no_bundle() {
        let req = PluginInstallRequest { signature_policy: Some(PluginPolicyMode::Strict), ..Default::default() };
        let prov = release_provenance_with_bundle(None);
        let status = resolve_signature_status(&req, &prov).expect("resolves");
        assert!(matches!(&status, SignatureStatus::Unsigned { .. }));
        let policy = effective_policy_mode(&req);
        assert_eq!(policy, PluginPolicyMode::Strict);
    }

    #[test]
    fn warn_policy_yields_unsigned_when_no_bundle() {
        let req = PluginInstallRequest { signature_policy: Some(PluginPolicyMode::Warn), ..Default::default() };
        let prov = release_provenance_with_bundle(None);
        let status = resolve_signature_status(&req, &prov).expect("resolves");
        assert!(matches!(&status, SignatureStatus::Unsigned { .. }));
    }

    /// `--trust-key` was the old (pre-v0.4.12) entry point into the
    /// key-based PEM verifier. Keyless cosign has no PEM trust anchor,
    /// so the flag is now a deprecated no-op — passing it must not error
    /// out the install, just log a warning and proceed through the
    /// normal keyless path.
    #[test]
    fn trust_key_is_deprecated_noop_in_v0_4_12_keyless() {
        let req = PluginInstallRequest {
            signature_policy: Some(PluginPolicyMode::Warn),
            trust_key: Some(PathBuf::from("/definitely/does/not/exist.pem")),
            ..Default::default()
        };
        let prov = release_provenance_with_bundle(None);
        // No bundle => Unsigned (the keyless path produces this whether or
        // not --trust-key was passed). Crucially, no `--trust-key path does
        // not exist` error any more.
        let status = resolve_signature_status(&req, &prov).expect("trust_key must NOT error in keyless mode");
        assert!(matches!(&status, SignatureStatus::Unsigned { .. }));
    }

    #[test]
    fn map_host_result_to_status_preserves_variants() {
        use orchestrator_plugin_host::VerificationResult as VR;
        let bundle = std::path::PathBuf::from("/tmp/b.bundle");
        assert!(matches!(map_host_result_to_status(VR::Skipped, &bundle), SignatureStatus::Skipped));
        assert!(matches!(
            map_host_result_to_status(VR::Unsigned { reason: "x".into() }, &bundle),
            SignatureStatus::Unsigned { .. }
        ));
        assert!(matches!(
            map_host_result_to_status(VR::Invalid { identity_pattern: "x".into(), message: "y".into() }, &bundle),
            SignatureStatus::Invalid { .. }
        ));
        assert!(matches!(
            map_host_result_to_status(VR::Verified { identity: "x".into(), bundle_path: "z".into() }, &bundle),
            SignatureStatus::Verified { .. }
        ));
    }

    // ===== v0.4.13 W1+W5: lockfile + audit hook coverage ======================

    /// Set up isolated env + project root for a lockfile install test. Returns
    /// `(tempdir, project_root, config_guard, plugin_guard, home_guard)`.
    /// All guards must stay in scope for the duration of the install call,
    /// otherwise the install pipeline will leak into the developer's real
    /// `~/.animus/`.
    #[cfg(unix)]
    fn setup_lockfile_test_env(
        tmp: &tempfile::TempDir,
    ) -> (
        std::path::PathBuf,
        protocol::test_utils::EnvVarGuard,
        protocol::test_utils::EnvVarGuard,
        protocol::test_utils::EnvVarGuard,
    ) {
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        let home_dir = tmp.path().join("home");
        let project_root = tmp.path().join("project");
        let project_animus = project_root.join(".animus");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::create_dir_all(&home_dir).unwrap();
        std::fs::create_dir_all(&project_animus).unwrap();
        let config_guard =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let plugin_guard =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));
        // Redirect HOME so scoped_state_root() and global lockfile fallbacks
        // both land inside the tempdir, never under the developer's real $HOME.
        let home_guard = protocol::test_utils::EnvVarGuard::set("HOME", Some(home_dir.to_str().unwrap()));
        (project_root, config_guard, plugin_guard, home_guard)
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn lockfile_install_persists_sha256() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_root, _config_env, _plugin_env, _home_env) = setup_lockfile_test_env(&tmp);

        let source = tmp.path().join("animus-plugin-locked");
        write_fake_plugin_binary(&source, "animus-plugin-locked", "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("install must succeed");

        let lock_path = PluginLockfile::default_path(Some(&project_root));
        assert!(lock_path.exists(), "lockfile should exist at {}", lock_path.display());
        let lock = PluginLockfile::load_or_empty(&lock_path).unwrap();
        let entry = lock.find(&output.name).expect("lockfile entry must be present");
        // `--path` install records a single-target claim for the current
        // platform; both the archive and installed-binary shas equal the
        // installed-binary hash (no tarball involved).
        let triple = current_target_triple().expect("known triple");
        let integrity = entry.target(triple).expect("current-target claim recorded");
        assert_eq!(integrity.archive_sha256, output.sha256);
        assert_eq!(integrity.installed_binary_sha256.as_deref(), Some(output.sha256.as_str()));
        assert!(!entry.installed_at.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn lockfile_upgrade_refuses_on_hash_mismatch_without_force() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_root, _config_env, _plugin_env, _home_env) = setup_lockfile_test_env(&tmp);

        let source = tmp.path().join("animus-plugin-tamper");
        write_fake_plugin_binary(&source, "animus-plugin-tamper", "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("initial install must succeed");

        // Tamper with the installed binary so its sha256 no longer matches the
        // lockfile entry the install just wrote.
        let installed_path = std::path::PathBuf::from(&output.installed_path);
        std::fs::write(&installed_path, b"tampered binary").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&installed_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&installed_path, perms).unwrap();

        // Re-install from a fresh source with force=false -> must be refused.
        let source_v2 = tmp.path().join("animus-plugin-tamper-v2");
        write_fake_plugin_binary(&source_v2, "animus-plugin-tamper", "subject_backend");
        let req2 = PluginInstallRequest {
            path: Some(source_v2.to_string_lossy().to_string()),
            name: Some(output.name.clone()),
            skip_signature: true,
            yes: true,
            force: false,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let err = run_plugin_install(req2).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("lockfile mismatch"), "unexpected error: {msg}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn lockfile_verify_detects_tampered_binary() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_root, _config_env, _plugin_env, _home_env) = setup_lockfile_test_env(&tmp);

        let source = tmp.path().join("animus-plugin-verify-me");
        write_fake_plugin_binary(&source, "animus-plugin-verify-me", "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("install must succeed");

        // Tamper.
        let installed_path = std::path::PathBuf::from(&output.installed_path);
        std::fs::write(&installed_path, b"different bytes").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&installed_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&installed_path, perms).unwrap();

        let lock_path = PluginLockfile::default_path(Some(&project_root));
        let lock = PluginLockfile::load_or_empty(&lock_path).unwrap();
        match lock.verify_installed(&output.name, &installed_path).expect("verify ok") {
            LockVerifyResult::Mismatch { expected, actual } => {
                assert_eq!(expected, output.sha256);
                assert_ne!(actual, output.sha256);
            }
            other => panic!("expected Mismatch after tamper, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn audit_log_records_install_event_with_signature_status() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_root, _config_env, _plugin_env, _home_env) = setup_lockfile_test_env(&tmp);

        let source = tmp.path().join("animus-plugin-audited");
        write_fake_plugin_binary(&source, "animus-plugin-audited", "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("install must succeed");

        let scoped =
            protocol::repository_scope::scoped_state_root(&project_root).expect("scoped state root must resolve");
        let audit_path = orchestrator_daemon_runtime::audit_log_path(&scoped);
        assert!(audit_path.exists(), "audit log must exist at {}", audit_path.display());
        let body = std::fs::read_to_string(&audit_path).unwrap();
        let install_lines: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|v: &serde_json::Value| v["event"] == "plugin_install")
            .collect();
        assert!(!install_lines.is_empty(), "expected at least one plugin_install line, body: {body}");
        let event = &install_lines[0];
        assert_eq!(event["actor"], "user");
        assert_eq!(event["details"]["plugin"], output.name);
        // skip_signature=true -> install pipeline returns SignatureStatus::Skipped.
        assert_eq!(event["details"]["signature_status"], "skipped");
    }

    // ---- v0.5.7 kind translator: install auto-increment + --as-kind ----

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn install_records_native_kind_for_uncontested_subject_backend() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        let source = tmp.path().join("animus-plugin-task-a");
        write_fake_plugin_binary_with_capabilities(
            &source,
            "animus-plugin-task-a",
            "subject_backend",
            &["subject_kind:task"],
        );

        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("install must succeed");
        assert_eq!(output.assigned_kind.as_deref(), Some("task"));
        assert_eq!(output.native_kind.as_deref(), Some("task"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn install_auto_increments_installed_kind_on_subject_collision() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        let first = tmp.path().join("animus-plugin-task-default");
        write_fake_plugin_binary_with_capabilities(
            &first,
            "animus-plugin-task-default",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req_first = PluginInstallRequest {
            path: Some(first.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let out_first = run_plugin_install(req_first).await.expect("first install");
        assert_eq!(out_first.assigned_kind.as_deref(), Some("task"));

        let second = tmp.path().join("animus-plugin-task-archive");
        write_fake_plugin_binary_with_capabilities(
            &second,
            "animus-plugin-task-archive",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req_second = PluginInstallRequest {
            path: Some(second.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let out_second = run_plugin_install(req_second).await.expect("second install");
        assert_eq!(
            out_second.assigned_kind.as_deref(),
            Some("task-2"),
            "second install of subject_kind:task must auto-increment to task-2"
        );
        assert_eq!(out_second.native_kind.as_deref(), Some("task"));

        let lock = PluginLockfile::load_default(Some(&project_root)).expect("lockfile loads");
        let entry = lock.find(&out_second.name).expect("second plugin lock entry");
        assert_eq!(entry.installed_kind.as_deref(), Some("task-2"));
        assert_eq!(entry.native_kind.as_deref(), Some("task"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn install_as_kind_overrides_auto_increment() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        let first = tmp.path().join("animus-plugin-task-default-a");
        write_fake_plugin_binary_with_capabilities(
            &first,
            "animus-plugin-task-default-a",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req_first = PluginInstallRequest {
            path: Some(first.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        run_plugin_install(req_first).await.expect("first install");

        let second = tmp.path().join("animus-plugin-task-archive-a");
        write_fake_plugin_binary_with_capabilities(
            &second,
            "animus-plugin-task-archive-a",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req_second = PluginInstallRequest {
            path: Some(second.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            as_kind: Some("archive".to_string()),
            ..Default::default()
        };
        let out_second = run_plugin_install(req_second).await.expect("second install");
        assert_eq!(out_second.assigned_kind.as_deref(), Some("archive"), "--as-kind must override auto-increment");
        assert_eq!(out_second.native_kind.as_deref(), Some("task"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn install_as_kind_with_existing_collision_returns_error() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        let first = tmp.path().join("animus-plugin-task-existing");
        write_fake_plugin_binary_with_capabilities(
            &first,
            "animus-plugin-task-existing",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req_first = PluginInstallRequest {
            path: Some(first.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        run_plugin_install(req_first).await.expect("first install");

        let second = tmp.path().join("animus-plugin-task-colliding");
        write_fake_plugin_binary_with_capabilities(
            &second,
            "animus-plugin-task-colliding",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req_second = PluginInstallRequest {
            path: Some(second.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            as_kind: Some("task".to_string()),
            ..Default::default()
        };
        let err = run_plugin_install(req_second).await.expect_err("explicit collision must fail");
        let message = format!("{err:#}");
        assert!(message.contains("already claimed by installed plugin"), "error must explain the collision: {message}");
    }

    #[test]
    fn pick_installed_kind_returns_native_for_first_install() {
        let dir = tempfile::tempdir().unwrap();
        let lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let chosen = pick_installed_kind_for_install(&lock, &[], "plugin-a", "task", None, &[]).expect("first install");
        assert_eq!(chosen, "task");
    }

    #[test]
    fn pick_installed_kind_auto_increments_through_task_2_task_3() {
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "plugin-a".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now.clone(),
            installed_kind: Some("task".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let chosen_b =
            pick_installed_kind_for_install(&lock, &[], "plugin-b", "task", None, &[]).expect("second install");
        assert_eq!(chosen_b, "task-2");

        lock.upsert(LockEntry {
            name: "plugin-b".into(),
            version: "v0.1".into(),
            targets: lock_targets("b".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now.clone(),
            installed_kind: Some(chosen_b.clone()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let chosen_c =
            pick_installed_kind_for_install(&lock, &[], "plugin-c", "task", None, &[]).expect("third install");
        assert_eq!(chosen_c, "task-3");
    }

    #[test]
    fn pick_installed_kind_uses_live_claims_to_cover_legacy_lockfile_rows() {
        // Pre-v0.5.7 lockfile rows carry no `installed_kind`/`native_kind`
        // fields, so collision detection must come from live discovery
        // (`currently_claimed_kinds`) rather than the lockfile alone.
        // When the legacy plugin's binary still declares
        // `subject_kind:task`, the discovery pass surfaces `task` and
        // the next install gets bumped to `task-2`.
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "legacy-task".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
        });
        let live_claims = vec!["task".to_string()];
        let chosen = pick_installed_kind_for_install(&lock, &live_claims, "new-task", "task", None, &[])
            .expect("legacy lockfile row must still trigger collision detection via live discovery");
        assert_eq!(chosen, "task-2", "auto-increment must run when legacy plugin still claims native kind");
    }

    #[test]
    fn pick_installed_kind_lets_legacy_provider_row_pass_through() {
        // Codex P1 round-3 v0.5.7: a legacy provider lockfile row must
        // NOT spuriously force a fresh `subject_kind:task` install to
        // auto-increment. Provider rows do not claim subject kinds, so
        // `currently_claimed_kinds` carries no `task` entry and the new
        // install lands on the native value.
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "legacy-provider".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
        });
        let chosen = pick_installed_kind_for_install(&lock, &[], "new-task", "task", None, &[])
            .expect("legacy unrelated row must not block first subject install");
        assert_eq!(chosen, "task", "first subject_kind:task install must keep the native value");
    }

    #[test]
    fn pick_installed_kind_preserves_prior_alias_on_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "plugin-archive".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: Some("archive".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let chosen = pick_installed_kind_for_install(&lock, &[], "plugin-archive", "task", None, &[])
            .expect("upgrade must keep prior alias");
        assert_eq!(chosen, "archive", "upgrade without --as-kind must NOT move plugin back to native kind");
    }

    #[test]
    fn pick_installed_kind_skips_collision_when_upgrading_same_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "plugin-a".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: Some("task".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let chosen = pick_installed_kind_for_install(&lock, &[], "plugin-a", "task", None, &[])
            .expect("upgrade must not collide with itself");
        assert_eq!(chosen, "task", "re-install of same plugin keeps native kind");
    }

    #[test]
    fn rename_eligible_native_kind_picks_subject_kind_for_subject_backend() {
        let manifest = PluginManifest {
            name: "subject-x".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["subject_kind:task".into(), "subject_kind:incident".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        assert_eq!(rename_eligible_native_kind(&manifest).as_deref(), Some("task"));
    }

    #[test]
    fn rename_eligible_native_kind_returns_none_for_provider_in_v0_5_7() {
        let manifest = PluginManifest {
            name: "provider-x".into(),
            version: "0.1".into(),
            plugin_kind: "provider".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["provider_tool:claude".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        // v0.5.7 only renames subject_backend kinds; provider dispatch
        // does not consult plugins.lock yet, so providers must skip the
        // rename pipeline to avoid recording aliases the runtime cannot honor.
        assert_eq!(rename_eligible_native_kind(&manifest), None);
    }

    #[test]
    fn rename_eligible_native_kind_skips_glob_subject_kinds() {
        // Codex P2 round-3 v0.5.7: glob kinds (`subject_kind:task.*`) are
        // passed through unrenamed by the SubjectRouter, so the install
        // pipeline must skip them too — otherwise the lockfile would
        // record an `installed_kind` the router never registers.
        let manifest = PluginManifest {
            name: "subject-glob".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["subject_kind:task.*".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        assert_eq!(rename_eligible_native_kind(&manifest), None);
    }

    #[test]
    fn rename_eligible_native_kind_picks_exact_kind_even_when_globs_present() {
        let manifest = PluginManifest {
            name: "subject-mixed".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["subject_kind:task.*".into(), "subject_kind:incident".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        assert_eq!(rename_eligible_native_kind(&manifest).as_deref(), Some("incident"));
    }

    #[test]
    fn rename_eligible_native_kind_is_none_for_unknown_plugin_kind() {
        let manifest = PluginManifest {
            name: "transport-x".into(),
            version: "0.1".into(),
            plugin_kind: "transport".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["http_routes:foo".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        assert_eq!(rename_eligible_native_kind(&manifest), None);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn plugin_doctor_flags_collision_between_two_task_backends() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        // Stage two installed subject_backend plugins claiming the same
        // native kind, then hand-edit the lockfile so BOTH share the same
        // installed_kind. That is the broken-state the doctor must flag —
        // the install pipeline itself prevents it from happening through
        // the normal CLI path.
        let plugin_a = install_dir.join("animus-plugin-collide-a");
        write_fake_plugin_binary_with_capabilities(
            &plugin_a,
            "animus-plugin-collide-a",
            "subject_backend",
            &["subject_kind:task"],
        );
        let plugin_b = install_dir.join("animus-plugin-collide-b");
        write_fake_plugin_binary_with_capabilities(
            &plugin_b,
            "animus-plugin-collide-b",
            "subject_backend",
            &["subject_kind:task"],
        );
        let mut lock = PluginLockfile::empty_at(&animus_dir.join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "animus-plugin-collide-a".into(),
            version: "v0".into(),
            targets: lock_targets("1".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now.clone(),
            installed_kind: Some("task".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        lock.upsert(LockEntry {
            name: "animus-plugin-collide-b".into(),
            version: "v0".into(),
            targets: lock_targets("2".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: Some("task".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        lock.save().expect("save lockfile");

        use orchestrator_core::{summarize_discovered_plugins_with_lock, PluginPreflightSpec, RequiredRole};
        let discovered = discover_plugins(&project_root).expect("discover");
        let lock_loaded_for_summaries = PluginLockfile::load_default(Some(&project_root)).expect("reload lock");
        let summaries = summarize_discovered_plugins_with_lock(&discovered, Some(&lock_loaded_for_summaries));
        assert!(
            summaries.iter().any(|s| s.name == "animus-plugin-collide-a"),
            "discovery must surface plugin-a from the install dir"
        );
        assert!(
            summaries.iter().any(|s| s.name == "animus-plugin-collide-b"),
            "discovery must surface plugin-b from the install dir"
        );

        // Replicate the doctor's grouping logic locally to avoid invoking
        // the print path inside a multi-threaded test runner.
        let spec = PluginPreflightSpec::daemon_default();
        let subject_role = spec
            .required_roles
            .iter()
            .find(|r| matches!(r, RequiredRole::AtLeastOneSubjectBackend))
            .expect("at_least_one_subject_backend role present in daemon default spec");
        let claims: Vec<&str> = summaries
            .iter()
            .filter(|s| s.is_subject_backend() && s.covers_subject_kind("task"))
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(claims.len(), 2, "both plugins claim subject_kind:task: {claims:?}");
        let lock_loaded = PluginLockfile::load_default(Some(&project_root)).expect("reload lock");
        let installed_kinds: Vec<String> = claims
            .iter()
            .map(|name| {
                lock_loaded
                    .find(name)
                    .and_then(|e| e.effective_installed_kind())
                    .map(str::to_string)
                    .unwrap_or_else(|| "task".to_string())
            })
            .collect();
        let mut by_kind: std::collections::HashMap<&String, usize> = std::collections::HashMap::new();
        for k in &installed_kinds {
            *by_kind.entry(k).or_insert(0) += 1;
        }
        assert!(
            by_kind.values().any(|&v| v >= 2),
            "doctor must see at least one installed_kind claimed by two plugins for role {}",
            subject_role.label(),
        );
    }

    #[test]
    fn all_rename_eligible_native_kinds_returns_every_exact_kind_in_declaration_order() {
        let manifest = PluginManifest {
            name: "subject-multi".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec![
                "subject_kind:task".into(),
                "subject_kind:requirement".into(),
                "subject_kind:incident".into(),
            ],
            env_required: vec![],
            notification_buffer_size: None,
        };
        let kinds = all_rename_eligible_native_kinds(&manifest);
        assert_eq!(kinds, vec!["task".to_string(), "requirement".to_string(), "incident".to_string()]);
    }

    #[test]
    fn all_rename_eligible_native_kinds_drops_globs_and_duplicates() {
        let manifest = PluginManifest {
            name: "subject-mixed".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec![
                "subject_kind:task".into(),
                "subject_kind:task".into(),
                "subject_kind:task.*".into(),
                "subject_kind:requirement".into(),
            ],
            env_required: vec![],
            notification_buffer_size: None,
        };
        let kinds = all_rename_eligible_native_kinds(&manifest);
        assert_eq!(kinds, vec!["task".to_string(), "requirement".to_string()]);
    }

    #[test]
    fn compute_kind_assignment_blocks_multi_kind_secondary_collision() {
        // Closes codex P2 round-4 v0.5.7: a plugin declaring both `task` and
        // `requirement` must be refused at install time when EITHER kind
        // collides — not just the primary. The lockfile records a single
        // installed_kind per plugin, so we cannot auto-increment a
        // secondary collision.
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "existing-requirement".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: Some("requirement".into()),
            native_kind: Some("requirement".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let manifest = PluginManifest {
            name: "subject-multi".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["subject_kind:task".into(), "subject_kind:requirement".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        let err = compute_kind_assignment(Some(&manifest), &lock, &[], "subject-multi", None)
            .expect_err("secondary kind collision must refuse the install");
        let msg = format!("{err:#}");
        assert!(msg.contains("secondary subject_kind 'requirement'"), "error must name the secondary kind: {msg}");
        assert!(msg.contains("existing-requirement"), "error must name the colliding plugin: {msg}");
    }

    #[test]
    fn pick_installed_kind_avoids_own_secondary_native_during_auto_increment() {
        // Codex round-2 v0.5.8 P2: a manifest declaring `task` + `task-2`
        // installed after another plugin claims `task` must NOT receive
        // primary alias `task-2` (its own secondary native kind). The
        // SubjectRouter would otherwise refuse the duplicate at boot.
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "other-task".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: Some("task".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let chosen = pick_installed_kind_for_install(&lock, &[], "multi-plugin", "task", None, &["task-2".to_string()])
            .expect("auto-increment must skip own secondary kind");
        assert_eq!(chosen, "task-3", "auto-increment must skip task-2 because the plugin declares it natively");
    }

    #[test]
    fn pick_installed_kind_rejects_explicit_as_kind_matching_own_secondary() {
        let dir = tempfile::tempdir().unwrap();
        let lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let err = pick_installed_kind_for_install(
            &lock,
            &[],
            "multi-plugin",
            "task",
            Some("requirement"),
            &["requirement".to_string()],
        )
        .expect_err("--as-kind matching own secondary must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("own native subject kinds"), "error must explain self-collision: {msg}");
    }

    #[test]
    fn compute_kind_assignment_auto_increments_primary_when_secondary_clear() {
        // Multi-kind backend with a primary collision and a secondary that's
        // free — the install must auto-increment the primary slot rather
        // than refuse.
        let dir = tempfile::tempdir().unwrap();
        let mut lock = PluginLockfile::empty_at(&dir.path().join("plugins.lock"));
        let now = chrono::Utc::now().to_rfc3339();
        lock.upsert(LockEntry {
            name: "existing-task".into(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: now,
            installed_kind: Some("task".into()),
            native_kind: Some("task".into()),
            source_repo: None,
            resolved_commit: None,
        });
        let manifest = PluginManifest {
            name: "subject-multi".into(),
            version: "0.1".into(),
            plugin_kind: "subject_backend".into(),
            description: "t".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec!["subject_kind:task".into(), "subject_kind:incident".into()],
            env_required: vec![],
            notification_buffer_size: None,
        };
        let (assigned, native) = compute_kind_assignment(Some(&manifest), &lock, &[], "subject-multi", None)
            .expect("primary auto-increment with free secondary must succeed");
        assert_eq!(assigned.as_deref(), Some("task-2"));
        assert_eq!(native.as_deref(), Some("task"));
    }

    fn rename_test_lockfile(dir: &std::path::Path, entries: Vec<LockEntry>) -> PluginLockfile {
        let path = dir.join(".animus").join("plugins.lock");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut lock = PluginLockfile::empty_at(&path);
        for entry in entries {
            lock.upsert(entry);
        }
        lock.save().expect("save initial lockfile");
        lock
    }

    fn rename_lock_entry(name: &str, installed: &str, native: &str) -> LockEntry {
        LockEntry {
            name: name.to_string(),
            version: "v0.1".into(),
            targets: lock_targets("a".repeat(64)),
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
            installed_at: chrono::Utc::now().to_rfc3339(),
            installed_kind: Some(installed.to_string()),
            native_kind: Some(native.to_string()),
            source_repo: None,
            resolved_commit: None,
        }
    }

    #[test]
    fn run_plugin_rename_renames_lockfile_entry_when_target_is_free() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().to_path_buf();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        rename_test_lockfile(&project_root, vec![rename_lock_entry("plugin-a", "task", "task")]);

        let out = run_plugin_rename(PluginRenameRequest {
            name: "plugin-a".into(),
            to: "task-archive".into(),
            force: false,
            project_root: project_root.to_string_lossy().to_string(),
        })
        .expect("rename succeeds");
        assert_eq!(out.old_kind, "task");
        assert_eq!(out.new_kind, "task-archive");
        assert_eq!(out.native_kind, "task");
        assert!(!out.auto_incremented);

        let lock = PluginLockfile::load_default(Some(&project_root)).expect("reload");
        let entry = lock.find("plugin-a").expect("entry");
        assert_eq!(entry.effective_installed_kind(), Some("task-archive"));
        assert_eq!(entry.effective_native_kind(), Some("task"));
    }

    #[test]
    fn run_plugin_rename_missing_plugin_returns_not_found() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().to_path_buf();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        rename_test_lockfile(&project_root, vec![rename_lock_entry("plugin-a", "task", "task")]);

        let err = run_plugin_rename(PluginRenameRequest {
            name: "ghost-plugin".into(),
            to: "archive".into(),
            force: false,
            project_root: project_root.to_string_lossy().to_string(),
        })
        .expect_err("missing plugin must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("ghost-plugin"), "error must name the missing plugin: {msg}");
        assert!(msg.contains("no entry in"), "error must mention the lockfile: {msg}");
    }

    #[test]
    fn run_plugin_rename_collision_without_force_errors() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().to_path_buf();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        rename_test_lockfile(
            &project_root,
            vec![
                rename_lock_entry("plugin-a", "task", "task"),
                rename_lock_entry("plugin-b", "requirement", "requirement"),
            ],
        );

        let err = run_plugin_rename(PluginRenameRequest {
            name: "plugin-a".into(),
            to: "requirement".into(),
            force: false,
            project_root: project_root.to_string_lossy().to_string(),
        })
        .expect_err("colliding target without --force must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("plugin-b"), "error must name the colliding plugin: {msg}");
        assert!(msg.contains("--force"), "error must suggest --force: {msg}");
    }

    #[test]
    fn run_plugin_rename_collision_with_force_auto_increments() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().to_path_buf();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        rename_test_lockfile(
            &project_root,
            vec![
                rename_lock_entry("plugin-a", "task", "task"),
                rename_lock_entry("plugin-b", "requirement", "requirement"),
            ],
        );

        let out = run_plugin_rename(PluginRenameRequest {
            name: "plugin-a".into(),
            to: "requirement".into(),
            force: true,
            project_root: project_root.to_string_lossy().to_string(),
        })
        .expect("--force must auto-increment past collision");
        assert!(out.auto_incremented);
        assert_eq!(out.requested_kind, "requirement");
        assert_eq!(out.new_kind, "requirement-2");
    }

    #[test]
    fn run_plugin_rename_rejects_invalid_to_value() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().to_path_buf();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        rename_test_lockfile(&project_root, vec![rename_lock_entry("plugin-a", "task", "task")]);

        for bad in ["task/sub", "task:foo", "task *", "task*", ""] {
            let err = run_plugin_rename(PluginRenameRequest {
                name: "plugin-a".into(),
                to: bad.to_string(),
                force: false,
                project_root: project_root.to_string_lossy().to_string(),
            })
            .expect_err("invalid --to must be rejected");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("--to") || msg.contains("must not be empty"),
                "rejection must explain the invalid --to value '{bad}': {msg}",
            );
        }
    }

    /// Codex round-1 v0.5.8 P2: renaming a multi-kind subject backend to
    /// one of its OWN secondary native subject kinds would alias the
    /// primary slot while the secondary stays registered under the same
    /// native value — the SubjectRouter rejects the duplicate at startup.
    /// Refuse the rename here so the operator sees a clear error.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn run_plugin_rename_refuses_self_secondary_native_kind_collision() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        let source = tmp.path().join("animus-subject-multi");
        write_fake_plugin_binary_with_capabilities(
            &source,
            "animus-subject-multi",
            "subject_backend",
            &["subject_kind:task", "subject_kind:requirement"],
        );
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        run_plugin_install(req).await.expect("install multi-kind plugin");

        let err = run_plugin_rename(PluginRenameRequest {
            name: "animus-subject-multi".into(),
            to: "requirement".into(),
            force: false,
            project_root: project_root.to_string_lossy().to_string(),
        })
        .expect_err("rename to plugin's own secondary native kind must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("own native subject kinds"), "error must explain the self-collision: {msg}");
    }

    /// v0.5.8 fold-in: `--name <NAME>` install override is now recorded in
    /// plugins.yaml as `name_override`. Discovery uses the override when
    /// matching the lockfile entry, and the daemon SubjectRouter alias map
    /// stays consistent across boots.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn install_name_override_round_trips_through_yaml_lockfile_and_discovery() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();
        let config_dir = tmp.path().join("config");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();
        let _config_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().unwrap()));
        let _plugin_env =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(install_dir.to_str().unwrap()));

        let source = tmp.path().join("animus-subject-default");
        write_fake_plugin_binary_with_capabilities(
            &source,
            "animus-subject-default",
            "subject_backend",
            &["subject_kind:task"],
        );
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            name: Some("custom-task".to_string()),
            skip_signature: true,
            yes: true,
            project_root: Some(project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("install with --name override");
        assert_eq!(output.name, "custom-task");

        let yaml_path = std::path::PathBuf::from(&output.plugins_yaml);
        let yaml: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&yaml_path).unwrap()).unwrap();
        let entry =
            yaml.get("plugins").and_then(|p| p.get("custom-task")).expect("plugins.yaml table keyed under override");
        let recorded = entry
            .get("name_override")
            .and_then(|v| v.as_str())
            .expect("name_override field persisted when --name was passed");
        assert_eq!(recorded, "custom-task", "name_override must record the install-time override");

        let lock = PluginLockfile::load_default(Some(&project_root)).expect("load lock");
        assert!(lock.find("custom-task").is_some(), "lockfile entry must be keyed under the install-time override");

        // Discovery uses the override as the canonical name — without this
        // round-trip the daemon's SubjectRouter alias map could not find
        // the lockfile entry on next start.
        let discovered = orchestrator_plugin_host::PluginDiscovery::new()
            .with_project_root(&project_root)
            .discover()
            .expect("discover");
        let canonical_names: Vec<String> = discovered.iter().map(|p| p.name.clone()).collect();
        assert!(
            canonical_names.contains(&"custom-task".to_string()),
            "discovery must surface the plugin under its name_override: {canonical_names:?}",
        );
    }

    // ===== Project-scoped plugin installation (`--project`) =====
    //
    // Every test pins HOME + ANIMUS_CONFIG_DIR + ANIMUS_PLUGIN_DIR to a
    // tempdir (via EnvVarGuard, behind INSTALL_ENV_GUARD + the crate env
    // lock) so the real `~/.animus` is never touched.

    #[cfg(unix)]
    struct ProjectScopeEnv {
        _tmp: tempfile::TempDir,
        _home: protocol::test_utils::EnvVarGuard,
        _config: protocol::test_utils::EnvVarGuard,
        _plugin_dir: protocol::test_utils::EnvVarGuard,
        global_dir: PathBuf,
        project_root: PathBuf,
        scratch: PathBuf,
    }

    #[cfg(unix)]
    fn project_scope_env() -> ProjectScopeEnv {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = tmp.path().join("config");
        let global_dir = tmp.path().join("global-plugins");
        let project_root = tmp.path().join("project");
        let scratch = tmp.path().join("scratch");
        for dir in [&home, &config_dir, &global_dir, &project_root, &scratch] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.to_str().expect("utf-8")));
        let _config =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_dir.to_str().expect("utf-8")));
        let _plugin_dir =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(global_dir.to_str().expect("utf-8")));
        ProjectScopeEnv { _tmp: tmp, _home, _config, _plugin_dir, global_dir, project_root, scratch }
    }

    #[cfg(unix)]
    async fn install_for_test(env: &ProjectScopeEnv, binary_name: &str, project: bool) -> PluginInstallOutput {
        let source = env.scratch.join(binary_name);
        write_fake_plugin_binary(&source, binary_name, "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            force: true,
            project,
            project_root: if project { Some(env.project_root.to_string_lossy().to_string()) } else { None },
            ..Default::default()
        };
        run_plugin_install(req).await.expect("install must succeed")
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn install_project_scope_lands_in_project_dirs_and_lockfile() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();

        let output = install_for_test(&env, "animus-plugin-projscoped", true).await;

        assert_eq!(output.scope, "project");
        let binary = env.project_root.join(".animus/plugins/animus-plugin-projscoped");
        assert!(binary.is_file(), "binary must land in <project>/.animus/plugins/");
        assert_eq!(output.installed_path, binary.to_string_lossy());

        // Registry: project-local plugins.yaml, not the global one.
        let registry = env.project_root.join(".animus/plugins.yaml");
        assert_eq!(output.plugins_yaml, registry.to_string_lossy());
        let yaml: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&registry).unwrap()).unwrap();
        assert!(yaml.get("plugins").and_then(|p| p.get("animus-plugin-projscoped")).is_some());
        assert!(!plugins_registry_path().exists(), "global plugins.yaml must stay untouched");

        // Lockfile: project-local plugins.lock with an entry; the global
        // lockfile under the pinned HOME must not exist.
        let lock = PluginLockfile::load_or_empty(&project_lockfile_path(&env.project_root)).unwrap();
        assert!(lock.find("animus-plugin-projscoped").is_some(), "project lockfile must record the install");
        assert!(!global_lockfile_path().exists(), "global lockfile must stay untouched");

        // Binaries stay out of version control; the lockfile stays committable.
        let gitignore = std::fs::read_to_string(env.project_root.join(".animus/.gitignore")).unwrap();
        assert!(gitignore.lines().any(|l| l.trim() == "plugins/"), "gitignore must cover plugins/: {gitignore}");
        assert!(
            !gitignore.lines().any(|l| l.trim() == "plugins.lock"),
            "lockfile must remain committable: {gitignore}"
        );

        // Nothing may leak into the global install dir.
        assert_eq!(std::fs::read_dir(&env.global_dir).unwrap().count(), 0, "global install dir must stay empty");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn uninstall_project_scope_removes_binary_registry_and_lock_entry() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-projgone", true).await;

        let output = run_plugin_uninstall(PluginUninstallRequest {
            name: "animus-plugin-projgone".to_string(),
            plugin_dir: None,
            project_root: Some(env.project_root.to_string_lossy().to_string()),
            project: true,
        })
        .expect("uninstall must succeed");
        assert_eq!(output.scope, "project");
        assert!(output.removed_path.is_some());
        assert!(!env.project_root.join(".animus/plugins/animus-plugin-projgone").exists());

        let registry = env.project_root.join(".animus/plugins.yaml");
        let yaml: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&registry).unwrap()).unwrap();
        assert!(yaml.get("plugins").and_then(|p| p.get("animus-plugin-projgone")).is_none());

        let lock = PluginLockfile::load_or_empty(&project_lockfile_path(&env.project_root)).unwrap();
        assert!(lock.find("animus-plugin-projgone").is_none(), "project lock entry must be removed");
    }

    /// A project-local install shadows a global install of the same name:
    /// discovery returns the project copy (scope=project) and `plugin list`
    /// surfaces the hidden global binary in `shadowed`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn plugin_list_marks_scope_and_renders_shadowed_global_install() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-shadowed", false).await;
        install_for_test(&env, "animus-plugin-shadowed", true).await;

        let output = run_plugin_list(PluginListRequest {
            project_root: env.project_root.to_string_lossy().to_string(),
            include_system_path: false,
        })
        .expect("plugin list");
        let rows: Vec<_> = output.plugins.iter().filter(|p| p.name == "animus-plugin-shadowed").collect();
        assert_eq!(rows.len(), 1, "name dedup must keep exactly one row");
        assert_eq!(rows[0].scope, "project", "project install must win discovery");
        assert!(rows[0].path.starts_with(&*env.project_root.to_string_lossy()), "winning path must be project-local");

        assert_eq!(output.shadowed.len(), 1, "the hidden global binary must be surfaced");
        let shadow = &output.shadowed[0];
        assert_eq!(shadow.name, "animus-plugin-shadowed");
        assert_eq!(shadow.note, "shadowed by project install");
        assert!(shadow.path.starts_with(&*env.global_dir.to_string_lossy()));
        assert_eq!(shadow.shadowed_by, rows[0].path);
    }

    /// `plugin lock verify` sweeps BOTH lockfile roots: global entries
    /// against the global dir, project entries against the project dir.
    /// Tampering with the project-installed binary fails the gate.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn lock_verify_covers_global_and_project_roots() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-globalroot", false).await;
        install_for_test(&env, "animus-plugin-projroot", true).await;

        let verify_args = || PluginLockVerifyArgs { lockfile: None, plugin_dir: None, json: true };
        let output = compute_lock_verify(verify_args(), env.project_root.to_string_lossy().as_ref())
            .expect("verify must compute");
        assert_eq!(output.lockfiles.len(), 2, "both lockfile roots must be swept: {:?}", output.lockfiles);
        let scope_of = |name: &str| {
            output.entries.iter().find(|e| e.name == name).map(|e| (e.scope, e.status)).unwrap_or_else(|| {
                panic!("entry for {name} missing in {:?}", output.entries.iter().map(|e| &e.name).collect::<Vec<_>>())
            })
        };
        assert_eq!(scope_of("animus-plugin-globalroot"), ("global", "ok"));
        assert_eq!(scope_of("animus-plugin-projroot"), ("project", "ok"));
        assert_eq!(output.mismatched, 0);
        assert_eq!(output.missing_binary, 0);

        // Explicit `--lockfile <project>/.animus/plugins.lock` (the
        // committed-lockfile CI use case) must probe the lockfile's sibling
        // `plugins/` dir, not just the global install dir.
        let explicit = compute_lock_verify(
            PluginLockVerifyArgs {
                lockfile: Some(project_lockfile_path(&env.project_root)),
                plugin_dir: None,
                json: true,
            },
            env.project_root.to_string_lossy().as_ref(),
        )
        .expect("explicit verify must compute");
        let proj_entry = explicit.entries.iter().find(|e| e.name == "animus-plugin-projroot").expect("project entry");
        assert_eq!((proj_entry.scope, proj_entry.status), ("explicit", "ok"));

        // Tamper with the project binary: verify must flag a project-scope mismatch.
        std::fs::write(env.project_root.join(".animus/plugins/animus-plugin-projroot"), b"tampered").unwrap();
        let tampered = compute_lock_verify(verify_args(), env.project_root.to_string_lossy().as_ref()).unwrap();
        assert_eq!(tampered.mismatched, 1);
        let bad = tampered.entries.iter().find(|e| e.name == "animus-plugin-projroot").unwrap();
        assert_eq!(bad.scope, "project");
        assert_eq!(bad.status, "mismatch");
    }

    #[test]
    fn is_commit_sha_accepts_only_40_hex_lowercase() {
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(is_commit_sha("0123456789abcdef0123456789abcdef01234567"));
        // Wrong length.
        assert!(!is_commit_sha(&"a".repeat(39)));
        assert!(!is_commit_sha(&"a".repeat(41)));
        // Branch name / non-hex.
        assert!(!is_commit_sha("main"));
        // Uppercase hex must be rejected (GitHub returns lowercase shas).
        assert!(!is_commit_sha(&"A".repeat(40)));
        assert!(!is_commit_sha(""));
    }

    /// `plugin lock verify` flags an installed plugin that is absent from the
    /// lockfile as "extra" drift and exits the gate non-zero.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn lock_verify_flags_extra_plugin_not_in_lockfile() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-tracked", false).await;

        // Drop the lockfile entry but leave the binary on disk: it is now
        // installed-but-unlocked, i.e. "extra".
        let mut global = PluginLockfile::load_or_empty(&global_lockfile_path()).unwrap();
        global.remove("animus-plugin-tracked");
        global.save().unwrap();

        let output = compute_lock_verify(
            PluginLockVerifyArgs { lockfile: None, plugin_dir: None, json: true },
            env.project_root.to_string_lossy().as_ref(),
        )
        .expect("verify must compute");
        assert!(output.extra >= 1, "an installed-but-unlocked plugin must be flagged extra: {:?}", output.entries);
        let extra = output
            .entries
            .iter()
            .find(|e| e.name == "animus-plugin-tracked")
            .expect("extra entry for the unlocked plugin");
        assert_eq!(extra.status, "extra");
    }

    /// A global-scope install/uninstall of a name that is ALSO
    /// project-scope installed must not touch the project lockfile entry
    /// that protects the project binary: the global op is routed to the
    /// global `~/.animus/plugins.lock` instead.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn global_ops_do_not_clobber_project_lock_entry_for_shadowed_name() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-bothscopes", true).await;
        let project_lock = || PluginLockfile::load_or_empty(&project_lockfile_path(&env.project_root)).unwrap();
        let project_sha = project_lock().find("animus-plugin-bothscopes").unwrap().targets.clone();

        // Global install of the SAME name, with the project root supplied
        // (as the CLI always does). The lock entry must land in the global
        // lockfile, leaving the project entry untouched.
        let source = env.scratch.join("animus-plugin-bothscopes");
        write_fake_plugin_binary(&source, "animus-plugin-bothscopes", "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            force: true,
            project: false,
            project_root: Some(env.project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        run_plugin_install(req).await.expect("global install must succeed");
        let global_lock = PluginLockfile::load_or_empty(&global_lockfile_path()).unwrap();
        assert!(global_lock.find("animus-plugin-bothscopes").is_some(), "global op must write the global lockfile");
        assert_eq!(
            project_lock().find("animus-plugin-bothscopes").unwrap().targets,
            project_sha,
            "project lock entry must be untouched by the global install"
        );

        // With both installs present (identical artifact!), deleting the
        // PROJECT binary must surface as missing_binary on the project
        // entry — the same-named global binary must not satisfy it.
        let project_binary = env.project_root.join(".animus/plugins/animus-plugin-bothscopes");
        let project_binary_bytes = std::fs::read(&project_binary).unwrap();
        std::fs::remove_file(&project_binary).unwrap();
        let verify = compute_lock_verify(
            PluginLockVerifyArgs { lockfile: None, plugin_dir: None, json: true },
            env.project_root.to_string_lossy().as_ref(),
        )
        .unwrap();
        let project_entry = verify
            .entries
            .iter()
            .find(|e| e.scope == "project" && e.name == "animus-plugin-bothscopes")
            .expect("project entry");
        assert_eq!(
            project_entry.status, "missing_binary",
            "a same-named global binary must not satisfy a project-scoped lock entry"
        );
        std::fs::write(&project_binary, project_binary_bytes).unwrap();
        ensure_executable(&project_binary).unwrap();

        // Global uninstall: removes the global binary + global lock entry,
        // but never the project entry.
        run_plugin_uninstall(PluginUninstallRequest {
            name: "animus-plugin-bothscopes".to_string(),
            plugin_dir: None,
            project_root: Some(env.project_root.to_string_lossy().to_string()),
            project: false,
        })
        .expect("global uninstall must succeed");
        let global_lock = PluginLockfile::load_or_empty(&global_lockfile_path()).unwrap();
        assert!(global_lock.find("animus-plugin-bothscopes").is_none(), "global lock entry must be removed");
        assert!(
            project_lock().find("animus-plugin-bothscopes").is_some(),
            "project lock entry must survive a global uninstall"
        );
        assert!(
            env.project_root.join(".animus/plugins/animus-plugin-bothscopes").exists(),
            "project binary must survive a global uninstall"
        );
    }

    /// `.animus/plugin-scope.yaml` admit-filtering applies to
    /// project-installed plugins exactly as it does to global ones.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn plugin_scope_allowlist_filters_project_installed_plugin() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-scopedout", true).await;

        let list = |root: &Path| {
            run_plugin_list(PluginListRequest {
                project_root: root.to_string_lossy().to_string(),
                include_system_path: false,
            })
            .expect("plugin list")
        };
        assert!(
            list(&env.project_root).plugins.iter().any(|p| p.name == "animus-plugin-scopedout"),
            "without a scope file the project install must be discovered"
        );

        std::fs::write(
            env.project_root.join(".animus/plugin-scope.yaml"),
            "schema: animus.plugin-scope.v1\nmode: allowlist\nallow:\n  - some-other-plugin\n",
        )
        .unwrap();
        assert!(
            !list(&env.project_root).plugins.iter().any(|p| p.name == "animus-plugin-scopedout"),
            "an allowlist that omits the project-installed plugin must exclude it"
        );
    }

    /// `plugin update --project` reads the PROJECT registry, not the global one.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn plugin_update_project_reads_project_registry() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-globalonly", false).await;
        install_for_test(&env, "animus-plugin-projonly", true).await;

        let output = marketplace::run_plugin_update(marketplace::PluginUpdateRequest {
            selector: PluginUpdateSelector::All,
            tag_override: None,
            check: true,
            force: false,
            project_root: Some(env.project_root.to_string_lossy().to_string()),
            project: true,
        })
        .await
        .expect("update --check must succeed");
        let names: Vec<&str> = output.results.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"animus-plugin-projonly"), "project install must be considered: {names:?}");
        assert!(!names.contains(&"animus-plugin-globalonly"), "global install must NOT leak into --project: {names:?}");
    }

    /// `plugin outdated` includes project-scope rows alongside global ones.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn plugin_outdated_includes_project_scope_rows() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        install_for_test(&env, "animus-plugin-globaldrift", false).await;
        install_for_test(&env, "animus-plugin-projdrift", true).await;

        // `offline: true` keeps the test off the network: the registry fetch
        // degrades to latest=unknown without failing the command.
        let output = marketplace::run_plugin_outdated(marketplace::PluginOutdatedRequest {
            registry_url: String::new(),
            no_cache: false,
            offline: true,
            project_root: Some(env.project_root.to_string_lossy().to_string()),
        })
        .await
        .expect("outdated must succeed offline");
        let scope_of = |name: &str| output.rows.iter().find(|r| r.name == name).map(|r| r.scope);
        assert_eq!(scope_of("animus-plugin-globaldrift"), Some("global"));
        assert_eq!(scope_of("animus-plugin-projdrift"), Some("project"));
    }

    /// A project-scoped install whose name falls outside the dir-scanned
    /// prefixes (official plugins like `animus-subject-default`, or a
    /// custom `--name`) must still be discoverable — via the project
    /// registry tier (`<project>/.animus/plugins.yaml`).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn project_install_with_unscanned_name_is_discoverable_via_project_registry() {
        let _guard = INSTALL_ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let env = project_scope_env();
        let source = env.scratch.join("animus-subject-default");
        write_fake_plugin_binary(&source, "animus-subject-default", "subject_backend");
        let req = PluginInstallRequest {
            path: Some(source.to_string_lossy().to_string()),
            skip_signature: true,
            yes: true,
            project: true,
            project_root: Some(env.project_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let output = run_plugin_install(req).await.expect("project install of official subject plugin");
        assert_eq!(output.scope, "project");

        let list = run_plugin_list(PluginListRequest {
            project_root: env.project_root.to_string_lossy().to_string(),
            include_system_path: false,
        })
        .expect("plugin list");
        let row = list
            .plugins
            .iter()
            .find(|p| p.name == "animus-subject-default")
            .expect("project registry tier must surface the unscanned name");
        assert_eq!(row.scope, "project");
        assert!(row.path.starts_with(&*env.project_root.to_string_lossy()));
    }

    #[test]
    fn resolve_install_scope_rejects_plugin_dir_and_missing_root() {
        let err = resolve_install_scope(true, Some("/tmp/x"), Some("/tmp/dir")).unwrap_err();
        assert!(format!("{err}").contains("mutually exclusive"), "got: {err}");
        let err = resolve_install_scope(true, None, None).unwrap_err();
        assert!(format!("{err}").contains("project root"), "got: {err}");
    }

    #[test]
    fn cli_rejects_project_with_plugin_dir() {
        use clap::Parser;
        let err = crate::Cli::try_parse_from([
            "animus",
            "plugin",
            "install",
            "owner/repo",
            "--project",
            "--plugin-dir",
            "/tmp/x",
        ])
        .expect_err("--project + --plugin-dir must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

        let err = crate::Cli::try_parse_from([
            "animus",
            "plugin",
            "uninstall",
            "--name",
            "x",
            "--project",
            "--plugin-dir",
            "/tmp/x",
        ])
        .expect_err("--project + --plugin-dir must conflict on uninstall");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn ensure_project_plugins_gitignore_is_idempotent_and_appends() {
        let _lock = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(tmp.path().to_str().unwrap()));
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();

        ensure_project_plugins_gitignore(&root).unwrap();
        let first = std::fs::read_to_string(root.join(".animus/.gitignore")).unwrap();
        assert!(first.lines().any(|l| l.trim() == "plugins/"));
        ensure_project_plugins_gitignore(&root).unwrap();
        let second = std::fs::read_to_string(root.join(".animus/.gitignore")).unwrap();
        assert_eq!(first, second, "second call must be a no-op");

        // Operator-managed file with other lines: pattern is appended once.
        std::fs::write(root.join(".animus/.gitignore"), "daemon.log\n").unwrap();
        ensure_project_plugins_gitignore(&root).unwrap();
        let appended = std::fs::read_to_string(root.join(".animus/.gitignore")).unwrap();
        assert!(appended.starts_with("daemon.log\n"), "existing lines preserved: {appended}");
        assert!(appended.lines().any(|l| l.trim() == "plugins/"));
    }
}
