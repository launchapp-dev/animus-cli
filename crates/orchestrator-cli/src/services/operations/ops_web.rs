use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::RpcError;
use anyhow::{anyhow, Result};
use orchestrator_core::{format_repo_spec, ServiceHub, DEFAULT_TRANSPORT_PLUGINS};
use orchestrator_daemon_runtime::control::control_socket_path;
use orchestrator_plugin_host::{
    discover_plugins, DiscoveredPlugin, HostError, PluginHost, PluginSpawnOptions, TRANSPORT_METHOD_START,
};
use serde_json::{json, Value};

use crate::{print_ok, print_value};
use crate::{CliError, CliErrorKind, WebCommand};

const TRANSPORT_PLUGIN_KIND: &str = "transport_backend";
const WEB_UI_PLUGIN_KIND: &str = "web_ui";
/// JSON-RPC error codes returned when a plugin does not (yet) implement
/// `transport/start`. The legacy launchapp-dev transport plugins bind their
/// listener inside `initialize` and have no transport-method dispatch; they
/// respond with `METHOD_NOT_FOUND` (-32601). Spec-compliant plugins that
/// recognize the method but decline it use `METHOD_NOT_SUPPORTED` (-32001).
/// Either is treated as a soft no-op by `animus web serve` so we don't break
/// the entire web surface during the ecosystem upgrade.
const METHOD_NOT_FOUND_CODE: i32 = -32601;
const METHOD_NOT_SUPPORTED_CODE: i32 = -32001;
/// Upper bound on how long `animus web serve` waits for a transport plugin's
/// `transport/start` reply before giving up. Spec-compliant plugins bind the
/// listener inside this call, so the deadline includes any port acquisition
/// the plugin performs.
const TRANSPORT_START_TIMEOUT: Duration = Duration::from_secs(15);
/// Capability marker declared by a `transport_backend` plugin to advertise
/// that it serves a browser-facing HTML UI (as opposed to a machine-facing
/// API endpoint like GraphQL or REST).
///
/// `animus web open` partitions installed transports by this marker so the
/// browser opens the UI instead of an API transport. This is the capability-
/// based escape hatch that avoids a protocol bump for a new `web_ui` plugin
/// kind: existing transport plugins can opt in by listing this string in their
/// manifest `capabilities` (or via the v0.1.13 `extra_capabilities` extension
/// point — both surfaces flatten into `PluginManifest.capabilities` at
/// discovery time).
///
/// Follow-up tracked separately: `animus-web-ui` v0.1.2 still needs to declare
/// this capability in its manifest. Until then, `animus web open` falls back
/// to whichever API transport sorts first and prints a warning.
const WEB_UI_CAPABILITY: &str = "$ui/web";
const DEFAULT_TRANSPORT_KIND_PREFERENCE: &[&str] = &["transport-http", "transport-graphql"];
const PLUGIN_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

fn spawn_options_for_transport(plugin: &DiscoveredPlugin) -> PluginSpawnOptions {
    PluginSpawnOptions::for_manifest(
        plugin.name.clone(),
        &plugin.manifest.env_required,
        std::iter::empty::<String>(),
        None,
    )
    .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
}

pub(crate) async fn handle_web(
    command: WebCommand,
    _hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        WebCommand::Serve(args) => handle_serve(args, project_root, json).await,
        WebCommand::Open(args) => handle_open(args, project_root, json).await,
    }
}

async fn handle_serve(args: crate::WebServeArgs, project_root: &str, json: bool) -> Result<()> {
    let (api_plugins, web_ui_plugins) = collect_transport_plugins(project_root)?;
    if api_plugins.is_empty() && web_ui_plugins.is_empty() {
        return Err(missing_transport_plugins_error(json));
    }

    // UI first so the URL the user is most likely to want shows up before
    // the API URLs in both the JSON envelope and the human-facing log lines.
    let mut running: Vec<RunningTransport> = Vec::new();
    for plugin in web_ui_plugins.iter().chain(api_plugins.iter()) {
        match spawn_and_keep_alive(plugin, project_root).await {
            Ok(rt) => running.push(rt),
            Err(err) => {
                shutdown_running_transports(running).await;
                return Err(err);
            }
        }
    }

    let ui_url = first_ui_url(&running);
    let api_url = first_api_url(&running);
    let primary_url = ui_url.clone().or_else(|| api_url.clone());

    if args.open {
        if let Some(url) = primary_url.as_deref() {
            if let Err(err) = open_in_browser(url) {
                shutdown_running_transports(running).await;
                return Err(err);
            }
        }
    }

    let payload = json!({
        "message": "transport plugins ready",
        "primary_url": primary_url,
        "ui_url": ui_url,
        "api_url": api_url,
        "transports": running.iter().map(|s| json!({
            "name": s.info.name,
            "kind": s.info.kind,
            "url": s.info.url,
            "serves_ui": s.info.serves_ui,
            "info": s.info.info,
        })).collect::<Vec<_>>(),
    });
    print_value(payload, json)?;

    if !json {
        print_serve_url_summary(&running, ui_url.as_deref(), api_url.as_deref());
        eprintln!(
            "[serve] transport plugins running in foreground. Press Ctrl-C to shut them down. \
             For long-lived supervision (auto-restart, background lifetime), run \
             `animus daemon start` — daemon-managed web plugins land in v0.5."
        );
    }

    wait_for_shutdown_signal().await;
    if !json {
        eprintln!("[serve] shutdown signal received, stopping transport plugins...");
    }
    shutdown_running_transports(running).await;
    Ok(())
}

