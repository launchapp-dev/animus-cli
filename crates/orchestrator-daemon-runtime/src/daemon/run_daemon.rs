use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use orchestrator_core::services::ServiceHub;
use orchestrator_core::DaemonStatus;
use orchestrator_core::FileServiceHub;
use tokio::time::sleep;

use crate::recover_orphaned_running_workflows;
use crate::run_project_tick;
use crate::DaemonRunEvent;
use crate::DaemonRunGuard;
use crate::DaemonRunHooks;
use crate::DaemonRuntimeOptions;
use crate::DaemonRuntimeState;
use crate::ProjectTickHooks;
use crate::ProjectTickRunMode;

pub async fn run_daemon<D, H>(
    project_root: &str,
    options: &DaemonRuntimeOptions,
    hub: Arc<dyn ServiceHub>,
    driver: &mut D,
    hooks: &mut H,
    mut active_process_count: impl FnMut(&D) -> usize,
) -> Result<()>
where
    D: ProjectTickHooks,
    H: DaemonRunHooks,
{
    let _run_guard = DaemonRunGuard::acquire(project_root)?;
    let daemon_pid = std::process::id();
    let primary_root = canonicalize_lossy(project_root);

    hooks.handle_event(DaemonRunEvent::Startup {
        project_root: primary_root.clone(),
        daemon_pid,
    })?;

    let daemon = hub.daemon();
    let initial_status = daemon.status().await?;
    let mut stop_daemon_on_exit = false;
    if !matches!(initial_status, DaemonStatus::Running | DaemonStatus::Paused) {
        daemon.start().await?;
        stop_daemon_on_exit = true;
    }
    let _ = DaemonRuntimeState::set_runtime_paused(project_root, false);

    hooks.handle_event(DaemonRunEvent::Status {
        project_root: primary_root.clone(),
        status: "running".to_string(),
    })?;

    if options.startup_cleanup {
        hooks.handle_event(DaemonRunEvent::StartupCleanup {
            project_root: primary_root.clone(),
        })?;

        let startup_orphans =
            recover_orphaned_running_workflows(hub.clone(), &primary_root, &HashSet::new()).await;
        if startup_orphans > 0 {
            hooks.handle_event(DaemonRunEvent::OrphanDetection {
                project_root: primary_root.clone(),
                orphaned_workflows_recovered: startup_orphans,
            })?;
        }
    }

    match orchestrator_core::compile_and_write_yaml_workflows(Path::new(project_root)) {
        Ok(Some(result)) => {
            hooks.handle_event(DaemonRunEvent::YamlCompileSucceeded {
                project_root: primary_root.clone(),
                source_files: result.source_files.len(),
                output_path: result.output_path.display().to_string(),
                phase_definitions: result.config.phase_definitions.len(),
                agent_profiles: result.config.agent_profiles.len(),
            })?;
        }
        Ok(None) => {}
        Err(error) => {
            hooks.handle_event(DaemonRunEvent::YamlCompileFailed {
                project_root: primary_root.clone(),
                error: error.to_string(),
            })?;
        }
    }

    let interval = Duration::from_secs(options.interval_secs.max(1));
    let mut sigterm_stream = SigtermStream::new()?;
    loop {
        let externally_paused =
            DaemonRuntimeState::is_runtime_paused(project_root).unwrap_or(false);
        let tick_result = run_project_tick(
            &primary_root,
            options,
            ProjectTickRunMode {
                active_process_count: active_process_count(driver),
            },
            externally_paused,
            driver,
        )
        .await;

        match tick_result {
            Ok(summary) => hooks.handle_event(DaemonRunEvent::TickSummary { summary })?,
            Err(error) => hooks.handle_event(DaemonRunEvent::TickError {
                project_root: primary_root.clone(),
                message: error.to_string(),
            })?,
        }

        if externally_paused {
            break;
        }

        if let Err(error) = hooks.flush_notifications(&primary_root).await {
            hooks.handle_event(DaemonRunEvent::NotificationRuntimeError {
                project_root: Some(primary_root.clone()),
                stage: "flush".to_string(),
                message: error.to_string(),
            })?;
        }

        if options.once {
            break;
        }

        let shutdown =
            DaemonRuntimeState::is_shutdown_requested(project_root).unwrap_or((false, None));
        if shutdown.0 {
            hooks.handle_event(DaemonRunEvent::GracefulShutdown {
                project_root: primary_root.clone(),
                timeout_secs: shutdown.1,
            })?;
            let _ = DaemonRuntimeState::set_shutdown_requested(project_root, false, None);
            break;
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                hooks.handle_event(DaemonRunEvent::Draining {
                    project_root: primary_root.clone(),
                    trigger: "ctrl_c".to_string(),
                })?;
                break;
            }
            _ = sigterm_stream.recv() => {
                hooks.handle_event(DaemonRunEvent::Draining {
                    project_root: primary_root.clone(),
                    trigger: "sigterm".to_string(),
                })?;
                break;
            }
            _ = sleep(interval) => {}
        }
    }

    if stop_daemon_on_exit {
        if let Ok(project_hub) = FileServiceHub::new(&primary_root) {
            let _ = project_hub.daemon().stop().await;
        }
    }

    hooks.handle_event(DaemonRunEvent::Status {
        project_root: primary_root.clone(),
        status: "stopped".to_string(),
    })?;
    hooks.handle_event(DaemonRunEvent::Shutdown {
        project_root: primary_root,
        daemon_pid,
    })?;
    Ok(())
}

struct SigtermStream {
    #[cfg(unix)]
    inner: tokio::signal::unix::Signal,
}

impl SigtermStream {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            let inner = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("failed to subscribe to SIGTERM")?;
            Ok(Self { inner })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            self.inner.recv().await;
        }
        #[cfg(not(unix))]
        {
            std::future::pending::<()>().await;
        }
    }
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate
        .canonicalize()
        .unwrap_or(candidate)
        .to_string_lossy()
        .to_string()
}
