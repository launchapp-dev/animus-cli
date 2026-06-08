mod agent_types;
mod auth_types;
mod cost_types;
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
mod secret_types;
mod self_types;
mod shared_types;
mod skill_types;
mod subject_types;
mod trigger_types;
mod web_types;
mod workflow_types;

pub(crate) use agent_types::*;
pub(crate) use auth_types::*;
pub(crate) use cost_types::*;
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
pub(crate) use secret_types::*;
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

    fn live_workspace_crates() -> Vec<String> {
        let mut in_members = false;
        let mut crates = Vec::new();
        for line in read_doc("Cargo.toml").lines() {
            let trimmed = line.trim();
            if trimmed == "members = [" {
                in_members = true;
                continue;
            }
            if in_members && trimmed == "]" {
                break;
            }
            if !in_members {
                continue;
            }
            let Some(member) = trimmed.strip_prefix('"').and_then(|value| value.strip_suffix("\",")) else {
                continue;
            };
            let Some(crate_name) = member.strip_prefix("crates/") else {
                continue;
            };
            crates.push(crate_name.to_string());
        }
        crates
    }

    fn documented_workspace_crates() -> Vec<String> {
        read_doc("docs/architecture/crate-map.md")
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if !trimmed.starts_with("| `") {
                    return None;
                }
                let crate_name = trimmed.split('`').nth(1)?;
                if crate_name.starts_with("crates/") {
                    return None;
                }
                Some(crate_name.to_string())
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
    fn parses_auth_whoami_command() {
        let cli = Cli::try_parse_from(["animus", "auth", "whoami"]).expect("auth whoami should parse");
        match cli.command {
            Command::Auth { command: AuthCommand::Whoami } => {}
            other => panic!("expected auth whoami, got {other:?}"),
        }
        assert!(cli.as_principal.is_none());
    }

    #[test]
    fn parses_global_as_flag() {
        let cli =
            Cli::try_parse_from(["animus", "--as", "alice", "auth", "whoami"]).expect("--as should parse globally");
        assert_eq!(cli.as_principal.as_deref(), Some("alice"));
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
    fn parses_plugin_scaffold_trigger_command() {
        let cli = Cli::try_parse_from([
            "animus",
            "plugin",
            "scaffold",
            "trigger",
            "fswatch",
            "--owner",
            "acme-co",
            "--license",
            "Apache-2.0",
            "--protocol-tag",
            "v0.5.5",
        ])
        .expect("plugin scaffold trigger should parse");

        match cli.command {
            Command::Plugin { command: PluginCommand::Scaffold(PluginScaffoldCommand::Trigger(args)) } => {
                assert_eq!(args.name, "fswatch");
                assert_eq!(args.owner.as_deref(), Some("acme-co"));
                assert_eq!(args.license, "Apache-2.0");
                assert_eq!(args.protocol_tag, "v0.5.5");
                assert!(!args.force);
                assert!(!args.json);
                assert!(args.out_dir.is_none());
            }
            _ => panic!("expected plugin scaffold trigger command"),
        }
    }

    #[test]
    fn plugin_scaffold_trigger_requires_name() {
        let error =
            Cli::try_parse_from(["animus", "plugin", "scaffold", "trigger"]).expect_err("missing NAME should fail");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_plugin_rename_command() {
        let cli =
            Cli::try_parse_from(["animus", "plugin", "rename", "animus-subject-default", "--to", "archive", "--force"])
                .expect("plugin rename should parse");
        match cli.command {
            Command::Plugin { command: PluginCommand::Rename(args) } => {
                assert_eq!(args.name, "animus-subject-default");
                assert_eq!(args.to, "archive");
                assert!(args.force);
                assert!(!args.json);
            }
            _ => panic!("expected plugin rename command"),
        }
    }

    #[test]
    fn plugin_rename_requires_to_flag() {
        let error =
            Cli::try_parse_from(["animus", "plugin", "rename", "some-plugin"]).expect_err("missing --to should fail");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
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

    #[test]
    fn crate_map_matches_live_workspace_members() {
        let mut live = live_workspace_crates();
        let mut documented = documented_workspace_crates();

        live.sort();
        documented.sort();

        assert_eq!(documented, live, "docs/architecture/crate-map.md drifted from Cargo.toml workspace membership");

        let crate_map = read_doc("docs/architecture/crate-map.md");
        assert!(
            crate_map.contains(&format!("Cargo workspace of {} crates", live.len())),
            "docs/architecture/crate-map.md should publish the live workspace crate count ({})",
            live.len()
        );
    }
}
