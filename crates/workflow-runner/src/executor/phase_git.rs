use anyhow::{Context, Result};
use std::process::{Command as ProcessCommand, Stdio};

fn git_status(cwd: &str, args: &[&str], operation: &str) -> Result<()> {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run git operation '{operation}' in {}", cwd))?;
    if !status.success() {
        anyhow::bail!(
            "git operation '{}' failed in {}: git {}",
            operation,
            cwd,
            args.join(" ")
        );
    }
    Ok(())
}

pub fn is_git_repo(project_root: &str) -> bool {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn git_has_pending_changes(cwd: &str) -> Result<bool> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to inspect git status in {}", cwd))?;

    if !output.status.success() {
        anyhow::bail!("git status --porcelain failed in {}", cwd);
    }

    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

pub fn ensure_git_identity(cwd: &str) -> Result<()> {
    let email = format!("{}@local", protocol::ACTOR_DAEMON);
    for (key, default_value) in [
        ("user.name", "AO Daemon"),
        ("user.email", email.as_str()),
    ] {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["config", "--get", key])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to read git config {} in {}", key, cwd))?;

        let configured =
            output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty();
        if !configured {
            git_status(
                cwd,
                &["config", key, default_value],
                "configure git identity",
            )?;
        }
    }

    Ok(())
}

pub fn commit_implementation_changes(cwd: &str, commit_message: &str) -> Result<()> {
    let commit_message = commit_message.trim();
    if commit_message.is_empty() {
        anyhow::bail!("implementation phase requires a non-empty commit message");
    }
    if !is_git_repo(cwd) {
        anyhow::bail!(
            "implementation phase requires a git repository for commit at {}",
            cwd
        );
    }
    if !git_has_pending_changes(cwd)? {
        anyhow::bail!(
            "implementation phase requires file changes to commit, but no changes were detected in {}",
            cwd
        );
    }

    ensure_git_identity(cwd)?;
    git_status(cwd, &["add", "-A"], "stage implementation changes")?;
    git_status(
        cwd,
        &["commit", "-m", commit_message],
        "commit implementation changes",
    )?;
    Ok(())
}
