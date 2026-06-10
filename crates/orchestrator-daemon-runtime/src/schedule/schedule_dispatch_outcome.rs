#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleDispatchOutcome {
    pub schedule_id: String,
    /// The cron occurrence this dispatch covers. Recorded as `last_run` so
    /// occurrences missed between ticks are caught up on later ticks.
    pub run_at: chrono::DateTime<chrono::Utc>,
    pub status: String,
}
