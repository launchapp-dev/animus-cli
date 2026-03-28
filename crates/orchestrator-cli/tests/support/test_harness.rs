use anyhow::{Context, Result};
use protocol::CLI_SCHEMA_ID;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub struct CliHarness {
    binary_path: PathBuf,
    project_root: TempDir,
    config_root: TempDir,
}

impl CliHarness {
    pub fn new() -> Result<Self> {
        let binary_path = assert_cmd::cargo::cargo_bin!("ao").to_path_buf();
        let project_root = tempfile::tempdir().context("failed to create project root tempdir")?;
        let config_root = tempfile::tempdir().context("failed to create config root tempdir")?;
        Ok(Self { binary_path, project_root, config_root })
    }

    pub fn project_root(&self) -> &Path {
        self.project_root.path()
    }

    pub fn config_root(&self) -> &Path {
        self.config_root.path()
    }

    pub fn scoped_root(&self) -> PathBuf {
        let scope = protocol::repository_scope_for_path(self.project_root.path());
        self.config_root.path().join(".ao").join(scope)
    }

    pub fn run_json_ok(&self, args: &[&str]) -> Result<Value> {
        let output = self.run_json_command(args)?;

        if !output.status.success() {
            anyhow::bail!(
                "command failed ({:?}): ao --json --project-root {} {}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                self.project_root.path().display(),
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let payload = serde_json::from_slice::<Value>(&output.stdout)
            .with_context(|| format!("failed to parse json output from ao command: {}", args.join(" ")))?;

        if payload.get("schema").and_then(Value::as_str) != Some(CLI_SCHEMA_ID) {
            anyhow::bail!("unexpected schema for command {}: {}", args.join(" "), payload);
        }
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            anyhow::bail!("command returned non-ok envelope for {}: {}", args.join(" "), payload);
        }

        Ok(payload)
    }

    pub fn run_json_err(&self, args: &[&str]) -> Result<Value> {
        let (payload, _) = self.run_json_err_with_exit(args)?;
        Ok(payload)
    }

    pub fn run_json_err_with_exit(&self, args: &[&str]) -> Result<(Value, i32)> {
        let output = self.run_json_command(args)?;

        if output.status.success() {
            anyhow::bail!(
                "expected command to fail but it succeeded: ao --json --project-root {} {}\nstdout:\n{}\nstderr:\n{}",
                self.project_root.path().display(),
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let payload = serde_json::from_slice::<Value>(&output.stderr)
            .with_context(|| format!("failed to parse json error output from ao command: {}", args.join(" ")))?;

        if payload.get("schema").and_then(Value::as_str) != Some(CLI_SCHEMA_ID) {
            anyhow::bail!("unexpected schema for failing command {}: {}", args.join(" "), payload);
        }
        if payload.get("ok").and_then(Value::as_bool) != Some(false) {
            anyhow::bail!("expected non-ok envelope for failing command {}: {}", args.join(" "), payload);
        }

        Ok((payload, output.status.code().unwrap_or(-1)))
    }

    pub fn run_json_output(&self, args: &[&str]) -> Result<std::process::Output> {
        self.run_json_command(args)
    }

    fn run_json_command(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new(&self.binary_path)
            .arg("--json")
            .arg("--project-root")
            .arg(self.project_root.path())
            .args(args)
            .env("HOME", self.config_root.path())
            .env("XDG_CONFIG_HOME", self.config_root.path())
            .env("AO_CONFIG_DIR", self.config_root.path())
            .env("AGENT_ORCHESTRATOR_CONFIG_DIR", self.config_root.path())
            .env("AO_SKIP_RUNNER_START", "1")
            .output()
            .with_context(|| format!("failed to execute ao command: {}", args.join(" ")))
    }
}
