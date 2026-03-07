use anyhow::Result;
use chrono::{Datelike, Timelike};

pub struct ScheduleDispatch;

impl ScheduleDispatch {
    pub fn allows_proactive_dispatch(active_hours: Option<&str>, now: chrono::NaiveTime) -> bool {
        active_hours
            .map(|spec| is_within_active_hours(spec, now))
            .unwrap_or(true)
    }

    pub fn process_due_schedules<PipelineSpawner, CommandSpawner>(
        project_root: &str,
        now: chrono::DateTime<chrono::Utc>,
        mut spawn_pipeline: PipelineSpawner,
        mut spawn_command: CommandSpawner,
    ) where
        PipelineSpawner: FnMut(&str, &str, Option<&str>) -> Result<()>,
        CommandSpawner: FnMut(&str, &str) -> Result<()>,
    {
        let config =
            orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
        let mut state = orchestrator_core::load_schedule_state(std::path::Path::new(project_root))
            .unwrap_or_default();
        let due = evaluate_schedules(&config.config.schedules, &state, now);
        if due.is_empty() {
            return;
        }

        let schedule_lookup: std::collections::HashMap<
            &str,
            &orchestrator_core::workflow_config::WorkflowSchedule,
        > = config
            .config
            .schedules
            .iter()
            .map(|schedule| (schedule.id.as_str(), schedule))
            .collect();

        for schedule_id in &due {
            let mut status = "evaluated".to_string();

            if let Some(schedule) = schedule_lookup.get(schedule_id.as_str()) {
                if let Some(ref pipeline_id) = schedule.pipeline {
                    let input_json = schedule
                        .input
                        .as_ref()
                        .and_then(|value| serde_json::to_string(value).ok());
                    match spawn_pipeline(schedule_id, pipeline_id, input_json.as_deref()) {
                        Ok(()) => {
                            status = "dispatched".to_string();
                        }
                        Err(error) => {
                            status = format!("failed: {error}");
                            eprintln!(
                                "{}: schedule '{}' pipeline '{}' dispatch failed: {}",
                                protocol::ACTOR_DAEMON,
                                schedule_id,
                                pipeline_id,
                                error
                            );
                        }
                    }
                } else if let Some(ref command) = schedule.command {
                    match spawn_command(schedule_id, command) {
                        Ok(()) => {
                            status = "dispatched".to_string();
                        }
                        Err(error) => {
                            status = format!("failed: {error}");
                            eprintln!(
                                "{}: schedule '{}' command dispatch failed: {}",
                                protocol::ACTOR_DAEMON,
                                schedule_id,
                                error
                            );
                        }
                    }
                }
            }

            let entry = state
                .schedules
                .entry(schedule_id.clone())
                .or_insert_with(orchestrator_core::ScheduleRunState::default);
            entry.last_run = Some(now);
            entry.last_status = status;
            entry.run_count = entry.run_count.saturating_add(1);
        }

        let _ = orchestrator_core::save_schedule_state(std::path::Path::new(project_root), &state);
    }

    pub fn update_completion_state(project_root: &str, schedule_id: &str, status: &str) {
        let path = std::path::Path::new(project_root);
        let mut state = orchestrator_core::load_schedule_state(path).unwrap_or_default();
        if let Some(entry) = state.schedules.get_mut(schedule_id) {
            entry.last_status = status.to_string();
            let _ = orchestrator_core::save_schedule_state(path, &state);
        }
    }
}

