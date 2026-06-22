//! CLI-side `PluginRouting` adapter — bridges the daemon's control
//! surface back to the same `run_plugin_*` helpers the CLI uses for its
//! in-process code path.
//!
//! Build one of these at daemon startup via [`build_plugin_routing`] and
//! pass the resulting `Arc<dyn PluginRouting>` into
//! [`orchestrator_daemon_runtime::control::InProcessSurfaceBuilder::plugin_routing`].
//!
//! ## Shape conversion
//!
//! The control protocol wire types (e.g.
//! [`animus_control_protocol::types::PluginInfo`]) carry a small subset
//! of the CLI's internal output structs (`DiscoveredPluginRow`,
//! `PluginInfoOutput`). The adapter does the lossy projection in one
//! place so the CLI's print pipelines and the wire pipeline stay in
//! lockstep.
//!
//! ## Error mapping
//!
//! The underlying `anyhow::Error` from `run_plugin_*` carries no machine-
//! readable code. We surface failures as [`ControlError::Internal`] with
//! the original message preserved.

use std::path::PathBuf;
use std::sync::Arc;

use animus_control_protocol::{
    types::{
        PluginBrowseRequest as WireBrowseRequest, PluginCallRequest as WireCallRequest,
        PluginCallResponse as WireCallResponse, PluginInfo as WirePluginInfo, PluginInfoRequest as WireInfoRequest,
        PluginInstallRequest as WireInstallRequest, PluginInstallResponse as WireInstallResponse,
        PluginListRequest as WireListRequest, PluginListResponse as WireListResponse,
        PluginPingRequest as WirePingRequest, PluginPingResponse as WirePingResponse,
        PluginRegistryEntry as WireRegistryEntry, PluginSearchRequest as WireSearchRequest,
        PluginSearchResponse as WireSearchResponse, PluginUninstallRequest as WireUninstallRequest,
        PluginUpdateRequest as WireUpdateRequest, PluginUpdateResponse as WireUpdateResponse,
        PluginWarning as WirePluginWarning, Unit,
    },
    ControlError,
};
use async_trait::async_trait;
use orchestrator_daemon_runtime::control::PluginRouting;

use super::{
    run_plugin_browse, run_plugin_call, run_plugin_info, run_plugin_install, run_plugin_list, run_plugin_ping,
    run_plugin_search, run_plugin_update, PluginBrowseRequest, PluginCallRequest, PluginInfoRequest,
    PluginInstallRequest, PluginListRequest, PluginPingRequest, PluginSearchRequest, PluginUpdateRequest,
};

/// Build a [`PluginRouting`] implementation bound to `project_root`.
///
/// `project_root` is captured once at daemon startup and reused for
/// every routed call. The returned handle is `Clone` + `Send + Sync`
/// via the `Arc<dyn>` wrapper.
pub fn build_plugin_routing(project_root: PathBuf) -> Arc<dyn PluginRouting> {
    Arc::new(PluginRoutingImpl { project_root })
}

/// Adapter that translates the control protocol wire shapes into the
/// CLI's existing `run_plugin_*` requests and back.
struct PluginRoutingImpl {
    project_root: PathBuf,
}

impl PluginRoutingImpl {
    fn project_root_str(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }
}

fn internal(err: anyhow::Error) -> ControlError {
    ControlError::Internal(format!("{err:#}"))
}

#[async_trait]
impl PluginRouting for PluginRoutingImpl {
    async fn plugin_list(&self, request: WireListRequest) -> Result<WireListResponse, ControlError> {
        let output =
            run_plugin_list(PluginListRequest { project_root: self.project_root_str(), include_system_path: false })
                .map_err(internal)?;
        let plugins: Vec<WirePluginInfo> = output
            .plugins
            .into_iter()
            .filter(|p| match &request.kind {
                Some(kind) => p.plugin_kind.eq_ignore_ascii_case(kind),
                None => true,
            })
            .map(|p| WirePluginInfo {
                name: p.name,
                version: p.version,
                kind: p.plugin_kind,
                source: Some(p.source.to_string()),
                signature_verified: false,
                description: if p.description.is_empty() { None } else { Some(p.description) },
                binary_path: Some(PathBuf::from(p.path)),
            })
            .collect();
        let warnings: Vec<WirePluginWarning> = if request.include_warnings {
            output
                .warnings
                .into_iter()
                .map(|w| WirePluginWarning { plugin: w.name, message: format!("{}: {}", w.path, w.reason) })
                .collect()
        } else {
            Vec::new()
        };
        Ok(WireListResponse { plugins, warnings })
    }

