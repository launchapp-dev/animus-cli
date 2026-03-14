use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use orchestrator_core::services::ServiceHub;
use orchestrator_core::DaemonStatus;
use orchestrator_daemon_runtime::DaemonRuntimeState;
use orchestrator_notifications::{
    clear_notification_config, parse_notification_config_value,
    read_notification_config_from_pm_config, serialize_notification_config,
    NOTIFICATION_CONFIG_SCHEMA,
};

use crate::{
    print_ok, print_value, DaemonCommand, DaemonConfigArgs, DaemonEventsArgs, DaemonStartArgs,
    DaemonStopArgs, RunnerScopeArg,
};

mod daemon_events;
pub(crate) mod daemon_reconciliation;
mod daemon_run;
mod daemon_run_host;
pub(crate) mod daemon_scheduler;

use daemon_events::handle_daemon_events_impl;
use daemon_run::handle_daemon_run;

pub(crate) use daemon_events::{daemon_events_log_path, poll_daemon_events, DaemonEventRecord};

use protocol::{is_process_alive, terminate_process};

const AUTONOMOUS_STARTUP_PROBE_SECS: u64 = 3;
const AUTONOMOUS_STARTUP_PROBE_POLL_MILLIS: u64 = 100;
const AUTONOMOUS_STARTUP_LOG_TAIL_LINES: usize = 40;
const AUTONOMOUS_STARTUP_LOG_TAIL_MAX_CHARS: usize = 8_000;
const AUTONOMOUS_STARTUP_LOG_MAX_READ_BYTES: u64 = 128 * 1024;

struct AutonomousDaemonSpawn {
    child: Child,
    log_path: PathBuf,
    startup_log_offset: u64,
}

fn runner_scope_value(scope: &RunnerScopeArg) -> &'static str {
    match scope {
        RunnerScopeArg::Project => "project",
        RunnerScopeArg::Global => "global",
    }
}

pub(crate) fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate
        .canonicalize()
        .unwrap_or(candidate)
        .to_string_lossy()
        .to_string()
}

pub(super) fn get_daemon_pid(project_root: &str) -> Result<Option<u32>> {
    DaemonRuntimeState::get_daemon_pid(project_root)
}

pub(super) fn set_daemon_pid(project_root: &str, daemon_pid: Option<u32>) -> Result<()> {
    DaemonRuntimeState::set_daemon_pid(project_root, daemon_pid)
}

pub(super) fn set_runtime_paused(project_root: &str, paused: bool) -> Result<()> {
    DaemonRuntimeState::set_runtime_paused(project_root, paused)
}

pub(super) fn set_shutdown_requested(
    project_root: &str,
    requested: bool,
    timeout_secs: Option<u64>,
) -> Result<()> {
    DaemonRuntimeState::set_shutdown_requested(project_root, requested, timeout_secs)
}

fn pm_config_path(project_root: &str) -> PathBuf {
    PathBuf::from(canonicalize_lossy(project_root))
        .join(".ao")
        .join("pm-config.json")
}

fn write_daemon_pid(project_root: &str, pid: u32) {
    DaemonRuntimeState::write_daemon_pid_file(project_root, pid);
}

fn remove_daemon_pid(project_root: &str) {
    DaemonRuntimeState::remove_daemon_pid_file(project_root);
}

fn read_daemon_pid(project_root: &str) -> Option<u32> {
    DaemonRuntimeState::read_daemon_pid_file(project_root)
}

pub(crate) fn autonomous_daemon_log_path(project_root: &str) -> PathBuf {
    let canonical_root = PathBuf::from(canonicalize_lossy(project_root));
    let scoped_runtime_root = dirs::home_dir().map(|home| {
        home.join(".ao")
            .join(protocol::repository_scope_for_path(&canonical_root))
    });

    scoped_runtime_root
        .unwrap_or_else(|| canonical_root.join(".ao"))
        .join("daemon")
        .join("daemon.log")
}