fn evaluate_schedules(
    schedules: &[orchestrator_core::workflow_config::WorkflowSchedule],
    state: &orchestrator_core::ScheduleState,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let mut due = Vec::new();
    for schedule in schedules {
        if !schedule.enabled || !cron_matches(&schedule.cron, now) {
            continue;
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

fn cron_matches(expression: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    let expression = expression.trim().to_ascii_lowercase();
    if expression.is_empty() {
        return false;
    }

    if expression.starts_with('@') {
        return match expression.as_str() {
            "@hourly" => now.minute() == 0,
            "@daily" => now.hour() == 0 && now.minute() == 0,
            "@weekly" => {
                now.weekday().num_days_from_sunday() == 0 && now.hour() == 0 && now.minute() == 0
            }
            "@monthly" => now.day() == 1 && now.hour() == 0 && now.minute() == 0,
            _ => false,
        };
    }

    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }

    let current_weekday = now.weekday().num_days_from_sunday();
    field_matches(fields[0], now.minute(), 0, 59)
        && field_matches(fields[1], now.hour(), 0, 23)
        && field_matches(fields[2], now.day(), 1, 31)
        && field_matches(fields[3], now.month(), 1, 12)
        && field_matches(fields[4], current_weekday, 0, 7)
}

fn field_matches(raw_field: &str, value: u32, min: u32, max: u32) -> bool {
    if raw_field == "*" {
        return true;
    }
    let parsed = match raw_field.parse::<u32>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    if parsed < min || parsed > max {
        return false;
    }
    let normalized = if max == 7 && parsed == 7 { 0 } else { parsed };
    normalized == value
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
    use super::*;

    #[test]
    fn cron_matches_exact_expression() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z"
            .parse()
            .expect("timestamp should parse");
        assert!(cron_matches("30 12 4 3 3", now));
        assert!(!cron_matches("31 12 4 3 4", now));
    }

    #[test]
    fn cron_matches_with_wildcards() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:00:00Z"
            .parse()
            .expect("timestamp should parse");
        assert!(cron_matches("* * * * *", now));
        assert!(cron_matches("0 * * * *", now));
    }

    #[test]
    fn cron_matches_shortcut_expressions() {
        let sunday_midnight: chrono::DateTime<chrono::Utc> = "2026-03-01T00:00:00Z"
            .parse()
            .expect("timestamp should parse");
        let quarter_hour: chrono::DateTime<chrono::Utc> = "2026-03-01T12:15:00Z"
            .parse()
            .expect("timestamp should parse");
        assert!(cron_matches("@weekly", sunday_midnight));
        assert!(cron_matches("@monthly", sunday_midnight));
        assert!(!cron_matches("@hourly", quarter_hour));
    }

    #[test]
    fn evaluate_schedules_skips_disabled_schedules() {
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z"
            .parse()
            .expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "disabled".to_string(),
            cron: "30 12 * * *".to_string(),
            pipeline: Some("standard".to_string()),
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
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z"
            .parse()
            .expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "midday".to_string(),
            cron: "30 12 * * *".to_string(),
            pipeline: Some("standard".to_string()),
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
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T00:00:00Z"
            .parse()
            .expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "daily".to_string(),
            cron: "@daily".to_string(),
            pipeline: Some("standard".to_string()),
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
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z"
            .parse()
            .expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "broken".to_string(),
            cron: "30 12".to_string(),
            pipeline: Some("standard".to_string()),
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
        let now: chrono::DateTime<chrono::Utc> = "2026-03-04T12:30:00Z"
            .parse()
            .expect("timestamp should parse");
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "recent".to_string(),
            cron: "30 12 * * *".to_string(),
            pipeline: Some("standard".to_string()),
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

    fn make_due_schedule() -> (
        Vec<orchestrator_core::WorkflowSchedule>,
        orchestrator_core::ScheduleState,
        chrono::DateTime<chrono::Utc>,
    ) {
        let schedules = vec![orchestrator_core::WorkflowSchedule {
            id: "every-minute".to_string(),
            cron: "* * * * *".to_string(),
            pipeline: None,
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
        let within =
            ScheduleDispatch::allows_proactive_dispatch(Some("22:00-06:00"), outside_hours);
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

        let within = ScheduleDispatch::allows_proactive_dispatch(
            None,
            chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
        );
        assert!(within, "no active_hours config should allow all schedules");
    }
}