    async fn plugin_info(&self, request: WireInfoRequest) -> Result<WirePluginInfo, ControlError> {
        let output = run_plugin_info(PluginInfoRequest {
            project_root: self.project_root_str(),
            name: request.name,
            include_system_path: false,
        })
        .await
        .map_err(internal)?;
        Ok(WirePluginInfo {
            name: output.name,
            version: output.manifest.version,
            kind: output.manifest.plugin_kind,
            source: Some(output.source.to_string()),
            signature_verified: false,
            description: if output.manifest.description.is_empty() { None } else { Some(output.manifest.description) },
            binary_path: Some(PathBuf::from(output.path)),
        })
    }

    async fn plugin_install(&self, request: WireInstallRequest) -> Result<WireInstallResponse, ControlError> {
        // The wire request only carries `source`, `version`, `yes`,
        // `allow_unsigned`. Map to the richer in-tree PluginInstallRequest
        // best-effort; CLI install-via-control is currently routed via
        // the local path, so this is primarily for MCP/WebAPI parity
        // landing in C7/C8.
        let install_req = PluginInstallRequest {
            source: Some(request.source.clone()),
            path: None,
            url: None,
            tag: request.version,
            name: None,
            sha256: None,
            force: false,
            skip_manifest_check: false,
            plugin_dir: None,
            signature_policy: None,
            trust_key: None,
            require_signature: !request.allow_unsigned,
            skip_signature: request.allow_unsigned,
            trusted_signers: None,
            allow_shadow_builtin: false,
            allow_org: Vec::new(),
            yes: request.yes,
            project_root: Some(self.project_root_str()),
            // Control-plane installs (MCP / WebAPI parity surface) default to
            // fail-closed when the project lockfile is corrupt. Operators
            // must inspect via CLI and re-run with `--force-rewrite-lockfile`.
            force_rewrite_lockfile: false,
            as_kind: None,
            project: false,
            expected_archive_sha256: None,
            locked_secondary_archive_shas: std::collections::BTreeMap::new(),
        };
        let output = run_plugin_install(install_req).await.map_err(internal)?;
        let plugin_kind = output.manifest.as_ref().map(|m| m.plugin_kind.clone()).unwrap_or_default();
        let plugin_version = output.manifest.as_ref().map(|m| m.version.clone()).unwrap_or_default();
        let description = output.manifest.as_ref().and_then(|m| {
            if m.description.is_empty() {
                None
            } else {
                Some(m.description.clone())
            }
        });
        Ok(WireInstallResponse {
            plugin: WirePluginInfo {
                name: output.name,
                version: plugin_version,
                kind: plugin_kind,
                source: output.origin.clone(),
                signature_verified: matches!(output.signature_status.as_str(), "verified"),
                description,
                binary_path: Some(PathBuf::from(output.installed_path)),
            },
            steps: vec![format!("sha256={}", output.sha256), format!("signature_status={}", output.signature_status)],
        })
    }

    async fn plugin_uninstall(&self, request: WireUninstallRequest) -> Result<Unit, ControlError> {
        super::run_plugin_uninstall(super::PluginUninstallRequest {
            name: request.name,
            plugin_dir: None,
            project_root: Some(self.project_root_str()),
            project: false,
        })
        .map_err(internal)?;
        Ok(Unit::default())
    }

