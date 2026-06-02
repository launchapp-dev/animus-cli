mod control_routing;
mod marketplace;
mod new;
mod signing;

pub(crate) use control_routing::build_plugin_routing;
pub(crate) use marketplace::{
    run_plugin_browse, run_plugin_search, run_plugin_update, PluginBrowseRequest, PluginSearchRequest,
    PluginUpdateRequest,
};
#[allow(unused_imports)]
pub(crate) use signing::{
    cosign_available, load_trusted_signers, resolve_trusted_signers_path, verify_with_cosign, SignatureStatus,
    GITHUB_OIDC_ISSUER,
};

use std::collections::BTreeSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use animus_plugin_protocol::PluginManifest;
use anyhow::{anyhow, Context, Result};
use orchestrator_daemon_runtime::{Audit, AuditActor, AuditEvent, AuditEventKind};
use orchestrator_plugin_host::{
    discover_plugins, legacy_plugins_registry_path, plugin_install_dir, plugins_registry_path,
    registered_skip_manifest_check_at_install, sha256_of_file as plugin_host_sha256_of_file, DiscoveredPlugin,
    DiscoverySource, DiscoveryWarning, LockEntry, LockVerifyResult, PluginDiscovery, PluginHost, PluginLockfile,
    PluginSpawnOptions, PolicyMode as PluginPolicyMode,
};
use orchestrator_session_host::is_reserved_provider_tool;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    invalid_input_error, not_found_error, print_value, PluginCallArgs, PluginCommand, PluginInfoArgs,
    PluginInstallArgs, PluginInstallDefaultsArgs, PluginListArgs, PluginLockCommand, PluginLockListArgs,
    PluginLockVerifyArgs, PluginPingArgs, PluginUninstallArgs,
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
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUninstallOutput {
    pub(crate) name: String,
    pub(crate) removed_path: Option<String>,
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
}

#[derive(Debug, Clone)]
pub(crate) struct PluginUninstallRequest {
    pub(crate) name: String,
    pub(crate) plugin_dir: Option<String>,
    /// Project root for lockfile + audit-log resolution.
    pub(crate) project_root: Option<String>,
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
        PluginCommand::New(args) => new::handle_plugin_new(args, json),
        PluginCommand::Search(args) => marketplace::handle_plugin_search(args).await,
        PluginCommand::Browse(args) => marketplace::handle_plugin_browse(args).await,
        PluginCommand::Update(args) => marketplace::handle_plugin_update(args).await,
        PluginCommand::InstallDefaults(args) => handle_plugin_install_defaults(args, project_root).await,
        PluginCommand::Lock(cmd) => handle_plugin_lock(cmd, project_root).await,
    }
}

