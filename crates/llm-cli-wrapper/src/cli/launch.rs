//! Runtime launch parsing and normalization utilities shared by runners.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde_json::Value;

use crate::error::Result;

use super::types::CliType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchInvocation {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub prompt_via_stdin: bool,
}

/// Look up a binary in PATH and return its full path if found.
///
/// This function provides cross-platform binary lookup:
/// - On Unix: uses the `which` command
/// - On Windows: uses the `where` command
/// - On other platforms: returns None
pub fn lookup_binary_in_path(binary_name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("which").arg(binary_name).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }

    #[cfg(windows)]
    {
        let output = std::process::Command::new("where").arg(binary_name).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let first_line =
            String::from_utf8_lossy(&output.stdout).lines().next().map(str::trim).unwrap_or_default().to_string();
        if first_line.is_empty() {
            None
        } else {
            Some(PathBuf::from(first_line))
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = binary_name;
        None
    }
}

/// Check if a binary is available on PATH.
///
/// Convenience wrapper around [`lookup_binary_in_path`].
pub fn is_binary_on_path(binary_name: &str) -> bool {
    lookup_binary_in_path(binary_name).is_some()
}

pub fn parse_cli_type(name: &str) -> Option<CliType> {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude" => Some(CliType::Claude),
        "codex" => Some(CliType::Codex),
        "gemini" => Some(CliType::Gemini),
        "opencode" | "open-code" => Some(CliType::OpenCode),
        "oai-runner" | "ao-oai-runner" => Some(CliType::OaiRunner),
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
    let file_name = Path::new(trimmed).file_name().and_then(|value| value.to_str()).unwrap_or(trimmed);
    file_name.to_ascii_lowercase()
}

/// Ensure a flag is present in args, inserting it at the specified position if missing.
pub fn ensure_flag(args: &mut Vec<String>, flag: &str, insert_at: usize) {
    if args.iter().any(|value| value == flag) {
        return;
    }
    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, flag.to_string());
}

/// Ensure a flag-value pair is present in args, updating or inserting as needed.
pub fn ensure_flag_value(args: &mut Vec<String>, flag: &str, value: &str, insert_at: usize) {
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

/// Ensure a flag is present in JSON args, inserting it at the specified position if missing.
pub fn ensure_flag_value_json(args: &mut Vec<Value>, flag: &str, value: &str, insert_at: usize) {
    if args.iter().any(|item| item.as_str().is_some_and(|existing| existing == flag)) {
        return;
    }

    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, Value::String(flag.to_string()));
    args.insert((insert_at + 1).min(args.len()), Value::String(value.to_string()));
}

/// Ensure a Codex config override is present in args (for `Vec<String>`).
///
/// Codex uses `-c key=value` or `--config key=value` for configuration overrides.
/// This function ensures a specific config key has the desired value expression.
pub fn ensure_codex_config_override(args: &mut Vec<String>, key: &str, value_expr: &str) {
    let key_prefix = format!("{key}=");
    let target = format!("{key}={value_expr}");

    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str();
        if flag == "-c" || flag == "--config" {
            if args[index + 1].starts_with(&key_prefix) {
                args[index + 1] = target;
                return;
            }
            index += 2;
            continue;
        }
        index += 1;
    }

    // Keep prompt payload as the final argv token when present.
    let insert_at = args.len().saturating_sub(1);
    args.insert(insert_at, "-c".to_string());
    args.insert(insert_at + 1, target);
}

/// Ensure a Codex config override is present in JSON args (for `Vec<Value>`).
///
/// Codex uses `-c key=value` or `--config key=value` for configuration overrides.
/// This function ensures a specific config key has the desired value expression.
pub fn ensure_codex_config_override_json(args: &mut Vec<Value>, key: &str, value_expr: &str) {
    let key_prefix = format!("{key}=");
    let target = format!("{key}={value_expr}");

    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str().unwrap_or_default();
        let value = args.get(index + 1).and_then(Value::as_str).unwrap_or_default();
        if (flag == "-c" || flag == "--config") && value.starts_with(&key_prefix) {
            args[index + 1] = Value::String(target);
            return;
        }
        index += 1;
    }

    let insert_at = codex_exec_insert_index_json(args);
    args.insert(insert_at, Value::String("-c".to_string()));
    args.insert(insert_at + 1, Value::String(target));
}

