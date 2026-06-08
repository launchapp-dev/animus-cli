use std::path::{Path, PathBuf};

use orchestrator_plugin_host::{PluginDiscovery, PluginRegistry};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;
use crate::services::operations::ops_plugin::{
    run_plugin_info, run_plugin_install, run_plugin_list, run_plugin_ping, run_plugin_uninstall, PluginInfoRequest,
    PluginInstallRequest, PluginListRequest, PluginPingRequest, PluginUninstallRequest,
};
use crate::services::runtime::canonicalize_lossy;

fn anyhow_to_mcp(err: anyhow::Error) -> McpError {
    let chain: Vec<String> = err.chain().map(|cause| cause.to_string()).collect();
    if chain.iter().any(|c| c.contains("not found") || c.contains("invalid_input") || c.contains("must")) {
        McpError::invalid_params(chain.join(": "), None)
    } else {
        McpError::internal_error(chain.join(": "), None)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct PluginListInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Also scan $PATH for animus-provider-* and animus-plugin-* binaries.
    #[serde(default)]
    include_system_path: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct PluginInfoInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Plugin name as discovered (matches manifest.name or filename).
    name: String,
    /// Also scan $PATH while resolving the plugin.
    #[serde(default)]
    include_system_path: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct PluginPingInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Plugin name to spawn and ping.
    name: String,
    /// Also scan $PATH while resolving the plugin.
    #[serde(default)]
    include_system_path: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct PluginCallInput {
    #[serde(default)]
    project_root: Option<String>,
    /// Plugin name as discovered (matches manifest.name or filename).
    name: String,
    /// JSON-RPC method to invoke on the plugin.
    method: String,
    /// Optional JSON params object passed verbatim to the plugin.
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub(super) struct PluginInstallInput {
    /// Override the resolved project root. When omitted, the server uses
    /// its configured default project root. The resolved value is forwarded
    /// to the install pipeline so the project-local `.animus/plugins.lock`
    /// (when present) participates in the install-time integrity check
    /// rather than silently falling through to `~/.animus/plugins.lock`.
    #[serde(default)]
    project_root: Option<String>,
    /// Public GitHub repo slug to install from (e.g.
    /// `launchapp-dev/animus-provider-claude`, or with a tag
    /// `launchapp-dev/animus-provider-claude@v0.1.0`). Mutually exclusive
    /// with `path` and `url`. When the public-repo install pipeline is not
    /// yet wired in, this call returns a clear error directing callers at
    /// `path` or `url`.
    #[serde(default)]
    source: Option<String>,
    /// Local filesystem path to the plugin binary to install.
    #[serde(default)]
    path: Option<String>,
    /// HTTPS URL to download the plugin binary from.
    #[serde(default)]
    url: Option<String>,
    /// Optional release tag override for `source` installs.
    #[serde(default)]
    tag: Option<String>,
    /// Optional logical plugin name. Defaults to the binary file name.
    #[serde(default)]
    name: Option<String>,
    /// Expected SHA256 hex digest. Required when `url` is set; optional for
    /// `path` and `source`. The install fails if the downloaded/copied
    /// binary's checksum does not match.
    #[serde(default)]
    sha256: Option<String>,
    /// Overwrite an existing installed plugin with the same name.
    #[serde(default)]
    force: Option<bool>,
    /// Skip running `--manifest` against the installed binary.
    #[serde(default)]
    skip_manifest_check: Option<bool>,
    /// Override the plugin install directory. Takes precedence over
    /// `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/`.
    #[serde(default)]
    plugin_dir: Option<String>,
    /// Refuse install when no cosign signature bundle is present or when
    /// verification fails. Defaults to false (verify-if-present).
    #[serde(default)]
    require_signature: Option<bool>,
    /// Skip cosign signature verification entirely.
    #[serde(default)]
    skip_signature: Option<bool>,
    /// Optional path to a trusted-signers YAML allowlist (overrides default
    /// `~/.animus/trusted-signers.yaml`).
    #[serde(default)]
    trusted_signers: Option<String>,
    /// Discard a corrupt or incompatible `.animus/plugins.lock` and start a
    /// fresh in-memory lockfile for this install. SECURITY: this drops the
    /// existing integrity history; only enable after auditing the lockfile
    /// damage was not the result of tampering. Mirrors the CLI's
    /// `--force-rewrite-lockfile` flag added in v0.4.14 (G2 fail-closed fix).
    /// Defaults to `false` — the install fails closed when the lockfile
    /// cannot be parsed.
    #[serde(default)]
    force_rewrite_lockfile: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct PluginUninstallInput {
    /// Override the resolved project root. When omitted, the server uses
    /// its configured default project root. The resolved value is forwarded
    /// to the uninstall pipeline so the project-local `.animus/plugins.lock`
    /// and audit log are updated (when present), matching the CLI surface.
    #[serde(default)]
    project_root: Option<String>,
    /// Logical plugin name to uninstall.
    name: String,
    /// Override the plugin install directory. Takes precedence over
    /// `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/`.
    #[serde(default)]
    plugin_dir: Option<String>,
}

impl AoMcpServer {
    fn project_root_or_default(&self, override_root: Option<String>) -> String {
        normalize_non_empty(override_root).unwrap_or_else(|| self.default_project_root.clone())
    }

    /// Build the cache key for the plugin registry. We canonicalize so two
    /// spellings of the same root (relative vs absolute, symlinked vs real)
    /// share a single registry. When canonicalization fails (e.g. path does
    /// not yet exist on disk) we fall back to the raw `PathBuf`, matching the
    /// rule used elsewhere in the CLI (see `canonicalize_lossy`).
    ///
    /// Sentinel: when the caller does not provide a project_root override,
    /// `project_root_or_default` resolves to the server's
    /// `default_project_root`, which is also canonicalized here. There is no
    /// separate "global" entry — every cache entry is keyed by a real path,
    /// even the default. This avoids the trap of two empty/sentinel paths
    /// colliding with a project root whose canonical form is the same.
    fn registry_cache_key(project_root: &str) -> PathBuf {
        PathBuf::from(canonicalize_lossy(project_root))
    }

    /// Resolve (or build) the per-project `PluginRegistry`. The returned
    /// `Arc<Mutex<PluginRegistry>>` is cached under the canonical project
    /// root so cross-project calls never see each other's discovered plugin
    /// sets.
    async fn registry_for(&self, project_root: &str) -> Result<PluginRegistryEntry, McpError> {
        let key = Self::registry_cache_key(project_root);
        let mut guard = self.plugin_registry.lock().await;
        if let Some(entry) = guard.get(&key) {
            return Ok(entry.clone());
        }
        let registry = PluginRegistry::discover(Path::new(project_root))
            .map_err(|err| McpError::internal_error(format!("plugin discovery failed: {err}"), None))?;
        let entry: PluginRegistryEntry = std::sync::Arc::new(tokio::sync::Mutex::new(registry));
        guard.insert(key, entry.clone());
        Ok(entry)
    }

    /// Drop all cached registries. Called after install/uninstall so the next
    /// MCP call rediscovers the freshly mutated plugin set across every
    /// project root the server has touched.
    async fn invalidate_plugin_registry_cache(&self) {
        let mut guard = self.plugin_registry.lock().await;
        guard.clear();
    }
}

#[tool_router(router = plugin_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.plugin.list",
        description = "List discovered Animus STDIO plugins (providers, subject backends, custom). Returns name, version, plugin_kind, description, protocol_version, capabilities, source, and path for each plugin found via plugins.yaml, .animus/plugins/, $ANIMUS_PLUGIN_DIR, or $ANIMUS_PLUGIN_PATH. The structured payload also includes `warnings` for plugins that were discovered but failed to load (manifest probe failed, malformed yaml, etc.).",
        input_schema = ao_schema_for_type::<PluginListInput>()
    )]
    async fn ao_plugin_list(&self, params: Parameters<PluginListInput>) -> Result<CallToolResult, McpError> {
        let PluginListInput { project_root, include_system_path } = params.0;
        let project_root = self.project_root_or_default(project_root);
        let output = run_plugin_list(PluginListRequest {
            project_root,
            include_system_path: include_system_path.unwrap_or(false),
        })
        .map_err(anyhow_to_mcp)?;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.plugin.list",
            "result": output,
        })))
    }

    #[tool(
        name = "animus.plugin.info",
        description = "Spawn the named plugin, complete the initialize handshake, then shut it down. Returns the plugin's full manifest (name, version, plugin_kind, description, capabilities, protocol_version, license, repository, homepage) plus the initialize response (plugin_info + capabilities advertised at runtime). Use this to inspect a plugin's surface before calling it.",
        input_schema = ao_schema_for_type::<PluginInfoInput>()
    )]
    async fn ao_plugin_info(&self, params: Parameters<PluginInfoInput>) -> Result<CallToolResult, McpError> {
        let PluginInfoInput { project_root, name, include_system_path } = params.0;
        let project_root = self.project_root_or_default(project_root);
        let output = run_plugin_info(PluginInfoRequest {
            project_root,
            name,
            include_system_path: include_system_path.unwrap_or(false),
        })
        .await
        .map_err(anyhow_to_mcp)?;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.plugin.info",
            "result": output,
        })))
    }

    #[tool(
        name = "animus.plugin.ping",
        description = "Health-check a plugin by spawning it, completing the initialize handshake, dispatching `$/ping`, then shutting it down. Returns `{ok: true, name, plugin_info}` on success. Use this to verify that an installed plugin can actually start and respond — `animus.plugin.list` only checks discovery, not liveness.",
        input_schema = ao_schema_for_type::<PluginPingInput>()
    )]
    async fn ao_plugin_ping(&self, params: Parameters<PluginPingInput>) -> Result<CallToolResult, McpError> {
        let PluginPingInput { project_root, name, include_system_path } = params.0;
        let project_root = self.project_root_or_default(project_root);
        let output = run_plugin_ping(PluginPingRequest {
            project_root,
            name,
            include_system_path: include_system_path.unwrap_or(false),
        })
        .await
        .map_err(anyhow_to_mcp)?;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.plugin.ping",
            "result": output,
        })))
    }

    #[tool(
        name = "animus.plugin.call",
        description = "Send a JSON-RPC request to a discovered plugin and return its response. The plugin is spawned (or reused via the cached PluginRegistry), handshaked, then invoked with the supplied method and optional params. Use this for direct one-off calls; for sustained dispatch use `animus.agent.run` (provider plugins) or the subject-routed tools (subject backends).",
        input_schema = ao_schema_for_type::<PluginCallInput>()
    )]
    async fn ao_plugin_call(&self, params: Parameters<PluginCallInput>) -> Result<CallToolResult, McpError> {
        let PluginCallInput { project_root, name, method, params: rpc_params } = params.0;
        let project_root = self.project_root_or_default(project_root);
        let trimmed_name = name.trim().to_string();
        let trimmed_method = method.trim().to_string();
        if trimmed_name.is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        if trimmed_method.is_empty() {
            return Err(McpError::invalid_params("method must not be empty", None));
        }

        let registry_entry = self.registry_for(&project_root).await?;
        let mut registry = registry_entry.lock().await;
        let host = registry
            .get_plugin(&trimmed_name)
            .await
            .map_err(|err| McpError::internal_error(format!("failed to load plugin '{trimmed_name}': {err}"), None))?;
        let result = host.request(trimmed_method.clone(), rpc_params).await.map_err(|err| {
            McpError::internal_error(format!("plugin call failed ({}): {}", err.code, err.message), None)
        })?;

        Ok(CallToolResult::structured(json!({
            "tool": "animus.plugin.call",
            "result": {
                "name": trimmed_name,
                "method": trimmed_method,
                "response": result,
            }
        })))
    }

    #[tool(
        name = "animus.plugin.install",
        description = "Install an Animus plugin binary. Exactly one of `source` (public GitHub repo slug like `owner/repo[@tag]`), `path` (local filesystem path), or `url` (https download) must be provided. When `url` is set, `sha256` is required for integrity verification. The plugin is copied into the install directory (`$ANIMUS_PLUGIN_DIR` or `~/.animus/plugins/`, overridable via `plugin_dir`), made executable, probed via `--manifest`, and registered in `~/.animus/plugins.yaml`. `project_root` is forwarded to the install pipeline so the project-local `.animus/plugins.lock` participates in the install-time integrity check (rather than falling through to `~/.animus/plugins.lock`). Set `force_rewrite_lockfile=true` to discard a corrupt lockfile (mirrors the CLI `--force-rewrite-lockfile` flag added in v0.4.14). Returns name, installed_path, sha256, manifest, plugins_yaml, and provenance fields (source_kind, origin, release_tag, asset_name, sha256_verified).",
        input_schema = ao_schema_for_type::<PluginInstallInput>()
    )]
    async fn ao_plugin_install(&self, params: Parameters<PluginInstallInput>) -> Result<CallToolResult, McpError> {
        let PluginInstallInput {
            project_root,
            source,
            path,
            url,
            tag,
            name,
            sha256,
            force,
            skip_manifest_check,
            plugin_dir,
            require_signature,
            skip_signature,
            trusted_signers,
            force_rewrite_lockfile,
        } = params.0;
        // Resolve to the server's default project root when the caller did
        // not override it. Forwarding the project root is what makes the
        // install-time fail-closed lockfile check honor a project-local
        // `.animus/plugins.lock` instead of silently falling through to
        // the global `~/.animus/plugins.lock`.
        let resolved_project_root = self.project_root_or_default(project_root);
        let output = run_plugin_install(PluginInstallRequest {
            source,
            path,
            url,
            tag,
            name,
            sha256,
            force: force.unwrap_or(false),
            skip_manifest_check: skip_manifest_check.unwrap_or(false),
            plugin_dir,
            signature_policy: None,
            trust_key: None,
            require_signature: require_signature.unwrap_or(false),
            skip_signature: skip_signature.unwrap_or(false),
            trusted_signers: trusted_signers.map(std::path::PathBuf::from),
            allow_shadow_builtin: false,
            allow_org: Vec::new(),
            // MCP runs non-interactively; let unknown-org installs proceed
            // silently rather than blocking on a TTY prompt. The TOFU record
            // still lands in trusted-orgs.yaml after a successful install.
            yes: true,
            project_root: Some(resolved_project_root),
            // Authenticated escape hatch for a corrupt lockfile. Defaults to
            // `false` (fail-closed) so that the G2 v0.4.14 fix still holds for
            // MCP callers who don't opt in.
            force_rewrite_lockfile: force_rewrite_lockfile.unwrap_or(false),
            as_kind: None,
        })
        .await
        .map_err(anyhow_to_mcp)?;
        // Drop every cached plugin registry so subsequent calls (against
        // either this project root or any other) re-discover the freshly
        // installed binary.
        self.invalidate_plugin_registry_cache().await;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.plugin.install",
            "result": output,
        })))
    }

    #[tool(
        name = "animus.plugin.uninstall",
        description = "Remove an installed plugin. Deletes the binary from the install directory (`$ANIMUS_PLUGIN_DIR` or `~/.animus/plugins/`, overridable via `plugin_dir`) and drops the entry from `~/.animus/plugins.yaml`. `project_root` is forwarded to the uninstall pipeline so the project-local `.animus/plugins.lock` and audit log are updated (when present). Returns `{name, removed_path, plugins_yaml}`. Fails with `not_found` when the plugin is not installed.",
        input_schema = ao_schema_for_type::<PluginUninstallInput>()
    )]
    async fn ao_plugin_uninstall(&self, params: Parameters<PluginUninstallInput>) -> Result<CallToolResult, McpError> {
        let PluginUninstallInput { project_root, name, plugin_dir } = params.0;
        let resolved_project_root = self.project_root_or_default(project_root);
        let output = run_plugin_uninstall(PluginUninstallRequest {
            name,
            plugin_dir,
            project_root: Some(resolved_project_root),
        })
        .map_err(anyhow_to_mcp)?;
        // Drop every cached plugin registry so subsequent calls re-discover
        // the current set of installed plugins.
        self.invalidate_plugin_registry_cache().await;
        Ok(CallToolResult::structured(json!({
            "tool": "animus.plugin.uninstall",
            "result": output,
        })))
    }
}

