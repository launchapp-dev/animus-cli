use anyhow::Result;
use chrono::Timelike;
use croner::parser::{CronParser, Seconds, Year};
use tracing::warn;

use super::ScheduleDispatchOutcome;
use crate::{SubjectDispatch, SubjectDispatchExt};

pub struct ScheduleDispatch;

impl ScheduleDispatch {
    pub fn allows_proactive_dispatch(active_hours: Option<&str>, now: chrono::NaiveTime) -> bool {
        active_hours.map(|spec| is_within_active_hours(spec, now)).unwrap_or(true)
    }

    pub fn process_due_schedules<PipelineSpawner>(
        project_root: &str,
        now: chrono::DateTime<chrono::Utc>,
        mut spawn_pipeline: PipelineSpawner,
    ) -> Vec<ScheduleDispatchOutcome>
    where
        PipelineSpawner: FnMut(&str, &SubjectDispatch) -> Result<()>,
    {
        let config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
        let state = orchestrator_core::load_schedule_state(std::path::Path::new(project_root)).unwrap_or_default();
        let active_hours = config.config.daemon.as_ref().and_then(|daemon| daemon.active_hours.clone());
        let due = evaluate_schedules(&config.config.schedules, &state, now, |occurrence| {
            Self::allows_proactive_dispatch(active_hours.as_deref(), occurrence.with_timezone(&chrono::Local).time())
        });
        if due.is_empty() {
            return Vec::new();
        }

        let schedule_lookup: std::collections::HashMap<&str, &orchestrator_core::workflow_config::WorkflowSchedule> =
            config.config.schedules.iter().map(|schedule| (schedule.id.as_str(), schedule)).collect();

        let mut outcomes = Vec::with_capacity(due.len());
        for (schedule_id, run_at) in due {
            if let Some(schedule) = schedule_lookup.get(schedule_id.as_str()) {
                let status = dispatch_schedule(&schedule_id, schedule, now, "schedule", &mut spawn_pipeline);
                outcomes.push(ScheduleDispatchOutcome { schedule_id, run_at, status });
            }
        }

        outcomes
    }

    /// Earliest upcoming cron occurrence (strictly after `now`) across all
    /// enabled schedules compiled for `project_root`. Returns `None` when
    /// no schedule is configured (pure heartbeat mode). The daemon loop
    /// uses this as a `sleep_until` deadline so cron fires on time instead
    /// of on the next heartbeat tick; the catch-up scan in
    /// [`due_occurrence`] remains the recovery path for missed deadlines.
    pub fn next_schedule_deadline(
        project_root: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
        next_deadline_for_schedules(&config.config.schedules, now)
    }
}

/// Pure deadline computation across a schedule list: the minimum next
/// occurrence strictly after `now` over all enabled schedules. Disabled
/// schedules, empty cron expressions, and invalid cron expressions are
/// skipped (invalid ones with a warning, mirroring [`evaluate_schedules`]).
fn next_deadline_for_schedules(
    schedules: &[orchestrator_core::workflow_config::WorkflowSchedule],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let parser = CronParser::builder().seconds(Seconds::Disallowed).year(Year::Disallowed).build();
    let mut earliest: Option<chrono::DateTime<chrono::Utc>> = None;
    for schedule in schedules {
        if !schedule.enabled {
            continue;
        }
        let expression = schedule.cron.trim();
        if expression.is_empty() {
            continue;
        }
        let cron = match parser.parse(expression) {
            Ok(cron) => cron,
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    schedule_id = %schedule.id,
                    cron = %schedule.cron,
                    error = %error,
                    "schedule has invalid cron expression; excluded from deadline computation"
                );
                continue;
            }
        };
        // `false` = exclusive of `now`: an occurrence exactly at `now` is
        // handled by the tick currently running, so the next deadline must
        // be strictly in the future (no busy-loop re-fires).
        let Ok(next) = cron.find_next_occurrence(&now, false) else {
            continue;
        };
        earliest = Some(match earliest {
            Some(current) if current <= next => current,
            _ => next,
        });
    }
    earliest
}

