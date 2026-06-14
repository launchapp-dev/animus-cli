use crate::{dispatch_capacity_for_options, DaemonRuntimeOptions, ScheduleDispatch};
use chrono::NaiveTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickPlan {
    pub within_active_hours: bool,
    pub should_process_due_schedules: bool,
    /// Capacity for draining explicitly enqueued dispatch-queue entries.
    /// `animus queue enqueue` is an operator command whose entries must
    /// drain up to available pool capacity. Only a draining pool zeroes
    /// this limit.
    pub queue_drain_limit: usize,
}

impl ProjectTickPlan {
    pub fn build(active_hours: Option<&str>, now: NaiveTime, pool_draining: bool, dispatch_capacity: usize) -> Self {
        let within_active_hours = ScheduleDispatch::allows_proactive_dispatch(active_hours, now);
        let should_process_due_schedules = within_active_hours && !pool_draining;
        let queue_drain_limit = if pool_draining { 0 } else { dispatch_capacity };

        Self { within_active_hours, should_process_due_schedules, queue_drain_limit }
    }

    pub fn for_slim_tick(
        options: &DaemonRuntimeOptions,
        active_hours: Option<&str>,
        now: NaiveTime,
        pool_draining: bool,
        daemon_pool_size: Option<usize>,
        active_process_count: usize,
    ) -> Self {
        let dispatch_capacity = dispatch_capacity_for_options(options, active_process_count, daemon_pool_size);

        Self::build(active_hours, now, pool_draining, dispatch_capacity)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use super::ProjectTickPlan;
    use crate::DaemonRuntimeOptions;

    #[test]
    fn disables_schedule_dispatch_outside_active_hours() {
        let plan = ProjectTickPlan::build(
            Some("09:00-17:00"),
            NaiveTime::from_hms_opt(8, 30, 0).expect("time should be valid"),
            false,
            2,
        );

        assert!(!plan.within_active_hours);
        assert!(!plan.should_process_due_schedules);
        assert_eq!(plan.queue_drain_limit, 2);
    }

    #[test]
    fn disables_all_dispatch_while_pool_is_draining() {
        let plan =
            ProjectTickPlan::build(None, NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"), true, 3);

        assert!(plan.within_active_hours);
        assert!(!plan.should_process_due_schedules, "paused/draining daemon must not dispatch schedules or triggers");
        assert_eq!(plan.queue_drain_limit, 0, "draining pool must not drain the dispatch queue either");
    }

    #[test]
    fn drains_enqueued_entries_up_to_available_capacity() {
        let plan =
            ProjectTickPlan::build(None, NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"), false, 4);

        assert!(plan.within_active_hours);
        assert!(plan.should_process_due_schedules);
        assert_eq!(plan.queue_drain_limit, 4, "explicitly enqueued entries drain up to available capacity");
    }

    #[test]
    fn slim_tick_uses_active_process_count_against_configured_capacity() {
        let plan = ProjectTickPlan::for_slim_tick(
            &DaemonRuntimeOptions { pool_size: Some(4), max_tasks_per_tick: 5, ..DaemonRuntimeOptions::default() },
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(8),
            3,
        );

        assert_eq!(plan.queue_drain_limit, 1);
    }

    #[test]
    fn slim_tick_uses_smallest_capacity_across_pool_sizes() {
        let plan = ProjectTickPlan::for_slim_tick(
            &DaemonRuntimeOptions { pool_size: Some(6), max_tasks_per_tick: 5, ..DaemonRuntimeOptions::default() },
            None,
            NaiveTime::from_hms_opt(12, 0, 0).expect("time should be valid"),
            false,
            Some(3),
            1,
        );

        assert_eq!(plan.queue_drain_limit, 2);
    }
}
