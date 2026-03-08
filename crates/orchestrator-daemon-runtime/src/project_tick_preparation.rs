use chrono::NaiveTime;
use orchestrator_core::DaemonHealth;

use crate::{DaemonRuntimeOptions, ProjectTickMode, ProjectTickPlan, ProjectTickScript};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickPreparation {
    pub schedule_plan: ProjectTickPlan,
    pub tick_script: ProjectTickScript,
}

impl ProjectTickPreparation {
    pub fn for_project_tick(
        options: &DaemonRuntimeOptions,
        active_hours: Option<&str>,
        now: NaiveTime,
        pool_draining: bool,
        daemon_health: Option<&DaemonHealth>,
    ) -> Self {
        let schedule_plan =
            ProjectTickPlan::for_project_tick(options, active_hours, now, pool_draining, None);
        let tick_plan = ProjectTickPlan::for_project_tick(
            options,
            active_hours,
            now,
            pool_draining,
            daemon_health,
        );
        let tick_script = ProjectTickScript::build(ProjectTickMode::Full, options, &tick_plan);

        Self {
            schedule_plan,
            tick_script,
        }
    }

    pub fn for_slim_tick(
        options: &DaemonRuntimeOptions,
        active_hours: Option<&str>,
        now: NaiveTime,
        pool_draining: bool,
        daemon_max_agents: Option<usize>,
        daemon_pool_size: Option<usize>,
        active_process_count: usize,
    ) -> Self {
        let schedule_plan = ProjectTickPlan::for_slim_tick(
            options,
            active_hours,
            now,
            pool_draining,
            None,
            None,
            0,
        );
        let tick_plan = ProjectTickPlan::for_slim_tick(
            options,
            active_hours,
            now,
            pool_draining,
            daemon_max_agents,
            daemon_pool_size,
            active_process_count,
        );
        let tick_script = ProjectTickScript::build(ProjectTickMode::Slim, options, &tick_plan);

        Self {
            schedule_plan,
            tick_script,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;
    use orchestrator_core::{DaemonHealth, DaemonStatus};

    use super::ProjectTickPreparation;
    use crate::{DaemonRuntimeOptions, ProjectTickAction};

    #[test]
    fn project_tick_preparation_uses_health_for_dispatch_but_not_schedule_gate() {
        let preparation = ProjectTickPreparation::for_project_tick(
            &DaemonRuntimeOptions {
                max_tasks_per_tick: 5,
                ..DaemonRuntimeOptions::default()
            },
            Some("09:00-17:00"),
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(&DaemonHealth {
                healthy: true,
                status: DaemonStatus::Running,
                runner_connected: true,
                runner_pid: Some(42),
                active_agents: 1,
                max_agents: Some(2),
                project_root: Some("/tmp/project".to_string()),
                daemon_pid: Some(24),
                process_alive: Some(true),
                pool_size: Some(2),
                pool_utilization_percent: Some(50.0),
                queued_tasks: Some(0),
                total_agents_spawned: Some(1),
                total_agents_completed: Some(0),
                total_agents_failed: Some(0),
            }),
        );

        assert!(preparation.schedule_plan.should_process_due_schedules);
        assert_eq!(
            preparation.tick_script.actions(),
            &[
                ProjectTickAction::BootstrapFromVision,
                ProjectTickAction::ResumeInterrupted,
                ProjectTickAction::RecoverOrphanedRunningWorkflows,
                ProjectTickAction::ReconcileStaleTasks,
                ProjectTickAction::ReconcileDependencyTasks,
                ProjectTickAction::ReconcileMergeTasks,
                ProjectTickAction::RetryFailedTaskWorkflows,
                ProjectTickAction::PromoteBacklogTasksToReady,
                ProjectTickAction::DispatchReadyTasks { limit: 1 },
                ProjectTickAction::RefreshRuntimeBinaries,
            ]
        );
    }
}
