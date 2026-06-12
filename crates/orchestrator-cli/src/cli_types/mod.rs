mod agent_types;
mod approval_types;
mod auth_types;
mod chat_types;
mod cost_types;
mod daemon_types;
mod doctor_types;
mod events_types;
mod flavor_types;
mod git_types;
mod history_types;
mod init_types;
mod logs_types;
mod mcp_types;
mod metrics_types;
mod output_types;
mod pack_types;
mod plugin_types;
mod queue_types;

mod project_types;
mod root_types;
mod secret_types;
mod self_types;
mod shared_types;
mod skill_types;
mod state_types;
mod subject_types;
mod trigger_types;
mod update_types;
mod web_types;
mod workflow_types;

pub(crate) use agent_types::*;
pub(crate) use approval_types::*;
pub(crate) use auth_types::*;
pub(crate) use chat_types::*;
pub(crate) use cost_types::*;
pub(crate) use daemon_types::*;
pub(crate) use doctor_types::*;
pub(crate) use events_types::*;
pub(crate) use flavor_types::*;
pub(crate) use git_types::*;
pub(crate) use history_types::*;
pub(crate) use init_types::*;
pub(crate) use logs_types::*;
pub(crate) use mcp_types::*;
pub(crate) use metrics_types::*;
pub(crate) use output_types::*;
pub(crate) use pack_types::*;
pub(crate) use plugin_types::*;
pub(crate) use queue_types::*;

