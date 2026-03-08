use anyhow::Result;
use orchestrator_core::DaemonStatus;

use crate::DaemonRunEvent;

#[async_trait::async_trait(?Send)]
pub trait DaemonRunHooks {
    fn handle_event(&mut self, event: DaemonRunEvent) -> Result<()>;

    async fn daemon_status(&mut self, project_root: &str) -> Result<DaemonStatus> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide daemon_status")
    }

    async fn start_daemon(&mut self, project_root: &str) -> Result<()> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide start_daemon")
    }

    async fn stop_daemon(&mut self, project_root: &str) -> Result<()> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide stop_daemon")
    }

    async fn recover_startup_orphans(&mut self, project_root: &str) -> Result<usize> {
        let _ = project_root;
        anyhow::bail!("daemon lifecycle hooks must provide recover_startup_orphans")
    }

    async fn flush_notifications(&mut self, _project_root: &str) -> Result<()> {
        Ok(())
    }
}