async fn wait_for_autonomous_startup_probe(
    child: &mut Child,
    probe_window: Duration,
) -> Result<Option<ExitStatus>> {
    let deadline = tokio::time::Instant::now() + probe_window;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to check autonomous daemon process state")?
        {
            return Ok(Some(status));
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }

        let remaining = deadline.saturating_duration_since(now);
        let sleep_for = remaining.min(Duration::from_millis(AUTONOMOUS_STARTUP_PROBE_POLL_MILLIS));
        tokio::time::sleep(sleep_for).await;
    }
}

fn read_autonomous_startup_log_tail(
    log_path: &Path,
    startup_log_offset: u64,
    max_lines: usize,
) -> Result<Option<String>> {
    if max_lines == 0 {
        return Ok(None);
    }

    let mut file = match std::fs::File::open(log_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let file_len = file
        .metadata()
        .with_context(|| format!("failed to read log metadata for {}", log_path.display()))?
        .len();
    if file_len <= startup_log_offset {
        return Ok(None);
    }

    let available_bytes = file_len - startup_log_offset;
    let read_bytes = available_bytes.min(AUTONOMOUS_STARTUP_LOG_MAX_READ_BYTES);
    let read_start = file_len - read_bytes;

    file.seek(SeekFrom::Start(read_start))
        .with_context(|| format!("failed to seek startup log {}", log_path.display()))?;
    let mut bytes = Vec::with_capacity(read_bytes as usize);
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to read startup log {}", log_path.display()))?;

    let mut content = String::from_utf8_lossy(&bytes).into_owned();
    if read_start > startup_log_offset {
        if let Some((_, remainder)) = content.split_once('\n') {
            content = remainder.to_string();
        }
    }

    let mut lines = content
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }

    if lines.is_empty() {
        return Ok(None);
    }

    Ok(Some(lines.join("\n")))
}

fn truncate_from_end(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_string();
    }

    let suffix = value
        .chars()
        .skip(char_count.saturating_sub(max_chars))
        .collect::<String>();
    format!("...[truncated]\n{suffix}")
}

fn autonomous_startup_log_diagnostics(log_path: &Path, startup_log_offset: u64) -> String {
    match read_autonomous_startup_log_tail(
        log_path,
        startup_log_offset,
        AUTONOMOUS_STARTUP_LOG_TAIL_LINES,
    ) {
        Ok(Some(tail)) => format!(
            "startup log tail (last {} lines):\n{}",
            AUTONOMOUS_STARTUP_LOG_TAIL_LINES,
            truncate_from_end(&tail, AUTONOMOUS_STARTUP_LOG_TAIL_MAX_CHARS)
        ),
        Ok(None) => "startup log tail unavailable: log file is missing or empty".to_string(),
        Err(err) => format!("startup log tail unavailable: failed to read log ({err})"),
    }
}

fn autonomous_startup_failure_error(
    daemon_pid: u32,
    exit_status: Option<ExitStatus>,
    log_path: &Path,
    startup_log_offset: u64,
) -> anyhow::Error {
    let status_detail = exit_status
        .map(|status| format!("process exited before startup probe completed ({status})"))
        .unwrap_or_else(|| "process was not alive after startup probe completed".to_string());
    let diagnostics = autonomous_startup_log_diagnostics(log_path, startup_log_offset);

    let log_tail = read_autonomous_startup_log_tail(
        log_path,
        startup_log_offset,
        AUTONOMOUS_STARTUP_LOG_TAIL_LINES,
    )
    .ok()
    .flatten();

    let message = format!(
        "autonomous daemon failed startup validation for pid {daemon_pid}: {status_detail}. startup log path: {}.\n{diagnostics}",
        log_path.display()
    );

    let mut details = serde_json::json!({
        "daemon_pid": daemon_pid,
        "log_path": log_path.display().to_string(),
    });
    if let Some(tail) = log_tail {
        details["startup_log_tail"] = serde_json::Value::String(truncate_from_end(
            &tail,
            AUTONOMOUS_STARTUP_LOG_TAIL_MAX_CHARS,
        ));
    }
    if let Some(status) = exit_status {
        details["exit_code"] = serde_json::json!(status.code());
    }

    crate::CliError::new(crate::CliErrorKind::Internal, message)
        .with_details(details)
        .into()
}

