mod agent_types;
mod architecture_types;
mod daemon_types;
mod doctor_types;
mod errors_types;
mod execute_types;
mod git_types;
mod history_types;
mod mcp_types;
mod model_types;
mod output_types;
mod planning_types;
mod project_types;
mod qa_types;
mod requirements_types;
mod review_types;
mod root_types;
mod runner_types;
mod setup_types;
mod shared_types;
mod skill_types;
mod task_control_types;
mod task_types;
mod tui_types;
mod vision_types;
mod web_types;
mod workflow_types;

pub(crate) use agent_types::*;
pub(crate) use architecture_types::*;
pub(crate) use daemon_types::*;
pub(crate) use doctor_types::*;
pub(crate) use errors_types::*;
pub(crate) use execute_types::*;
pub(crate) use git_types::*;
pub(crate) use history_types::*;
pub(crate) use mcp_types::*;
pub(crate) use model_types::*;
pub(crate) use output_types::*;
pub(crate) use planning_types::*;
pub(crate) use project_types::*;
pub(crate) use qa_types::*;
pub(crate) use requirements_types::*;
pub(crate) use review_types::*;
pub(crate) use root_types::*;
pub(crate) use runner_types::*;
pub(crate) use setup_types::*;
pub(crate) use shared_types::*;
pub(crate) use skill_types::*;
pub(crate) use task_control_types::*;
pub(crate) use task_types::*;
pub(crate) use tui_types::*;
pub(crate) use vision_types::*;
pub(crate) use web_types::*;
pub(crate) use workflow_types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::Parser;

    #[test]
    fn agent_run_help_includes_actionable_field_descriptions() {
        let error = Cli::try_parse_from(["ao", "agent", "run", "--help"])
            .expect_err("help output should short-circuit parsing");
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("Run identifier. Omit to auto-generate a UUID."));
        assert!(help.contains("CLI provider to execute, for example codex or claude."));
        assert!(help.contains("Runner config scope: project or global."));
    }

    #[test]
    fn daemon_run_rejects_zero_interval_with_clear_validation_error() {
        let error = Cli::try_parse_from(["ao", "daemon", "run", "--interval-secs", "0"])
            .expect_err("zero interval should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--interval-secs"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_run_rejects_zero_max_tasks_per_tick_with_clear_validation_error() {
        let error = Cli::try_parse_from(["ao", "daemon", "run", "--max-tasks-per-tick", "0"])
            .expect_err("zero max-tasks-per-tick should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--max-tasks-per-tick"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_run_rejects_zero_stale_threshold_hours_with_clear_validation_error() {
        let error = Cli::try_parse_from(["ao", "daemon", "run", "--stale-threshold-hours", "0"])
            .expect_err("zero stale threshold should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--stale-threshold-hours"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_events_rejects_zero_limit() {
        let error = Cli::try_parse_from(["ao", "daemon", "events", "--limit", "0"])
            .expect_err("zero limit should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--limit"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn parses_top_level_status_command() {
        let cli = Cli::try_parse_from(["ao", "status"]).expect("status command should parse");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parses_task_list_filters_from_task_module() {
        let cli = Cli::try_parse_from([
            "ao",
            "task",
            "list",
            "--task-type",
            "feature",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--assignee-type",
            "human",
            "--tag",
            "api",
            "--linked-requirement",
            "REQ-123",
            "--linked-architecture-entity",
            "ARCH-42",
            "--search",
            "critical path",
        ])
        .expect("task list command should parse");

        match cli.command {
            Command::Task {
                command: TaskCommand::List(args),
            } => {
                assert_eq!(args.task_type.as_deref(), Some("feature"));
                assert_eq!(args.status.as_deref(), Some("in-progress"));
                assert_eq!(args.priority.as_deref(), Some("high"));
                assert_eq!(args.assignee_type.as_deref(), Some("human"));
                assert_eq!(args.tag, vec!["api".to_string()]);
                assert_eq!(args.linked_requirement.as_deref(), Some("REQ-123"));
                assert_eq!(args.linked_architecture_entity.as_deref(), Some("ARCH-42"));
                assert_eq!(args.search.as_deref(), Some("critical path"));
            }
            _ => panic!("expected task list command"),
        }
    }

    #[test]
    fn task_stats_parses_stale_threshold_override() {
        let cli = Cli::try_parse_from(["ao", "task", "stats", "--stale-threshold-hours", "72"])
            .expect("task stats command should parse");

        match cli.command {
            Command::Task {
                command: TaskCommand::Stats(args),
            } => {
                assert_eq!(args.stale_threshold_hours, 72);
            }
            _ => panic!("expected task stats command"),
        }
    }

    #[test]
    fn task_stats_rejects_zero_stale_threshold_hours_with_clear_validation_error() {
        let error = Cli::try_parse_from(["ao", "task", "stats", "--stale-threshold-hours", "0"])
            .expect_err("zero stale threshold should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--stale-threshold-hours"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn task_control_rebalance_priority_parses_apply_and_overrides() {
        let cli = Cli::try_parse_from([
            "ao",
            "task-control",
            "rebalance-priority",
            "--high-budget-percent",
            "15",
            "--essential-task-id",
            "TASK-001",
            "--nice-to-have-task-id",
            "TASK-009",
            "--apply",
            "--confirm",
            "apply",
        ])
        .expect("task-control rebalance-priority should parse");

        match cli.command {
            Command::TaskControl {
                command: TaskControlCommand::RebalancePriority(args),
            } => {
                assert_eq!(args.high_budget_percent, 15);
                assert_eq!(args.essential_task_id, vec!["TASK-001".to_string()]);
                assert_eq!(args.nice_to_have_task_id, vec!["TASK-009".to_string()]);
                assert!(args.apply);
                assert_eq!(args.confirm.as_deref(), Some("apply"));
            }
            _ => panic!("expected task-control rebalance-priority command"),
        }
    }

    #[test]
    fn task_control_rebalance_priority_rejects_budget_above_100() {
        let error = Cli::try_parse_from([
            "ao",
            "task-control",
            "rebalance-priority",
            "--high-budget-percent",
            "101",
        ])
        .expect_err("budget above 100 should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--high-budget-percent"));
        assert!(message.contains("between 0 and 100"));
    }

    #[test]
    fn parses_workflow_phase_approve_from_workflow_module() {
        let cli = Cli::try_parse_from([
            "ao",
            "workflow",
            "phase",
            "approve",
            "--id",
            "WF-001",
            "--phase",
            "testing",
            "--note",
            "gate approved",
        ])
        .expect("workflow phase approve should parse");

        match cli.command {
            Command::Workflow {
                command:
                    WorkflowCommand::Phase {
                        command: WorkflowPhaseCommand::Approve(args),
                    },
            } => {
                assert_eq!(args.id, "WF-001");
                assert_eq!(args.phase, "testing");
                assert_eq!(args.note, "gate approved");
            }
            _ => panic!("expected workflow phase approve command"),
        }
    }
}