/// Find the insertion index for Codex exec flags in JSON args.
pub fn codex_exec_insert_index_json(args: &[Value]) -> usize {
    args.iter().position(|item| item.as_str().is_some_and(|value| value == "exec")).unwrap_or(0)
}

/// Find the insertion index for prompt-related flags in JSON args.
pub fn launch_prompt_insert_index_json(args: &[Value]) -> usize {
    args.len().saturating_sub(1)
}

pub fn ensure_machine_json_output(invocation: &mut LaunchInvocation) {
    let cli = canonical_cli_name(&invocation.command);

    match cli.as_str() {
        "codex" => {
            let insert_at =
                invocation.args.iter().position(|entry| entry == "exec").map(|index| index + 1).unwrap_or(0);
            ensure_flag(&mut invocation.args, "--json", insert_at);
        }
        "claude" => {
            let insert_at =
                invocation.args.iter().position(|entry| entry == "--print").map(|index| index + 1).unwrap_or(0);
            ensure_flag(&mut invocation.args, "--verbose", insert_at);
            ensure_flag_value(&mut invocation.args, "--output-format", "stream-json", insert_at);
        }
        "gemini" => {
            let insert_at = invocation.args.iter().position(|entry| entry == "-p").unwrap_or(invocation.args.len());
            ensure_flag_value(&mut invocation.args, "--output-format", "json", insert_at);
        }
        "opencode" => {
            let insert_at = invocation.args.iter().position(|entry| entry == "run").map(|index| index + 1).unwrap_or(0);
            ensure_flag_value(&mut invocation.args, "--format", "json", insert_at);
        }
        "ao-oai-runner" | "oai-runner" => {
            let insert_at = invocation.args.iter().position(|entry| entry == "run").map(|index| index + 1).unwrap_or(0);
            ensure_flag_value(&mut invocation.args, "--format", "json", insert_at);
        }
        _ => {}
    }
}

pub fn parse_launch_from_runtime_contract(runtime_contract: Option<&Value>) -> Result<Option<LaunchInvocation>> {
    let Some(contract) = runtime_contract else {
        return Ok(None);
    };

    let Some(launch) = contract.pointer("/cli/launch").or_else(|| contract.get("launch")) else {
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

    let prompt_via_stdin = launch.get("prompt_via_stdin").and_then(Value::as_bool).unwrap_or(false);
    let env = launch
        .get("env")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string())).ok_or_else(|| {
                        anyhow!("Invalid runtime contract launch env for key '{}': expected string value", key)
                    })
                })
                .collect::<std::result::Result<BTreeMap<_, _>, anyhow::Error>>()
        })
        .transpose()?
        .unwrap_or_default();

    let mut invocation = LaunchInvocation { command: command.to_string(), args, env, prompt_via_stdin };

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
        let idx = launch.args.iter().position(|arg| arg == "--format").expect("opencode format flag should be present");
        assert_eq!(launch.args.get(idx + 1).map(String::as_str), Some("json"));
        assert!(launch.env.is_empty());

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
        assert_eq!(claude_launch.args.get(output_idx + 1).map(String::as_str), Some("stream-json"));
    }

    #[test]
    fn parse_launch_preserves_environment_variables() {
        let contract = json!({
            "cli": {
                "launch": {
                    "command": "codex",
                    "args": ["exec", "hello"],
                    "env": {
                        "SKILL_MODE": "review",
                        "AO_FLAG": "1"
                    },
                    "prompt_via_stdin": false
                }
            }
        });

        let launch = parse_launch_from_runtime_contract(Some(&contract))
            .expect("launch should parse")
            .expect("launch should be present");
        assert_eq!(launch.env.get("SKILL_MODE").map(String::as_str), Some("review"));
        assert_eq!(launch.env.get("AO_FLAG").map(String::as_str), Some("1"));
    }
}