    async fn plugin_ping(&self, request: WirePingRequest) -> Result<WirePingResponse, ControlError> {
        let started = std::time::Instant::now();
        match run_plugin_ping(PluginPingRequest {
            project_root: self.project_root_str(),
            name: request.name,
            include_system_path: false,
        })
        .await
        {
            Ok(output) => Ok(WirePingResponse {
                ok: output.ok,
                latency_ms: Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
                error: None,
            }),
            Err(err) => Ok(WirePingResponse {
                ok: false,
                latency_ms: Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
                error: Some(format!("{err:#}")),
            }),
        }
    }

    async fn plugin_call(&self, request: WireCallRequest) -> Result<WireCallResponse, ControlError> {
        let params = if request.params.is_null() { None } else { Some(request.params) };
        let output = run_plugin_call(PluginCallRequest {
            project_root: self.project_root_str(),
            name: request.name,
            method: request.method,
            params,
            include_system_path: false,
        })
        .await
        .map_err(internal)?;
        Ok(WireCallResponse(output.result))
    }

    async fn plugin_search(&self, request: WireSearchRequest) -> Result<WireSearchResponse, ControlError> {
        let tags: Vec<String> = request.tag.into_iter().collect();
        let req = PluginSearchRequest {
            query: if request.query.is_empty() { None } else { Some(request.query) },
            kind: request.kind,
            tag: tags,
            org: None,
            stability: None,
            registry_url: String::new(),
            no_cache: false,
            offline: false,
        };
        let output = run_plugin_search(req).await.map_err(internal)?;
        let entries: Vec<WireRegistryEntry> = output
            .results
            .into_iter()
            .map(|row| WireRegistryEntry {
                id: row.name.clone(),
                name: row.name,
                version: row.latest_tag.clone().unwrap_or_default(),
                kind: row.kind,
                description: if row.description.is_empty() { None } else { Some(row.description) },
                url: Some(row.repo),
                tags: row.tags,
                installed: false,
            })
            .collect();
        Ok(WireSearchResponse { entries })
    }

    async fn plugin_browse(&self, request: WireBrowseRequest) -> Result<WireSearchResponse, ControlError> {
        let req = PluginBrowseRequest {
            kind: request.kind,
            installed: request.installed,
            available: request.available,
            registry_url: String::new(),
            no_cache: false,
            offline: false,
        };
        let output = run_plugin_browse(req).await.map_err(internal)?;
        let mut entries: Vec<WireRegistryEntry> = Vec::new();
        for (_group, rows) in output.groups {
            for row in rows {
                entries.push(WireRegistryEntry {
                    id: row.name.clone(),
                    name: row.name,
                    version: row.latest_tag.clone().unwrap_or_default(),
                    kind: row.kind,
                    description: if row.description.is_empty() { None } else { Some(row.description) },
                    url: Some(row.repo),
                    tags: Vec::new(),
                    installed: row.installed,
                });
            }
        }
        Ok(WireSearchResponse { entries })
    }

    async fn plugin_update(&self, request: WireUpdateRequest) -> Result<WireUpdateResponse, ControlError> {
        use super::marketplace::PluginUpdateSelector;
        let selector = match request.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => PluginUpdateSelector::Name(name.to_string()),
            None => PluginUpdateSelector::All,
        };
        let req = PluginUpdateRequest {
            selector,
            tag_override: request.tag,
            check: request.dry_run,
            force: false,
            // Pin lockfile + audit resolution to the daemon's project root so
            // a control-routed update touches `.animus/plugins.lock` instead
            // of silently falling back to `~/.animus/plugins.lock`.
            project_root: Some(self.project_root_str()),
            project: false,
        };
        let output = run_plugin_update(req).await.map_err(internal)?;
        let updates = output
            .results
            .into_iter()
            .map(|row| animus_control_protocol::types::PluginUpdateEntry {
                name: row.name,
                from_version: row.installed_tag.unwrap_or_default(),
                to_version: row.recommended_tag.unwrap_or_default(),
                applied: row.action == "update",
            })
            .collect();
        Ok(WireUpdateResponse { updates })
    }
}