// Discovery helpers re-export for cfg(test) so tests can drive discovery
// without spinning up the full MCP server.
#[allow(dead_code)]
pub(super) fn discover_for_tests(
    project_root: &str,
) -> Result<Vec<orchestrator_plugin_host::DiscoveredPlugin>, anyhow::Error> {
    PluginDiscovery::new().with_project_root(Path::new(project_root)).discover()
}

#[cfg(test)]
// `isolated_plugin_dirs()` returns a guard that pins process-wide env vars
// (`ANIMUS_PLUGIN_DIR`, `ANIMUS_CONFIG_DIR`, `ANIMUS_PLUGIN_PATH`) for the
// duration of one test. The Mutex held across `.await` is intentional: the
// contended resource is the env, and an async-aware Mutex wouldn't change
// that. The `plugin_registry` field on `AoMcpServer` is a `tokio::Mutex`
// (legitimately async), and that one shows up in the same lint because
// clippy can't distinguish the two; the std Mutex is the one we're
// suppressing.
#[allow(clippy::await_holding_lock)]
mod plugin_tool_tests {
    use super::super::new_ao_mcp_server;
    use super::*;
    use protocol::test_utils::EnvVarGuard;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn structured(result: &rmcp::model::CallToolResult) -> Value {
        result.structured_content.clone().expect("expected structured_content on tool result")
    }

