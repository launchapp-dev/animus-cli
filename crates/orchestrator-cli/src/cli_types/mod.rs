mod agent_types;
mod daemon_types;
mod doctor_types;
mod errors_types;
mod git_types;
mod history_types;
mod mcp_types;
mod model_types;
mod output_types;
mod pack_types;
mod queue_types;

mod project_types;
mod requirements_types;
mod root_types;
mod runner_types;
mod setup_types;
mod shared_types;
mod skill_types;
mod sync_types;
mod task_types;
mod web_types;
mod workflow_types;

pub(crate) use agent_types::*;
pub(crate) use daemon_types::*;
pub(crate) use doctor_types::*;
pub(crate) use errors_types::*;
pub(crate) use git_types::*;
pub(crate) use history_types::*;
pub(crate) use mcp_types::*;
pub(crate) use model_types::*;
pub(crate) use output_types::*;
pub(crate) use pack_types::*;
pub(crate) use queue_types::*;

pub(crate) use project_types::*;
pub(crate) use requirements_types::*;
pub(crate) use root_types::*;
pub(crate) use runner_types::*;
pub(crate) use setup_types::*;
pub(crate) use shared_types::*;
pub(crate) use skill_types::*;
pub(crate) use sync_types::*;
pub(crate) use task_types::*;
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
        assert!(help.contains("CLI provider to execute, for example claude, codex, or gemini."));
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
    fn parses_pack_install_command() {
        let cli = Cli::try_parse_from(["ao", "pack", "install", "--path", "./fixtures/ao.review", "--activate"])
            .expect("pack install should parse");

        match cli.command {
            Command::Pack { command: PackCommand::Install(args) } => {
                assert_eq!(args.path.as_deref(), Some("./fixtures/ao.review"));
                assert!(args.activate);
                assert!(!args.force);
            }
            _ => panic!("expected pack install command"),
        }
    }

    #[test]
    fn parses_queue_enqueue_command() {
        let cli = Cli::try_parse_from(["ao", "queue", "enqueue", "--task-id", "TASK-123", "--workflow-ref", "ops"])
            .expect("queue enqueue command should parse");

        match cli.command {
            Command::Queue { command: QueueCommand::Enqueue(args) } => {
                assert_eq!(args.task_id.as_deref(), Some("TASK-123"));
                assert_eq!(args.workflow_ref.as_deref(), Some("ops"));
            }
            _ => panic!("expected queue enqueue command"),
        }
    }

    #[test]
    fn parses_requirements_execute_with_single_id() {
        let cli = Cli::try_parse_from(["ao", "requirements", "execute", "--id", "REQ-123"])
            .expect("requirements execute should parse");

        match cli.command {
            Command::Requirements { command: RequirementsCommand::Execute(args) } => {
                assert_eq!(args.requirement_id, "REQ-123");
            }
            _ => panic!("expected requirements execute command"),
        }
    }

    #[test]
    fn parses_workflow_run_with_positional_pipeline() {
        let cli = Cli::try_parse_from(["ao", "workflow", "run", "ao.task/standard", "--task-id", "TASK-123"])
            .expect("workflow run should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::Run(args) } => {
                assert_eq!(args.pipeline.as_deref(), Some("ao.task/standard"));
                assert_eq!(args.task_id.as_deref(), Some("TASK-123"));
            }
            _ => panic!("expected workflow run command"),
        }
    }

    #[test]
    fn rejects_removed_task_prioritized_command() {
        let error = Cli::try_parse_from(["ao", "task", "prioritized"]).expect_err("removed command should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_removed_workflow_update_definition_command() {
        let error =
            Cli::try_parse_from(["ao", "workflow", "update-definition"]).expect_err("removed command should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }
}