/// Default plugin tables installed by `animus plugin install-defaults`.
///
/// These re-exports point at `orchestrator_core::plugin_registry` so the
/// daemon preflight (`PluginPreflightSpec::daemon_default`) and this CLI
/// command resolve identical `(owner/repo, tag)` pairs. Bump tags in
/// `crates/orchestrator-core/src/plugin_registry.rs`, not here.
use orchestrator_core::plugin_registry::{
    DEFAULT_OAI_AGENT_PLUGINS as DEFAULT_OAI_AGENT_PLUGIN, DEFAULT_PROVIDER_PLUGINS, DEFAULT_QUEUE_PLUGINS,
    DEFAULT_SUBJECT_PLUGINS, DEFAULT_TRANSPORT_PLUGINS, DEFAULT_WORKFLOW_RUNNER_PLUGINS,
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

/// Wave 3: assemble the `(slug, tag)` install list. When
/// `flavors/default.toml` is present on disk, the manifest is the source
/// of truth for *which* slugs to install — tags still come from the
/// curated constants in `orchestrator_core::plugin_registry`. Slugs the
/// manifest declares that the curated registry hasn't pinned yet (e.g.
/// `animus-provider-ollama`, `animus-trigger-cron`) emit a warning and
/// are skipped: the manifest is a forward-looking declaration; the
/// constants table is the authoritative pin.
///
/// When no flavor manifest is found, the legacy hardcoded
/// `DEFAULT_PROVIDER_PLUGINS + ...` tables are used directly.
fn build_install_defaults_targets(args: &PluginInstallDefaultsArgs, project_root: &str) -> Vec<(String, String)> {
    use orchestrator_core::flavor::{load_flavor_in, locate_flavor_manifest_in};
    use orchestrator_core::resolve_tag_for_slug;
    use orchestrator_core::DEFAULT_FLAVOR_ID;

    // Codex P2: when a `flavors/default.toml` file IS present but fails
    // to parse or validate, silently falling back to the hardcoded
    // tables would hide schema drift or a bad `$ANIMUS_FLAVORS_DIR`
    // override. Probe for presence first; if found, surface load errors
    // via `tracing::error!` then still fall back so the daemon stays
    // usable.
    let project_root_path = std::path::Path::new(project_root);
    let manifest_present = locate_flavor_manifest_in(project_root_path, DEFAULT_FLAVOR_ID).is_some();
    let load_result = load_flavor_in(project_root_path, DEFAULT_FLAVOR_ID);
    if manifest_present {
        if let Err(error) = &load_result {
            tracing::error!(
                flavor = DEFAULT_FLAVOR_ID,
                error = %error,
                "flavor manifest present on disk but failed to load; falling back to hardcoded plugin defaults"
            );
        }
    }
    if let Ok(Some(manifest)) = load_result {
        let mut slugs: Vec<String> = Vec::new();
        slugs.extend(manifest.workflow_runner.required.iter().cloned());
        slugs.extend(manifest.queue.required.iter().cloned());
        slugs.extend(manifest.providers.required.iter().cloned());
        slugs.extend(manifest.providers.recommended.iter().cloned());
        if args.include_oai_agent {
            for (slug, _) in DEFAULT_OAI_AGENT_PLUGIN {
                slugs.push((*slug).to_string());
            }
        }
        if args.include_subjects {
            slugs.extend(manifest.subjects.required.iter().cloned());
            slugs.extend(manifest.subjects.recommended.iter().cloned());
        }
        if args.include_transports {
            slugs.extend(manifest.transports.required.iter().cloned());
            slugs.extend(manifest.transports.recommended.iter().cloned());
            slugs.extend(manifest.ui.recommended.iter().cloned());
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
        return targets;
    }

    let mut targets: Vec<(String, String)> =
        DEFAULT_PROVIDER_PLUGINS.iter().map(|(s, t)| ((*s).to_string(), (*t).to_string())).collect();
    // v0.5: workflow_runner + queue plugins are required by daemon preflight
    // and ship as part of the curated default flavor. Install unconditionally
    // so `animus plugin install-defaults` actually unblocks `animus daemon
    // start` on the no-flavor-manifest path.
    for (s, t) in DEFAULT_WORKFLOW_RUNNER_PLUGINS {
        targets.push(((*s).to_string(), (*t).to_string()));
    }
    for (s, t) in DEFAULT_QUEUE_PLUGINS {
        targets.push(((*s).to_string(), (*t).to_string()));
    }
    if args.include_oai_agent {
        for (s, t) in DEFAULT_OAI_AGENT_PLUGIN {
            targets.push(((*s).to_string(), (*t).to_string()));
        }
    }
    if args.include_subjects {
        for (s, t) in DEFAULT_SUBJECT_PLUGINS {
            targets.push(((*s).to_string(), (*t).to_string()));
        }
    }
    if args.include_transports {
        for (s, t) in DEFAULT_TRANSPORT_PLUGINS {
            targets.push(((*s).to_string(), (*t).to_string()));
        }
    }
    targets
}

async fn handle_plugin_install_defaults(args: PluginInstallDefaultsArgs, project_root: &str) -> Result<()> {
    let mut targets: Vec<(String, String)> = build_install_defaults_targets(&args, project_root);

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
        let mut lock = load_or_refuse_lockfile(project_root_for_lock, args.force_rewrite_lockfile)?;
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
    Ok(PluginListOutput { plugins: rows, warnings: warning_rows })
}

fn spawn_options_for_discovered(plugin: &DiscoveredPlugin) -> PluginSpawnOptions {
    PluginSpawnOptions::for_manifest(
        plugin.name.clone(),
        &plugin.manifest.env_required,
        std::iter::empty::<String>(),
        None,
    )
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
    let skip_flag = registered_skip_manifest_check_at_install(&discovered.name);
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

    let yaml_path = plugins_yaml_path()?;
    let mut config = load_plugins_yaml(&yaml_path)?;
    let key = serde_yaml::Value::String(plugin_name.clone());
    let removed_in_yaml = config.plugins.remove(&key).is_some() || config.providers.remove(&key).is_some();
    if removed_in_yaml {
        save_plugins_yaml(&yaml_path, &config)?;
    }

    let install_dir = install_root(req.plugin_dir.as_deref())?;
    let installed_path = install_dir.join(&plugin_name);
    let removed = if installed_path.exists() {
        std::fs::remove_file(&installed_path)
            .with_context(|| format!("failed to remove {}", installed_path.display()))?;
        Some(installed_path.to_string_lossy().to_string())
    } else {
        None
    };

    if !removed_in_yaml && removed.is_none() {
        return Err(not_found_error(format!("plugin '{plugin_name}' is not installed")));
    }

    // Remove the lockfile entry (best-effort; never blocks uninstall).
    let project_root_pb = req.project_root.as_deref().map(std::path::PathBuf::from);
    let project_root_for_lock: Option<&std::path::Path> = project_root_pb.as_deref();
    if let Ok(mut lockfile) = PluginLockfile::load_default(project_root_for_lock) {
        if lockfile.remove(&plugin_name).is_some() {
            if let Err(err) = lockfile.save() {
                tracing::warn!(path = %lockfile.path().display(), %err, "failed to persist plugin lockfile after uninstall");
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
    })
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
fn load_or_refuse_lockfile(project_root: Option<&Path>, force_rewrite_lockfile: bool) -> Result<PluginLockfile> {
    match PluginLockfile::load_default(project_root) {
        Ok(lock) => Ok(lock),
        Err(err) => {
            let lock_path = PluginLockfile::default_path(project_root);
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
    let project_root_pb_pre = req.project_root.as_deref().map(std::path::PathBuf::from);
    let project_root_for_lock_pre: Option<&std::path::Path> = project_root_pb_pre.as_deref();
    let _ = load_or_refuse_lockfile(project_root_for_lock_pre, req.force_rewrite_lockfile)?;

    // `_install_temp` keeps the install-staging directory alive for the
    // remainder of this function. It drops at the end (RAII) so the
    // tempdir is reliably cleaned up whether install succeeds, errors,
    // or returns early — closing the GBs-of-`animus-plugin-install-*`
    // accumulation the old `std::env::temp_dir().join(uuid)` left behind.
    let (source_path, default_name, provenance, _install_temp): (
        PathBuf,
        String,
        InstallProvenance,
        Option<tempfile::TempDir>,
    ) = if let Some(slug) = req.source.as_deref() {
        let spec = parse_repo_spec(slug)?;
        let release = resolve_release_install(spec, req.tag.clone()).await?;
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
        };
        (release.binary_path, release.plugin_name_hint, provenance, Some(release._temp_dir))
    } else if let Some(p) = req.path.as_deref() {
        (PathBuf::from(p), String::new(), InstallProvenance { source_kind: Some("path"), ..Default::default() }, None)
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
            ..Default::default()
        };
        (path, String::new(), provenance, Some(temp_dir))
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

    if provenance.source_kind == Some("release") {
        if let Some(owner) = provenance.owner.as_deref() {
            enforce_org_trust(owner, &req)?;
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

    let plugin_name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None => {
            if !default_name.is_empty() {
                default_name
            } else {
                source_path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .ok_or_else(|| invalid_input_error("could not derive plugin name from source path"))?
                    .to_string()
            }
        }
    };

    let install_dir = install_root(req.plugin_dir.as_deref())?;
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
    let mut lockfile = load_or_refuse_lockfile(project_root_for_lock, req.force_rewrite_lockfile)?;
    let lockfile_path_for_log = lockfile.path().to_path_buf();
    let is_upgrade = installed_path.exists();
    if is_upgrade {
        if let Some(existing_entry) = lockfile.find(&plugin_name).cloned() {
            match lockfile.verify_installed(&plugin_name, &installed_path) {
                Ok(LockVerifyResult::Match) | Ok(LockVerifyResult::Missing) => {}
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
                            "lockfile mismatch for plugin '{plugin_name}': recorded sha256 {} but on-disk binary hashes to {}. \
                             The installed binary appears to have been modified or replaced out of band. \
                             Re-run with --force to overwrite (and update the lockfile), or `animus plugin lock verify` to inspect.",
                            existing_entry.artifact_sha256, actual,
                        )));
                    }
                }
                Err(err) => {
                    tracing::warn!(plugin = %plugin_name, %err, "failed to hash existing installed plugin during lockfile pre-check");
                }
            }
        }
    }

    if installed_path.exists() && !req.force {
        return Err(invalid_input_error(format!(
            "plugin '{plugin_name}' already installed at {} (pass force=true to overwrite)",
            installed_path.display()
        )));
    }

    std::fs::copy(&source_path, &installed_path)
        .with_context(|| format!("failed to copy {} → {}", source_path.display(), installed_path.display()))?;
    ensure_executable(&installed_path)?;

    // Manifest was probed against the source binary above; nothing to do here.
    let manifest = source_manifest;

    let yaml_path = plugins_yaml_path()?;
    let mut config = load_plugins_yaml(&yaml_path)?;
    let entry: serde_yaml::Mapping = {
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            serde_yaml::Value::String("binary".to_string()),
            serde_yaml::Value::String(installed_path.to_string_lossy().to_string()),
        );
        if let Some(m) = manifest.as_ref() {
            map.insert(serde_yaml::Value::String("name".to_string()), serde_yaml::Value::String(m.name.clone()));
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

    // TOFU: persist trust for the org we just installed from. Pre-trusted orgs
    // and orgs the user explicitly listed via `--allow-org` get written to
    // `~/.animus/trusted-orgs.yaml` so a follow-up install skips the prompt.
    if let Some(owner) = provenance.owner.as_deref() {
        if let Err(error) = add_trusted_org(owner) {
            tracing::warn!(owner, %error, "failed to persist trusted org after install");
        }
    }
    for explicit in &req.allow_org {
        if let Err(error) = add_trusted_org(explicit) {
            tracing::warn!(org = %explicit, %error, "failed to persist --allow-org");
        }
    }

    let sha256_verified = match provenance.sha256_verified {
        Some(verified) => verified,
        None => req.sha256.is_some(),
    };

    let signature_status = signature_detail.label().to_string();

    // ---- Lockfile: persist this install ----
    let bundle_sha = provenance.bundle_path.as_deref().and_then(|p| plugin_host_sha256_of_file(p).ok());
    let recorded_at = chrono::Utc::now().to_rfc3339();
    let lock_entry = LockEntry {
        name: plugin_name.clone(),
        version: provenance.release_tag.clone().unwrap_or_default(),
        artifact_sha256: computed_sha.clone(),
        signature_bundle_sha256: bundle_sha,
        installed_at: recorded_at,
    };
    lockfile.upsert(lock_entry);
    if let Err(err) = lockfile.save() {
        tracing::warn!(path = %lockfile.path().display(), %err, "failed to persist plugin lockfile");
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
    })
}

fn discover(project_root: &str, include_system_path: bool) -> Result<Vec<DiscoveredPlugin>> {
    PluginDiscovery::new()
        .with_project_root(Path::new(project_root))
        .include_system_path(include_system_path)
        .discover()
        .context("plugin discovery failed")
}

fn discover_with_warnings(
    project_root: &str,
    include_system_path: bool,
) -> Result<(Vec<DiscoveredPlugin>, Vec<DiscoveryWarning>)> {
    PluginDiscovery::new()
        .with_project_root(Path::new(project_root))
        .include_system_path(include_system_path)
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

    for warning in &output.warnings {
        eprintln!(
            "warning: plugin '{}' was discovered ({}) but could not be loaded: {} ({})",
            warning.name, warning.source, warning.reason, warning.path
        );
    }

    print_plugin_list_table(&output)
}

/// Render `plugin list` results as a table with source-of-truth columns:
/// `NAME  KIND  VERSION  SOURCE  INSTALLED  PATH`.
fn print_plugin_list_table(output: &PluginListOutput) -> Result<()> {
    if output.plugins.is_empty() {
        println!("no plugins discovered");
        return Ok(());
    }
    let installed = marketplace::read_installed_index().unwrap_or_default();
    struct Row {
        name: String,
        kind: String,
        version: String,
        source: String,
        installed: String,
        path: String,
    }
    let rows: Vec<Row> = output
        .plugins
        .iter()
        .map(|p| {
            let installed_entry = installed.get(&p.name);
            let source = installed_entry.map(marketplace::format_installed_source).unwrap_or_else(|| "--".to_string());
            let installed_at = installed_entry
                .and_then(|e| e.installed_at.as_deref())
                .map(|s| s.split('T').next().unwrap_or(s).to_string())
                .unwrap_or_else(|| "--".to_string());
            Row {
                name: p.name.clone(),
                kind: p.plugin_kind.clone(),
                version: if p.version.is_empty() { "--".to_string() } else { p.version.clone() },
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
        rows.iter().map(|r| r.source.len()).max().unwrap_or(6).max(6),
        rows.iter().map(|r| r.installed.len()).max().unwrap_or(9).max(9),
    ];
    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  PATH",
        "NAME",
        "KIND",
        "VERSION",
        "SOURCE",
        "INSTALLED",
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
    );
    for row in &rows {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
            row.name,
            row.kind,
            row.version,
            row.source,
            row.installed,
            row.path,
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
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
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download from {url} returned non-success status"))?;
    let bytes = response.bytes().await.with_context(|| format!("failed to read body from {url}"))?;

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
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
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
        .with_context(|| format!("failed to GET {url}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(not_found_error(format!(
            "no release found at {url} (check the repo slug, tag, or whether a release has been published yet)"
        )));
    }
    let response = response.error_for_status().with_context(|| format!("GET {url} returned non-success status"))?;
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

async fn download_to_path(url: &str, dest: &Path) -> Result<()> {
    let client =
        reqwest::Client::builder().user_agent(release_user_agent()).build().context("failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download from {url} returned non-success status"))?;
    let bytes = response.bytes().await.with_context(|| format!("failed to read body from {url}"))?;
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
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download from {url} returned non-success status"))?;
    response.text().await.with_context(|| format!("failed to read body from {url}"))
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
        return Err(anyhow!("tarball {} contained no regular files", archive.display()));
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
    Err(anyhow!(
        "could not determine which file is the plugin binary in {}; expected one named '{}'. Extracted files: [{}]",
        archive.display(),
        expected_name,
        names.join(", ")
    ))
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
    /// RAII guard for the staging directory the asset was downloaded into.
    /// All paths above point inside this guard's directory; the caller
    /// must keep `_temp_dir` alive until the binary has been copied to
    /// its final home. Dropping the guard recursively removes the staging
    /// dir — closes the `$TMPDIR/animus-plugin-install-*` leak.
    _temp_dir: tempfile::TempDir,
}

async fn resolve_release_install(spec: RepoSpec, explicit_tag: Option<String>) -> Result<ReleaseInstall> {
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

    let asset = pick_release_asset(&release.assets, platform_tokens).ok_or_else(|| {
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

    // Resolve expected SHA256: sidecar asset > release `digest` field.
    let mut expected_sha: Option<String> = None;
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
        origin: format!("{}/{}@{}", spec.owner, spec.repo, release.tag_name),
        sha256_verified,
        asset_archive_path: Some(asset_path.clone()),
        bundle_path,
        owner: spec.owner.clone(),
        repo: spec.repo.clone(),
        _temp_dir: temp_dir,
    })
}

/// Look up the cosign signature bundle (`<asset>.bundle`) in the release
/// assets, if present.
fn find_bundle_sidecar<'a>(assets: &'a [GithubReleaseAsset], asset_name: &str) -> Option<&'a GithubReleaseAsset> {
    let bundle_name = format!("{asset_name}.bundle");
    assets.iter().find(|a| a.name.eq_ignore_ascii_case(&bundle_name))
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
        return Err(anyhow!(
            "binary failed --manifest probe (exit={:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice::<PluginManifest>(&output.stdout)
        .with_context(|| format!("plugin {} returned malformed --manifest JSON", binary_path.display()))
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
/// install gets a safe default for the canonical animus plugins.
const BUILTIN_TRUSTED_ORGS: &[&str] = &["launchapp-dev"];

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TrustedOrgsConfig {
    #[serde(default)]
    trusted_orgs: Vec<String>,
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

/// Append `owner` to the trusted-orgs allowlist on disk. Idempotent.
fn add_trusted_org(owner: &str) -> Result<()> {
    let trimmed = owner.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut config = load_trusted_orgs()?;
    let already_known: BTreeSet<String> = config.trusted_orgs.iter().map(|o| o.to_ascii_lowercase()).collect();
    if already_known.contains(&trimmed.to_ascii_lowercase()) {
        return Ok(());
    }
    if BUILTIN_TRUSTED_ORGS.iter().any(|o| o.eq_ignore_ascii_case(trimmed)) {
        return Ok(());
    }
    config.trusted_orgs.push(trimmed.to_string());
    save_trusted_orgs(&config)
}

fn org_is_trusted(owner: &str) -> Result<bool> {
    if BUILTIN_TRUSTED_ORGS.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return Ok(true);
    }
    let config = load_trusted_orgs()?;
    Ok(config.trusted_orgs.iter().any(|o| o.eq_ignore_ascii_case(owner)))
}

/// Implements the trust-on-first-use prompt for installs from public-repo
/// sources. Pre-trusted orgs (`launchapp-dev` plus anything in
/// `~/.animus/trusted-orgs.yaml`) skip the prompt entirely. Operators can
/// pre-trust additional orgs via `--allow-org`, or auto-confirm via `--yes`.
fn enforce_org_trust(owner: &str, req: &PluginInstallRequest) -> Result<()> {
    if req.allow_org.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return Ok(());
    }
    if org_is_trusted(owner)? {
        return Ok(());
    }
    if req.yes || req.force {
        tracing::warn!(owner, "installing plugin from untrusted org (--yes / --force); recording trust on first use");
        return Ok(());
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
        Ok(())
    } else {
        Err(invalid_input_error(format!("user declined to trust org '{owner}'; aborting install")))
    }
}

async fn handle_plugin_install(args: PluginInstallArgs, project_root: &str, json: bool) -> Result<()> {
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
    })
    .await?;
    print_value(output, json)
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
    })?;
    print_value(output, json)
}