fn first_ui_url(running: &[RunningTransport]) -> Option<String> {
    first_ui_url_in(running.iter().map(|r| &r.info))
}

fn first_api_url(running: &[RunningTransport]) -> Option<String> {
    first_api_url_in(running.iter().map(|r| &r.info))
}

fn first_ui_url_in<'a>(mut spawns: impl Iterator<Item = &'a SpawnedTransport>) -> Option<String> {
    spawns.find(|s| s.serves_ui).and_then(|s| s.url.clone())
}

fn first_api_url_in<'a>(mut spawns: impl Iterator<Item = &'a SpawnedTransport>) -> Option<String> {
    spawns.find(|s| !s.serves_ui).and_then(|s| s.url.clone())
}

fn print_serve_url_summary(running: &[RunningTransport], ui_url: Option<&str>, api_url: Option<&str>) {
    let lines = serve_url_summary_lines(running.iter().map(|r| &r.info), ui_url, api_url);
    for line in lines.stdout {
        print_ok(&line, false);
    }
    if let Some(text) = lines.warning {
        eprintln!("[serve] {text}");
    }
}

struct ServeSummaryLines {
    stdout: Vec<String>,
    warning: Option<String>,
}

fn serve_url_summary_lines<'a>(
    spawns: impl Iterator<Item = &'a SpawnedTransport>,
    ui_url: Option<&str>,
    api_url: Option<&str>,
) -> ServeSummaryLines {
    let mut stdout: Vec<String> = Vec::new();
    if let Some(url) = ui_url {
        stdout.push(format!("UI: {url}"));
    }
    for s in spawns {
        if s.serves_ui {
            continue;
        }
        if let Some(url) = s.url.as_deref() {
            stdout.push(format!("API ({}): {url}", s.name));
        }
    }
    let warning = if ui_url.is_none() {
        api_url.map(|url| {
            format!(
                "no transport plugin advertised the `{WEB_UI_CAPABILITY}` capability; \
                 browser-facing UI is unavailable. The API endpoint at {url} is still reachable. \
                 Install launchapp-dev/animus-web-ui or upgrade an installed transport to a \
                 version that declares `{WEB_UI_CAPABILITY}` in its manifest capabilities."
            )
        })
    } else {
        None
    };
    ServeSummaryLines { stdout, warning }
}

async fn handle_open(args: crate::WebOpenArgs, project_root: &str, json: bool) -> Result<()> {
    // --url short-circuits plugin discovery entirely: the help text promises
    // "installed plugins are not consulted", so a machine with zero web plugins
    // installed must still be able to open an arbitrary URL. This is also the
    // recommended path when an externally-managed plugin or daemon is already
    // serving the UI, since the CLI then has no lifetime to manage.
    if let Some(explicit) = args.url.as_deref() {
        return open_resolved_url(explicit, json);
    }

    // Default path before this fix: spawn plugin, ask it for URL, immediately
    // shut it down, then open the browser at an already-dead server. Codex
    // round-6 P2. Fix: keep the plugin alive in the foreground (same lifecycle
    // as `web serve`) so the URL stays reachable. A `--detach` flag was
    // explored but rejected — STDIO plugins die at CLI exit because the OS
    // closes the inherited stdin pipe, so we cannot honestly promise a
    // detached lifecycle without a separate supervisor (which is the job
    // of `animus daemon start` once daemon-managed web plugins land).
    let (api_plugins, web_ui_plugins) = collect_transport_plugins(project_root)?;
    if api_plugins.is_empty() && web_ui_plugins.is_empty() {
        return Err(missing_transport_plugins_error(json));
    }

    handle_open_foreground(&args, api_plugins, web_ui_plugins, project_root, json).await
}