fn dispatch_schedule<PipelineSpawner>(
    schedule_id: &str,
    schedule: &orchestrator_core::workflow_config::WorkflowSchedule,
    _now: chrono::DateTime<chrono::Utc>,
    trigger_source: &str,
    spawn_pipeline: &mut PipelineSpawner,
) -> String
where
    PipelineSpawner: FnMut(&str, &SubjectDispatch) -> Result<()>,
{
    let status = if let Some(ref workflow_ref) = schedule.workflow_ref {
        let dispatch = SubjectDispatch::for_custom(
            format!("schedule:{schedule_id}"),
            format!("Triggered by schedule '{schedule_id}'"),
            workflow_ref.clone(),
            schedule.input.clone(),
            trigger_source.to_string(),
        )
        .with_actor(mint_schedule_actor(schedule));
        match spawn_pipeline(schedule_id, &dispatch) {
            Ok(()) => "dispatched".to_string(),
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    schedule_id,
                    workflow_ref,
                    error = %error,
                    "schedule dispatch failed"
                );
                format!("failed: {error}")
            }
        }
    } else {
        warn!(
            actor = protocol::ACTOR_DAEMON,
            schedule_id, "schedule is missing workflow_ref and will not be dispatched"
        );
        "failed: schedule is missing workflow_ref".to_string()
    };

    status
}

/// Mint the [`Actor`] an owner-scoped schedule runs as, or `None` for a global
/// (system) schedule.
///
/// TRUST BOUNDARY: this is the ONE place the kernel CONSTRUCTS an actor rather
/// than relaying a transport-asserted one. It is sound because the owner is
/// asserted at config-authoring time: the workflow config (and thus
/// `schedule.owner_id`) is itself owner-scoped / admin-authored — served by a
/// trusted `config_source` (e.g. `config-postgres` team_* rows or
/// admin-curated YAML), NEVER derived from runtime, agent output, or subject
/// content. Minting the owner here therefore respects the
/// transport-asserted-identity model. A schedule with no `owner_id` keeps the
/// legacy global dispatch (`actor = None`).
fn mint_schedule_actor(schedule: &orchestrator_core::workflow_config::WorkflowSchedule) -> Option<animus_actor::Actor> {
    let owner_id = schedule.owner_id.as_deref().map(str::trim).filter(|id| !id.is_empty())?;
    Some(animus_actor::Actor { user_id: owner_id.to_string(), claims: schedule.claims.clone(), tenant_id: None })
}

/// `occurrence_allowed` is the active-hours gate applied to the caught-up
/// occurrence itself: an occurrence that fell inside a closed `active_hours`
/// window (but within the catch-up horizon) must be skipped, not replayed
/// when the window reopens.
fn evaluate_schedules(
    schedules: &[orchestrator_core::workflow_config::WorkflowSchedule],
    state: &orchestrator_core::ScheduleState,
    now: chrono::DateTime<chrono::Utc>,
    occurrence_allowed: impl Fn(chrono::DateTime<chrono::Utc>) -> bool,
) -> Vec<(String, chrono::DateTime<chrono::Utc>)> {
    let mut due = Vec::new();
    for schedule in schedules {
        if !schedule.enabled {
            continue;
        }

        let last_run = state.schedules.get(&schedule.id).and_then(|run_state| run_state.last_run);
        match due_occurrence(&schedule.cron, last_run, now) {
            Ok(Some(occurrence)) => {
                if occurrence_allowed(occurrence) {
                    due.push((schedule.id.clone(), occurrence));
                }
            }
            Ok(None) => {}
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    schedule_id = %schedule.id,
                    cron = %schedule.cron,
                    error = %error,
                    "schedule has invalid cron expression"
                );
            }
        }
    }

    due
}

/// Maximum sleep between scheduler passes while at least one schedule is
/// enabled: half the catch-up horizon. A cron occurrence that woke the loop
/// but could not dispatch (pool/budget full) is retried by the catch-up
/// scan on later passes — but only while it is still inside
/// [`CATCH_UP_HORIZON_MINS`]. Capping the sleep at half the horizon
/// guarantees at least one retry pass lands within the horizon even when
/// the configured heartbeat (`interval_secs`) is much longer.
pub(crate) const SCHEDULE_RETRY_SWEEP_MAX: std::time::Duration =
    std::time::Duration::from_secs(CATCH_UP_HORIZON_MINS as u64 * 60 / 2);

