use anyhow::Result;
use std::path::Path;
use std::time::Duration;

pub fn execute_command(working_dir: &Path, command: &str, timeout_secs: Option<u64>) -> Result<String> {
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(120));

    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;

    let output = match wait_with_timeout(child, timeout) {
        Ok(output) => output,
        Err(e) => return Ok(format!("Command timed out after {}s: {}", timeout.as_secs(), e)),
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

fn wait_with_timeout(child: std::process::Child, timeout: Duration) -> Result<std::process::Output> {
    let start = std::time::Instant::now();
    let mut child = child;

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let output = child.wait_with_output()?;
                return Ok(output);
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    anyhow::bail!("timed out");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => anyhow::bail!("Error waiting for process: {}", e),
        }
    }
}