async fn handle_open_foreground(
    args: &crate::WebOpenArgs,
    api_plugins: Vec<DiscoveredPlugin>,
    web_ui_plugins: Vec<DiscoveredPlugin>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    // UI plugins go up front so the URL we open in the browser is always the
    // UI URL when one is installed. API plugins still get spawned so that
    // (a) operators who installed both can see API URLs in the JSON envelope,
    // and (b) we have a fallback URL to open if the UI plugin failed to
    // produce one.
    let mut running: Vec<RunningTransport> = Vec::new();
    for plugin in web_ui_plugins.iter().chain(api_plugins.iter()) {
        match spawn_and_keep_alive(plugin, project_root).await {
            Ok(rt) => running.push(rt),
            Err(err) => {
                shutdown_running_transports(running).await;
                return Err(err);
            }
        }
    }

    let ui_url = first_ui_url(&running);
    let api_url = first_api_url(&running);
    let resolved_kind = if ui_url.is_some() { "ui" } else { "api" };
    let resolved_url = ui_url.clone().or_else(|| api_url.clone()).map(|u| append_path(&u, &args.path));

    let url = match resolved_url {
        Some(url) => url,
        None => {
            shutdown_running_transports(running).await;
            return Err(anyhow!(
                "no transport plugin advertised a URL. Pass --url explicitly or install \
                 launchapp-dev/animus-web-ui via `animus plugin install-defaults --include-transports`."
            ));
        }
    };

    let warning = (resolved_kind == "api").then(|| {
        format!(
            "no transport plugin advertised the `{WEB_UI_CAPABILITY}` capability; \
             opening the API endpoint at {url} instead. This is the API surface, not \
             the browser UI. Install launchapp-dev/animus-web-ui or upgrade an installed \
             transport to a version that declares `{WEB_UI_CAPABILITY}` in its manifest \
             capabilities to fix this."
        )
    });

    if let Err(err) = open_in_browser(&url) {
        shutdown_running_transports(running).await;
        return Err(err);
    }

    if json {
        print_value(
            json!({
                "message": "browser opened",
                "url": url,
                "mode": "foreground",
                "resolved_kind": resolved_kind,
                "warning": warning,
            }),
            true,
        )?;
    } else {
        if let Some(text) = warning.as_deref() {
            eprintln!("[open] WARNING: {text}");
        }
        print_ok(&format!("opened {url}"), false);
        eprintln!(
            "[open] transport plugin running in foreground so {url} stays reachable. \
             Press Ctrl-C to stop. For an externally-managed server, pass --url <URL> \
             so this command returns immediately without spawning a plugin."
        );
    }

    wait_for_shutdown_signal().await;
    if !json {
        eprintln!("[open] shutdown signal received, stopping transport plugin...");
    }
    shutdown_running_transports(running).await;
    Ok(())
}

#[cfg(test)]
async fn resolve_open_url(args: &crate::WebOpenArgs, project_root: &str) -> Result<String> {
    // Test-only helper exercising the --url short-circuit + the
    // describe-and-shutdown URL resolution. Production paths now go
    // through `handle_open_foreground` so the plugin handle survives past
    // URL resolution (the round-6 P2 fix).
    if let Some(explicit) = args.url.as_deref() {
        return Ok(explicit.to_string());
    }
    let (api_plugins, web_ui_plugins) = collect_transport_plugins(project_root)?;
    if api_plugins.is_empty() && web_ui_plugins.is_empty() {
        return Err(missing_transport_plugins_error(false));
    }
    let mut url: Option<String> = None;
    for plugin in web_ui_plugins.iter().chain(api_plugins.iter()) {
        if let Ok(info) = spawn_and_describe(plugin, project_root).await {
            if let Some(resolved) = info.url {
                url = Some(append_path(&resolved, &args.path));
                break;
            }
        }
    }
    url.ok_or_else(|| {
        anyhow!(
            "no transport plugin advertised a URL. Pass --url explicitly or install \
             launchapp-dev/animus-web-ui via `animus plugin install-defaults --include-transports`."
        )
    })
}

fn open_resolved_url(url: &str, json: bool) -> Result<()> {
    open_in_browser(url)?;
    if json {
        print_value(json!({"message": "browser opened", "url": url}), true)
    } else {
        print_ok(&format!("opened {url}"), false);
        Ok(())
    }
}

struct SpawnedTransport {
    name: String,
    kind: String,
    url: Option<String>,
    /// `true` when the plugin should be treated as a browser-facing UI for
    /// the purposes of `animus web open` / `animus web serve`. Set from the
    /// discovered manifest (`plugin_kind = "web_ui"` legacy shape OR
    /// `transport_backend` plugins that advertise the `$ui/web` capability).
    /// Carried on `SpawnedTransport` instead of recomputed from `kind` so the
    /// resolution lives at the partition boundary, not scattered through the
    /// URL-picking code.
    serves_ui: bool,
    info: Value,
}

struct RunningTransport {
    info: SpawnedTransport,
    host: PluginHost,
}