/// How far back the catch-up scan looks for a missed cron occurrence. Wide
/// enough to absorb long ticks and `interval_secs` well above 60, but narrow
/// enough that occurrences suppressed for hours (daemon stopped, schedules
/// gated off by `active_hours`) are NOT replayed when dispatch resumes —
/// the documented behavior is that those fires are skipped, not delayed.
const CATCH_UP_HORIZON_MINS: i64 = 10;

/// Returns the most recent cron occurrence that is due at `now`, or `None`
/// when the schedule should not fire on this tick.
///
/// This is the latest occurrence at or before `now` that is strictly after
/// `max(last_run, now - CATCH_UP_HORIZON_MINS)` — so an occurrence missed
/// by a long tick or `interval_secs > 60` still fires on the next tick
/// instead of being silently skipped. Taking the latest (not the earliest)
/// caps catch-up at one fire total: when dispatch resumes after a gap
/// (e.g. the `active_hours` window reopens), older occurrences inside the
/// horizon are skipped rather than replayed one tick at a time. Without a
/// `last_run` (first evaluation) the same horizon applies from
/// `now - CATCH_UP_HORIZON_MINS`, so a fresh daemon catches up at most one
/// recent occurrence and never replays a stale backlog.
fn due_occurrence(
    expression: &str,
    last_run: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Ok(None);
    }

    let parser = CronParser::builder().seconds(Seconds::Disallowed).year(Year::Disallowed).build();
    let cron = parser.parse(expression).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let horizon_start = now - chrono::Duration::minutes(CATCH_UP_HORIZON_MINS);
    let catch_up_from = match last_run {
        Some(last_run) => std::cmp::max(last_run, horizon_start),
        None => horizon_start,
    };
    let mut due = None;
    let mut cursor = catch_up_from;
    loop {
        let next = cron.find_next_occurrence(&cursor, false).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if next > now {
            break;
        }
        due = Some(next);
        cursor = next;
    }
    Ok(due)
}

#[cfg(test)]
fn cron_matches(expression: &str, now: chrono::DateTime<chrono::Utc>) -> Result<bool> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Ok(false);
    }

    let parser = CronParser::builder().seconds(Seconds::Disallowed).year(Year::Disallowed).build();
    let cron = parser.parse(expression).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let normalized = now
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("utc timestamps should support zero second normalization");

    cron.is_time_matching(&normalized).map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn parse_active_hours(spec: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = spec.trim().split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let parse_minutes = |value: &str| -> Option<u32> {
        let hm: Vec<&str> = value.trim().split(':').collect();
        if hm.len() != 2 {
            return None;
        }
        let hour: u32 = hm[0].parse().ok()?;
        let minute: u32 = hm[1].parse().ok()?;
        if hour >= 24 || minute >= 60 {
            return None;
        }
        Some(hour * 60 + minute)
    };
    Some((parse_minutes(parts[0])?, parse_minutes(parts[1])?))
}

