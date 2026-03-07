/// End-to-end test for session continuation via `ao agent run`.
///
/// Tests that an agent can be invoked, then re-invoked with the same session ID
/// and the second invocation has context from the first (proving session resume works).
///
/// Prerequisites:
///   - Agent runner must be running (`ao runner start`) or will auto-start
///   - CLI tools must be installed (claude, codex, gemini)
///   - API credentials must be configured for each tool
///   - Set `AO_E2E_SESSION_CONTINUATION=1` to enable (skipped by default)
///
/// Run with:
///   AO_E2E_SESSION_CONTINUATION=1 cargo test -p orchestrator-cli --test session_continuation_e2e -- --nocapture
///
/// Environment variables:
///   AO_E2E_SESSION_CONTINUATION=1  — required to run (skipped otherwise)
///   AO_E2E_TOOLS=claude,codex      — comma-separated list of tools to test (default: claude)
///   AO_E2E_PROJECT_ROOT=<path>     — project root (default: current directory)
///   AO_E2E_TIMEOUT=120             — agent timeout in seconds (default: 120)

use anyhow::{Context, Result};
use cli_wrapper::{extract_text_from_line, NormalizedTextEvent};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

fn is_enabled() -> bool {
    std::env::var("AO_E2E_SESSION_CONTINUATION")
        .ok()
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn ao_binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin!("ao").to_path_buf()
}

fn project_root() -> String {
    std::env::var("AO_E2E_PROJECT_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .expect("current dir")
                .to_string_lossy()
                .to_string()
        })
}

fn e2e_tools() -> Vec<String> {
    std::env::var("AO_E2E_TOOLS")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_else(|| vec!["claude".to_string()])
}

fn timeout_secs() -> String {
    std::env::var("AO_E2E_TIMEOUT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "120".to_string())
}

fn default_model_for_tool(tool: &str) -> &'static str {
    match tool {
        "claude" => "claude-sonnet-4-6",
        "codex" => "gpt-5.3-codex",
        "gemini" => "gemini-2.5-pro",
        "opencode" => "zai-coding-plan/glm-5",
        "oai-runner" => "deepseek/deepseek-chat",
        _ => "claude-sonnet-4-6",
    }
}

