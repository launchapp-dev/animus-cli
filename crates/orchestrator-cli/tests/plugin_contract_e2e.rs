//! End-to-end contract tests for the Animus STDIO plugin surface.
//!
//! Drives the real `animus` binary against the bundled `animus-plugin-smoke`
//! plugin binary via the `ao plugin {list,info,call,ping}` CLI. Locks in:
//!
//! - Discovery via `ANIMUS_PLUGIN_PATH`
//! - Manifest contract (name, version, plugin_kind, capabilities)
//! - Plugin lifecycle (initialize handshake, $/ping)
//! - JSON-RPC dispatch through `ao plugin call`
//!
//! Provider plugin contracts (claude/codex/gemini/opencode/oai) are exercised
//! out-of-tree in their standalone repositories under launchapp-dev/ — those
//! crates ship as release-installed plugins via
//! `animus plugin install-defaults` rather than as in-tree workspace members.

#[path = "support/test_harness.rs"]
pub mod test_harness;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use test_harness::CliHarness;

fn smoke_plugin_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_animus-plugin-smoke"))
}

fn ensure_plugin_binary() -> PathBuf {
    let candidate = smoke_plugin_binary();
    assert!(candidate.is_file(), "Cargo-provided smoke plugin fixture is missing: {}", candidate.display());
    candidate
}

fn plugin_path_env() -> String {
    ensure_plugin_binary().parent().unwrap_or_else(|| Path::new(".")).to_string_lossy().into_owned()
}

fn run_plugin_command(args: &[&str]) -> Result<Value> {
    let plugin_path = plugin_path_env();
    let harness = CliHarness::new()?;
    harness.run_json_ok_with_env(args, &[("ANIMUS_PLUGIN_PATH", plugin_path.as_str())])
}

#[test]
fn plugin_list_discovers_smoke_plugin() -> Result<()> {
    ensure_plugin_binary();

    let response = run_plugin_command(&["plugin", "list"])?;
    let plugins = response
        .pointer("/data/plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("data.plugins should be an array: {response}"))?;
    let names: Vec<&str> = plugins.iter().filter_map(|p| p.get("name").and_then(Value::as_str)).collect();

    assert!(names.contains(&"animus-plugin-smoke"), "animus-plugin-smoke should be discovered; got {names:?}");

    for plugin in plugins {
        for required in ["name", "version", "plugin_kind", "description", "source", "path"] {
            assert!(plugin.get(required).is_some(), "discovered plugin entry should include `{required}`: {plugin}");
        }
    }

    Ok(())
}

#[test]
fn plugin_info_completes_handshake_for_smoke() -> Result<()> {
    ensure_plugin_binary();
    let response = run_plugin_command(&["plugin", "info", "animus-plugin-smoke"])?;
    let init = response.pointer("/data/initialize").context("initialize block missing")?;
    assert_eq!(
        init.pointer("/plugin_info/name").and_then(Value::as_str),
        Some("animus-plugin-smoke"),
        "plugin_info.name should match: {init}"
    );
    assert_eq!(
        init.pointer("/plugin_info/plugin_kind").and_then(Value::as_str),
        Some("subject_backend"),
        "smoke plugin should advertise subject_backend kind: {init}"
    );
    assert!(
        init.pointer("/capabilities/subject_kinds")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("smoke"))),
        "smoke plugin should advertise subject_kinds=[smoke]: {init}"
    );
    Ok(())
}

#[test]
fn plugin_ping_round_trips() -> Result<()> {
    ensure_plugin_binary();
    let response = run_plugin_command(&["plugin", "ping", "--name", "animus-plugin-smoke"])?;
    assert_eq!(response.pointer("/data/ok").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn plugin_call_dispatches_smoke_subject_get() -> Result<()> {
    ensure_plugin_binary();
    let response = run_plugin_command(&[
        "plugin",
        "call",
        "--name",
        "animus-plugin-smoke",
        "--method",
        "smoke/get",
        "--params",
        r#"{"id":"SMOKE-CONTRACT-1"}"#,
    ])?;
    let result = response.pointer("/data/result").context("result missing")?;
    assert_eq!(
        result.pointer("/id").and_then(Value::as_str),
        Some("SMOKE-CONTRACT-1"),
        "smoke plugin should echo the id back: {result}"
    );
    assert_eq!(
        result.pointer("/attributes/kind").and_then(Value::as_str),
        Some("smoke"),
        "smoke plugin should tag attributes.kind=smoke"
    );
    Ok(())
}

#[test]
fn plugin_info_for_unknown_plugin_returns_not_found() -> Result<()> {
    let plugin_path = plugin_path_env();
    let harness = CliHarness::new()?;
    let (payload, status) = harness.run_json_err_with_exit_and_env(
        &["plugin", "info", "animus-plugin-does-not-exist"],
        &[("ANIMUS_PLUGIN_PATH", plugin_path.as_str())],
    )?;
    assert_eq!(status, 3, "missing plugin should map to not_found exit code");
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"));
    Ok(())
}

#[test]
fn plugin_install_url_without_sha256_is_rejected() -> Result<()> {
    let harness = CliHarness::new()?;
    let (payload, status) = harness.run_json_err_with_exit(&[
        "plugin",
        "install",
        "--url",
        "https://example.invalid/animus-plugin-malicious",
    ])?;
    assert_eq!(status, 2, "missing --sha256 with --url should map to invalid_input exit code");
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
    let message = payload.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
    assert!(
        message.contains("--sha256") && message.contains("URL"),
        "error message should mention --sha256 and URL: {message}"
    );
    Ok(())
}