fn load_pm_config(project_root: &str) -> Result<serde_json::Value> {
    let path = pm_config_path(project_root);
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read daemon config at {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }

    serde_json::from_str(&content)
        .with_context(|| format!("invalid daemon config JSON at {}", path.display()))
}

fn save_pm_config(project_root: &str, value: &serde_json::Value) -> Result<()> {
    let path = pm_config_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(value).context("failed to serialize daemon config JSON")?;
    fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write daemon config at {}", path.display()))?;
    Ok(())
}

fn daemon_config_bool(config: &serde_json::Value, key: &str) -> Option<bool> {
    config.get(key).and_then(serde_json::Value::as_bool)
}

fn handle_daemon_config(args: DaemonConfigArgs, project_root: &str, json: bool) -> Result<()> {
    if args.notification_config_json.is_some() && args.notification_config_file.is_some() {
        anyhow::bail!(
            "--notification-config-json and --notification-config-file cannot be used together"
        );
    }

    let mut config = load_pm_config(project_root)?;
    if !config.is_object() {
        config = serde_json::json!({});
    }

    let mut updated = false;
    if let Some(enabled) = args.auto_merge {
        config["auto_merge_enabled"] = serde_json::Value::Bool(enabled);
        updated = true;
    }
    if let Some(enabled) = args.auto_pr {
        config["auto_pr_enabled"] = serde_json::Value::Bool(enabled);
        updated = true;
    }
    if let Some(enabled) = args.auto_commit_before_merge {
        config["auto_commit_before_merge"] = serde_json::Value::Bool(enabled);
        updated = true;
    }
    if let Some(enabled) = args.auto_prune_worktrees_after_merge {
        config["auto_prune_worktrees_after_merge"] = serde_json::Value::Bool(enabled);
        updated = true;
    }

    if args.clear_notification_config {
        clear_notification_config(&mut config);
        updated = true;
    }

    if let Some(raw_json) = args.notification_config_json.as_deref() {
        let value: serde_json::Value =
            serde_json::from_str(raw_json).context("failed to parse --notification-config-json")?;
        let notification_config = parse_notification_config_value(&value)?;
        config["notification_config"] = serialize_notification_config(&notification_config)?;
        updated = true;
    }

    if let Some(config_path) = args.notification_config_file.as_deref() {
        let raw_json = fs::read_to_string(config_path).with_context(|| {
            format!(
                "failed to read daemon notification config file at {}",
                config_path
            )
        })?;
        let value: serde_json::Value =
            serde_json::from_str(raw_json.as_str()).with_context(|| {
                format!(
                    "failed to parse daemon notification config file at {}",
                    config_path
                )
            })?;
        let notification_config = parse_notification_config_value(&value)?;
        config["notification_config"] = serialize_notification_config(&notification_config)?;
        updated = true;
    }

    if updated {
        save_pm_config(project_root, &config)?;
    }

    let notification_config = read_notification_config_from_pm_config(&config).unwrap_or_default();

    print_value(
        serde_json::json!({
            "config_path": pm_config_path(project_root).display().to_string(),
            "auto_merge_enabled": daemon_config_bool(&config, "auto_merge_enabled").unwrap_or(false),
            "auto_pr_enabled": daemon_config_bool(&config, "auto_pr_enabled").unwrap_or(false),
            "auto_commit_before_merge": daemon_config_bool(&config, "auto_commit_before_merge").unwrap_or(false),
            "auto_prune_worktrees_after_merge": daemon_config_bool(&config, "auto_prune_worktrees_after_merge").unwrap_or(false),
            "notification_config_schema": NOTIFICATION_CONFIG_SCHEMA,
            "notification_config": serialize_notification_config(&notification_config)?,
            "updated": updated
        }),
        json,
    )
}

