use anyhow::{bail, Result};
use cli_wrapper::{is_ai_cli_tool, parse_launch_from_runtime_contract, LaunchInvocation};
use tracing::{debug, warn};

pub(super) fn resolve_idle_timeout_secs(
    tool: &str,
    hard_timeout_secs: Option<u64>,
    runtime_contract: Option<&serde_json::Value>,
) -> Option<u64> {
    if !is_ai_cli_tool(tool) {
        return None;
    }

    let contract_override = runtime_contract.and_then(|contract| {
        contract
            .pointer("/policy/idle_timeout_secs")
            .or_else(|| contract.pointer("/cli/policy/idle_timeout_secs"))
            .or_else(|| contract.pointer("/runner/idle_timeout_secs"))
            .and_then(|value| value.as_u64())
    });

    let requested = contract_override.unwrap_or(600);
    if requested == 0 {
        return None;
    }

    match hard_timeout_secs.filter(|value| *value > 0) {
        Some(hard_timeout_secs) => {
            let upper_bound = hard_timeout_secs.max(1);
            let lower_bound = if upper_bound < 30 { 1 } else { 30 };
            Some(requested.clamp(lower_bound, upper_bound))
        }
        None => Some(requested.max(30)),
    }
}

fn parse_prompt_as_args(prompt: &str) -> Vec<String> {
    prompt.split_whitespace().map(|s| s.to_string()).collect()
}

fn is_command_on_path(command: &str) -> bool {
    cli_wrapper::is_binary_on_path(command)
}

pub(super) async fn build_cli_invocation(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&serde_json::Value>,
) -> Result<LaunchInvocation> {
    if let Some(invocation) = parse_contract_launch(runtime_contract)? {
        debug!(
            tool,
            model,
            command = %invocation.command,
            args = ?invocation.args,
            prompt_via_stdin = invocation.prompt_via_stdin,
            "Using runtime contract launch configuration"
        );
        return Ok(invocation);
    }

    if is_ai_cli_tool(tool) {
        warn!(tool, model, "AI CLI tool requested without runtime contract launch configuration");
        bail!(
            "Missing runtime contract launch for AI CLI '{}'. Provide context.runtime_contract.cli.launch from cli-wrapper.",
            tool
        );
    }

    let _ = model;
    let args = match tool {
        "npm" | "pnpm" | "yarn" | "cargo" | "git" | "python" | "python3" | "node" => parse_prompt_as_args(prompt),
        "echo" => vec![prompt.to_string()],
        _ if is_command_on_path(tool) => parse_prompt_as_args(prompt),
        _ => {
            warn!(tool, "Unsupported tool requested");
            bail!(
                "Unsupported tool: {}. Configure a supported CLI (claude, codex, gemini, opencode, oai-runner) or provide an executable on PATH.",
                tool
            )
        }
    };

    let invocation = LaunchInvocation { command: tool.to_string(), args, prompt_via_stdin: false };
    debug!(
        tool,
        model,
        command = %invocation.command,
        args = ?invocation.args,
        "Built fallback CLI invocation"
    );
    Ok(invocation)
}

fn parse_contract_launch(runtime_contract: Option<&serde_json::Value>) -> Result<Option<LaunchInvocation>> {
    let invocation = parse_launch_from_runtime_contract(runtime_contract)?;
    if let Some(ref inv) = invocation {
        debug!(
            command = %inv.command,
            args = ?inv.args,
            prompt_via_stdin = inv.prompt_via_stdin,
            "Parsed runtime contract launch block via cli-wrapper"
        );
    }
    Ok(invocation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_runtime_contract_launch() {
        let contract = json!({
            "cli": {
                "launch": {
                    "command": "codex",
                    "args": ["exec", "--skip-git-repo-check", "hello"],
                    "prompt_via_stdin": false
                }
            }
        });

        let invocation = parse_contract_launch(Some(&contract)).expect("parse launch").expect("launch present");

        assert_eq!(invocation.command, "codex");
        assert_eq!(
            invocation.args,
            vec!["exec".to_string(), "--json".to_string(), "--skip-git-repo-check".to_string(), "hello".to_string()]
        );
        assert!(!invocation.prompt_via_stdin);
    }

    #[test]
    fn missing_launch_returns_none() {
        let contract = json!({ "cli": { "name": "codex" } });
        let invocation = parse_contract_launch(Some(&contract)).expect("parse launch");
        assert!(invocation.is_none());
    }

    #[test]
    fn parses_runtime_contract_launch_enforces_machine_mode_via_cli_wrapper() {
        let contract = json!({
            "cli": {
                "launch": {
                    "command": "claude",
                    "args": ["--print", "hello"],
                    "prompt_via_stdin": false
                }
            }
        });

        let invocation = parse_contract_launch(Some(&contract)).expect("parse launch").expect("launch present");
        let idx = invocation
            .args
            .iter()
            .position(|entry| entry == "--output-format")
            .expect("claude output format flag should be present");
        assert_eq!(invocation.args.get(idx + 1).map(String::as_str), Some("stream-json"));
        assert!(invocation.args.contains(&"--verbose".to_string()));
    }

    #[tokio::test]
    async fn ai_cli_without_runtime_launch_contract_is_rejected() {
        let err = build_cli_invocation("codex", "codex", "hello", None)
            .await
            .expect_err("missing launch contract should fail");
        assert!(err.to_string().contains("Missing runtime contract launch for AI CLI"), "unexpected error: {err}");
    }
}