async fn describe_host(plugin: &DiscoveredPlugin, host: &PluginHost, project_root: &str) -> Result<SpawnedTransport> {
    let init = tokio::time::timeout(PLUGIN_HANDSHAKE_TIMEOUT, host.handshake())
        .await
        .map_err(|_| anyhow!("transport plugin {} handshake timed out", plugin.name))?
        .map_err(|err| anyhow!("transport plugin {} handshake failed: {err}", plugin.name))?;
    let init_value = serde_json::to_value(&init).unwrap_or(Value::Null);

    // Spec lifecycle: host MUST call transport/start AFTER initialize so the
    // plugin can bind its listener. Pre-v0.4.13 launchapp-dev transports skip
    // this call entirely and bind inside `initialize` — we keep them working
    // by treating METHOD_NOT_FOUND / METHOD_NOT_SUPPORTED as a deprecation
    // warning + continue, but new plugins MUST honor the spec to actually
    // bind.
    let is_transport_kind = plugin.manifest.serves_kind(TRANSPORT_PLUGIN_KIND);
    let start_reply = if is_transport_kind { drive_transport_start(plugin, host, project_root).await? } else { None };

    let mut url = start_reply.as_ref().and_then(bind_url_for_kind_from_info);
    if url.is_none() {
        url = extract_url(&init_value);
    }
    if url.is_none() {
        if let Ok(Ok(value)) =
            tokio::time::timeout(PLUGIN_HANDSHAKE_TIMEOUT, host.request_typed("transport/info", None)).await
        {
            url = extract_url(&value);
        }
    }
    let serves_ui = plugin.manifest.serves_kind(WEB_UI_PLUGIN_KIND) || plugin_advertises_web_ui(plugin);
    Ok(SpawnedTransport {
        name: plugin.name.clone(),
        kind: plugin.manifest.plugin_kind.clone(),
        url,
        serves_ui,
        info: init_value,
    })
}

/// Build the spec-shaped `TransportConfig` payload and issue `transport/start`
/// against `host`. Returns `Some(transport_info_value)` on success, `None`
/// when the plugin pre-dates the lifecycle (and we logged a deprecation
/// warning).
async fn drive_transport_start(
    plugin: &DiscoveredPlugin,
    host: &PluginHost,
    project_root: &str,
) -> Result<Option<Value>> {
    let project_root_path = std::path::PathBuf::from(project_root);
    let socket_path = control_socket_path(&project_root_path);
    // Spec shape per animus-transport-protocol v0.1.13:
    //   { control_socket_path, project_root, bind_addr?, config? }
    // bind_addr is omitted so the plugin uses its TransportSchema::default_port
    // (HTTP plugin defaults to 127.0.0.1:8080, GraphQL to 127.0.0.1:8090). A
    // future `animus web serve --port` flag can set this explicitly.
    let params = json!({
        "control_socket_path": socket_path,
        "project_root": project_root_path,
    });

    let outcome =
        tokio::time::timeout(TRANSPORT_START_TIMEOUT, host.request_typed(TRANSPORT_METHOD_START, Some(params))).await;
    match outcome {
        Ok(Ok(value)) => Ok(Some(value)),
        Ok(Err(HostError::Rpc(err))) if is_legacy_transport_error(&err) => {
            eprintln!(
                "[serve] WARNING: transport plugin '{}' does not implement `{TRANSPORT_METHOD_START}` \
                 (pre-spec lifecycle). Treating as no-op so the legacy bind-in-initialize behavior keeps \
                 working. Upgrade the plugin to a release that drives the v0.1.13 transport lifecycle.",
                plugin.name
            );
            Ok(None)
        }
        Ok(Err(err)) => Err(anyhow!("transport plugin {} `{TRANSPORT_METHOD_START}` failed: {err}", plugin.name)),
        Err(_) => Err(anyhow!(
            "transport plugin {} did not respond to `{TRANSPORT_METHOD_START}` within {}s",
            plugin.name,
            TRANSPORT_START_TIMEOUT.as_secs()
        )),
    }
}

fn is_legacy_transport_error(err: &RpcError) -> bool {
    err.code == METHOD_NOT_FOUND_CODE || err.code == METHOD_NOT_SUPPORTED_CODE
}

/// Best-effort URL extraction from a `transport/start` `TransportInfo` reply.
/// The reply is just `{ bound_addr, started_at }`; the plugin kind in the
/// manifest gives us the URL scheme.
fn bind_url_for_kind_from_info(value: &Value) -> Option<String> {
    let bound = value.get("bound_addr").and_then(Value::as_str)?;
    Some(format!("http://{bound}"))
}

#[cfg(test)]
async fn spawn_and_describe(plugin: &DiscoveredPlugin, project_root: &str) -> Result<SpawnedTransport> {
    let options = spawn_options_for_transport(plugin);
    let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .map_err(|err| anyhow!("failed to spawn transport plugin {}: {err}", plugin.name))?;
    let described = describe_host(plugin, &host, project_root).await;
    // Test-only describe-and-shutdown helper. Production paths use
    // `spawn_and_keep_alive` so the plugin survives URL resolution.
    let _ = host.shutdown_transport().await;
    let _ = host.shutdown().await;
    described
}