fn spawn_autonomous_daemon_run(
    project_root: &str,
    args: &DaemonStartArgs,
) -> Result<AutonomousDaemonSpawn> {
    let current_exe = std::env::current_exe().context("failed to resolve current ao binary")?;
    let log_path = autonomous_daemon_log_path(project_root);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create autonomous daemon log directory {}",
                parent.display()
            )
        })?;
    }
    let stdout_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| {
            format!(
                "failed to open autonomous daemon log file {}",
                log_path.display()
            )
        })?;
    let startup_log_offset = stdout_log
        .metadata()
        .with_context(|| {
            format!(
                "failed to read autonomous daemon log metadata {}",
                log_path.display()
            )
        })?
        .len();
    let stderr_log = stdout_log.try_clone().with_context(|| {
        format!(
            "failed to clone autonomous daemon log handle {}",
            log_path.display()
        )
    })?;

    let mut command = ProcessCommand::new(current_exe);
    command
        .arg("--project-root")
        .arg(project_root)
        .arg("daemon")
        .arg("run")
        .arg("--interval-secs")
        .arg(args.scheduler.interval_secs.to_string())
        .arg("--auto-run-ready")
        .arg(args.scheduler.auto_run_ready.to_string())
        .arg("--startup-cleanup")
        .arg(args.scheduler.startup_cleanup.to_string())
        .arg("--resume-interrupted")
        .arg(args.scheduler.resume_interrupted.to_string())
        .arg("--reconcile-stale")
        .arg(args.scheduler.reconcile_stale.to_string())
        .arg("--stale-threshold-hours")
        .arg(args.scheduler.stale_threshold_hours.to_string())
        .arg("--max-tasks-per-tick")
        .arg(args.scheduler.max_tasks_per_tick.to_string());
    if let Some(pool_size) = args.scheduler.pool_size {
        command.arg("--pool-size").arg(pool_size.to_string());
    }
    command
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .stdin(Stdio::null());
    if let Some(auto_merge) = args.scheduler.auto_merge {
        command.arg("--auto-merge").arg(auto_merge.to_string());
    }
    if let Some(auto_pr) = args.scheduler.auto_pr {
        command.arg("--auto-pr").arg(auto_pr.to_string());
    }
    if let Some(auto_commit_before_merge) = args.scheduler.auto_commit_before_merge {
        command
            .arg("--auto-commit-before-merge")
            .arg(auto_commit_before_merge.to_string());
    }
    if let Some(auto_prune_worktrees_after_merge) =
        args.scheduler.auto_prune_worktrees_after_merge
    {
        command
            .arg("--auto-prune-worktrees-after-merge")
            .arg(auto_prune_worktrees_after_merge.to_string());
    }
    if let Some(phase_timeout_secs) = args.scheduler.phase_timeout_secs {
        command
            .arg("--phase-timeout-secs")
            .arg(phase_timeout_secs.to_string());
    }
    if let Some(idle_timeout_secs) = args.scheduler.idle_timeout_secs {
        command
            .arg("--idle-timeout-secs")
            .arg(idle_timeout_secs.to_string());
    }

    if let Some(pool_size) = args.scheduler.pool_size {
        command.env("AO_MAX_AGENTS", pool_size.to_string());
    }
    if args.skip_runner {
        command.env("AO_SKIP_RUNNER_START", "1");
    }
    if let Some(scope) = args.runner_scope.as_ref() {
        command.env("AO_RUNNER_SCOPE", runner_scope_value(scope));
    }

    command.env_remove("CLAUDECODE");
    command.env_remove("CLAUDE_CODE_ENTRYPOINT");

    let child = command
        .spawn()
        .context("failed to spawn autonomous daemon run")?;
    Ok(AutonomousDaemonSpawn {
        child,
        log_path,
        startup_log_offset,
    })
}

