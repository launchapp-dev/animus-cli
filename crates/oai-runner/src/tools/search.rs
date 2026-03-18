use anyhow::Result;
use std::path::Path;

pub fn search_files(
    working_dir: &Path,
    pattern: &str,
    search_path: Option<&str>,
    include: Option<&str>,
) -> Result<String> {
    let base = match search_path {
        Some(p) if !p.is_empty() => working_dir.join(p),
        _ => working_dir.to_path_buf(),
    };

    let output = if let Ok(rg_output) = try_ripgrep(&base, pattern, include) {
        rg_output
    } else {
        try_grep(&base, pattern, include)?
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stderr.is_empty() && output.status.code() != Some(1) {
        return Ok(format!("Search error: {}", stderr));
    }

    if stdout.is_empty() {
        return Ok("No matches found.".to_string());
    }

    let wd_prefix = working_dir.to_string_lossy();
    let lines: Vec<String> = stdout
        .lines()
        .take(200)
        .map(|line| {
            if line.starts_with(wd_prefix.as_ref()) {
                line[wd_prefix.len()..].trim_start_matches('/').to_string()
            } else {
                line.to_string()
            }
        })
        .collect();

    let total = stdout.lines().count();
    let mut result = lines.join("\n");
    if total > 200 {
        result.push_str(&format!("\n... ({} total matches, showing first 200)", total));
    }

    Ok(result)
}

fn try_ripgrep(base: &Path, pattern: &str, include: Option<&str>) -> Result<std::process::Output> {
    let mut cmd = std::process::Command::new("rg");
    cmd.arg("-n").arg("--color=never").arg(pattern);

    if let Some(inc) = include {
        cmd.arg("--glob").arg(inc);
    }

    cmd.arg(base.to_string_lossy().as_ref());

    cmd.output().map_err(|e| anyhow::anyhow!("rg not available: {}", e))
}

fn try_grep(base: &Path, pattern: &str, include: Option<&str>) -> Result<std::process::Output> {
    let mut cmd = std::process::Command::new("grep");
    cmd.arg("-rn").arg("--color=never").arg("-E").arg(pattern);

    for dir in &["target", ".git", "node_modules", ".ao", "__pycache__", ".next", "dist", "build"] {
        cmd.arg("--exclude-dir").arg(dir);
    }

    if let Some(inc) = include {
        cmd.arg("--include").arg(inc);
    }

    cmd.arg(base.to_string_lossy().as_ref());

    cmd.output().map_err(|e| anyhow::anyhow!("Failed to execute grep: {}", e))
}
