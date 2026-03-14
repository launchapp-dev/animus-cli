use super::*;
use orchestrator_daemon_runtime::ProcessManager;

#[path = "daemon_task_dispatch.rs"]
pub(super) mod task_dispatch;

#[path = "daemon_tick_executor.rs"]
mod tick_executor;

use task_dispatch::*;
pub(crate) use tick_executor::{slim_project_tick_driver, SlimProjectTickDriver};