pub(crate) async fn handle_daemon(
    command: DaemonCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let daemon = hub.daemon();

    match command {
        DaemonCommand::Start(args) => {
            if let Some(existing_pid) = get_daemon_pid(project_root)? {
                if is_process_alive(existing_pid) {
                    if args.autonomous {
                        let _ = set_runtime_paused(project_root, false);
                        return print_value(
                            serde_json::json!({
                                "message": "daemon already running",
                                "autonomous": true,
                                "daemon_pid": existing_pid,
                            }),
                            json,
                        );
                    }
                    return Err(anyhow!(
                        "autonomous daemon is already running (pid {}); stop it before non-autonomous start",
                        existing_pid
                    ));
                }
                let _ = set_daemon_pid(project_root, None);
            }

            if args.autonomous {
                let mut daemon_spawn = spawn_autonomous_daemon_run(project_root, &args)?;
                let daemon_pid = daemon_spawn.child.id();
                let startup_status = wait_for_autonomous_startup_probe(
                    &mut daemon_spawn.child,
                    Duration::from_secs(AUTONOMOUS_STARTUP_PROBE_SECS),
                )
                .await?;
                if startup_status.is_some() {
                    let _ = set_daemon_pid(project_root, None);
                    return Err(autonomous_startup_failure_error(
                        daemon_pid,
                        startup_status,
                        daemon_spawn.log_path.as_path(),
                        daemon_spawn.startup_log_offset,
                    ));
                }

                if !is_process_alive(daemon_pid) {
                    let _ = set_daemon_pid(project_root, None);
                    return Err(autonomous_startup_failure_error(
                        daemon_pid,
                        None,
                        daemon_spawn.log_path.as_path(),
                        daemon_spawn.startup_log_offset,
                    ));
                }

                drop(daemon_spawn.child);
                let _ = set_daemon_pid(project_root, Some(daemon_pid));
                write_daemon_pid(project_root, daemon_pid);

                if let Ok(Some(recorded_pid)) = get_daemon_pid(project_root) {
                    if recorded_pid != daemon_pid {
                        let _ = set_daemon_pid(project_root, None);
                        return Err(anyhow!(
                            "autonomous daemon startup validation failed: daemon-state.json recorded pid {} but expected {}",
                            recorded_pid,
                            daemon_pid
                        ));
                    }
                }

                let _ = set_runtime_paused(project_root, false);
                return print_value(
                    serde_json::json!({
                        "message": "daemon started",
                        "autonomous": true,
                        "daemon_pid": daemon_pid,
                    }),
                    json,
                );
            }

            if let Some(pool_size) = args.scheduler.pool_size {
                std::env::set_var("AO_MAX_AGENTS", pool_size.to_string());
            } else {
                std::env::remove_var("AO_MAX_AGENTS");
            }

            if args.skip_runner {
                std::env::set_var("AO_SKIP_RUNNER_START", "1");
            } else {
                std::env::remove_var("AO_SKIP_RUNNER_START");
            }

            if let Some(scope) = args.runner_scope {
                let scope = match scope {
                    RunnerScopeArg::Project => "project",
                    RunnerScopeArg::Global => "global",
                };
                std::env::set_var("AO_RUNNER_SCOPE", scope);
            } else {
                std::env::remove_var("AO_RUNNER_SCOPE");
            }

            let result = daemon.start(Default::default()).await;
            std::env::remove_var("AO_MAX_AGENTS");
            std::env::remove_var("AO_SKIP_RUNNER_START");
            std::env::remove_var("AO_RUNNER_SCOPE");
            if result.is_ok() {
                let _ = set_daemon_pid(project_root, None);
                let _ = set_runtime_paused(project_root, false);
            }
            result.map(|_| print_ok("daemon started", json))
        }
        DaemonCommand::Run(args) => handle_daemon_run(args, project_root, json).await,
        DaemonCommand::Events(args) => handle_daemon_events(args, json).await,
        DaemonCommand::Stop(args) => {
            handle_daemon_stop(args, hub.clone(), project_root, json).await?;
            Ok(())
        }
        DaemonCommand::Pause => {
            let _ = set_runtime_paused(project_root, true);
            let result = daemon.pause().await;
            result.map(|_| print_ok("daemon paused", json))
        }
        DaemonCommand::Resume => {
            let result = daemon.resume().await;
            if result.is_ok() {
                let _ = set_runtime_paused(project_root, false);
            }
            result.map(|_| print_ok("daemon resumed", json))
        }
        DaemonCommand::Status => {
            let mut status = daemon.status().await?;
            if let Some(pid) = read_daemon_pid(project_root) {
                let alive = is_process_alive(pid);
                if !alive && matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
                    status = DaemonStatus::Crashed;
                    remove_daemon_pid(project_root);
                    let _ = set_daemon_pid(project_root, None);
                }
            } else if matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
                status = DaemonStatus::Crashed;
            }
            print_value(status, json)
        }
        DaemonCommand::Health => {
            let mut health = daemon.health().await?;
            let pid = read_daemon_pid(project_root);
            if let Some(pid) = pid {
                let alive = is_process_alive(pid);
                health.daemon_pid = Some(pid);
                health.process_alive = Some(alive);
                if !alive && matches!(health.status, DaemonStatus::Running | DaemonStatus::Paused) {
                    health.status = DaemonStatus::Crashed;
                    health.healthy = false;
                    remove_daemon_pid(project_root);
                    let _ = set_daemon_pid(project_root, None);
                }
            } else if matches!(health.status, DaemonStatus::Running | DaemonStatus::Paused) {
                health.status = DaemonStatus::Crashed;
                health.healthy = false;
            }
            print_value(health, json)
        }
        DaemonCommand::Logs(args) => {
            handle_daemon_logs(args.limit, args.search, project_root, json)
        }
        DaemonCommand::ClearLogs => daemon
            .clear_logs()
            .await
            .map(|_| print_ok("daemon logs cleared", json)),
        DaemonCommand::Agents => {
            let active_agents = daemon.active_agents().await?;
            print_value(serde_json::json!({ "active_agents": active_agents }), json)
        }
        DaemonCommand::Config(args) => handle_daemon_config(args, project_root, json),
    }
}

