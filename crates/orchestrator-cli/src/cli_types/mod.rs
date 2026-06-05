mod agent_types;
mod daemon_types;
mod doctor_types;
mod flavor_types;
mod git_types;
mod history_types;
mod init_types;
mod logs_types;
mod mcp_types;
mod metrics_types;
mod model_types;
mod output_types;
mod pack_types;
mod plugin_types;
mod queue_types;

mod project_types;
mod root_types;
mod runner_types;
mod self_types;
mod shared_types;
mod skill_types;
mod subject_types;
mod trigger_types;
mod web_types;
mod workflow_types;

pub(crate) use agent_types::*;
pub(crate) use daemon_types::*;
pub(crate) use doctor_types::*;
pub(crate) use flavor_types::*;
pub(crate) use git_types::*;
pub(crate) use history_types::*;
pub(crate) use init_types::*;
pub(crate) use logs_types::*;
pub(crate) use mcp_types::*;
pub(crate) use metrics_types::*;
pub(crate) use model_types::*;
pub(crate) use output_types::*;
pub(crate) use pack_types::*;
pub(crate) use plugin_types::*;
pub(crate) use queue_types::*;

pub(crate) use project_types::*;
pub(crate) use root_types::*;
pub(crate) use runner_types::*;
pub(crate) use self_types::*;
pub(crate) use shared_types::*;
pub(crate) use skill_types::*;
pub(crate) use subject_types::*;
pub(crate) use trigger_types::*;
pub(crate) use web_types::*;
pub(crate) use workflow_types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::CommandFactory;
    use clap::Parser;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read_doc(relative_path: &str) -> String {
        let path = repo_root().join(relative_path);
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
    }

    fn documented_top_level_commands() -> Vec<String> {
        read_doc("docs/reference/cli/index.md")
            .lines()
            .filter(|line| line.starts_with("├── ") || line.starts_with("└── "))
            .filter_map(|line| {
                let entry =
                    line.strip_prefix("├── ").or_else(|| line.strip_prefix("└── ")).expect("prefix already checked");
                entry.split_whitespace().next().map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn agent_run_help_includes_actionable_field_descriptions() {
        let error = Cli::try_parse_from(["animus", "agent", "run", "--help"])
            .expect_err("help output should short-circuit parsing");
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("Run identifier. Omit to auto-generate a UUID."));
        assert!(help.contains("CLI provider to execute, for example claude, codex, or gemini."));
        assert!(help.contains("Runner config scope: project or global."));
    }

    #[test]
    fn daemon_run_rejects_zero_interval_with_clear_validation_error() {
        let error = Cli::try_parse_from(["animus", "daemon", "run", "--interval-secs", "0"])
            .expect_err("zero interval should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--interval-secs"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_run_rejects_zero_max_tasks_per_tick_with_clear_validation_error() {
        let error = Cli::try_parse_from(["animus", "daemon", "run", "--max-tasks-per-tick", "0"])
            .expect_err("zero max-tasks-per-tick should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--max-tasks-per-tick"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_run_rejects_zero_stale_threshold_hours_with_clear_validation_error() {
        let error = Cli::try_parse_from(["animus", "daemon", "run", "--stale-threshold-hours", "0"])
            .expect_err("zero stale threshold should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--stale-threshold-hours"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_events_rejects_zero_limit() {
        let error = Cli::try_parse_from(["animus", "daemon", "events", "--limit", "0"])
            .expect_err("zero limit should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--limit"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn parses_top_level_status_command() {
        let cli = Cli::try_parse_from(["animus", "status"]).expect("status command should parse");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parses_self_update_command() {
        let cli = Cli::try_parse_from(["animus", "self", "update", "--check-only", "--prerelease"])
            .expect("self update should parse");
        match cli.command {
            Command::SelfCmd { command: SelfCommand::Update(args) } => {
                assert!(args.check_only);
                assert!(args.prerelease);
                assert!(!args.force);
                assert!(!args.yes);
            }
            _ => panic!("expected self update command"),
        }
    }

    #[test]
    fn parses_pack_install_command() {
        let cli =
            Cli::try_parse_from(["animus", "pack", "install", "--path", "./fixtures/animus.review", "--activate"])
                .expect("pack install should parse");

        match cli.command {
            Command::Pack { command: PackCommand::Install(args) } => {
                assert_eq!(args.path.as_deref(), Some("./fixtures/animus.review"));
                assert!(args.activate);
                assert!(!args.force);
            }
            _ => panic!("expected pack install command"),
        }
    }

    #[test]
    fn parses_queue_enqueue_command() {
        let cli = Cli::try_parse_from(["animus", "queue", "enqueue", "--task-id", "TASK-123", "--workflow-ref", "ops"])
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
    fn parses_workflow_run_with_positional_pipeline() {
        let cli = Cli::try_parse_from(["animus", "workflow", "run", "animus.task/standard", "--task-id", "TASK-123"])
            .expect("workflow run should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::Run(args) } => {
                assert_eq!(args.pipeline.as_deref(), Some("animus.task/standard"));
                assert_eq!(args.task_id.as_deref(), Some("TASK-123"));
            }
            _ => panic!("expected workflow run command"),
        }
    }

    #[test]
    fn rejects_removed_workflow_update_definition_command() {
        let error =
            Cli::try_parse_from(["animus", "workflow", "update-definition"]).expect_err("removed command should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    /// Codex round-5 P3 regression: replacing `IdArgs` with
    /// `WorkflowResumeArgs` dropped the `-i` short alias and broke
    /// `animus workflow resume -i <id>` scripts. The short flag must keep
    /// parsing alongside the canonical long form.
    #[test]
    fn workflow_resume_accepts_short_i_flag() {
        let cli = Cli::try_parse_from(["animus", "workflow", "resume", "-i", "wf-abc-123"])
            .expect("workflow resume -i must parse");
        match cli.command {
            Command::Workflow { command: WorkflowCommand::Resume(args) } => {
                assert_eq!(args.id, "wf-abc-123");
                assert!(!args.force, "force should default to false");
            }
            _ => panic!("expected workflow resume command"),
        }

        // Long form must still work in parallel.
        let cli_long = Cli::try_parse_from(["animus", "workflow", "resume", "--id", "wf-xyz"])
            .expect("workflow resume --id must continue to parse");
        match cli_long.command {
            Command::Workflow { command: WorkflowCommand::Resume(args) } => {
                assert_eq!(args.id, "wf-xyz");
            }
            _ => panic!("expected workflow resume command"),
        }
    }

    #[test]
    fn rejects_removed_task_command_tree() {
        let error = Cli::try_parse_from(["animus", "task", "list"]).expect_err("legacy task tree should be removed");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_removed_requirements_command_tree() {
        let error = Cli::try_parse_from(["animus", "requirements", "list"])
            .expect_err("legacy requirements tree should be removed");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_removed_cloud_command_tree() {
        let error =
            Cli::try_parse_from(["animus", "cloud", "status"]).expect_err("legacy cloud tree should be removed");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_removed_errors_command_tree() {
        let error =
            Cli::try_parse_from(["animus", "errors", "list"]).expect_err("legacy errors tree should be removed");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_removed_setup_command() {
        let error = Cli::try_parse_from(["animus", "setup"]).expect_err("legacy setup command should be removed");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_removed_now_command() {
        let error = Cli::try_parse_from(["animus", "now"]).expect_err("legacy now command should be removed");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn cli_reference_top_level_tree_matches_live_clap_commands() {
        let mut command = Cli::command();
        command.build();

        let mut actual: Vec<String> =
            command.get_subcommands().map(|subcommand| subcommand.get_name().to_string()).collect();
        let mut documented = documented_top_level_commands();

        actual.sort();
        documented.sort();

        assert_eq!(
            documented, actual,
            "docs/reference/cli/index.md top-level command tree drifted from Cli::command()"
        );
    }
}