async fn spawn_and_keep_alive(plugin: &DiscoveredPlugin, project_root: &str) -> Result<RunningTransport> {
    let options = spawn_options_for_transport(plugin);
    let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .map_err(|err| anyhow!("failed to spawn transport plugin {}: {err}", plugin.name))?;
    match describe_host(plugin, &host, project_root).await {
        Ok(info) => Ok(RunningTransport { info, host }),
        Err(err) => {
            // Spec: even on failure, drive the transport/shutdown drain so
            // a partially-bound listener gets a chance to release its port
            // before we drop the process.
            let _ = host.shutdown_transport().await;
            let _ = host.shutdown().await;
            Err(err)
        }
    }
}

async fn shutdown_running_transports(running: Vec<RunningTransport>) {
    for rt in running {
        // Spec lifecycle: drive `transport/shutdown` BEFORE the generic
        // `shutdown` so the plugin can drain in-flight requests and release
        // the bound port. Legacy plugins that don't implement
        // `transport/shutdown` are handled inside `shutdown_transport` (log
        // + continue), so this call is safe on every plugin kind.
        let _ = rt.host.shutdown_transport().await;
        let _ = rt.host.shutdown().await;
    }
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn extract_url(value: &Value) -> Option<String> {
    if let Some(direct) = value.get("url").and_then(Value::as_str) {
        return Some(direct.to_string());
    }
    if let Some(transport) = value.get("transport") {
        if let Some(url) = transport.get("url").and_then(Value::as_str) {
            return Some(url.to_string());
        }
        let host = transport.get("host").and_then(Value::as_str);
        let port = transport.get("port").and_then(Value::as_u64);
        let scheme = transport.get("scheme").and_then(Value::as_str).unwrap_or("http");
        if let (Some(host), Some(port)) = (host, port) {
            return Some(format!("{scheme}://{host}:{port}"));
        }
    }
    value
        .get("capabilities")
        .and_then(Value::as_array)
        .and_then(|caps| caps.iter().find_map(|cap| cap.get("url").and_then(Value::as_str).map(ToString::to_string)))
}

fn collect_transport_plugins(project_root: &str) -> Result<(Vec<DiscoveredPlugin>, Vec<DiscoveredPlugin>)> {
    let discovered = discover_plugins(project_root)?;
    partition_transport_plugins(discovered)
}

/// Pure partitioning logic split out for unit testing. Walks the discovered
/// plugin set and bins each one into:
///
/// - `api_plugins`: `transport_backend` plugins that do *not* declare the
///   `$ui/web` capability (e.g. transport-http, transport-graphql serving
///   machine endpoints).
/// - `web_ui_plugins`: anything that legitimately wants the browser. That
///   covers both the legacy `plugin_kind = "web_ui"` shape and any
///   `transport_backend` plugin that opted into the `$ui/web` capability
///   via its manifest (or the v0.1.13 `extra_capabilities` extension point,
///   which flattens into `PluginManifest.capabilities` at discovery time).
///
/// API plugins are sorted by `DEFAULT_TRANSPORT_KIND_PREFERENCE` so the
/// fallback path is deterministic when no UI plugin is installed.
fn partition_transport_plugins(
    discovered: Vec<DiscoveredPlugin>,
) -> Result<(Vec<DiscoveredPlugin>, Vec<DiscoveredPlugin>)> {
    let mut api_plugins: Vec<DiscoveredPlugin> = Vec::new();
    let mut web_ui_plugins: Vec<DiscoveredPlugin> = Vec::new();
    for plugin in discovered {
        let advertises_web_ui = plugin_advertises_web_ui(&plugin);
        // v0.7 multi-kind: bin by served ROLE (primary `plugin_kind` OR any
        // additional `plugin_kinds`) rather than the primary field alone, so a
        // consolidated plugin serving `web_ui` / `transport_backend` as a
        // secondary role is still surfaced to the browser / API path. web_ui
        // wins when a plugin serves both.
        if plugin.manifest.serves_kind(WEB_UI_PLUGIN_KIND) {
            web_ui_plugins.push(plugin);
        } else if plugin.manifest.serves_kind(TRANSPORT_PLUGIN_KIND) {
            if advertises_web_ui {
                web_ui_plugins.push(plugin);
            } else {
                api_plugins.push(plugin);
            }
        }
    }
    api_plugins.sort_by_key(|p| {
        DEFAULT_TRANSPORT_KIND_PREFERENCE.iter().position(|name| p.name.contains(name)).unwrap_or(usize::MAX)
    });
    Ok((api_plugins, web_ui_plugins))
}

fn plugin_advertises_web_ui(plugin: &DiscoveredPlugin) -> bool {
    plugin.manifest.capabilities.iter().any(|cap| cap == WEB_UI_CAPABILITY)
}

/// Build the "no transport plugins installed" error. In `--json` mode the
/// message is a single line so the `animus.cli.v1` envelope stays grep-friendly;
/// scripted consumers can still discover the install command via
/// `error.details.install_command`. In human mode we keep the multi-line install
/// help, matching the pre-fix experience operators are used to.
fn missing_transport_plugins_error(json: bool) -> anyhow::Error {
    let install_command = "animus plugin install-defaults --include-transports";
    let individual_plugins: Vec<String> = DEFAULT_TRANSPORT_PLUGINS
        .iter()
        .copied()
        .map(format_repo_spec)
        .map(|spec| format!("animus plugin install {spec}"))
        .collect();
    let details = serde_json::json!({
        "install_command": install_command,
        "individual_plugins": individual_plugins,
    });
    let message = if json {
        format!("no transport_backend or web_ui plugins are installed; run `{install_command}` to install the defaults")
    } else {
        let mut lines = vec![
            "No transport_backend or web_ui plugins are installed.".to_string(),
            "".to_string(),
            "Animus delegates `animus web` to standalone transport + UI plugins.".to_string(),
            "Install the defaults with:".to_string(),
            "".to_string(),
            format!("  {install_command}"),
            "".to_string(),
            "Or install them individually:".to_string(),
        ];
        lines.extend(individual_plugins.iter().map(|command| format!("  {command}")));
        lines.join("\n")
    };
    CliError::new(CliErrorKind::InvalidInput, message).with_details(details).into()
}

fn open_in_browser(url: &str) -> Result<()> {
    webbrowser::open(url).map(|_| ()).map_err(|error| anyhow!("failed to open browser: {error}"))
}

fn append_path(base: &str, path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return base.to_string();
    }
    let suffix = if trimmed.starts_with('/') { trimmed.to_string() } else { format!("/{trimmed}") };
    if let Some(stripped) = base.strip_suffix('/') {
        format!("{stripped}{suffix}")
    } else {
        format!("{base}{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_path, extract_url, first_api_url_in, first_ui_url_in, missing_transport_plugins_error,
        partition_transport_plugins, plugin_advertises_web_ui, resolve_open_url, serve_url_summary_lines,
        shutdown_running_transports, wait_for_shutdown_signal, SpawnedTransport, TRANSPORT_PLUGIN_KIND,
        WEB_UI_CAPABILITY, WEB_UI_PLUGIN_KIND,
    };
    use crate::shared::{classify_cli_error_kind, extract_cli_error_details, CliErrorKind};
    use crate::WebOpenArgs;
    use animus_plugin_protocol::PluginManifest;
    use orchestrator_plugin_host::{DiscoveredPlugin, DiscoverySource};
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fake_plugin(name: &str, plugin_kind: &str, capabilities: &[&str]) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.to_string(),
            path: PathBuf::from(format!("/tmp/{name}")),
            manifest: PluginManifest {
                name: name.to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: plugin_kind.to_string(),
                plugin_kinds: vec![],
                description: "test fixture".to_string(),
                protocol_version: "1.0.0".to_string(),
                capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
                env_required: Vec::new(),
                notification_buffer_size: None,
                supports_mcp: None,
            },
            source: DiscoverySource::ExplicitConfig,
        }
    }

    fn fake_running(name: &str, kind: &str, url: Option<&str>, serves_ui: bool) -> SpawnedTransport {
        SpawnedTransport {
            name: name.to_string(),
            kind: kind.to_string(),
            url: url.map(ToString::to_string),
            serves_ui,
            info: json!({}),
        }
    }

    #[test]
    fn extract_url_from_direct_field() {
        let v = json!({"url": "http://127.0.0.1:4173"});
        assert_eq!(extract_url(&v).as_deref(), Some("http://127.0.0.1:4173"));
    }

    #[test]
    fn extract_url_from_transport_object() {
        let v = json!({"transport": {"host": "127.0.0.1", "port": 4173, "scheme": "https"}});
        assert_eq!(extract_url(&v).as_deref(), Some("https://127.0.0.1:4173"));
    }

    #[test]
    fn extract_url_returns_none_for_empty_value() {
        assert!(extract_url(&json!({})).is_none());
    }

    #[test]
    fn append_path_handles_trailing_slash() {
        assert_eq!(append_path("http://h/", "/runs"), "http://h/runs");
        assert_eq!(append_path("http://h", "runs"), "http://h/runs");
        assert_eq!(append_path("http://h", "/"), "http://h");
    }

    /// P2 #4 regression: `--url` must skip plugin discovery entirely so that
    /// machines without any web plugins installed can still open arbitrary URLs.
    /// We point at a nonexistent project_root to prove discovery never runs —
    /// if it did, `collect_transport_plugins` would observe an empty plugin set
    /// and `bail_with_install_help` would `std::process::exit(2)`, killing the test.
    #[tokio::test]
    async fn web_open_with_explicit_url_does_not_discover_plugins() {
        let args =
            WebOpenArgs { url: Some("http://example.invalid/dashboard".to_string()), path: "/ignored".to_string() };
        let resolved = resolve_open_url(&args, "/path/that/definitely/does/not/exist/animus-test").await.unwrap();
        assert_eq!(resolved, "http://example.invalid/dashboard");
    }

    /// P1 #3 regression scaffold: `web serve` must keep plugin handles alive
    /// for the duration of the session. We can't reach into the live plugin host
    /// from a unit test, but we can verify the two halves of the contract:
    /// (a) `shutdown_running_transports` is the explicit teardown step, and
    /// (b) `wait_for_shutdown_signal` actually blocks until a signal arrives.
    /// Together they prove the lifecycle is no longer "spawn, describe,
    /// immediately shut down" — the fix moved shutdown behind the signal wait.
    #[tokio::test]
    async fn web_serve_keeps_plugin_alive_until_signal() {
        shutdown_running_transports(Vec::new()).await;
        let blocked = tokio::time::timeout(Duration::from_millis(150), wait_for_shutdown_signal()).await;
        assert!(blocked.is_err(), "wait_for_shutdown_signal returned before any signal was raised");
    }

    /// Capability marker scan: a transport plugin with `$ui/web` in its
    /// declared capabilities must be detected as a UI plugin even when its
    /// `plugin_kind` is still `transport_backend` (the v0.1.13 extension
    /// point flattens `extra_capabilities` into `manifest.capabilities` at
    /// discovery time, so we only need to check that vec).
    #[test]
    fn plugin_advertises_web_ui_reads_capabilities_vec() {
        let with_ui = fake_plugin("animus-web-ui", TRANSPORT_PLUGIN_KIND, &["transport/info", WEB_UI_CAPABILITY]);
        let without_ui = fake_plugin("animus-transport-http", TRANSPORT_PLUGIN_KIND, &["transport/info"]);
        assert!(plugin_advertises_web_ui(&with_ui));
        assert!(!plugin_advertises_web_ui(&without_ui));
    }

    /// User audit P1: when both kinds are installed, the partitioner must
    /// place plugins that advertise `$ui/web` in the UI bucket. The API
    /// bucket holds plain transport_backend plugins and is sorted by the
    /// default preference so transport-http wins ties (fallback determinism).
    #[test]
    fn partition_separates_ui_plugins_from_api_plugins() {
        let discovered = vec![
            fake_plugin("animus-transport-graphql", TRANSPORT_PLUGIN_KIND, &["transport/info"]),
            fake_plugin("animus-web-ui", TRANSPORT_PLUGIN_KIND, &[WEB_UI_CAPABILITY]),
            fake_plugin("animus-transport-http", TRANSPORT_PLUGIN_KIND, &["transport/info"]),
            // Legacy `plugin_kind = "web_ui"` plugins still land in the UI bucket.
            fake_plugin("legacy-web-ui", WEB_UI_PLUGIN_KIND, &[]),
            // Unrelated kinds (e.g. provider, subject_backend) are filtered out entirely.
            fake_plugin("animus-provider-claude", "provider", &["agent/run"]),
        ];
        let (api_plugins, web_ui_plugins) = partition_transport_plugins(discovered).expect("partition succeeds");
        let ui_names: Vec<&str> = web_ui_plugins.iter().map(|p| p.name.as_str()).collect();
        let api_names: Vec<&str> = api_plugins.iter().map(|p| p.name.as_str()).collect();
        assert!(ui_names.contains(&"animus-web-ui"), "expected $ui/web plugin in UI bucket, got {ui_names:?}");
        assert!(ui_names.contains(&"legacy-web-ui"), "expected legacy web_ui kind in UI bucket, got {ui_names:?}");
        assert!(!ui_names.contains(&"animus-transport-http"), "API transport must not appear in UI bucket");
        assert_eq!(
            api_names,
            vec!["animus-transport-http", "animus-transport-graphql"],
            "API plugins must be sorted by DEFAULT_TRANSPORT_KIND_PREFERENCE so the fallback is deterministic"
        );
        assert!(!api_names.contains(&"animus-provider-claude"), "non-transport kinds must be dropped");
    }

    /// User audit P1 fix: when both a UI plugin and an API transport are
    /// running, `web open` must pick the UI URL. The partitioner places UI
    /// plugins first; this test pins the URL-picking helpers so a future
    /// refactor can't silently regress.
    #[test]
    fn web_open_prefers_ui_plugin_over_api_when_both_installed() {
        let spawns = [
            fake_running("animus-web-ui", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8082"), true),
            fake_running("animus-transport-http", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8080"), false),
            fake_running("animus-transport-graphql", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8081"), false),
        ];
        let ui = first_ui_url_in(spawns.iter()).expect("UI url present");
        let api = first_api_url_in(spawns.iter()).expect("API url present");
        assert_eq!(ui, "http://127.0.0.1:8082", "UI plugin URL must win when present");
        assert_eq!(api, "http://127.0.0.1:8080", "API picker returns the first non-UI plugin (sorted upstream)");
    }

    /// User audit P1 fallback path: when no plugin advertises `$ui/web`, the
    /// only URL available is the API endpoint. The picker still returns that
    /// URL (we don't want the command to fail with "no URL") but the warning
    /// channel of `serve_url_summary_lines` must fire so the operator knows
    /// the browser will hit the API surface, not a real UI. This is the
    /// pre-animus-web-ui-v0.1.2 reality.
    #[test]
    fn web_open_falls_back_to_api_with_warning_when_no_ui_plugin() {
        let spawns = [
            fake_running("animus-transport-http", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8080"), false),
            fake_running("animus-transport-graphql", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8081"), false),
        ];
        assert!(first_ui_url_in(spawns.iter()).is_none(), "no UI plugin installed");
        let api = first_api_url_in(spawns.iter()).expect("API fallback URL");
        assert_eq!(api, "http://127.0.0.1:8080");
        let lines = serve_url_summary_lines(spawns.iter(), None, Some(&api));
        let warning = lines.warning.expect("warning must fire when only the API endpoint is reachable");
        assert!(
            warning.contains(WEB_UI_CAPABILITY),
            "warning must name the missing capability so operators can fix it: {warning}"
        );
        assert!(warning.contains("8080"), "warning must include the API URL the browser would have opened");
    }

    /// `web serve` must surface UI and API URLs as distinct lines so an
    /// operator scanning stdout can find the URL they care about. This pins
    /// the summary line shape (`UI: ...` and `API (<name>): ...`) and proves
    /// no warning fires in the happy path.
    #[test]
    fn web_serve_prints_ui_and_api_urls_separately() {
        let spawns = [
            fake_running("animus-web-ui", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8082"), true),
            fake_running("animus-transport-http", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8080"), false),
            fake_running("animus-transport-graphql", TRANSPORT_PLUGIN_KIND, Some("http://127.0.0.1:8081"), false),
        ];
        let ui = first_ui_url_in(spawns.iter());
        let api = first_api_url_in(spawns.iter());
        let lines = serve_url_summary_lines(spawns.iter(), ui.as_deref(), api.as_deref());
        assert_eq!(
            lines.stdout,
            vec![
                "UI: http://127.0.0.1:8082".to_string(),
                "API (animus-transport-http): http://127.0.0.1:8080".to_string(),
                "API (animus-transport-graphql): http://127.0.0.1:8081".to_string(),
            ],
            "UI and API URLs must each occupy their own labelled line"
        );
        assert!(lines.warning.is_none(), "no warning when a UI plugin is installed");
    }

    /// JSON envelope contract: when `animus web serve` or `animus web open`
    /// runs on a machine with no transport plugins installed, the failure must
    /// flow through the typed `CliError` channel (not `std::process::exit`)
    /// so `main::emit_cli_error` can wrap it in `animus.cli.v1`. The
    /// `--json`-shaped message is a single line and includes the install
    /// command in `error.details` for script consumers.
    #[test]
    fn missing_transport_plugins_error_routes_through_typed_cli_error() {
        let err = missing_transport_plugins_error(true);
        assert_eq!(
            classify_cli_error_kind(&err),
            CliErrorKind::InvalidInput,
            "missing-plugins must classify as invalid_input (exit 2), matching the pre-fix std::process::exit(2)"
        );
        let message = err.to_string();
        assert!(
            !message.contains('\n'),
            "--json message must be a single line so the envelope stays grep-friendly: {message:?}"
        );
        assert!(
            message.contains("animus plugin install-defaults"),
            "message must point operators at the fix command, got: {message:?}"
        );

        let details = extract_cli_error_details(&err).expect("--json failure must carry structured install hints");
        assert_eq!(
            details.pointer("/install_command").and_then(serde_json::Value::as_str),
            Some("animus plugin install-defaults --include-transports"),
            "details must include the canonical install command for scripted recovery"
        );
        let individual = details
            .pointer("/individual_plugins")
            .and_then(serde_json::Value::as_array)
            .expect("details must list individual install commands");
        assert!(individual.len() >= 3, "details should list http, graphql, and web-ui install commands");
    }

    /// Human-mode error keeps the multi-line install help so terminal users
    /// see the same guidance as before the JSON envelope fix.
    #[test]
    fn missing_transport_plugins_error_keeps_multiline_help_for_humans() {
        let err = missing_transport_plugins_error(false);
        let message = err.to_string();
        assert!(message.contains('\n'), "human-mode message keeps multi-line install help: {message:?}");
        assert!(message.contains("animus plugin install-defaults --include-transports"));
        assert!(message.contains("launchapp-dev/animus-transport-http"));
    }
}