async fn handle_daemon_stop(
    args: DaemonStopArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let daemon = hub.daemon();
    let existing_pid = get_daemon_pid(project_root)?;

    if let Some(pid) = existing_pid {
        if is_process_alive(pid) {
            let _ = set_shutdown_requested(project_root, true, Some(args.shutdown_timeout_secs));

            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(args.shutdown_timeout_secs);

            loop {
                if !is_process_alive(pid) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = terminate_process(pid);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }

            let _ = set_shutdown_requested(project_root, false, None);
            let _ = set_daemon_pid(project_root, None);
        } else {
            let _ = set_daemon_pid(project_root, None);
        }
    }

    remove_daemon_pid(project_root);
    let result = daemon.stop().await;
    if result.is_ok() {
        let _ = set_runtime_paused(project_root, true);
    }
    result?;

    let graceful = existing_pid
        .map(|pid| !is_process_alive(pid))
        .unwrap_or(true);

    print_value(
        serde_json::json!({
            "message": "daemon stopped",
            "graceful": graceful,
            "shutdown_timeout_secs": args.shutdown_timeout_secs,
        }),
        json,
    )
}

const DEFAULT_DAEMON_LOG_LINES: usize = 100;

fn handle_daemon_logs(
    limit: Option<usize>,
    search: Option<String>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let log_path = autonomous_daemon_log_path(project_root);
    let limit = limit.unwrap_or(DEFAULT_DAEMON_LOG_LINES);

    let content = match fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if json {
                return print_value(
                    serde_json::json!({
                        "log_path": log_path.display().to_string(),
                        "line_count": 0,
                        "lines": [],
                        "has_more": false,
                    }),
                    json,
                );
            }
            eprintln!("no daemon log file found at {}", log_path.display());
            return Ok(());
        }
        Err(err) => {
            return Err(anyhow!(
                "failed to read daemon log at {}: {}",
                log_path.display(),
                err
            ));
        }
    };

    let mut lines: Vec<&str> = content.lines().collect();

    if let Some(ref needle) = search {
        lines.retain(|line| line.contains(needle.as_str()));
    }

    let total = lines.len();
    let has_more = total > limit;
    if total > limit {
        lines = lines.split_off(total - limit);
    }

    if json {
        let json_lines: Vec<&str> = lines.clone();
        return print_value(
            serde_json::json!({
                "log_path": log_path.display().to_string(),
                "line_count": lines.len(),
                "lines": json_lines,
                "has_more": has_more,
            }),
            json,
        );
    }

    for line in &lines {
        println!("{line}");
    }
    Ok(())
}

