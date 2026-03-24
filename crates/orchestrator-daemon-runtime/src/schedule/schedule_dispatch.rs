use anyhow::Result;
use chrono::{Datelike, Timelike};
use croner::parser::{CronParser, Seconds, Year};
use tracing::warn;

use super::ScheduleDispatchOutcome;
use crate::SubjectDispatch;

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
        let due = evaluate_schedules(&config.config.schedules, &state, now);
        if due.is_empty() {
            return Vec::new();
        }

        let schedule_lookup: std::collections::HashMap<&str, &orchestrator_core::workflow_config::WorkflowSchedule> =
            config.config.schedules.iter().map(|schedule| (schedule.id.as_str(), schedule)).collect();

        let mut outcomes = Vec::with_capacity(due.len());
        for schedule_id in due {
            if let Some(schedule) = schedule_lookup.get(schedule_id.as_str()) {
                let status = dispatch_schedule(&schedule_id, schedule, now, "schedule", &mut spawn_pipeline);
                outcomes.push(ScheduleDispatchOutcome { schedule_id, status });
            }
        }

        outcomes
    }
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
        );
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
            schedule_id,
            "schedule is missing workflow_ref and will not be dispatched"
        );
        "failed: schedule is missing workflow_ref".to_string()
    };

    status
}

fn evaluate_schedules(
    schedules: &[orchestrator_core::workflow_config::WorkflowSchedule],
    state: &orchestrator_core::ScheduleState,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let mut due = Vec::new();
    for schedule in schedules {
        if !schedule.enabled {
            continue;
        }

        match cron_matches(&schedule.cron, now) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    schedule_id = %schedule.id,
                    cron = %schedule.cron,
                    error = %error,
                    "schedule has invalid cron expression"
                );
                continue;
            }
        }

        if let Some(run_state) = state.schedules.get(&schedule.id) {
            if let Some(last_run) = run_state.last_run {
                if last_run.year() == now.year()
                    && last_run.month() == now.month()
                    && last_run.day() == now.day()
                    && last_run.hour() == now.hour()
                    && last_run.minute() == now.minute()
                {
                    continue;
                }
            }
        }

        due.push(schedule.id.clone());
    }

    due
}

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
            workflow_ref: Some("standard".to_string()),
            command: None,
            input: None,
            enabled: false,
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now);

        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_matches_five_field_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "midday".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard".to_string()),
            command: None,
            input: None,
            enabled: true,
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now);

        assert_eq!(due, vec!["midday".to_string()]);
    }

    #[test]
    fn evaluate_schedules_matches_shortcut_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T00:00:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "daily".to_string(),
            cron: "@daily".to_string(),
            workflow_ref: Some("standard".to_string()),
            command: None,
            input: None,
            enabled: true,
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now);

        assert_eq!(due, vec!["daily".to_string()]);
    }

    #[test]
    fn evaluate_schedules_skips_invalid_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "broken".to_string(),
            cron: "*/0 * * * *".to_string(),
            workflow_ref: Some("standard".to_string()),
            command: None,
            input: None,
            enabled: true,
        }];
        let state = orchestrator_core::ScheduleState::default();
        let due = evaluate_schedules(&schedules, &state, now);

        assert!(due.is_empty());
    }

    #[test]
    fn evaluate_schedules_skips_already_ran_this_minute() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "recent".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard".to_string()),
            command: None,
            input: None,
            enabled: true,
        }];
        let mut state = orchestrator_core::ScheduleState::default();
        state.schedules.insert(
            "recent".to_string(),
            orchestrator_core::ScheduleRunState {
                last_run: Some(now),
                last_status: "evaluated".to_string(),
                run_count: 1,
            },
        );
        let due = evaluate_schedules(&schedules, &state, now);

        assert!(due.is_empty());
    }

    #[test]
    fn process_due_schedules_records_pipeline_dispatch_and_input() {
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z".parse().expect("timestamp should parse");
        let mut config = orchestrator_core::builtin_workflow_config();
        config.schedules.push(orchestrator_core::WorkflowSchedule {
            id: "nightly".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: Some("standard".to_string()),
            command: None,
            input: Some(json!({"scope":"nightly"})),
            enabled: true,
        });
        orchestrator_core::write_workflow_config(project_root, &config).expect("workflow config should be written");

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
                now,
                &outcome.status,
            );
        }

        let calls = pipeline_calls.lock().expect("pipeline lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "nightly");
        assert_eq!(calls[0].1, "standard");
        assert_eq!(calls[0].2.as_deref(), Some(r#"{"scope":"nightly"}"#));
        assert_eq!(
            outcomes,
            vec![ScheduleDispatchOutcome { schedule_id: "nightly".to_string(), status: "dispatched".to_string() }]
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
        }];
        let state = orchestrator_core::ScheduleState::default();
        let now: chrono::DateTime<chrono::Utc> = "2026-03-07T14:00:00Z".parse().unwrap();
        (schedules, state, now)
    }

    #[test]
    fn active_hours_gate_skips_due_schedules() {
        let (schedules, state, now) = make_due_schedule();
        let due = evaluate_schedules(&schedules, &state, now);
        assert!(!due.is_empty(), "schedule should be due at this time");

        let outside_hours = chrono::NaiveTime::from_hms_opt(14, 0, 0).unwrap();
        let within = ScheduleDispatch::allows_proactive_dispatch(Some("22:00-06:00"), outside_hours);
        assert!(!within, "14:00 is outside 22:00-06:00");
    }

    #[test]
    fn active_hours_gate_allows_due_schedules_inside_window() {
        let (schedules, state, now) = make_due_schedule();
        let due = evaluate_schedules(&schedules, &state, now);
        assert!(!due.is_empty(), "schedule should be due at this time");

        let inside_hours = chrono::NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        let within = ScheduleDispatch::allows_proactive_dispatch(Some("22:00-06:00"), inside_hours);
        assert!(within, "23:00 is inside 22:00-06:00");
    }

    #[test]
    fn active_hours_unset_allows_all_schedules() {
        let (schedules, state, now) = make_due_schedule();
        let due = evaluate_schedules(&schedules, &state, now);
        assert!(!due.is_empty(), "schedule should be due");

        let within =
            ScheduleDispatch::allows_proactive_dispatch(None, chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap());
        assert!(within, "no active_hours config should allow all schedules");
    }
}
