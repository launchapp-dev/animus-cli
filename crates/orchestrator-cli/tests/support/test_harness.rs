use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub(crate) struct CliHarness {
    binary_path: PathBuf,
    project_root: TempDir,
    config_root: TempDir,
}

impl CliHarness {
    pub(crate) fn new() -> Result<Self> {
        let binary_path = assert_cmd::cargo::cargo_bin!("ao").to_path_buf();
        let project_root = tempfile::tempdir().context("failed to create project root tempdir")?;
        let config_root = tempfile::tempdir().context("failed to create config root tempdir")?;
        Ok(Self {
            binary_path,
            project_root,
            config_root,
        })
    }

    pub(crate) fn project_root(&self) -> &Path {
        self.project_root.path()
    }

    pub(crate) fn run_json_ok(&self, args: &[&str]) -> Result<Value> {
        let output = Command::new(&self.binary_path)
            .arg("--json")
            .arg("--project-root")
            .arg(self.project_root.path())
            .args(args)
            .env("AO_CONFIG_DIR", self.config_root.path())
            .env("AGENT_ORCHESTRATOR_CONFIG_DIR", self.config_root.path())
            .output()
            .with_context(|| format!("failed to execute ao command: {}", args.join(" ")))?;

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

        let payload = serde_json::from_slice::<Value>(&output.stdout).with_context(|| {
            format!(
                "failed to parse json output from ao command: {}",
                args.join(" ")
            )
        })?;

        if payload.get("schema").and_then(Value::as_str) != Some("ao.cli.v1") {
            anyhow::bail!(
                "unexpected schema for command {}: {}",
                args.join(" "),
                payload
            );
        }
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            anyhow::bail!(
                "command returned non-ok envelope for {}: {}",
                args.join(" "),
                payload
            );
        }

        Ok(payload)
    }
}