    fn data(result: &rmcp::model::CallToolResult) -> Value {
        let payload = structured(result);
        payload.get("result").cloned().expect("structured result should include `result`")
    }

    /// Per-test mutex so concurrent plugin-tool tests don't race on the
    /// process-global ANIMUS_PLUGIN_DIR / ANIMUS_CONFIG_DIR env vars.
    static PLUGIN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Isolate the plugin install dir and plugins.yaml location via
    /// `ANIMUS_PLUGIN_DIR` + `ANIMUS_CONFIG_DIR` (which both
    /// `plugin_install_dir` and `plugins_registry_path` honor) so tests
    /// don't touch the real `~/.animus/plugins/`. Holds [`PLUGIN_TEST_LOCK`]
    /// for the lifetime of the returned guard so the env vars are stable
    /// across the test body. HOME is intentionally NOT touched here — other
    /// tests in this binary read HOME for unrelated paths and a concurrent
    /// mutation would race them.
    fn isolated_plugin_dirs() -> (TempDir, std::sync::MutexGuard<'static, ()>, Vec<EnvVarGuard>) {
        let lock = PLUGIN_TEST_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let home = TempDir::new().expect("plugin dirs tempdir");
        let plugins_dir = home.path().join("plugins");
        std::fs::create_dir_all(&plugins_dir).expect("create plugins subdir");
        // Write an empty plugins.yaml at the canonical location so the legacy
        // fallback (~/.config/animus/plugins.yaml) does NOT win. Without this,
        // the contributor's real ~/.config/animus/plugins.yaml would leak
        // entries into discovery for the test.
        let registry_path = home.path().join("plugins.yaml");
        std::fs::write(&registry_path, "plugins: {}\nproviders: {}\n").expect("write empty plugins.yaml");
        let guards = vec![
            EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(plugins_dir.to_str().expect("plugin dir utf-8"))),
            EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(home.path().to_str().expect("config dir utf-8"))),
            EnvVarGuard::set("ANIMUS_PLUGIN_PATH", None),
        ];
        (home, lock, guards)
    }

    fn project_root_for(tmp: &TempDir) -> String {
        tmp.path().to_string_lossy().to_string()
    }

    /// Write a fake plugin binary that responds to `--manifest` with a minimal
    /// PluginManifest JSON document. The script is a portable POSIX shell.
    fn write_fake_plugin_binary(path: &std::path::Path, name: &str, kind: &str) {
        let body = format!(
            r#"#!/usr/bin/env sh
if [ "$1" = "--manifest" ]; then
  cat <<'JSON'
{{"name":"{name}","version":"0.0.1-test","plugin_kind":"{kind}","description":"fixture plugin","protocol_version":"1","capabilities":[],"license":null,"repository":null,"homepage":null}}
JSON
  exit 0
fi
exit 0
"#,
            name = name,
            kind = kind,
        );
        fs::write(path, body).expect("write fake plugin script");
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod fake plugin");
    }

    #[tokio::test]
    async fn plugin_router_registers_all_six_tools() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));
        let names: Vec<String> = server.tool_router.list_all().into_iter().map(|tool| tool.name.to_string()).collect();
        for required in [
            "animus.plugin.list",
            "animus.plugin.info",
            "animus.plugin.install",
            "animus.plugin.uninstall",
            "animus.plugin.ping",
            "animus.plugin.call",
        ] {
            assert!(names.contains(&required.to_string()), "router missing {required}; have: {names:?}");
            assert!(server.tool_router.has_route(required));
        }
    }

    #[tokio::test]
    async fn plugin_list_returns_empty_for_isolated_project() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let result = server
            .ao_plugin_list(Parameters(PluginListInput { project_root: None, include_system_path: Some(false) }))
            .await
            .expect("list ok");
        let payload = data(&result);
        let plugins = payload.pointer("/plugins").and_then(Value::as_array).expect("plugins array");
        assert!(
            plugins.is_empty(),
            "fresh isolated project should have zero plugins, got: {}",
            serde_json::to_string_pretty(plugins).unwrap_or_default()
        );
        // warnings field should be present but empty (or omitted by skip_serializing_if).
        let warnings = payload.pointer("/warnings").and_then(Value::as_array);
        if let Some(arr) = warnings {
            assert!(arr.is_empty(), "fresh project should have no warnings: {arr:?}");
        }
    }

    #[tokio::test]
    async fn plugin_install_uninstall_round_trip() {
        let (home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        // Stage a fake plugin binary outside the install dir.
        let staging = TempDir::new().expect("staging dir");
        let stage_path = staging.path().join("animus-plugin-mcp-fixture");
        write_fake_plugin_binary(&stage_path, "animus-plugin-mcp-fixture", "subject_backend");

        // Install via MCP.
        let install_result = server
            .ao_plugin_install(Parameters(PluginInstallInput {
                path: Some(stage_path.to_string_lossy().to_string()),
                force: Some(false),
                skip_manifest_check: Some(false),
                ..Default::default()
            }))
            .await
            .expect("install should succeed");
        let installed = data(&install_result);
        assert_eq!(
            installed.get("name").and_then(Value::as_str),
            Some("animus-plugin-mcp-fixture"),
            "installed payload should echo plugin name: {installed}"
        );
        let installed_path = installed.get("installed_path").and_then(Value::as_str).expect("installed_path string");
        assert!(
            installed_path.starts_with(home.path().to_str().expect("plugin dir utf-8")),
            "installed_path should land under isolated plugin dir: {installed_path}"
        );
        assert!(std::path::Path::new(installed_path).exists(), "installed binary should exist on disk");

        // Uninstall via MCP.
        let uninstall_result = server
            .ao_plugin_uninstall(Parameters(PluginUninstallInput {
                project_root: None,
                name: "animus-plugin-mcp-fixture".to_string(),
                plugin_dir: None,
            }))
            .await
            .expect("uninstall should succeed");
        let uninstalled = data(&uninstall_result);
        assert_eq!(uninstalled.get("name").and_then(Value::as_str), Some("animus-plugin-mcp-fixture"));
        let removed_path = uninstalled.get("removed_path").and_then(Value::as_str).expect("removed_path");
        assert_eq!(removed_path, installed_path, "removed_path should match installed_path");
        assert!(!std::path::Path::new(installed_path).exists(), "binary should be removed from disk");
    }

    #[tokio::test]
    async fn plugin_install_rejects_missing_source_and_path() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_plugin_install(Parameters(PluginInstallInput::default()))
            .await
            .expect_err("missing source/path/url should be rejected");
        let msg = err.message.to_string();
        assert!(
            msg.contains("source") && msg.contains("path") && msg.contains("url"),
            "error should mention all three source options: {msg}"
        );
    }

    #[tokio::test]
    async fn plugin_install_rejects_url_without_sha256() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_plugin_install(Parameters(PluginInstallInput {
                url: Some("https://example.invalid/plugin".to_string()),
                ..Default::default()
            }))
            .await
            .expect_err("url without sha256 should be rejected");
        let msg = err.message.to_string();
        assert!(msg.contains("sha256"), "error should mention sha256: {msg}");
    }

    #[tokio::test]
    async fn plugin_uninstall_unknown_plugin_returns_error() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_plugin_uninstall(Parameters(PluginUninstallInput {
                project_root: None,
                name: "animus-plugin-does-not-exist".to_string(),
                plugin_dir: None,
            }))
            .await
            .expect_err("missing plugin should error");
        let msg = err.message.to_string();
        assert!(msg.contains("not installed") || msg.contains("not found"), "error should be descriptive: {msg}");
    }

    #[tokio::test]
    async fn plugin_info_unknown_plugin_returns_error() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let err = server
            .ao_plugin_info(Parameters(PluginInfoInput {
                project_root: None,
                name: "animus-plugin-does-not-exist".to_string(),
                include_system_path: Some(false),
            }))
            .await
            .expect_err("missing plugin should error");
        let msg = err.message.to_string();
        assert!(msg.contains("not found") || msg.contains("plugin"), "error should mention plugin: {msg}");
    }

    /// Drop a fake plugin binary at `<project_root>/.animus/plugins/<name>` so
    /// project-local discovery picks it up. Returns the binary path so callers
    /// can assert against it.
    fn stage_project_local_plugin(project: &std::path::Path, name: &str, kind: &str) -> std::path::PathBuf {
        let plugins_dir = project.join(".animus").join("plugins");
        fs::create_dir_all(&plugins_dir).expect("create project plugins dir");
        let binary = plugins_dir.join(name);
        write_fake_plugin_binary(&binary, name, kind);
        binary
    }

    /// H1 regression: the per-project registry cache must NOT bleed across
    /// project roots. Two MCP calls against different project_roots in
    /// sequence each get their own registry entry, populated from that
    /// project's `.animus/plugins/` directory.
    #[tokio::test]
    async fn plugin_registry_cache_is_keyed_by_project_root() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();

        let project_a = TempDir::new().expect("project a tempdir");
        let project_b = TempDir::new().expect("project b tempdir");
        // Each project gets a distinctly-named local plugin so we can assert
        // discovery hit the right tree (and didn't leak the other project's
        // plugin set through the cache).
        stage_project_local_plugin(project_a.path(), "animus-plugin-h1-alpha", "subject_backend");
        stage_project_local_plugin(project_b.path(), "animus-plugin-h1-beta", "subject_backend");

        // The server's "default" project_root is irrelevant here — every call
        // overrides project_root explicitly.
        let server = new_ao_mcp_server(&project_root_for(&project_a));

        let root_a = project_root_for(&project_a);
        let root_b = project_root_for(&project_b);

        // Call against project A. Should see ONLY the alpha plugin.
        let result_a = server
            .ao_plugin_list(Parameters(PluginListInput {
                project_root: Some(root_a.clone()),
                include_system_path: Some(false),
            }))
            .await
            .expect("list against project a should succeed");
        let plugins_a: Vec<String> = data(&result_a)
            .pointer("/plugins")
            .and_then(Value::as_array)
            .expect("plugins array for project a")
            .iter()
            .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert!(plugins_a.iter().any(|n| n == "animus-plugin-h1-alpha"), "project a should see alpha: {plugins_a:?}");
        assert!(!plugins_a.iter().any(|n| n == "animus-plugin-h1-beta"), "project a must NOT see beta: {plugins_a:?}");

        // Warm the cache for project A via registry_for so we can assert the
        // cache shape directly.
        let entry_a_first = server.registry_for(&root_a).await.expect("registry for a");

        // Now call against project B. Should see ONLY the beta plugin.
        let result_b = server
            .ao_plugin_list(Parameters(PluginListInput {
                project_root: Some(root_b.clone()),
                include_system_path: Some(false),
            }))
            .await
            .expect("list against project b should succeed");
        let plugins_b: Vec<String> = data(&result_b)
            .pointer("/plugins")
            .and_then(Value::as_array)
            .expect("plugins array for project b")
            .iter()
            .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert!(plugins_b.iter().any(|n| n == "animus-plugin-h1-beta"), "project b should see beta: {plugins_b:?}");
        assert!(
            !plugins_b.iter().any(|n| n == "animus-plugin-h1-alpha"),
            "project b must NOT see alpha: {plugins_b:?}"
        );

        // Cache must contain two distinct entries keyed by canonical project
        // roots, and the entry for A must be the same Arc on re-lookup
        // (proof of cache hit, not rebuild).
        let entry_a_second = server.registry_for(&root_a).await.expect("registry for a again");
        assert!(
            std::sync::Arc::ptr_eq(&entry_a_first, &entry_a_second),
            "second registry_for against project a should return the cached Arc"
        );
        let entry_b = server.registry_for(&root_b).await.expect("registry for b");
        assert!(!std::sync::Arc::ptr_eq(&entry_a_first, &entry_b), "projects a and b must have distinct registry Arcs");

        let cache = server.plugin_registry.lock().await;
        let key_a = AoMcpServer::registry_cache_key(&root_a);
        let key_b = AoMcpServer::registry_cache_key(&root_b);
        assert!(cache.contains_key(&key_a), "cache should contain key for project a");
        assert!(cache.contains_key(&key_b), "cache should contain key for project b");
        assert_ne!(key_a, key_b, "cache keys for distinct projects must differ");
    }

    /// H2 regression: MCP-driven install must forward the resolved
    /// `project_root` so the install-time lockfile lands at
    /// `<project_root>/.animus/plugins.lock`, NOT at the global
    /// `~/.animus/plugins.lock`. v0.4.14 made the lockfile check fail-closed;
    /// this asserts MCP callers get the project-scoped path.
    #[tokio::test]
    async fn plugin_install_writes_project_local_lockfile() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        // PluginLockfile::default_path() prefers the project-local lockfile
        // when either the lockfile already exists OR the `.animus/` directory
        // exists. Create the directory up-front so the project-local path
        // wins — this matches how real Animus projects work (init creates
        // the dir).
        fs::create_dir_all(project.path().join(".animus")).expect("create .animus dir");
        // The server's default project_root is irrelevant — install passes
        // project_root explicitly.
        let server = new_ao_mcp_server(&project_root_for(&project));

        let staging = TempDir::new().expect("staging dir");
        let stage_path = staging.path().join("animus-plugin-h2-fixture");
        write_fake_plugin_binary(&stage_path, "animus-plugin-h2-fixture", "subject_backend");

        let project_root_str = project_root_for(&project);
        let install_result = server
            .ao_plugin_install(Parameters(PluginInstallInput {
                project_root: Some(project_root_str.clone()),
                path: Some(stage_path.to_string_lossy().to_string()),
                ..Default::default()
            }))
            .await
            .expect("install should succeed");
        let installed = data(&install_result);
        assert_eq!(
            installed.get("name").and_then(Value::as_str),
            Some("animus-plugin-h2-fixture"),
            "install payload should echo plugin name: {installed}"
        );

        // The project-local lockfile should now exist at
        // `<project_root>/.animus/plugins.lock`. This proves the MCP install
        // forwarded project_root into the install pipeline (without the
        // forward, install would fall through to `~/.animus/plugins.lock`).
        let project_lock = project.path().join(".animus").join("plugins.lock");
        assert!(
            project_lock.exists(),
            "expected project-local lockfile at {}; install did not forward project_root",
            project_lock.display(),
        );
    }

    /// H2 regression: the `force_rewrite_lockfile` MCP input field is wired
    /// into the install pipeline. We don't actually corrupt the lockfile
    /// here — we just assert the field round-trips and the install still
    /// succeeds with the flag enabled, matching the CLI surface.
    #[tokio::test]
    async fn plugin_install_accepts_force_rewrite_lockfile_flag() {
        let (_home, _lock, _guards) = isolated_plugin_dirs();
        let project = TempDir::new().expect("project tempdir");
        let server = new_ao_mcp_server(&project_root_for(&project));

        let staging = TempDir::new().expect("staging dir");
        let stage_path = staging.path().join("animus-plugin-h2-force");
        write_fake_plugin_binary(&stage_path, "animus-plugin-h2-force", "subject_backend");

        let install_result = server
            .ao_plugin_install(Parameters(PluginInstallInput {
                project_root: Some(project_root_for(&project)),
                path: Some(stage_path.to_string_lossy().to_string()),
                force_rewrite_lockfile: Some(true),
                ..Default::default()
            }))
            .await
            .expect("install with force_rewrite_lockfile should succeed");
        let installed = data(&install_result);
        assert_eq!(installed.get("name").and_then(Value::as_str), Some("animus-plugin-h2-force"));
    }
}
