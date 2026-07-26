//! Minimal STDIO subject-backend used by the plugin contract E2E tests.

use std::io::{self, IsTerminal, Write};

use animus_plugin_protocol::{
    error_codes, HealthCheckResult, HealthStatus, InitializeResult, McpTool, PluginCapabilities, PluginInfo,
    PluginManifest, RpcError, RpcRequest, RpcResponse, PROTOCOL_VERSION,
};
use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PLUGIN_NAME: &str = "animus-plugin-smoke";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
const PLUGIN_KIND: &str = animus_plugin_protocol::PLUGIN_KIND_SUBJECT_BACKEND;
const SUBJECT_KIND: &str = "smoke";

fn manifest() -> PluginManifest {
    PluginManifest {
        name: PLUGIN_NAME.to_string(),
        version: PLUGIN_VERSION.to_string(),
        plugin_kind: PLUGIN_KIND.to_string(),
        plugin_kinds: Vec::new(),
        description: "End-to-end smoke fixture for Animus plugin host verification".to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        capabilities: vec![
            "initialize".to_string(),
            "$/ping".to_string(),
            "smoke/get".to_string(),
            "health/check".to_string(),
        ],
        env_required: Vec::new(),
        notification_buffer_size: None,
        supports_mcp: None,
    }
}

fn initialize_result() -> InitializeResult {
    InitializeResult {
        protocol_version: PROTOCOL_VERSION.to_string(),
        plugin_info: PluginInfo {
            name: PLUGIN_NAME.to_string(),
            version: PLUGIN_VERSION.to_string(),
            plugin_kind: PLUGIN_KIND.to_string(),
            plugin_kinds: Vec::new(),
            description: None,
        },
        capabilities: PluginCapabilities {
            methods: vec!["smoke/get".to_string(), "health/check".to_string()],
            streaming: false,
            progress: false,
            cancellation: false,
            projections: Vec::new(),
            subject_kinds: vec![SUBJECT_KIND.to_string()],
            mcp_tools: vec![McpTool {
                name: "smoke.echo".to_string(),
                description: Some("Echoes the supplied params payload".to_string()),
                input_schema: Some(json!({ "type": "object", "additionalProperties": true })),
            }],
        },
        kind_capabilities: std::collections::HashMap::new(),
    }
}

fn print_manifest_and_exit() -> ! {
    let payload = serde_json::to_string(&manifest()).expect("serialize manifest");
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{payload}").expect("write manifest");
    let _ = stdout.flush();
    std::process::exit(0);
}

fn handle_cli_args() {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--manifest" | "-m" => print_manifest_and_exit(),
            "--help" | "-h" => {
                eprintln!("animus-plugin-smoke {PLUGIN_VERSION} — test-only STDIO plugin");
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    handle_cli_args();
    if io::stdin().is_terminal() {
        eprintln!("animus-plugin-smoke is a STDIO plugin; pipe JSON-RPC requests on stdin or pass --manifest");
        std::process::exit(2);
    }

    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[animus-plugin-smoke] invalid JSON-RPC frame: {error}");
                continue;
            }
        };
        if let Some(response) = handle_request(request) {
            let mut encoded = serde_json::to_string(&response)?;
            encoded.push('\n');
            stdout.write_all(encoded.as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

fn handle_request(request: RpcRequest) -> Option<RpcResponse> {
    let id = request.id.clone();
    match request.method.as_str() {
        "initialize" => Some(RpcResponse::ok(id, serde_json::to_value(initialize_result()).expect("encode init"))),
        "initialized" => None,
        "$/ping" => Some(RpcResponse::ok(id, json!({}))),
        "health/check" => Some(RpcResponse::ok(
            id,
            serde_json::to_value(HealthCheckResult {
                status: HealthStatus::Healthy,
                uptime_ms: None,
                memory_usage_bytes: None,
                last_error: None,
            })
            .expect("encode health"),
        )),
        "smoke/get" => Some(handle_smoke_get(id, request.params)),
        "shutdown" => Some(RpcResponse::ok(id, json!({}))),
        "exit" => std::process::exit(0),
        other if other.starts_with("$/") => None,
        other => Some(RpcResponse::err(
            id,
            RpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("method '{other}' not implemented by animus-plugin-smoke"),
                data: None,
            },
        )),
    }
}

fn handle_smoke_get(id: Option<Value>, params: Option<Value>) -> RpcResponse {
    let id_value = params.as_ref().and_then(|value| value.get("id")).and_then(Value::as_str).unwrap_or("smoke-unknown");
    RpcResponse::ok(
        id,
        json!({
            "id": id_value,
            "title": format!("Smoke subject {id_value}"),
            "description": "Synthetic subject returned by animus-plugin-smoke",
            "attributes": { "kind": SUBJECT_KIND, "echo": params }
        }),
    )
}