async fn handle_daemon_events(args: DaemonEventsArgs, json: bool) -> Result<()> {
    handle_daemon_events_impl(args, json).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn read_autonomous_startup_log_tail_returns_last_nonempty_lines() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let log_path = temp.path().join("daemon.log");
        fs::write(&log_path, "line-1\n\nline-2\nline-3\nline-4\n")
            .expect("log file should be written");

        let tail = read_autonomous_startup_log_tail(log_path.as_path(), 0, 2)
            .expect("log tail should be readable")
            .expect("log tail should be present");
        assert_eq!(tail, "line-3\nline-4");
    }

    #[test]
    fn read_autonomous_startup_log_tail_returns_none_when_log_missing() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let log_path = temp.path().join("missing.log");
        let tail = read_autonomous_startup_log_tail(log_path.as_path(), 0, 10)
            .expect("missing log should be handled");
        assert!(tail.is_none());
    }

    #[test]
    fn read_autonomous_startup_log_tail_ignores_preexisting_lines_before_start_offset() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let log_path = temp.path().join("daemon.log");
        fs::write(&log_path, "old-1\nold-2\n").expect("old log content should be written");
        let startup_log_offset = fs::metadata(&log_path)
            .expect("log metadata should be readable")
            .len();
        fs::write(&log_path, "old-1\nold-2\nnew-1\nnew-2\n")
            .expect("new log content should be written");

        let tail = read_autonomous_startup_log_tail(log_path.as_path(), startup_log_offset, 10)
            .expect("log tail should be readable")
            .expect("log tail should be present");
        assert_eq!(tail, "new-1\nnew-2");
    }

    #[test]
    fn autonomous_startup_failure_error_includes_log_context() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let log_path = temp.path().join("daemon.log");
        fs::write(&log_path, "first\nsecond\nthird\n").expect("log file should be written");

        let error = autonomous_startup_failure_error(4242, None, log_path.as_path(), 0);
        let message = error.to_string();
        assert!(message.contains("pid 4242"));
        assert!(message.contains(log_path.to_string_lossy().as_ref()));
        assert!(message.contains("startup log tail"));
        assert!(message.contains("third"));
    }

    #[test]
    fn autonomous_startup_failure_error_includes_structured_details() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let log_path = temp.path().join("daemon.log");
        fs::write(&log_path, "startup line\nerror: crashed\n").expect("log file should be written");

        let error = autonomous_startup_failure_error(9999, None, log_path.as_path(), 0);
        let details =
            crate::extract_cli_error_details(&error).expect("structured details should be present");
        assert_eq!(
            details
                .get("daemon_pid")
                .and_then(serde_json::Value::as_u64),
            Some(9999)
        );
        assert!(details
            .get("log_path")
            .and_then(serde_json::Value::as_str)
            .is_some());
        let tail = details
            .get("startup_log_tail")
            .and_then(serde_json::Value::as_str)
            .expect("startup_log_tail should be present");
        assert!(tail.contains("error: crashed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn autonomous_startup_probe_reports_running_process_as_alive() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 2"])
            .spawn()
            .expect("sleep process should spawn");

        let status = wait_for_autonomous_startup_probe(&mut child, Duration::from_millis(50))
            .await
            .expect("probe should succeed");
        assert!(status.is_none());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn autonomous_startup_probe_reports_early_exit() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 9"])
            .spawn()
            .expect("shell process should spawn");

        let status = wait_for_autonomous_startup_probe(&mut child, Duration::from_millis(50))
            .await
            .expect("probe should succeed");
        let exit_status = status.expect("process should have exited");
        assert_eq!(exit_status.code(), Some(9));
    }
}