pub(crate) use project_types::*;
pub(crate) use root_types::*;
pub(crate) use secret_types::*;
pub(crate) use self_types::*;
pub(crate) use shared_types::*;
pub(crate) use skill_types::*;
pub(crate) use state_types::*;
pub(crate) use subject_types::*;
pub(crate) use trigger_types::*;
pub(crate) use update_types::*;
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

    fn documented_agents_top_level_commands() -> Vec<String> {
        let binding = read_doc("AGENTS.md");
        let mut lines = binding.lines();
        for line in lines.by_ref() {
            if line.trim() == "Visible top-level commands:" {
                break;
            }
        }

        lines
            .skip_while(|line| line.trim().is_empty())
            .take_while(|line| !line.trim().is_empty())
            .filter_map(|line| line.trim().strip_prefix("- `").and_then(|entry| entry.strip_suffix('`')))
            .map(str::to_string)
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
    fn daemon_start_accepts_deprecated_autonomous_flag_as_hidden_noop() {
        // `daemon start` always detaches as of v0.6; `--autonomous` stays
        // accepted for fleet automation back-compat but changes nothing.
        let bare = Cli::try_parse_from(["animus", "daemon", "start"]).expect("daemon start should parse");
        match bare.command {
            Command::Daemon { command: DaemonCommand::Start(args) } => {
                assert!(!args.autonomous, "flag defaults to false (and is a no-op either way)");
            }
            other => panic!("expected daemon start, got {other:?}"),
        }

        let with_flag = Cli::try_parse_from(["animus", "daemon", "start", "--autonomous"])
            .expect("deprecated --autonomous must remain accepted");
        match with_flag.command {
            Command::Daemon { command: DaemonCommand::Start(args) } => assert!(args.autonomous),
            other => panic!("expected daemon start, got {other:?}"),
        }
    }

    #[test]
    fn daemon_start_help_hides_deprecated_autonomous_flag() {
        let error = Cli::try_parse_from(["animus", "daemon", "start", "--help"])
            .expect_err("help output should short-circuit parsing");
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(!help.contains("--autonomous"), "deprecated no-op flag should be hidden from help");
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
    fn daemon_events_defaults_to_one_shot_without_follow() {
        let cli =
            Cli::try_parse_from(["animus", "daemon", "events", "--limit", "10"]).expect("daemon events should parse");
        match cli.command {
            Command::Daemon { command: DaemonCommand::Events(args) } => {
                assert_eq!(args.limit, Some(10));
                assert!(!args.follow, "daemon events must print and exit unless --follow is passed");
            }
            other => panic!("expected daemon events, got {other:?}"),
        }
    }

    #[test]
    fn daemon_events_follow_accepts_bare_flag_and_explicit_values() {
        let bare = Cli::try_parse_from(["animus", "daemon", "events", "--follow"]).expect("bare --follow should parse");
        match bare.command {
            Command::Daemon { command: DaemonCommand::Events(args) } => assert!(args.follow),
            other => panic!("expected daemon events, got {other:?}"),
        }

        let explicit_true = Cli::try_parse_from(["animus", "daemon", "events", "--follow", "true"])
            .expect("--follow true should parse");
        match explicit_true.command {
            Command::Daemon { command: DaemonCommand::Events(args) } => assert!(args.follow),
            other => panic!("expected daemon events, got {other:?}"),
        }

        let explicit_false = Cli::try_parse_from(["animus", "daemon", "events", "--follow", "false"])
            .expect("--follow false should parse");
        match explicit_false.command {
            Command::Daemon { command: DaemonCommand::Events(args) } => assert!(!args.follow),
            other => panic!("expected daemon events, got {other:?}"),
        }
    }

    #[test]
    fn queue_hold_accepts_multiple_positional_subject_ids() {
        let cli = Cli::try_parse_from(["animus", "queue", "hold", "TASK-1", "TASK-2"])
            .expect("multiple positional subject ids should parse");
        match cli.command {
            Command::Queue { command: QueueCommand::Hold(args) } => {
                assert_eq!(args.subject_ids, vec!["TASK-1", "TASK-2"]);
                assert!(args.subject_id.is_none());
                assert!(!args.all);
                assert!(!args.yes);
            }
            other => panic!("expected queue hold, got {other:?}"),
        }
    }

    #[test]
    fn queue_drop_keeps_subject_id_flag_back_compat() {
        let cli = Cli::try_parse_from(["animus", "queue", "drop", "--subject-id", "TASK-1"])
            .expect("--subject-id flag form should still parse");
        match cli.command {
            Command::Queue { command: QueueCommand::Drop(args) } => {
                assert_eq!(args.subject_id.as_deref(), Some("TASK-1"));
                assert!(args.subject_ids.is_empty());
            }
            other => panic!("expected queue drop, got {other:?}"),
        }
    }

    #[test]
    fn queue_release_all_parses_with_yes() {
        let cli =
            Cli::try_parse_from(["animus", "queue", "release", "--all", "--yes"]).expect("--all --yes should parse");
        match cli.command {
            Command::Queue { command: QueueCommand::Release(args) } => {
                assert!(args.all);
                assert!(args.yes);
                assert!(args.subject_ids.is_empty());
            }
            other => panic!("expected queue release, got {other:?}"),
        }
    }

    #[test]
    fn queue_drop_all_conflicts_with_explicit_subject_ids() {
        let error = Cli::try_parse_from(["animus", "queue", "drop", "TASK-1", "--all"])
            .expect_err("--all with explicit ids should fail");
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);

        let flag_error = Cli::try_parse_from(["animus", "queue", "drop", "--subject-id", "TASK-1", "--all"])
            .expect_err("--all with --subject-id should fail");
        assert_eq!(flag_error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn queue_hold_requires_subject_ids_or_all() {
        let error = Cli::try_parse_from(["animus", "queue", "hold"]).expect_err("missing subject selector should fail");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_top_level_status_command() {
        let cli = Cli::try_parse_from(["animus", "status"]).expect("status command should parse");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parses_mcp_auth_with_scopes_yes_dry_run() {
        let cli = Cli::try_parse_from([
            "animus",
            "mcp",
            "auth",
            "robinhood-trading",
            "--scopes",
            "read:positions,trade",
            "--yes",
            "--dry-run",
        ])
        .expect("mcp auth flags should parse");
        match cli.command {
            Command::Mcp { command: McpCommand::Auth(args) } => {
                assert_eq!(args.server, "robinhood-trading");
                assert_eq!(
                    args.scopes.as_deref(),
                    Some(["read:positions".to_string(), "trade".to_string()].as_slice())
                );
                assert!(args.yes);
                assert!(args.dry_run);
            }
            other => panic!("expected mcp auth, got {other:?}"),
        }
    }

    #[test]
    fn mcp_auth_defaults_no_yes_no_dry_run_no_scopes() {
        let cli = Cli::try_parse_from(["animus", "mcp", "auth", "github"]).expect("bare mcp auth should parse");
        match cli.command {
            Command::Mcp { command: McpCommand::Auth(args) } => {
                assert!(args.scopes.is_none(), "no --scopes means least-privilege default");
                assert!(!args.yes);
                assert!(!args.dry_run);
            }
            other => panic!("expected mcp auth, got {other:?}"),
        }
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
    fn parses_top_level_update_command_with_check_and_channel() {
        let cli =
            Cli::try_parse_from(["animus", "update", "--check", "--channel", "nightly"]).expect("update should parse");
        match cli.command {
            Command::Update(args) => {
                assert!(args.check);
                assert!(!args.yes);
                assert!(matches!(args.channel, UpdateChannelArg::Nightly));
            }
            _ => panic!("expected update command"),
        }
    }

    #[test]
    fn top_level_update_defaults_to_stable_channel() {
        let cli = Cli::try_parse_from(["animus", "update"]).expect("update should parse with defaults");
        match cli.command {
            Command::Update(args) => {
                assert!(!args.check);
                assert!(!args.yes);
                assert!(matches!(args.channel, UpdateChannelArg::Stable));
            }
            _ => panic!("expected update command"),
        }
    }

    #[test]
    fn top_level_update_rejects_unknown_channel() {
        let error =
            Cli::try_parse_from(["animus", "update", "--channel", "canary"]).expect_err("unknown channel should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
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
    fn parses_pack_uninstall_command() {
        let cli = Cli::try_parse_from([
            "animus",
            "pack",
            "uninstall",
            "animus.review",
            "--version",
            "0.2.0",
            "--force",
            "--dry-run",
        ])
        .expect("pack uninstall should parse");

        match cli.command {
            Command::Pack { command: PackCommand::Uninstall(args) } => {
                assert_eq!(args.pack_id, "animus.review");
                assert_eq!(args.version.as_deref(), Some("0.2.0"));
                assert!(args.force);
                assert!(args.dry_run);
            }
            _ => panic!("expected pack uninstall command"),
        }
    }

    #[test]
    fn parses_skill_uninstall_command() {
        let cli = Cli::try_parse_from(["animus", "skill", "uninstall", "alpha", "--source", "local", "--dry-run"])
            .expect("skill uninstall should parse");

        match cli.command {
            Command::Skill { command: SkillCommand::Uninstall(args) } => {
                assert_eq!(args.name, "alpha");
                assert_eq!(args.source.as_deref(), Some("local"));
                assert!(args.dry_run);
            }
            _ => panic!("expected skill uninstall command"),
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

    fn subcommand_help(path: &[&str]) -> String {
        let mut argv = vec!["animus"];
        argv.extend_from_slice(path);
        argv.push("--help");
        let error = Cli::try_parse_from(argv).expect_err("help output should short-circuit parsing");
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        error.to_string()
    }

    fn help_lists_subcommand(help: &str, name: &str) -> bool {
        help.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed == name || trimmed.starts_with(&format!("{name} "))
        })
    }

    #[test]
    fn pack_info_is_primary_and_inspect_stays_a_hidden_alias() {
        for verb in ["info", "inspect"] {
            let cli = Cli::try_parse_from(["animus", "pack", verb, "--pack-id", "animus.task"])
                .unwrap_or_else(|error| panic!("pack {verb} should parse: {error}"));
            match cli.command {
                Command::Pack { command: PackCommand::Info(args) } => {
                    assert_eq!(args.pack_id.as_deref(), Some("animus.task"));
                }
                other => panic!("expected pack info, got {other:?}"),
            }
        }
        let help = subcommand_help(&["pack"]);
        assert!(help_lists_subcommand(&help, "info"), "pack help must list `info`:\n{help}");
        assert!(!help_lists_subcommand(&help, "inspect"), "pack help must hide the `inspect` alias:\n{help}");
    }

    #[test]
    fn skill_info_is_primary_and_show_stays_a_hidden_alias() {
        for verb in ["info", "show"] {
            let cli = Cli::try_parse_from(["animus", "skill", verb, "--name", "alpha"])
                .unwrap_or_else(|error| panic!("skill {verb} should parse: {error}"));
            match cli.command {
                Command::Skill { command: SkillCommand::Info(args) } => assert_eq!(args.name, "alpha"),
                other => panic!("expected skill info, got {other:?}"),
            }
        }
        let help = subcommand_help(&["skill"]);
        assert!(help_lists_subcommand(&help, "info"), "skill help must list `info`:\n{help}");
        assert!(!help_lists_subcommand(&help, "show"), "skill help must hide the `show` alias:\n{help}");
    }

    #[test]
    fn flavor_info_is_primary_and_describe_stays_a_hidden_alias() {
        for verb in ["info", "describe"] {
            let cli = Cli::try_parse_from(["animus", "flavor", verb, "--name", "default"])
                .unwrap_or_else(|error| panic!("flavor {verb} should parse: {error}"));
            match cli.command {
                Command::Flavor { command: FlavorCommand::Info(args) } => assert_eq!(args.name, "default"),
                other => panic!("expected flavor info, got {other:?}"),
            }
        }
        let help = subcommand_help(&["flavor"]);
        assert!(help_lists_subcommand(&help, "info"), "flavor help must list `info`:\n{help}");
        assert!(!help_lists_subcommand(&help, "describe"), "flavor help must hide the `describe` alias:\n{help}");
    }

    #[test]
    fn output_read_is_primary_and_run_stays_a_hidden_alias() {
        for verb in ["read", "run"] {
            let cli = Cli::try_parse_from(["animus", "output", verb, "--run-id", "RUN-1"])
                .unwrap_or_else(|error| panic!("output {verb} should parse: {error}"));
            match cli.command {
                Command::Output { command: OutputCommand::Read(args) } => assert_eq!(args.run_id, "RUN-1"),
                other => panic!("expected output read, got {other:?}"),
            }
        }
        let help = subcommand_help(&["output"]);
        assert!(help_lists_subcommand(&help, "read"), "output help must list `read`:\n{help}");
        assert!(!help_lists_subcommand(&help, "run"), "output help must hide the `run` alias:\n{help}");
    }

    #[test]
    fn project_set_active_is_primary_and_load_stays_a_hidden_alias() {
        for verb in ["set-active", "load"] {
            let cli = Cli::try_parse_from(["animus", "project", verb, "--id", "PRJ-1"])
                .unwrap_or_else(|error| panic!("project {verb} should parse: {error}"));
            match cli.command {
                Command::Project { command: ProjectCommand::SetActive(args) } => assert_eq!(args.id, "PRJ-1"),
                other => panic!("expected project set-active, got {other:?}"),
            }
        }
        let help = subcommand_help(&["project"]);
        assert!(help_lists_subcommand(&help, "set-active"), "project help must list `set-active`:\n{help}");
        assert!(!help_lists_subcommand(&help, "load"), "project help must hide the `load` alias:\n{help}");
    }

    #[test]
    fn workflow_id_commands_accept_workflow_id_flag_alias() {
        let get = Cli::try_parse_from(["animus", "workflow", "get", "--workflow-id", "wf-1"])
            .expect("workflow get --workflow-id should parse");
        match get.command {
            Command::Workflow { command: WorkflowCommand::Get(args) } => assert_eq!(args.id, "wf-1"),
            other => panic!("expected workflow get, got {other:?}"),
        }

        let pause = Cli::try_parse_from(["animus", "workflow", "pause", "--workflow-id", "wf-1", "--confirm", "wf-1"])
            .expect("workflow pause --workflow-id should parse");
        match pause.command {
            Command::Workflow { command: WorkflowCommand::Pause(args) } => {
                assert_eq!(args.id, "wf-1");
                assert_eq!(args.confirm.as_deref(), Some("wf-1"));
            }
            other => panic!("expected workflow pause, got {other:?}"),
        }

        let cancel = Cli::try_parse_from(["animus", "workflow", "cancel", "--workflow-id", "wf-2"])
            .expect("workflow cancel --workflow-id should parse");
        match cancel.command {
            Command::Workflow { command: WorkflowCommand::Cancel(args) } => assert_eq!(args.id, "wf-2"),
            other => panic!("expected workflow cancel, got {other:?}"),
        }

        let resume = Cli::try_parse_from(["animus", "workflow", "resume", "--workflow-id", "wf-3"])
            .expect("workflow resume --workflow-id should parse");
        match resume.command {
            Command::Workflow { command: WorkflowCommand::Resume(args) } => assert_eq!(args.id, "wf-3"),
            other => panic!("expected workflow resume, got {other:?}"),
        }

        let approve =
            Cli::try_parse_from(["animus", "workflow", "phase", "approve", "--workflow-id", "wf-4", "--phase", "impl"])
                .expect("workflow phase approve --workflow-id should parse");
        match approve.command {
            Command::Workflow { command: WorkflowCommand::Phase { command: WorkflowPhaseCommand::Approve(args) } } => {
                assert_eq!(args.id, "wf-4");
                assert_eq!(args.phase, "impl");
            }
            other => panic!("expected workflow phase approve, got {other:?}"),
        }

        let checkpoints = Cli::try_parse_from(["animus", "workflow", "checkpoints", "list", "--workflow-id", "wf-5"])
            .expect("workflow checkpoints list --workflow-id should parse");
        match checkpoints.command {
            Command::Workflow {
                command: WorkflowCommand::Checkpoints { command: WorkflowCheckpointCommand::List(args) },
            } => assert_eq!(args.id, "wf-5"),
            other => panic!("expected workflow checkpoints list, got {other:?}"),
        }

        // `--id` (and `-i` on resume) keep working as before.
        let id_form = Cli::try_parse_from(["animus", "workflow", "get", "--id", "wf-6"])
            .expect("workflow get --id should keep parsing");
        match id_form.command {
            Command::Workflow { command: WorkflowCommand::Get(args) } => assert_eq!(args.id, "wf-6"),
            other => panic!("expected workflow get, got {other:?}"),
        }
    }

    #[test]
    fn workflow_prune_older_than_accepts_bare_days_and_unit_suffixes() {
        for (spec, expected_secs) in [("30", 30 * 86_400), ("30d", 30 * 86_400), ("12h", 12 * 3_600)] {
            let cli = Cli::try_parse_from(["animus", "workflow", "prune", "--older-than", spec])
                .unwrap_or_else(|error| panic!("workflow prune --older-than {spec} should parse: {error}"));
            match cli.command {
                Command::Workflow { command: WorkflowCommand::Prune(args) } => {
                    assert_eq!(args.older_than, Some(expected_secs), "spec {spec}");
                    assert!(!args.yes, "prune must stay dry-run by default");
                }
                other => panic!("expected workflow prune, got {other:?}"),
            }
        }

        let error = Cli::try_parse_from(["animus", "workflow", "prune", "--older-than", "30w"])
            .expect_err("unknown unit should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(error.to_string().contains("unknown unit"), "got: {error}");
    }

    #[test]
    fn parse_duration_secs_default_days_handles_units_and_rejects_garbage() {
        assert_eq!(parse_duration_secs_default_days("30"), Ok(30 * 86_400));
        assert_eq!(parse_duration_secs_default_days("30d"), Ok(30 * 86_400));
        assert_eq!(parse_duration_secs_default_days("12h"), Ok(12 * 3_600));
        assert_eq!(parse_duration_secs_default_days("45m"), Ok(45 * 60));
        assert_eq!(parse_duration_secs_default_days("90s"), Ok(90));
        assert!(parse_duration_secs_default_days("").is_err());
        assert!(parse_duration_secs_default_days("abc").is_err());
        assert!(parse_duration_secs_default_days("5y").is_err());
    }

    #[test]
    fn approval_group_parses_request_respond_outcome() {
        let request = Cli::try_parse_from([
            "animus",
            "approval",
            "request",
            "--operation-type",
            "force_push",
            "--repo-name",
            "repo-a",
        ])
        .expect("approval request should parse");
        match request.command {
            Command::Approval { command: ApprovalCommand::Request(args) } => {
                assert_eq!(args.operation_type, "force_push");
                assert_eq!(args.repo_name, "repo-a");
                assert!(args.context_json.is_none());
            }
            other => panic!("expected approval request, got {other:?}"),
        }

        let respond = Cli::try_parse_from(["animus", "approval", "respond", "--request-id", "confirm-1", "--approved"])
            .expect("approval respond should parse");
        match respond.command {
            Command::Approval { command: ApprovalCommand::Respond(args) } => {
                assert_eq!(args.request_id, "confirm-1");
                assert!(args.approved);
            }
            other => panic!("expected approval respond, got {other:?}"),
        }

        let outcome = Cli::try_parse_from([
            "animus",
            "approval",
            "outcome",
            "--request-id",
            "confirm-1",
            "--success",
            "--message",
            "pushed",
        ])
        .expect("approval outcome should parse");
        match outcome.command {
            Command::Approval { command: ApprovalCommand::Outcome(args) } => {
                assert_eq!(args.request_id, "confirm-1");
                assert!(args.success);
                assert_eq!(args.message, "pushed");
            }
            other => panic!("expected approval outcome, got {other:?}"),
        }
    }

    #[test]
    fn git_confirm_alias_round_trips_to_the_same_approval_commands() {
        let request = Cli::try_parse_from([
            "animus",
            "git",
            "confirm",
            "request",
            "--operation-type",
            "force_push",
            "--repo-name",
            "repo-a",
        ])
        .expect("git confirm request alias should still parse");
        match request.command {
            Command::Git { command: GitCommand::Confirm { command: ApprovalCommand::Request(args) } } => {
                assert_eq!(args.operation_type, "force_push");
                assert_eq!(args.repo_name, "repo-a");
            }
            other => panic!("expected git confirm request alias, got {other:?}"),
        }

        let respond = Cli::try_parse_from(["animus", "git", "confirm", "respond", "--request-id", "confirm-1"])
            .expect("git confirm respond alias should still parse");
        match respond.command {
            Command::Git { command: GitCommand::Confirm { command: ApprovalCommand::Respond(args) } } => {
                assert_eq!(args.request_id, "confirm-1");
                assert!(!args.approved);
            }
            other => panic!("expected git confirm respond alias, got {other:?}"),
        }

        let outcome = Cli::try_parse_from([
            "animus",
            "git",
            "confirm",
            "outcome",
            "--request-id",
            "confirm-1",
            "--message",
            "aborted",
        ])
        .expect("git confirm outcome alias should still parse");
        match outcome.command {
            Command::Git { command: GitCommand::Confirm { command: ApprovalCommand::Outcome(args) } } => {
                assert_eq!(args.request_id, "confirm-1");
                assert!(!args.success);
                assert_eq!(args.message, "aborted");
            }
            other => panic!("expected git confirm outcome alias, got {other:?}"),
        }
    }

    #[test]
    fn approval_group_renders_in_top_level_help_and_git_confirm_stays_hidden() {
        let top_level = Cli::try_parse_from(["animus", "--help"]).expect_err("help short-circuits parsing");
        assert_eq!(top_level.kind(), ErrorKind::DisplayHelp);
        let top_level_help = top_level.to_string();
        assert!(top_level_help.contains("approval"), "top-level help should list the approval group");

        let git_help = Cli::try_parse_from(["animus", "git", "--help"]).expect_err("help short-circuits parsing");
        assert_eq!(git_help.kind(), ErrorKind::DisplayHelp);
        let git_help_text = git_help.to_string();
        assert!(
            !git_help_text.lines().any(|line| line.split_whitespace().next() == Some("confirm")),
            "git help must not advertise the hidden confirm alias"
        );

        let confirm_help =
            Cli::try_parse_from(["animus", "git", "confirm", "--help"]).expect_err("help short-circuits parsing");
        assert_eq!(confirm_help.kind(), ErrorKind::DisplayHelp, "hidden alias must still answer --help");
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
    fn agents_guide_top_level_commands_match_live_clap_commands() {
        let mut command = Cli::command();
        command.build();

        let mut actual: Vec<String> =
            command.get_subcommands().map(|subcommand| subcommand.get_name().to_string()).collect();
        actual.retain(|name| name != "help");
        let mut documented = documented_agents_top_level_commands();

        actual.sort();
        documented.sort();

        assert_eq!(documented, actual, "AGENTS.md top-level command list drifted from Cli::command()");
    }

    #[test]
    fn chat_reference_mentions_send_title_flag() {
        let chat_reference = read_doc("docs/reference/chat.md");
        assert!(
            chat_reference.contains("[--stream] [--title <title>]"),
            "docs/reference/chat.md should document `animus chat send --title` in the CLI surface block"
        );
        assert!(
            chat_reference.contains("`animus chat send --title` names a freshly-created conversation or renames the"),
            "docs/reference/chat.md should explain how `animus chat send --title` behaves"
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
