use chrono::NaiveTime;

use crate::{DaemonRuntimeOptions, ProjectTickPlan, ProjectTickScript};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickPreparation {
    pub schedule_plan: ProjectTickPlan,
    pub tick_script: ProjectTickScript,
}

impl ProjectTickPreparation {
    pub fn build(
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
        let tick_script = ProjectTickScript::build(options, &tick_plan);

        Self {
            schedule_plan,
            tick_script,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use super::ProjectTickPreparation;
    use crate::{DaemonRuntimeOptions, ProjectTickAction};

    #[test]
    fn project_tick_preparation_uses_capacity_for_dispatch_but_not_schedule_gate() {
        let preparation = ProjectTickPreparation::build(
            &DaemonRuntimeOptions {
                max_tasks_per_tick: 5,
                ..DaemonRuntimeOptions::default()
            },
            Some("09:00-17:00"),
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(2),
            Some(2),
            1,
        );

        assert!(preparation.schedule_plan.should_process_due_schedules);
        assert_eq!(
            preparation.tick_script.actions(),
            &[
                ProjectTickAction::ReconcileCompletedProcesses,
                ProjectTickAction::DispatchReadyTasks { limit: 1 },
            ]
        );
    }
}