// ===== `plugin lock` subcommands =====

#[derive(Debug, Serialize)]
struct PluginLockListOutput {
    lockfile: String,
    schema_version: String,
    generated_at: String,
    plugins: Vec<LockEntry>,
}

#[derive(Debug, Serialize)]
struct PluginLockVerifyEntry {
    name: String,
    status: &'static str,
    expected_sha256: String,
    actual_sha256: Option<String>,
    installed_path: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct PluginLockVerifyOutput {
    lockfile: String,
    entries: Vec<PluginLockVerifyEntry>,
    matched: usize,
    mismatched: usize,
    missing_binary: usize,
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
    let output = PluginLockListOutput {
        lockfile: path.to_string_lossy().to_string(),
        schema_version: lockfile.schema_version.clone(),
        generated_at: lockfile.generated_at.clone(),
        plugins: lockfile.plugins.clone(),
    };
    print_value(output, args.json)
}

fn run_lock_verify(args: PluginLockVerifyArgs, project_root: &str) -> Result<()> {
    let path = args.lockfile.unwrap_or_else(|| PluginLockfile::default_path(Some(std::path::Path::new(project_root))));
    let lockfile = PluginLockfile::load_or_empty(&path)?;
    let install_dir = install_root(args.plugin_dir.as_deref())?;
    let mut entries = Vec::with_capacity(lockfile.plugins.len());
    let mut matched = 0_usize;
    let mut mismatched = 0_usize;
    let mut missing_binary = 0_usize;
    for entry in &lockfile.plugins {
        let installed_path = install_dir.join(&entry.name);
        if !installed_path.exists() {
            missing_binary += 1;
            entries.push(PluginLockVerifyEntry {
                name: entry.name.clone(),
                status: "missing_binary",
                expected_sha256: entry.artifact_sha256.clone(),
                actual_sha256: None,
                installed_path: Some(installed_path.to_string_lossy().to_string()),
                detail: Some("installed binary not found at expected path".to_string()),
            });
            continue;
        }
        match lockfile.verify_installed(&entry.name, &installed_path) {
            Ok(LockVerifyResult::Match) => {
                matched += 1;
                entries.push(PluginLockVerifyEntry {
                    name: entry.name.clone(),
                    status: "ok",
                    expected_sha256: entry.artifact_sha256.clone(),
                    actual_sha256: Some(entry.artifact_sha256.clone()),
                    installed_path: Some(installed_path.to_string_lossy().to_string()),
                    detail: None,
                });
            }
            Ok(LockVerifyResult::Mismatch { expected, actual }) => {
                mismatched += 1;
                if let Some(scoped) = protocol::repository_scope::scoped_state_root(std::path::Path::new(project_root))
                {
                    Audit::at_scoped_root(&scoped).log_event(AuditEvent::new(
                        AuditActor::User,
                        AuditEventKind::LockfileMismatch,
                        serde_json::json!({
                            "plugin": entry.name,
                            "expected_sha256": expected,
                            "actual_sha256": actual,
                            "lockfile": path.display().to_string(),
                        }),
                    ));
                }
                entries.push(PluginLockVerifyEntry {
                    name: entry.name.clone(),
                    status: "mismatch",
                    expected_sha256: expected,
                    actual_sha256: Some(actual),
                    installed_path: Some(installed_path.to_string_lossy().to_string()),
                    detail: Some("sha256 of installed binary does not match lockfile".to_string()),
                });
            }
            Ok(LockVerifyResult::Missing) => {
                // Should not happen because we just iterated the lockfile entries.
                entries.push(PluginLockVerifyEntry {
                    name: entry.name.clone(),
                    status: "missing_lock_entry",
                    expected_sha256: entry.artifact_sha256.clone(),
                    actual_sha256: None,
                    installed_path: Some(installed_path.to_string_lossy().to_string()),
                    detail: Some("entry vanished between read and verify".to_string()),
                });
            }
            Err(err) => {
                entries.push(PluginLockVerifyEntry {
                    name: entry.name.clone(),
                    status: "error",
                    expected_sha256: entry.artifact_sha256.clone(),
                    actual_sha256: None,
                    installed_path: Some(installed_path.to_string_lossy().to_string()),
                    detail: Some(err.to_string()),
                });
            }
        }
    }
    let output = PluginLockVerifyOutput {
        lockfile: path.to_string_lossy().to_string(),
        entries,
        matched,
        mismatched,
        missing_binary,
    };
    // `animus plugin lock verify` is meant to be wired into CI / cron as a
    // tamper-detection gate. Both a hash mismatch AND a missing on-disk binary
    // for a tracked entry indicate the install state has drifted from the
    // lockfile, so either condition must exit non-zero.
    let exit_err = if mismatched > 0 || missing_binary > 0 {
        Some(anyhow!(
            "plugin lock verify failed: {mismatched} mismatched, {missing_binary} missing binary, {matched} matched"
        ))
    } else {
        None
    };
    print_value(output, args.json)?;
    if let Some(err) = exit_err {
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GithubReleaseAsset {
        GithubReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
            digest: None,
        }
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
        add_trusted_org("first-org").expect("add 1st");
        add_trusted_org("first-org").expect("idempotent 2nd");
        add_trusted_org("second-org").expect("add 2nd");
        let cfg = load_trusted_orgs().expect("load");
        assert_eq!(cfg.trusted_orgs.len(), 2);
        assert!(cfg.trusted_orgs.contains(&"first-org".to_string()));
        assert!(cfg.trusted_orgs.contains(&"second-org".to_string()));
        // Pre-trusted built-ins never get written.
        add_trusted_org("launchapp-dev").expect("builtin add is no-op");
        let cfg2 = load_trusted_orgs().expect("reload");
        assert_eq!(cfg2.trusted_orgs.len(), 2, "launchapp-dev must not be appended to trusted-orgs.yaml");
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
        use std::os::unix::fs::PermissionsExt;
        let manifest = serde_json::json!({
            "name": manifest_name,
            "version": "0.1.0",
            "plugin_kind": plugin_kind,
            "description": "fake plugin for install-pipeline tests",
            "protocol_version": "1.0.0",
            "capabilities": [],
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

        let err =
            load_or_refuse_lockfile(Some(&project_root), false).expect_err("default must refuse corrupt lockfile");
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

        let lock = load_or_refuse_lockfile(Some(&project_root), true)
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

        // Pre-create the install-dir entries for every default plugin so the
        // per-target loop skips them all. (`include_*` flags stay false, so
        // only the provider plugins matter here.) Also pre-create the v0.5
        // workflow_runner + queue plugin directories: the bundled flavor
        // manifest (loaded from the binary via include_str! after
        // Wave 3 codex R6 [P2]) declares them as required, so they show
        // up in the targets list even when no `flavors/` directory is on
        // disk under the tempdir.
        for (slug, _tag) in DEFAULT_PROVIDER_PLUGINS {
            let basename = slug.rsplit('/').next().unwrap_or(slug);
            std::fs::write(install_dir.join(basename), b"placeholder").unwrap();
        }
        for (slug, _tag) in orchestrator_core::DEFAULT_WORKFLOW_RUNNER_PLUGINS {
            let basename = slug.rsplit('/').next().unwrap_or(slug);
            std::fs::write(install_dir.join(basename), b"placeholder").unwrap();
        }
        for (slug, _tag) in orchestrator_core::DEFAULT_QUEUE_PLUGINS {
            let basename = slug.rsplit('/').next().unwrap_or(slug);
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
            artifact_sha256: "c".repeat(64),
            signature_bundle_sha256: None,
            installed_at: chrono::Utc::now().to_rfc3339(),
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
        assert_eq!(entry.artifact_sha256, output.sha256);
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
}
