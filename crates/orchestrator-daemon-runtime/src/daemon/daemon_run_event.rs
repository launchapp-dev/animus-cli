use crate::ProjectTickSummary;

#[derive(Debug, Clone)]
pub enum DaemonRunEvent {
    Startup {
        project_root: String,
        daemon_pid: u32,
    },
    Status {
        project_root: String,
        status: String,
    },
    StartupCleanup {
        project_root: String,
    },
    OrphanDetection {
        project_root: String,
        orphaned_workflows_recovered: usize,
    },
    YamlCompileSucceeded {
        project_root: String,
        source_files: usize,
        output_path: String,
        phase_definitions: usize,
        agent_profiles: usize,
    },
    YamlCompileFailed {
        project_root: String,
        error: String,
    },
    TickSummary {
        summary: ProjectTickSummary,
    },
    TickError {
        project_root: String,
        message: String,
    },
    GracefulShutdown {
        project_root: String,
        timeout_secs: Option<u64>,
    },
    Draining {
        project_root: String,
        trigger: String,
    },
    NotificationRuntimeError {
        project_root: Option<String>,
        stage: String,
        message: String,
    },
    ConfigReloaded {
        project_root: String,
        setting: String,
    },
    Shutdown {
        project_root: String,
        daemon_pid: u32,
    },
}
