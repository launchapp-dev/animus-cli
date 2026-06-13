use anyhow::{anyhow, Context, Result};
use protocol::CLI_SCHEMA_ID;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn parse_envelope_from_bytes(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).context("output was not valid utf-8")?;
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .ok_or_else(|| anyhow!("no JSON envelope found in output:\n{text}"))
}

pub struct CliHarness {
    binary_path: PathBuf,
    project_root: TempDir,
    config_root: SharedConfigRoot,
}

enum SharedConfigRoot {
    Owned(TempDir),
    Borrowed(PathBuf),
}

impl SharedConfigRoot {
    fn path(&self) -> &Path {
        match self {
            SharedConfigRoot::Owned(dir) => dir.path(),
            SharedConfigRoot::Borrowed(path) => path.as_path(),
        }
    }
}

impl CliHarness {
    pub fn new() -> Result<Self> {
        let binary_path = assert_cmd::cargo::cargo_bin!("animus").to_path_buf();
        let project_root = tempfile::tempdir().context("failed to create project root tempdir")?;
        let config_root = tempfile::tempdir().context("failed to create config root tempdir")?;
        Ok(Self { binary_path, project_root, config_root: SharedConfigRoot::Owned(config_root) })
    }

    /// Build a new harness that reuses the previous harness's HOME/config tempdir so the
    /// registry cache (and any other home-scoped state) persists across calls.
    pub fn with_existing_home(other: &Self) -> Result<Self> {
        let binary_path = assert_cmd::cargo::cargo_bin!("animus").to_path_buf();
        let project_root = tempfile::tempdir().context("failed to create project root tempdir")?;
        Ok(Self {
            binary_path,
            project_root,
            config_root: SharedConfigRoot::Borrowed(other.config_root.path().to_path_buf()),
        })
    }

    pub fn project_root(&self) -> &Path {
        self.project_root.path()
    }

    pub fn config_root(&self) -> &Path {
        self.config_root.path()
    }

    pub fn scoped_root(&self) -> PathBuf {
        let scope = protocol::repository_scope_for_path(self.project_root.path());
        self.config_root.path().join(".animus").join(scope)
    }

    pub fn run_json_ok(&self, args: &[&str]) -> Result<Value> {
        let output = self.run_json_command(args)?;
        self.expect_json_ok(args, output)
    }

    pub fn run_json_ok_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<Value> {
        let output = self.run_json_command_with_env(args, envs)?;
        self.expect_json_ok(args, output)
    }

    /// Parse the stdout `animus.cli.v1` envelope without asserting the exit
    /// code. Used by commands like `doctor` that emit a successful data
    /// envelope on stdout but may still exit non-zero (gateable exit
    /// contract) — returns the parsed payload alongside the exit code.
    pub fn run_json_stdout_with_exit(&self, args: &[&str]) -> Result<(Value, i32)> {
        let output = self.run_json_command(args)?;
        let payload = parse_envelope_from_bytes(&output.stdout)
            .with_context(|| format!("failed to parse json stdout from animus command: {}", args.join(" ")))?;
        if payload.get("schema").and_then(Value::as_str) != Some(CLI_SCHEMA_ID) {
            anyhow::bail!("unexpected schema for command {}: {}", args.join(" "), payload);
        }
        Ok((payload, output.status.code().unwrap_or(-1)))
    }

    fn expect_json_ok(&self, args: &[&str], output: std::process::Output) -> Result<Value> {
        if !output.status.success() {
            anyhow::bail!(
                "command failed ({:?}): animus --json --project-root {} {}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                self.project_root.path().display(),
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let payload = parse_envelope_from_bytes(&output.stdout)
            .with_context(|| format!("failed to parse json output from animus command: {}", args.join(" ")))?;

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
        self.expect_json_err_with_exit(args, output)
    }

    pub fn run_json_err_with_exit_and_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<(Value, i32)> {
        let output = self.run_json_command_with_env(args, envs)?;
        self.expect_json_err_with_exit(args, output)
    }

    fn expect_json_err_with_exit(&self, args: &[&str], output: std::process::Output) -> Result<(Value, i32)> {
        if output.status.success() {
            anyhow::bail!(
                "expected command to fail but it succeeded: animus --json --project-root {} {}\nstdout:\n{}\nstderr:\n{}",
                self.project_root.path().display(),
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let payload = parse_envelope_from_bytes(&output.stderr)
            .with_context(|| format!("failed to parse json error output from animus command: {}", args.join(" ")))?;

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

    /// Run a command but fail (and kill the child) if it does not exit within
    /// `deadline`. Use for commands where a regression could make the process
    /// never exit, so the test reports a failure instead of hanging the suite.
    pub fn run_json_output_within(&self, args: &[&str], deadline: std::time::Duration) -> Result<std::process::Output> {
        let mut command = self.build_json_command(args, &[]);
        command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child =
            command.spawn().with_context(|| format!("failed to spawn animus command: {}", args.join(" ")))?;
        let started = std::time::Instant::now();
        loop {
            match child.try_wait().context("failed to poll animus command")? {
                Some(_) => return child.wait_with_output().context("failed to collect animus command output"),
                None if started.elapsed() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "command did not exit within {:?}: animus --json --project-root {} {}",
                        deadline,
                        self.project_root.path().display(),
                        args.join(" ")
                    );
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }

    fn run_json_command(&self, args: &[&str]) -> Result<std::process::Output> {
        self.run_json_command_with_env(args, &[])
    }

    fn run_json_command_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<std::process::Output> {
        self.build_json_command(args, envs)
            .output()
            .with_context(|| format!("failed to execute animus command: {}", args.join(" ")))
    }

    fn build_json_command(&self, args: &[&str], envs: &[(&str, &str)]) -> Command {
        let mut command = Command::new(&self.binary_path);
        command
            .arg("--json")
            .arg("--project-root")
            .arg(self.project_root.path())
            .args(args)
            .env("HOME", self.config_root.path())
            .env("XDG_CONFIG_HOME", self.config_root.path())
            .env("ANIMUS_CONFIG_DIR", self.config_root.path())
            .env("AGENT_ORCHESTRATOR_CONFIG_DIR", self.config_root.path())
            .env("ANIMUS_SKIP_RUNNER_START", "1");
        for (key, value) in envs {
            command.env(key, value);
        }
        command
    }
}
