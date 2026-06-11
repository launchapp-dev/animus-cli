//! `animus plugin status [<name>]` — surface per-plugin runtime state.
//!
//! Wraps the daemon-side `plugin/status` control RPC introduced in v0.5.8.
//! Falls back to a discovery-only snapshot when the daemon is not running so
//! the user still sees what plugins exist on disk plus an explicit
//! "daemon not running" note.

use std::path::Path;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use orchestrator_daemon_runtime::control::ControlClient;
use orchestrator_plugin_host::{
    PluginRuntimeState, PluginRuntimeStatus, PluginStatusResponse, PLUGIN_STATUS_PROTOCOL_VERSION,
};

use crate::cli_types::PluginStatusArgs;
use crate::shared::print_value;

pub(crate) async fn handle_plugin_status(args: PluginStatusArgs, project_root: &str) -> Result<()> {
    let project_root_path = Path::new(project_root);
    let response = match ControlClient::try_connect(project_root_path).await? {
        Some(client) => client.plugin_status().await?,
        None => discover_only_fallback(project_root_path)?,
    };

    match args.name.as_deref() {
        Some(name) => render_named(&response, name, args.json),
        None => render_list(&response, args.json),
    }
}

fn render_list(response: &PluginStatusResponse, json: bool) -> Result<()> {
    if json {
        return print_value(response, true);
    }
    if response.plugins.is_empty() {
        println!("no plugins discovered");
        return Ok(());
    }
    let header_name = "NAME";
    let header_kind = "KIND";
    let header_state = "STATE";
    let header_pid = "PID";
    let header_rpc = "LAST RPC";
    let header_restarts = "RESTARTS";

    let name_w =
        response.plugins.iter().map(|p| p.name.len()).max().unwrap_or(header_name.len()).max(header_name.len());
    let kind_w =
        response.plugins.iter().map(|p| p.kind.len()).max().unwrap_or(header_kind.len()).max(header_kind.len());

    println!(
        "{name:<name_w$}  {kind:<kind_w$}  {state:<10}  {pid:<6}  {rpc:<13}  {restarts}",
        name = header_name,
        kind = header_kind,
        state = header_state,
        pid = header_pid,
        rpc = header_rpc,
        restarts = header_restarts,
    );
    for plugin in &response.plugins {
        let rpc = format_last_rpc(plugin.last_rpc_at);
        let pid = plugin.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let state = format_state(plugin.state);
        let restarts = plugin.restart_count.to_string();
        let mut trailing =
            plugin.last_error.as_ref().map(|err| format!("  (last err: {})", err.code)).unwrap_or_default();
        if plugin.disabled_by_supervisor {
            let until = plugin.cooldown_until.map(|t| format!(" until {}", t.to_rfc3339())).unwrap_or_default();
            trailing.push_str(&format!("  [disabled by supervisor{until}]"));
        }
        println!(
            "{name:<name_w$}  {kind:<kind_w$}  {state:<10}  {pid:<6}  {rpc:<13}  {restarts}{trailing}",
            name = plugin.name,
            kind = plugin.kind,
            state = state,
            pid = pid,
            rpc = rpc,
            restarts = restarts,
            trailing = trailing,
        );
    }
    Ok(())
}

fn render_named(response: &PluginStatusResponse, name: &str, json: bool) -> Result<()> {
    let plugin = response
        .plugins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow!("plugin '{name}' not found in daemon's status registry"))?;
    if json {
        return print_value(plugin, true);
    }
    println!("name:           {}", plugin.name);
    println!("kind:           {}", plugin.kind);
    println!("state:          {}", format_state(plugin.state));
    println!("pid:            {}", plugin.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()));
    println!("last_rpc_at:    {}", format_last_rpc(plugin.last_rpc_at));
    println!("restart_count:  {}", plugin.restart_count);
    if plugin.disabled_by_supervisor {
        println!(
            "supervisor:     disabled (cooldown until {})",
            plugin.cooldown_until.map(|t| t.to_rfc3339()).unwrap_or_else(|| "-".into())
        );
    }
    if let Some(err) = &plugin.last_error {
        println!("last_error:     {} ({})", err.code, err.at.to_rfc3339());
        println!("                {}", err.message);
    } else {
        println!("last_error:     -");
    }
    if let Some(path) = &plugin.binary_path {
        println!("binary_path:    {path}");
    }
    if let Some(manifest_name) = &plugin.manifest_name {
        println!("manifest_name:  {manifest_name}");
    }
    Ok(())
}

