//! Runtime launch parsing and normalization utilities shared by runners.

use std::path::Path;

use anyhow::anyhow;
use serde_json::Value;

use crate::error::Result;

use super::types::CliType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchInvocation {
    pub command: String,
    pub args: Vec<String>,
    pub prompt_via_stdin: bool,
}

pub fn parse_cli_type(name: &str) -> Option<CliType> {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude" => Some(CliType::Claude),
        "codex" => Some(CliType::Codex),
        "gemini" => Some(CliType::Gemini),
        "opencode" | "open-code" => Some(CliType::OpenCode),
        "aider" => Some(CliType::Aider),
        "cursor" => Some(CliType::Cursor),
        "cline" => Some(CliType::Cline),
        "custom" => Some(CliType::Custom),
        _ => None,
    }
}

pub fn is_ai_cli_tool(name: &str) -> bool {
    parse_cli_type(name).is_some()
}

fn canonical_cli_name(command: &str) -> String {
    let trimmed = command.trim();
    let file_name = Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed);
    file_name.to_ascii_lowercase()
}

fn ensure_flag(args: &mut Vec<String>, flag: &str, insert_at: usize) {
    if args.iter().any(|value| value == flag) {
        return;
    }
    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, flag.to_string());
}

fn ensure_flag_value(args: &mut Vec<String>, flag: &str, value: &str, insert_at: usize) {
    if let Some(index) = args.iter().position(|entry| entry == flag) {
        if index + 1 < args.len() {
            args[index + 1] = value.to_string();
        } else {
            args.push(value.to_string());
        }
        return;
    }

    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, flag.to_string());
    args.insert((insert_at + 1).min(args.len()), value.to_string());
}

pub fn ensure_machine_json_output(invocation: &mut LaunchInvocation) {
    let cli = canonical_cli_name(&invocation.command);

    match cli.as_str() {
        "codex" => {
            let insert_at = invocation
                .args
                .iter()
                .position(|entry| entry == "exec")
                .map(|index| index + 1)
                .unwrap_or(0);
            ensure_flag(&mut invocation.args, "--json", insert_at);
        }
        "claude" => {
            let insert_at = invocation
                .args
                .iter()
                .position(|entry| entry == "--print")
                .map(|index| index + 1)
                .unwrap_or(0);
            ensure_flag(&mut invocation.args, "--verbose", insert_at);
            ensure_flag_value(
                &mut invocation.args,
                "--output-format",
                "stream-json",
                insert_at,
            );
        }
        "gemini" => {
            let insert_at = invocation
                .args
                .iter()
                .position(|entry| entry == "-p")
                .unwrap_or(invocation.args.len());
            ensure_flag_value(&mut invocation.args, "--output-format", "json", insert_at);
        }
        "opencode" => {
            let insert_at = invocation
                .args
                .iter()
                .position(|entry| entry == "run")
                .map(|index| index + 1)
                .unwrap_or(0);
            ensure_flag_value(&mut invocation.args, "--format", "json", insert_at);
        }
        _ => {}
    }
}

pub fn parse_launch_from_runtime_contract(
    runtime_contract: Option<&Value>,
) -> Result<Option<LaunchInvocation>> {
    let Some(contract) = runtime_contract else {
        return Ok(None);
    };

    let Some(launch) = contract
        .pointer("/cli/launch")
        .or_else(|| contract.get("launch"))
    else {
        return Ok(None);
    };
    if launch.is_null() {
        return Ok(None);
    }

    let command = launch
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Invalid runtime contract launch command"))?;

    let args = launch
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(|value| value.to_string())
                        .ok_or_else(|| anyhow!("Invalid runtime contract launch arg"))
                })
                .collect::<std::result::Result<Vec<_>, anyhow::Error>>()
        })
        .transpose()?
        .unwrap_or_default();

    let prompt_via_stdin = launch
        .get("prompt_via_stdin")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut invocation = LaunchInvocation {
        command: command.to_string(),
        args,
        prompt_via_stdin,
    };

    ensure_machine_json_output(&mut invocation);
    Ok(Some(invocation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_cli_type_supports_known_aliases() {
        assert_eq!(parse_cli_type("claude"), Some(CliType::Claude));
        assert_eq!(parse_cli_type("open-code"), Some(CliType::OpenCode));
        assert_eq!(parse_cli_type("unknown"), None);
    }

    #[test]
    fn parse_launch_enforces_machine_output() {
        let contract = json!({
            "cli": {
                "launch": {
                    "command": "opencode",
                    "args": ["run", "hello"],
                    "prompt_via_stdin": false
                }
            }
        });

        let launch = parse_launch_from_runtime_contract(Some(&contract))
            .expect("launch should parse")
            .expect("launch should be present");
        let idx = launch
            .args
            .iter()
            .position(|arg| arg == "--format")
            .expect("opencode format flag should be present");
        assert_eq!(launch.args.get(idx + 1).map(String::as_str), Some("json"));

        let claude_contract = json!({
            "cli": {
                "launch": {
                    "command": "claude",
                    "args": ["--print", "hello"],
                    "prompt_via_stdin": false
                }
            }
        });
        let claude_launch = parse_launch_from_runtime_contract(Some(&claude_contract))
            .expect("launch should parse")
            .expect("launch should be present");
        assert!(claude_launch.args.contains(&"--verbose".to_string()));
        let output_idx = claude_launch
            .args
            .iter()
            .position(|arg| arg == "--output-format")
            .expect("claude output format flag should be present");
        assert_eq!(
            claude_launch.args.get(output_idx + 1).map(String::as_str),
            Some("stream-json")
        );
    }
}