fn build_launch_args(
    tool: &str,
    model: &str,
    prompt: &str,
    session_id: Option<&str>,
    reused: bool,
) -> Value {
    let args = match tool {
        "claude" => {
            let mut args = vec![
                "--print".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--verbose".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ];
            if let Some(sid) = session_id {
                if reused {
                    args.push("--resume".to_string());
                } else {
                    args.push("--session-id".to_string());
                }
                args.push(sid.to_string());
            }
            args.push("--model".to_string());
            args.push(model.to_string());
            args.push(prompt.to_string());
            args
        }
        "codex" => {
            let mut args = vec!["exec".to_string()];
            if reused {
                args.push("resume".to_string());
                args.push("--last".to_string());
            }
            args.push("--json".to_string());
            args.push("--full-auto".to_string());
            args.push("-c".to_string());
            args.push("sandbox_workspace_write.network_access=true".to_string());
            args.push("--skip-git-repo-check".to_string());
            args.push("--model".to_string());
            args.push(model.to_string());
            args.push(prompt.to_string());
            args
        }
        "gemini" => {
            let mut args = Vec::new();
            if reused {
                args.push("--resume".to_string());
                args.push("latest".to_string());
            }
            args.push("--model".to_string());
            args.push(model.to_string());
            args.push("--output-format".to_string());
            args.push("json".to_string());
            args.push("-p".to_string());
            args.push(prompt.to_string());
            args
        }
        "opencode" => {
            let mut args = vec!["run".to_string()];
            if let Some(sid) = session_id {
                if reused {
                    args.push("--session".to_string());
                    args.push(sid.to_string());
                }
            }
            args.push("-m".to_string());
            args.push(model.to_string());
            args.push("--format".to_string());
            args.push("json".to_string());
            args.push(prompt.to_string());
            args
        }
        "oai-runner" => {
            let mut args = vec!["run".to_string()];
            args.push("-m".to_string());
            args.push(model.to_string());
            args.push("--format".to_string());
            args.push("json".to_string());
            if let Some(sid) = session_id {
                args.push("--session-id".to_string());
                args.push(sid.to_string());
            }
            args.push(prompt.to_string());
            args
        }
        _ => vec![prompt.to_string()],
    };

    json!({
        "cli": {
            "name": tool,
            "launch": {
                "command": tool,
                "args": args,
                "prompt_via_stdin": false
            },
            "session": {
                "mode": "NativeId",
                "session_key": format!("e2e-test-{}", tool),
                "session_id": session_id,
                "reused": reused,
                "phase_thread_isolated": false
            }
        },
        "model": model
    })
}

/// Parsed result from an `ao --json agent run` invocation.
struct AgentRunResult {
    exit_code: Option<i32>,
    agent_text: String,
    raw_stderr: String,
    success: bool,
}

fn run_agent(
    tool: &str,
    model: &str,
    prompt: &str,
    session_id: Option<&str>,
    reused: bool,
    timeout: &str,
) -> Result<AgentRunResult> {
    let root = project_root();
    let contract = build_launch_args(tool, model, prompt, session_id, reused);

    let output = Command::new(ao_binary())
        .arg("--json")
        .arg("--project-root")
        .arg(&root)
        .arg("agent")
        .arg("run")
        .arg("--tool")
        .arg(tool)
        .arg("--model")
        .arg(model)
        .arg("--prompt")
        .arg(prompt)
        .arg("--timeout-secs")
        .arg(timeout)
        .arg("--runtime-contract-json")
        .arg(serde_json::to_string(&contract)?)
        .output()
        .with_context(|| format!("failed to execute: ao agent run --tool {}", tool))?;

    let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let raw_stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    let mut agent_text = String::new();
    let mut exit_code: Option<i32> = None;

    for line in raw_stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(envelope) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        let data = envelope.get("data");
        let kind = data
            .and_then(|d| d.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("");

        match kind {
            "output_chunk" => {
                if let Some(text) = data.and_then(|d| d.get("text")).and_then(Value::as_str) {
                    agent_text.push_str(text);
                    agent_text.push('\n');
                }
            }
            "finished" => {
                exit_code =
                    data.and_then(|d| d.get("exit_code")).and_then(Value::as_i64).map(|v| v as i32);
            }
            _ => {}
        }
    }

    Ok(AgentRunResult {
        exit_code,
        agent_text,
        raw_stderr,
        success,
    })
}

fn extract_text_from_agent_output(agent_text: &str, tool: &str) -> String {
    let mut extracted = String::new();

    for line in agent_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match extract_text_from_line(trimmed, tool) {
            NormalizedTextEvent::TextChunk { text } | NormalizedTextEvent::FinalResult { text } => {
                if !text.is_empty() {
                    extracted.push_str(&text);
                    if !text.ends_with('\n') {
                        extracted.push('\n');
                    }
                }
            }
            NormalizedTextEvent::Ignored => {
                if serde_json::from_str::<Value>(trimmed).is_err() {
                    extracted.push_str(trimmed);
                    extracted.push('\n');
                }
            }
        }
    }

    extracted
}

