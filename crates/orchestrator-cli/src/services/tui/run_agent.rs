use anyhow::{Context, Result};
use orchestrator_core::runtime_contract;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc::UnboundedSender;

use crate::services::tui::app_event::AppEvent;

pub(crate) async fn run_agent_session(
    project_root: String,
    tool: String,
    model: String,
    prompt: String,
    mcp_endpoint: String,
    mcp_agent_id: String,
    print_mode: bool,
    envelope_json: bool,
    event_tx: UnboundedSender<AppEvent>,
) -> Result<()> {
    let binary = std::env::current_exe().context("failed to resolve ao binary path")?;

    let mut runtime_contract = runtime_contract::build_runtime_contract(
        &tool,
        &model,
        &prompt,
        None,
        None,
        Some(&mcp_endpoint),
        Some(&mcp_agent_id),
    )
    .with_context(|| format!("failed to build runtime contract for tool `{tool}`"))?;
    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(serde_json::Value::as_object_mut)
    {
        mcp.insert(
            "stdio".to_string(),
            json!({
                "command": binary.to_string_lossy().to_string(),
                "args": [
                    "--project-root",
                    project_root.clone(),
                    "mcp",
                    "serve"
                ]
            }),
        );
    }
    let runtime_contract_json = serde_json::to_string(&runtime_contract)
        .context("failed to serialize runtime contract JSON")?;

    let mut child = TokioCommand::new(binary)
        .args(build_agent_run_args(
            &project_root,
            &tool,
            &model,
            &prompt,
            &runtime_contract_json,
            envelope_json,
        ))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start `ao agent run` for tool `{tool}`"))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture agent-run stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture agent-run stderr")?;

    let tx_stdout = event_tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let formatted = if print_mode {
                line.trim().to_string()
            } else if envelope_json {
                format_agent_line(&line, &tool)
            } else {
                line.trim().to_string()
            };
            if formatted.is_empty() {
                continue;
            }
            let _ = tx_stdout.send(AppEvent::AgentOutput {
                line: formatted,
                is_error: false,
            });
        }
    });

    let tx_stderr = event_tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_stderr.send(AppEvent::AgentOutput {
                line,
                is_error: true,
            });
        }
    });

    let status = child
        .wait()
        .await
        .context("failed while waiting for agent run process")?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let summary = match status.code() {
        Some(code) => format!("agent run finished with exit code {code}"),
        None => "agent run terminated by signal".to_string(),
    };
    let _ = event_tx.send(AppEvent::AgentFinished {
        summary,
        success: status.success(),
    });

    Ok(())
}

fn build_agent_run_args(
    project_root: &str,
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract_json: &str,
    envelope_json: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    if envelope_json {
        args.push("--json".to_string());
    }
    args.extend([
        "--project-root".to_string(),
        project_root.to_string(),
        "agent".to_string(),
        "run".to_string(),
        "--tool".to_string(),
        tool.to_string(),
        "--model".to_string(),
        model.to_string(),
        "--prompt".to_string(),
        prompt.to_string(),
        "--runtime-contract-json".to_string(),
        runtime_contract_json.to_string(),
        "--save-jsonl".to_string(),
        "false".to_string(),
    ]);
    args
}

fn format_agent_line(line: &str, tool: &str) -> String {
    use cli_wrapper::{NormalizedTextEvent, extract_text_from_line};

    let trimmed = line.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let parsed = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => value,
        Err(_) => return trimmed.to_string(),
    };
    if parsed
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema == "ao.agent.event.v1")
    {
        if let Some(data) = parsed.get("data") {
            if let Some(object) = data.as_object() {
                if let Some((event_name, payload)) = object.iter().next() {
                    if event_name == "OutputChunk" {
                        if let Some(text) = payload.get("text").and_then(Value::as_str) {
                            match extract_text_from_line(text, tool) {
                                NormalizedTextEvent::TextChunk { text: t }
                                | NormalizedTextEvent::FinalResult { text: t } => return t,
                                NormalizedTextEvent::Ignored => {
                                    if !text.trim_start().starts_with('{') {
                                        return text.trim().to_string();
                                    }
                                    return String::new();
                                }
                            }
                        }
                    }
                    return format!("{event_name}: {}", short_payload(payload));
                }
            }
        }
    }

    trimmed.to_string()
}

fn short_payload(payload: &Value) -> String {
    let mut compact = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    if compact.len() > 200 {
        compact.truncate(197);
        compact.push_str("...");
    }
    compact
}
