use anyhow::Result;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

pub async fn execute_command(working_dir: &Path, command: &str, timeout_secs: Option<u64>) -> Result<String> {
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(300));

    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Ok(format!("Command failed: {}", e)),
        Err(_) => {
            return Ok(format!("Command timed out after {}s", timeout.as_secs()));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);

    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(&stderr);
    }

    if exit_code != 0 {
        result.push_str(&format!("\n[exit code: {}]", exit_code));
    }

    if result.len() > 50_000 {
        let truncated = &result[..50_000];
        return Ok(format!("{}...\n[output truncated at 50000 chars]", truncated));
    }

    Ok(result)
}