fn is_within_active_hours(active_hours: &str, now: chrono::NaiveTime) -> bool {
    let Some((start, end)) = parse_active_hours(active_hours) else {
        return true;
    };
    let now_minutes = now.hour() * 60 + now.minute();
    if start <= end {
        now_minutes >= start && now_minutes < end
    } else {
        now_minutes >= start || now_minutes < end
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn cron_matches_exact_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        assert!(cron_matches("30 12 4 3 3", now).expect("cron should parse"));
        assert!(!cron_matches("31 12 4 3 4", now).expect("cron should parse"));
    }

    #[test]
    fn cron_matches_with_wildcards() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:00:00Z".parse().expect("timestamp should parse");
        assert!(cron_matches("* * * * *", now).expect("cron should parse"));
        assert!(cron_matches("0 * * * *", now).expect("cron should parse"));
    }

    #[test]
    fn cron_matches_shortcut_expressions() {
        let sunday_midnight: chrono::DateTime<chrono::Utc> =
            "2026-03-01T00:00:00Z".parse().expect("timestamp should parse");
        let quarter_hour: chrono::DateTime<chrono::Utc> =
            "2026-03-01T12:15:00Z".parse().expect("timestamp should parse");
        assert!(cron_matches("@weekly", sunday_midnight).expect("cron should parse"));
        assert!(cron_matches("@monthly", sunday_midnight).expect("cron should parse"));
        assert!(!cron_matches("@hourly", quarter_hour).expect("cron should parse"));
    }

    #[test]
    fn cron_matches_lists_ranges_and_steps() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:42Z".parse().expect("timestamp should parse");

        assert!(cron_matches("*/15 9-17 * * 1,3,5", now).expect("cron should parse"));
        assert!(!cron_matches("*/20 9-17 * * 1,3,5", now).expect("cron should parse"));
    }

    #[test]
    fn cron_matches_returns_error_for_invalid_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");

        let error = cron_matches("*/0 * * * *", now).expect_err("invalid cron should fail");
        let message = error.to_string().to_ascii_lowercase();
        assert!(
            message.contains("step") || message.contains("invalid") || message.contains("range"),
            "unexpected invalid cron error: {error}"
        );
    }

    #[test]
    fn evaluate_schedules_skips_disabled_schedules() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "disabled".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: false,
            owner_id: None,
            claims: Vec::new(),
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);

        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_matches_five_field_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "midday".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);

        assert_eq!(due, vec![("midday".to_string(), now)]);
    }

    #[test]
    fn evaluate_schedules_matches_shortcut_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T00:00:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "daily".to_string(),
            cron: "@daily".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);

        assert_eq!(due, vec![("daily".to_string(), now)]);
    }

    #[test]
    fn evaluate_schedules_skips_invalid_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "broken".to_string(),
            cron: "*/0 * * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);

        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_skips_already_ran_this_minute() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "recent".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let mut state = orchestrator_core::ScheduleState::default();
        state.schedules.insert(
            "recent".to_string(),
            orchestrator_core::ScheduleRunState {
                last_run: Some(now),
                last_status: "evaluated".to_string(),
                run_count: 1,
                missed_count: 0,
            },
        );
        let due = evaluate_schedules(&schedules, &state, now, |_| true);

        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_catches_up_missed_occurrence() {
        // The 12:00 occurrence fell between ticks (long tick / large
        // interval_secs): the daemon last fired 11:00 and the next tick only
        // lands at 12:05. The 12:00 occurrence must still fire instead of
        // being silently skipped.
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:05:42Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "hourly".to_string(),
            cron: "0 * * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let mut state = orchestrator_core::ScheduleState::default();
        state.schedules.insert(
            "hourly".to_string(),
            orchestrator_core::ScheduleRunState {
                last_run: Some("2026-03-04T11:00:00Z".parse().unwrap()),
                last_status: "dispatched".to_string(),
                run_count: 1,
                missed_count: 0,
            },
        );

        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert_eq!(due, vec![("hourly".to_string(), "2026-03-04T12:00:00Z".parse().unwrap())]);

        // Recording last_run = 12:00 fully catches up: the next occurrence
        // (13:00) is in the future, so nothing further fires this tick.
        state.schedules.get_mut("hourly").unwrap().last_run = Some("2026-03-04T12:00:00Z".parse().unwrap());
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_fires_only_latest_occurrence_after_gap() {
        // A per-minute cron resuming after a dispatch gap (active_hours
        // window reopened at 09:00) must fire only the most recent
        // occurrence, not replay the 08:5x backlog inside the horizon one
        // tick at a time.
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T09:00:05Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "every-minute".to_string(),
            cron: "* * * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let mut state = orchestrator_core::ScheduleState::default();
        state.schedules.insert(
            "every-minute".to_string(),
            orchestrator_core::ScheduleRunState {
                last_run: Some("2026-03-03T17:00:00Z".parse().unwrap()),
                last_status: "dispatched".to_string(),
                run_count: 1,
                missed_count: 0,
            },
        );

        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert_eq!(due, vec![("every-minute".to_string(), "2026-03-04T09:00:00Z".parse().unwrap())]);

        // Recording last_run = 09:00 leaves nothing due until 09:01.
        state.schedules.get_mut("every-minute").unwrap().last_run = Some("2026-03-04T09:00:00Z".parse().unwrap());
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_does_not_replay_occurrences_older_than_horizon() {
        // Occurrences suppressed for longer than the catch-up horizon (daemon
        // stopped, active_hours gate closed) are skipped, not replayed: a
        // daily 08:00 cron must not get a delayed run when dispatch resumes
        // at 09:00.
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T09:00:10Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "morning".to_string(),
            cron: "0 8 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let mut state = orchestrator_core::ScheduleState::default();
        state.schedules.insert(
            "morning".to_string(),
            orchestrator_core::ScheduleRunState {
                last_run: Some("2026-03-03T08:00:00Z".parse().unwrap()),
                last_status: "dispatched".to_string(),
                run_count: 1,
                missed_count: 0,
            },
        );

        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(due.is_empty(), "08:00 fire suppressed past the horizon must not replay at 09:00");
    }

    #[test]
    fn evaluate_schedules_first_run_catches_up_within_horizon() {
        // A fresh schedule (no recorded last_run) whose cron minute fell
        // between ticks must still fire when the tick lands inside the
        // catch-up horizon — otherwise schedules with interval_secs > 60
        // could miss every occurrence forever.
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:03:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "hourly".to_string(),
            cron: "0 * * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let mut state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert_eq!(due, vec![("hourly".to_string(), "2026-03-04T12:00:00Z".parse().unwrap())]);

        // Recording last_run = 12:00 fully catches up: the occurrence fires
        // once, not on every subsequent tick.
        state.schedules.insert(
            "hourly".to_string(),
            orchestrator_core::ScheduleRunState {
                last_run: Some("2026-03-04T12:00:00Z".parse().unwrap()),
                last_status: "dispatched".to_string(),
                run_count: 1,
                missed_count: 0,
            },
        );
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_first_run_skips_occurrence_outside_horizon() {
        // Without any recorded last_run, occurrences older than the catch-up
        // horizon are skipped: a fresh daemon never replays a stale backlog.
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:20:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "hourly".to_string(),
            cron: "0 * * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(due.is_empty(), "12:00 occurrence is past the horizon and must not fire at 12:20");
    }

    #[test]
    fn process_due_schedules_records_pipeline_dispatch_and_input() {
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let mut config = orchestrator_core::builtin_workflow_config();
        config.default_workflow_ref = "standard-workflow".to_string();
        for phase_id in ["requirements", "implementation", "code-review", "testing"] {
            config.phase_catalog.insert(
                phase_id.to_string(),
                orchestrator_core::PhaseUiDefinition {
                    label: phase_id.to_string(),
                    description: String::new(),
                    category: String::new(),
                    icon: None,
                    docs_url: None,
                    tags: Vec::new(),
                    visible: true,
                },
            );
        }
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "standard-workflow".to_string(),
            name: "Standard Workflow".to_string(),
            description: "Test fixture pipeline.".to_string(),
            phases: vec![
                orchestrator_core::WorkflowPhaseEntry::Simple("requirements".into()),
                orchestrator_core::WorkflowPhaseEntry::Simple("implementation".into()),
                orchestrator_core::WorkflowPhaseEntry::Simple("code-review".into()),
                orchestrator_core::WorkflowPhaseEntry::Simple("testing".into()),
            ],
            variables: Vec::new(),
            worktree: None,
            budget: None,
            environment: None,
            workspace: None,
        });
        config.schedules.push(orchestrator_core::WorkflowSchedule {
            id: "nightly".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: Some(json!({"scope":"nightly"})),
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("workflow config should be written");
        let _seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(project_root);

        let pipeline_calls = Arc::new(Mutex::new(Vec::new()));
        let pipeline_calls_ref = pipeline_calls.clone();

        let outcomes = ScheduleDispatch::process_due_schedules(
            project_root.to_string_lossy().as_ref(),
            now,
            move |schedule_id, dispatch| {
                pipeline_calls_ref.lock().expect("pipeline lock").push((
                    schedule_id.to_string(),
                    dispatch.workflow_ref.clone(),
                    dispatch.input.as_ref().map(|value| value.to_string()),
                ));
                Ok(())
            },
        );
        for outcome in &outcomes {
            orchestrator_core::project_schedule_dispatch_attempt(
                project_root.to_string_lossy().as_ref(),
                &outcome.schedule_id,
                outcome.run_at,
                &outcome.status,
            );
        }

        let calls = pipeline_calls.lock().expect("pipeline lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "nightly");
        assert_eq!(calls[0].1, "standard-workflow");
        assert_eq!(calls[0].2.as_deref(), Some(r#"{"scope":"nightly"}"#));
        assert_eq!(
            outcomes,
            vec![ScheduleDispatchOutcome {
                schedule_id: "nightly".to_string(),
                run_at: now,
                status: "dispatched".to_string()
            }]
        );

        let state = orchestrator_core::load_schedule_state(project_root).expect("schedule state loads");
        let entry = state.schedules.get("nightly").expect("nightly schedule state should exist");
        assert_eq!(entry.last_status, "dispatched");
        assert_eq!(entry.run_count, 1);
        assert_eq!(entry.last_run, Some(now));
    }

    #[test]
    fn process_due_schedules_marks_missing_workflow_ref_as_failed() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedule = orchestrator_core::WorkflowSchedule {
            id: "broken".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: None,
            command: Some("echo cleanup".to_string()),
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        };
        let pipeline_calls = Arc::new(Mutex::new(Vec::new()));
        let pipeline_calls_ref = pipeline_calls.clone();
        let mut record_pipeline_call = move |schedule_id: &str, dispatch: &SubjectDispatch| {
            pipeline_calls_ref
                .lock()
                .expect("pipeline lock")
                .push((schedule_id.to_string(), dispatch.workflow_ref.clone()));
            Ok(())
        };

        let status = dispatch_schedule("broken", &schedule, now, "schedule", &mut record_pipeline_call);
        assert_eq!(status, "failed: schedule is missing workflow_ref");

        let calls = pipeline_calls.lock().expect("pipeline lock");
        assert!(calls.is_empty());
    }

    #[test]
    fn owner_scoped_schedule_dispatches_minted_actor() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedule = orchestrator_core::WorkflowSchedule {
            id: "nightly".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: Some("alice".to_string()),
            claims: vec!["admin".to_string()],
        };
        let captured = Arc::new(Mutex::new(None));
        let captured_ref = captured.clone();
        let mut spawn = move |_id: &str, dispatch: &SubjectDispatch| {
            *captured_ref.lock().expect("lock") = dispatch.actor.clone();
            Ok(())
        };

        let status = dispatch_schedule("nightly", &schedule, now, "schedule", &mut spawn);
        assert_eq!(status, "dispatched");
        let actor = captured.lock().expect("lock").clone().expect("owner schedule must mint an actor");
        assert_eq!(actor.user_id, "alice");
        assert_eq!(actor.claims, vec!["admin".to_string()]);
        assert_eq!(actor.tenant_id, None);
    }

    #[test]
    fn global_schedule_dispatches_without_actor() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedule = orchestrator_core::WorkflowSchedule {
            id: "global".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        };
        let captured = Arc::new(Mutex::new(Some(animus_actor::Actor::new("sentinel"))));
        let captured_ref = captured.clone();
        let mut spawn = move |_id: &str, dispatch: &SubjectDispatch| {
            *captured_ref.lock().expect("lock") = dispatch.actor.clone();
            Ok(())
        };

        let status = dispatch_schedule("global", &schedule, now, "schedule", &mut spawn);
        assert_eq!(status, "dispatched");
        assert!(captured.lock().expect("lock").is_none(), "a schedule without owner_id must stay global (actor=None)");
    }

    #[test]
    fn owner_id_whitespace_only_stays_global() {
        let schedule = orchestrator_core::WorkflowSchedule {
            id: "blank-owner".to_string(),
            cron: "* * * * *".to_string(),
            workflow_ref: Some("wf".to_string()),
            command: None,
            input: None,
            enabled: true,
            owner_id: Some("   ".to_string()),
            claims: Vec::new(),
        };
        assert!(mint_schedule_actor(&schedule).is_none(), "a blank owner_id must not mint an actor");
    }

    fn deadline_schedule(id: &str, cron: &str, enabled: bool) -> orchestrator_core::WorkflowSchedule {
        orchestrator_core::WorkflowSchedule {
            id: id.to_string(),
            cron: cron.to_string(),
            workflow_ref: Some("standard-workflow".to_string()),
            command: None,
            input: None,
            enabled,
            owner_id: None,
            claims: Vec::new(),
        }
    }

    #[test]
    fn next_deadline_is_minimum_across_multiple_schedules() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:10Z".parse().expect("timestamp should parse");
        let schedules = vec![
            deadline_schedule("hourly", "0 * * * *", true),       // next: 13:00
            deadline_schedule("quarterly", "*/15 * * * *", true), // next: 12:45
            deadline_schedule("daily", "0 8 * * *", true),        // next: tomorrow 08:00
        ];
        let deadline = next_deadline_for_schedules(&schedules, now).expect("deadline should exist");
        assert_eq!(deadline, "2026-03-04T12:45:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap());
    }

    #[test]
    fn next_deadline_is_strictly_after_now() {
        // An occurrence exactly at `now` belongs to the tick that is
        // currently running; the deadline must be the NEXT one, otherwise
        // the loop would re-fire on the same minute (busy loop).
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:00:00Z".parse().expect("timestamp should parse");
        let schedules = vec![deadline_schedule("hourly", "0 * * * *", true)];
        let deadline = next_deadline_for_schedules(&schedules, now).expect("deadline should exist");
        assert_eq!(deadline, "2026-03-04T13:00:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap());
    }

    #[test]
    fn next_deadline_is_none_without_schedules() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        assert!(next_deadline_for_schedules(&[], now).is_none(), "no schedules means pure heartbeat mode");
    }

    #[test]
    fn next_deadline_skips_disabled_invalid_and_empty_cron() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:10Z".parse().expect("timestamp should parse");
        let schedules = vec![
            deadline_schedule("disabled", "*/5 * * * *", false),
            deadline_schedule("broken", "*/0 * * * *", true),
            deadline_schedule("blank", "   ", true),
        ];
        assert!(
            next_deadline_for_schedules(&schedules, now).is_none(),
            "disabled / invalid / empty cron entries must not contribute a deadline"
        );

        // A valid schedule alongside the broken ones still yields its own deadline.
        let mut with_valid = schedules;
        with_valid.push(deadline_schedule("hourly", "0 * * * *", true));
        let deadline = next_deadline_for_schedules(&with_valid, now).expect("valid schedule should yield deadline");
        assert_eq!(deadline, "2026-03-04T13:00:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap());
    }

    #[test]
    fn next_schedule_deadline_recomputes_from_reloaded_config() {
        // The deadline is recomputed from the compiled config on every loop
        // pass, so a workflow-config reload that adds/changes schedules is
        // reflected on the next computation without daemon restart.
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let root_str = project_root.to_string_lossy().to_string();
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:10Z".parse().expect("timestamp should parse");

        let mut config = orchestrator_core::builtin_workflow_config();
        config.default_workflow_ref = "standard-workflow".to_string();
        for phase_id in ["requirements", "implementation"] {
            config.phase_catalog.insert(
                phase_id.to_string(),
                orchestrator_core::PhaseUiDefinition {
                    label: phase_id.to_string(),
                    description: String::new(),
                    category: String::new(),
                    icon: None,
                    docs_url: None,
                    tags: Vec::new(),
                    visible: true,
                },
            );
        }
        config.workflows.push(orchestrator_core::WorkflowDefinition {
            id: "standard-workflow".to_string(),
            name: "Standard Workflow".to_string(),
            description: "Test fixture pipeline.".to_string(),
            phases: vec![
                orchestrator_core::WorkflowPhaseEntry::Simple("requirements".into()),
                orchestrator_core::WorkflowPhaseEntry::Simple("implementation".into()),
            ],
            variables: Vec::new(),
            worktree: None,
            budget: None,
            environment: None,
            workspace: None,
        });
        let workflow_ref = config.default_workflow_ref.clone();
        config.schedules.push(orchestrator_core::WorkflowSchedule {
            id: "hourly".to_string(),
            cron: "0 * * * *".to_string(),
            workflow_ref: Some(workflow_ref.clone()),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("workflow config should be written");
        let _seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(project_root);

        let first = ScheduleDispatch::next_schedule_deadline(&root_str, now).expect("deadline should exist");
        assert_eq!(first, "2026-03-04T13:00:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap());

        // Reload: a tighter schedule lands in the compiled config.
        config.schedules.push(orchestrator_core::WorkflowSchedule {
            id: "quarterly".to_string(),
            cron: "*/15 * * * *".to_string(),
            workflow_ref: Some(workflow_ref),
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("workflow config should be rewritten");
        drop(_seam);
        let _seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(project_root);

        let second = ScheduleDispatch::next_schedule_deadline(&root_str, now).expect("deadline should exist");
        assert_eq!(second, "2026-03-04T12:45:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap());
    }

    #[test]
    fn active_hours_normal_range() {
        let time = |hour, minute| chrono::NaiveTime::from_hms_opt(hour, minute, 0).unwrap();
        assert!(is_within_active_hours("09:00-17:00", time(9, 0)));
        assert!(is_within_active_hours("09:00-17:00", time(12, 30)));
        assert!(is_within_active_hours("09:00-17:00", time(16, 59)));
        assert!(!is_within_active_hours("09:00-17:00", time(17, 0)));
        assert!(!is_within_active_hours("09:00-17:00", time(8, 59)));
        assert!(!is_within_active_hours("09:00-17:00", time(0, 0)));
    }

    #[test]
    fn active_hours_wrap_around() {
        let time = |hour, minute| chrono::NaiveTime::from_hms_opt(hour, minute, 0).unwrap();
        assert!(is_within_active_hours("22:00-06:00", time(22, 0)));
        assert!(is_within_active_hours("22:00-06:00", time(23, 59)));
        assert!(is_within_active_hours("22:00-06:00", time(0, 0)));
        assert!(is_within_active_hours("22:00-06:00", time(5, 59)));
        assert!(!is_within_active_hours("22:00-06:00", time(6, 0)));
        assert!(!is_within_active_hours("22:00-06:00", time(12, 0)));
        assert!(!is_within_active_hours("22:00-06:00", time(21, 59)));
    }

    #[test]
    fn active_hours_invalid_returns_true() {
        let time = chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert!(is_within_active_hours("invalid", time));
        assert!(is_within_active_hours("", time));
        assert!(is_within_active_hours("25:00-06:00", time));
    }

    #[test]
    fn parse_active_hours_valid() {
        assert_eq!(parse_active_hours("09:00-17:00"), Some((540, 1020)));
        assert_eq!(parse_active_hours("00:00-06:00"), Some((0, 360)));
        assert_eq!(parse_active_hours("22:00-06:00"), Some((1320, 360)));
    }

    #[test]
    fn parse_active_hours_invalid() {
        assert_eq!(parse_active_hours("invalid"), None);
        assert_eq!(parse_active_hours("25:00-06:00"), None);
        assert_eq!(parse_active_hours("09:00"), None);
    }

    fn make_due_schedule(
    ) -> (Vec<orchestrator_core::WorkflowSchedule>, orchestrator_core::ScheduleState, chrono::DateTime<chrono::Utc>)
    {
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "every-minute".to_string(),
            cron: "* * * * *".to_string(),
            workflow_ref: None,
            command: None,
            input: None,
            enabled: true,
            owner_id: None,
            claims: Vec::new(),
        }];
        let state = orchestrator_core::ScheduleState::default();
        let now: chrono::DateTime<chrono::Utc> = "2026-03-07T14:00:00Z".parse().unwrap();
        (schedules, state, now)
    }

    #[test]
    fn active_hours_gate_skips_due_schedules() {
        let (schedules, state, now) = make_due_schedule();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(!due.is_empty(), "schedule should be due at this time");

        let outside_hours = chrono::NaiveTime::from_hms_opt(14, 0, 0).unwrap();
        let within = ScheduleDispatch::allows_proactive_dispatch(Some("22:00-06:00"), outside_hours);
        assert!(!within, "14:00 is outside 22:00-06:00");
    }

    #[test]
    fn active_hours_gate_allows_due_schedules_inside_window() {
        let (schedules, state, now) = make_due_schedule();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(!due.is_empty(), "schedule should be due at this time");

        let inside_hours = chrono::NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        let within = ScheduleDispatch::allows_proactive_dispatch(Some("22:00-06:00"), inside_hours);
        assert!(within, "23:00 is inside 22:00-06:00");
    }

    #[test]
    fn active_hours_unset_allows_all_schedules() {
        let (schedules, state, now) = make_due_schedule();
        let due = evaluate_schedules(&schedules, &state, now, |_| true);
        assert!(!due.is_empty(), "schedule should be due");

        let within =
            ScheduleDispatch::allows_proactive_dispatch(None, chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap());
        assert!(within, "no active_hours config should allow all schedules");
    }
}