fn format_state(state: PluginRuntimeState) -> &'static str {
    match state {
        PluginRuntimeState::Discovered => "discovered",
        PluginRuntimeState::Running => "running",
        PluginRuntimeState::Stopped => "stopped",
        PluginRuntimeState::Restarting => "restarting",
        PluginRuntimeState::Missing => "missing",
    }
}

fn format_last_rpc(at: Option<DateTime<Utc>>) -> String {
    match at {
        None => "-".into(),
        Some(ts) => {
            let now = Utc::now();
            let delta = now.signed_duration_since(ts);
            let secs = delta.num_seconds();
            if secs < 0 {
                "future".into()
            } else if secs < 60 {
                format!("{secs}s ago")
            } else if secs < 3_600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3_600)
            } else {
                format!("{}d ago", secs / 86_400)
            }
        }
    }
}

/// Fallback view when the daemon isn't running: discover plugins on disk,
/// surface them with state=Discovered. Propagates discovery errors so the
/// operator sees the underlying registry/config failure instead of an
/// empty "no plugins" report.
fn discover_only_fallback(project_root: &Path) -> Result<PluginStatusResponse> {
    let plugins = orchestrator_plugin_host::discover_plugins(project_root)
        .map_err(|err| anyhow!("plugin discovery failed: {err}"))?
        .into_iter()
        .map(|p| PluginRuntimeStatus {
            name: p.name.clone(),
            kind: p.manifest.plugin_kind.clone(),
            state: PluginRuntimeState::Discovered,
            pid: None,
            last_rpc_at: None,
            last_error: None,
            restart_count: 0,
            binary_path: Some(p.path.display().to_string()),
            manifest_name: Some(p.manifest.name),
            disabled_by_supervisor: false,
            cooldown_until: None,
        })
        .collect();
    Ok(PluginStatusResponse { protocol_version: PLUGIN_STATUS_PROTOCOL_VERSION, plugins })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_plugin_host::PluginLastError;

    fn sample_response() -> PluginStatusResponse {
        PluginStatusResponse {
            protocol_version: PLUGIN_STATUS_PROTOCOL_VERSION,
            plugins: vec![
                PluginRuntimeStatus {
                    name: "animus-provider-claude".into(),
                    kind: "claude".into(),
                    state: PluginRuntimeState::Running,
                    pid: Some(4242),
                    last_rpc_at: Some(Utc::now() - chrono::Duration::seconds(3)),
                    last_error: None,
                    restart_count: 0,
                    binary_path: Some("/bin/animus-provider-claude".into()),
                    manifest_name: Some("provider-claude".into()),
                    disabled_by_supervisor: false,
                    cooldown_until: None,
                },
                PluginRuntimeStatus {
                    name: "animus-queue-default".into(),
                    kind: "queue".into(),
                    state: PluginRuntimeState::Stopped,
                    pid: None,
                    last_rpc_at: None,
                    last_error: Some(PluginLastError {
                        code: "ConnectionLost".into(),
                        message: "broken pipe".into(),
                        at: Utc::now(),
                    }),
                    restart_count: 2,
                    binary_path: None,
                    manifest_name: None,
                    disabled_by_supervisor: true,
                    cooldown_until: Some(Utc::now() + chrono::Duration::seconds(120)),
                },
            ],
        }
    }

    #[test]
    fn render_list_succeeds_and_prints_protocol_envelope_in_json_mode() {
        let response = sample_response();
        // Render in JSON mode — function returns Ok and the rendered envelope
        // includes protocol_version + plugins[].
        render_list(&response, true).expect("json render succeeds");
    }

    #[test]
    fn render_list_succeeds_for_pretty_mode() {
        let response = sample_response();
        render_list(&response, false).expect("pretty render succeeds");
    }

    #[test]
    fn render_named_finds_existing_plugin() {
        let response = sample_response();
        render_named(&response, "animus-provider-claude", false).expect("named render succeeds");
    }

    #[test]
    fn render_named_errors_on_missing_plugin() {
        let response = sample_response();
        let err = render_named(&response, "does-not-exist", false).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[test]
    fn format_last_rpc_renders_relative_buckets() {
        let now = Utc::now();
        assert_eq!(format_last_rpc(None), "-");
        assert!(format_last_rpc(Some(now - chrono::Duration::seconds(10))).ends_with("s ago"));
        assert!(format_last_rpc(Some(now - chrono::Duration::minutes(5))).ends_with("m ago"));
        assert!(format_last_rpc(Some(now - chrono::Duration::hours(2))).ends_with("h ago"));
        assert!(format_last_rpc(Some(now - chrono::Duration::days(3))).ends_with("d ago"));
    }
}
