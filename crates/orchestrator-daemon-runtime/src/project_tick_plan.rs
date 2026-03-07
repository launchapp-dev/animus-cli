use chrono::NaiveTime;
use orchestrator_core::DaemonHealth;

use crate::{ready_task_dispatch_limit_for_options, DaemonRuntimeOptions, ScheduleDispatch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickPlan {
    pub within_active_hours: bool,
    pub should_process_due_schedules: bool,
    pub should_prepare_ready_tasks: bool,
    pub ready_dispatch_limit: usize,
}

impl ProjectTickPlan {
    pub fn build(
        options: &DaemonRuntimeOptions,
        active_hours: Option<&str>,
        now: NaiveTime,
        pool_draining: bool,
        requested_ready_dispatch_limit: usize,
    ) -> Self {
        let within_active_hours = ScheduleDispatch::allows_proactive_dispatch(active_hours, now);
        let should_process_due_schedules = within_active_hours;
        let should_prepare_ready_tasks = !pool_draining && options.auto_run_ready;
        let ready_dispatch_limit = if should_prepare_ready_tasks {
            requested_ready_dispatch_limit
        } else {
            0
        };

        Self {
            within_active_hours,
            should_process_due_schedules,
            should_prepare_ready_tasks,
            ready_dispatch_limit,
        }
    }

    pub fn for_project_tick(
        options: &DaemonRuntimeOptions,
        active_hours: Option<&str>,
        now: NaiveTime,
        pool_draining: bool,
        daemon_health: Option<&DaemonHealth>,
    ) -> Self {
        let requested_ready_dispatch_limit = daemon_health
            .map(|health| {
                ready_task_dispatch_limit_for_options(
                    options,
                    health.active_agents,
                    health.max_agents,
                    health.pool_size.map(|value| value as usize),
                )
            })
            .unwrap_or(options.max_tasks_per_tick);

        Self::build(
            options,
            active_hours,
            now,
            pool_draining,
            requested_ready_dispatch_limit,
        )
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
        let requested_ready_dispatch_limit = ready_task_dispatch_limit_for_options(
            options,
            active_process_count,
            daemon_max_agents,
            daemon_pool_size,
        );

        Self::build(
            options,
            active_hours,
            now,
            pool_draining,
            requested_ready_dispatch_limit,
        )
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;
    use orchestrator_core::{DaemonHealth, DaemonStatus};

    use super::ProjectTickPlan;
    use crate::DaemonRuntimeOptions;

    #[test]
    fn disables_schedule_dispatch_outside_active_hours() {
        let plan = ProjectTickPlan::build(
            &DaemonRuntimeOptions::default(),
            Some("09:00-17:00"),
            NaiveTime::from_hms_opt(8, 30, 0).expect("time should be valid"),
            false,
            2,
        );

        assert!(!plan.within_active_hours);
        assert!(!plan.should_process_due_schedules);
        assert!(plan.should_prepare_ready_tasks);
        assert_eq!(plan.ready_dispatch_limit, 2);
    }

    #[test]
    fn disables_ready_task_preparation_while_pool_is_draining() {
        let plan = ProjectTickPlan::build(
            &DaemonRuntimeOptions::default(),
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            true,
            3,
        );

        assert!(plan.within_active_hours);
        assert!(plan.should_process_due_schedules);
        assert!(!plan.should_prepare_ready_tasks);
        assert_eq!(plan.ready_dispatch_limit, 0);
    }

    #[test]
    fn disables_ready_task_preparation_when_auto_run_ready_is_off() {
        let plan = ProjectTickPlan::build(
            &DaemonRuntimeOptions {
                auto_run_ready: false,
                ..DaemonRuntimeOptions::default()
            },
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            4,
        );

        assert!(plan.within_active_hours);
        assert!(plan.should_process_due_schedules);
        assert!(!plan.should_prepare_ready_tasks);
        assert_eq!(plan.ready_dispatch_limit, 0);
    }

    #[test]
    fn project_tick_uses_daemon_health_capacity() {
        let plan = ProjectTickPlan::for_project_tick(
            &DaemonRuntimeOptions {
                max_tasks_per_tick: 5,
                ..DaemonRuntimeOptions::default()
            },
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(&DaemonHealth {
                healthy: true,
                status: DaemonStatus::Running,
                runner_connected: true,
                runner_pid: Some(42),
                max_agents: Some(2),
                active_agents: 1,
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

        assert_eq!(plan.ready_dispatch_limit, 1);
    }

    #[test]
    fn slim_tick_uses_active_process_count_against_configured_capacity() {
        let plan = ProjectTickPlan::for_slim_tick(
            &DaemonRuntimeOptions {
                pool_size: Some(4),
                max_tasks_per_tick: 5,
                ..DaemonRuntimeOptions::default()
            },
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(8),
            None,
            3,
        );

        assert_eq!(plan.ready_dispatch_limit, 1);
    }

    #[test]
    fn slim_tick_uses_smallest_capacity_across_pool_and_max_agents() {
        let plan = ProjectTickPlan::for_slim_tick(
            &DaemonRuntimeOptions {
                pool_size: Some(6),
                max_agents: Some(2),
                max_tasks_per_tick: 5,
                ..DaemonRuntimeOptions::default()
            },
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(4),
            Some(3),
            1,
        );

        assert_eq!(plan.ready_dispatch_limit, 1);
    }
}