fn test_session_continuation_for_tool(tool: &str) -> Result<()> {
    let session_id = Uuid::new_v4().to_string();
    let model = default_model_for_tool(tool);
    let timeout = timeout_secs();

    eprintln!(
        "[e2e] tool={} model={} session_id={} timeout={}s",
        tool, model, session_id, timeout
    );

    let first_prompt = "Reply with exactly: PINEAPPLE_SESSION_MARKER_42. Nothing else.";

    eprintln!("[e2e] tool={} — running first agent invocation", tool);
    let r1 = run_agent(tool, model, first_prompt, Some(&session_id), false, &timeout)?;

    eprintln!(
        "[e2e] tool={} — first run success={} exit_code={:?}",
        tool, r1.success, r1.exit_code
    );

    if !r1.success {
        eprintln!(
            "[e2e]   stderr: {}",
            r1.raw_stderr.chars().take(500).collect::<String>()
        );
    }

    assert!(
        r1.success,
        "[e2e] tool={} first agent run failed:\nstderr: {}",
        tool, r1.raw_stderr
    );

    let text1 = extract_text_from_agent_output(&r1.agent_text, tool);
    let marker_in_extracted = text1.contains("PINEAPPLE_SESSION_MARKER_42");
    let marker_in_raw = r1.agent_text.contains("PINEAPPLE_SESSION_MARKER_42");
    let has_marker = marker_in_extracted || marker_in_raw;
    eprintln!(
        "[e2e] tool={} — first run contains marker: {} (extracted={}, raw={})",
        tool, has_marker, marker_in_extracted, marker_in_raw
    );
    eprintln!(
        "[e2e] tool={} — extracted text (first 300): {}",
        tool,
        text1.chars().take(300).collect::<String>()
    );
    if !marker_in_extracted && marker_in_raw {
        eprintln!(
            "[e2e] WARNING: tool={} — marker found in raw agent_text but NOT in extracted text. \
             JSON extraction is incomplete for this tool's output format.",
            tool
        );
        eprintln!(
            "[e2e]   raw agent_text (first 500): {}",
            r1.agent_text.chars().take(500).collect::<String>()
        );
    }

    let second_prompt =
        "What exact phrase did I ask you to reply with in my previous message? Repeat it exactly.";

    eprintln!(
        "[e2e] tool={} — running second agent invocation (session resume)",
        tool
    );
    let r2 = run_agent(tool, model, second_prompt, Some(&session_id), true, &timeout)?;

    eprintln!(
        "[e2e] tool={} — second run success={} exit_code={:?}",
        tool, r2.success, r2.exit_code
    );

    assert!(
        r2.success,
        "[e2e] tool={} second agent run failed:\nstderr: {}",
        tool, r2.raw_stderr
    );

    let text2 = extract_text_from_agent_output(&r2.agent_text, tool);
    let text2_lower = text2.to_ascii_lowercase();
    let raw2_lower = r2.agent_text.to_ascii_lowercase();

    let recalled_in_extracted = text2_lower.contains("pineapple");
    let recalled_in_raw = raw2_lower.contains("pineapple");
    let session_recalled = recalled_in_extracted || recalled_in_raw;

    eprintln!(
        "[e2e] tool={} — extracted text (second 300): {}",
        tool,
        text2.chars().take(300).collect::<String>()
    );
    eprintln!(
        "[e2e] tool={} — second run recalled session: {} (extracted={}, raw={})",
        tool, session_recalled, recalled_in_extracted, recalled_in_raw
    );

    if !recalled_in_extracted && recalled_in_raw {
        eprintln!(
            "[e2e] WARNING: tool={} — marker found in raw agent_text but NOT in extracted text. \
             JSON extraction is incomplete for this tool's output format.",
            tool
        );
        eprintln!(
            "[e2e]   raw agent_text (first 500): {}",
            r2.agent_text.chars().take(500).collect::<String>()
        );
    }

    if !session_recalled {
        eprintln!(
            "[e2e]   full raw agent_text:\n{}",
            r2.agent_text.chars().take(2000).collect::<String>()
        );
    }

    assert!(
        session_recalled,
        "[e2e] tool={} — session continuation failed: second invocation did not recall the marker.\nextracted text: {}\nagent_text (first 1000): {}",
        tool,
        text2.chars().take(500).collect::<String>(),
        r2.agent_text.chars().take(1000).collect::<String>()
    );

    eprintln!(
        "[e2e] tool={} — PASSED (session continuation verified)",
        tool
    );
    Ok(())
}

#[test]
fn e2e_session_continuation_agent_run() {
    if !is_enabled() {
        eprintln!(
            "skipping session continuation e2e (set AO_E2E_SESSION_CONTINUATION=1 to enable)"
        );
        return;
    }

    let tools = e2e_tools();
    eprintln!("[e2e] testing session continuation for tools: {:?}", tools);

    let mut failures: Vec<(String, String)> = Vec::new();

    for tool in &tools {
        match test_session_continuation_for_tool(tool) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("[e2e] tool={} — FAILED: {}", tool, error);
                failures.push((tool.clone(), error.to_string()));
            }
        }
    }

    if !failures.is_empty() {
        let summary = failures
            .iter()
            .map(|(tool, err)| format!("  {}: {}", tool, err))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "[e2e] {} of {} tools failed:\n{}",
            failures.len(),
            tools.len(),
            summary
        );
    }
}
