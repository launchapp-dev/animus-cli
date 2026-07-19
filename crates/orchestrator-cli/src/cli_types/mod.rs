mod agent_types;
mod approval_types;
mod auth_types;
mod chat_types;
mod cost_types;
mod daemon_types;
mod doctor_types;
mod environment_types;
mod events_types;
mod flavor_types;
mod git_types;
mod history_types;
mod init_types;
mod logs_types;
mod manifest_types;
mod mcp_types;
mod output_types;
mod pack_types;
mod plugin_types;
mod queue_types;

mod root_types;
mod secret_types;
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
pub(crate) use environment_types::*;
pub(crate) use events_types::*;
pub(crate) use flavor_types::*;
pub(crate) use git_types::*;
pub(crate) use history_types::*;
pub(crate) use init_types::*;
pub(crate) use logs_types::*;
pub(crate) use manifest_types::*;
pub(crate) use mcp_types::*;
pub(crate) use output_types::*;
pub(crate) use pack_types::*;
pub(crate) use plugin_types::*;
pub(crate) use queue_types::*;

pub(crate) use root_types::*;
pub(crate) use secret_types::*;
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
    fn daemon_start_rejects_removed_autonomous_flag() {
        // `daemon start` always detaches; the deprecated `--autonomous` no-op
        // was removed. Passing it now errors instead of being silently accepted.
        let error = Cli::try_parse_from(["animus", "daemon", "start", "--autonomous"])
            .expect_err("removed --autonomous flag must be rejected");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
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
    fn queue_enqueue_accepts_subject_id() {
        let cli = Cli::try_parse_from(["animus", "queue", "enqueue", "--subject-id", "blog:BLOG-001"])
            .expect("queue enqueue --subject-id should parse");
        match cli.command {
            Command::Queue { command: QueueCommand::Enqueue(args) } => {
                assert_eq!(args.subject_id.as_deref(), Some("blog:BLOG-001"));
                assert!(args.title.is_none());
            }
            other => panic!("expected queue enqueue, got {other:?}"),
        }
    }

    #[test]
    fn queue_enqueue_subject_id_conflicts_with_title() {
        let error =
            Cli::try_parse_from(["animus", "queue", "enqueue", "--subject-id", "blog:BLOG-001", "--title", "x"])
                .expect_err("--subject-id must be mutually exclusive with --title");
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{error}");
    }

    #[test]
    fn queue_enqueue_rejects_removed_task_id_flag() {
        let error = Cli::try_parse_from(["animus", "queue", "enqueue", "--task-id", "TASK-1"])
            .expect_err("--task-id was removed");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{error}");
    }

    #[test]
    fn workflow_run_accepts_subject_id() {
        let cli = Cli::try_parse_from(["animus", "workflow", "run", "--subject-id", "blog:BLOG-001"])
            .expect("workflow run --subject-id should parse");
        match cli.command {
            Command::Workflow { command: WorkflowCommand::Run(args) } => {
                assert_eq!(args.subject_id.as_deref(), Some("blog:BLOG-001"));
                assert!(args.title.is_none());
            }
            other => panic!("expected workflow run, got {other:?}"),
        }
    }

    #[test]
    fn workflow_run_subject_id_conflicts_with_title() {
        let error = Cli::try_parse_from(["animus", "workflow", "run", "--subject-id", "blog:BLOG-001", "--title", "x"])
            .expect_err("--subject-id must be mutually exclusive with --title");
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{error}");
    }

    #[test]
    fn workflow_run_rejects_removed_task_id_flag() {
        let error = Cli::try_parse_from(["animus", "workflow", "run", "--task-id", "TASK-1"])
            .expect_err("--task-id was removed");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{error}");
    }

    #[test]
    fn daemon_metrics_bare_defaults_to_display() {
        let cli = Cli::try_parse_from(["animus", "daemon", "metrics"]).expect("bare daemon metrics should parse");
        match cli.command {
            Command::Daemon { command: DaemonCommand::Metrics(args) } => {
                assert!(args.command.is_none(), "bare invocation must keep the display path");
                assert!(!args.display.watch);
                assert_eq!(args.display.interval_secs, 5);
                assert!(!args.display.pretty);
            }
            other => panic!("expected daemon metrics, got {other:?}"),
        }
    }

    #[test]
    fn daemon_metrics_bare_accepts_display_flags() {
        let cli = Cli::try_parse_from(["animus", "daemon", "metrics", "--watch", "--interval-secs", "2", "--pretty"])
            .expect("daemon metrics display flags should parse");
        match cli.command {
            Command::Daemon { command: DaemonCommand::Metrics(args) } => {
                assert!(args.command.is_none());
                assert!(args.display.watch);
                assert_eq!(args.display.interval_secs, 2);
                assert!(args.display.pretty);
            }
            other => panic!("expected daemon metrics, got {other:?}"),
        }
    }

    #[test]
    fn daemon_metrics_parses_telemetry_subcommands() {
        for (verb, expected) in [
            ("status", DaemonMetricsSubcommand::Status),
            ("enable", DaemonMetricsSubcommand::Enable),
            ("disable", DaemonMetricsSubcommand::Disable),
            ("flush", DaemonMetricsSubcommand::Flush),
            ("cleanup", DaemonMetricsSubcommand::Cleanup),
        ] {
            let cli = Cli::try_parse_from(["animus", "daemon", "metrics", verb])
                .unwrap_or_else(|error| panic!("daemon metrics {verb} should parse: {error}"));
            match cli.command {
                Command::Daemon { command: DaemonCommand::Metrics(args) } => {
                    let parsed = args.command.unwrap_or_else(|| panic!("daemon metrics {verb} must set a subcommand"));
                    assert!(
                        std::mem::discriminant(&parsed) == std::mem::discriminant(&expected),
                        "daemon metrics {verb} parsed to the wrong subcommand: {parsed:?}"
                    );
                }
                other => panic!("expected daemon metrics {verb}, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_removed_top_level_metrics_command_tree() {
        for argv in
            [vec!["animus", "metrics"], vec!["animus", "metrics", "status"], vec!["animus", "metrics", "enable"]]
        {
            let error = Cli::try_parse_from(argv.clone())
                .expect_err("the top-level metrics group was folded into daemon metrics in v0.6");
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "argv {argv:?}");
        }
    }

    #[test]
    fn daemon_observe_bare_defaults() {
        let cli = Cli::try_parse_from(["animus", "daemon", "observe"]).expect("bare observe should parse");
        match cli.command {
            Command::Daemon { command: DaemonCommand::Observe(args) } => {
                assert!(!args.follow);
                assert!(args.since.is_none());
                assert!(args.source.is_none());
                assert!(args.workflow.is_none());
                assert_eq!(args.limit, 20);
            }
            other => panic!("expected daemon observe, got {other:?}"),
        }
    }

    #[test]
    fn daemon_observe_routes_per_flag() {
        let follow = Cli::try_parse_from(["animus", "daemon", "observe", "--follow"]).expect("follow parses");
        match follow.command {
            Command::Daemon { command: DaemonCommand::Observe(args) } => assert!(args.follow),
            other => panic!("expected observe, got {other:?}"),
        }

        let since = Cli::try_parse_from(["animus", "daemon", "observe", "--since", "15m"]).expect("since parses");
        match since.command {
            Command::Daemon { command: DaemonCommand::Observe(args) } => assert_eq!(args.since.as_deref(), Some("15m")),
            other => panic!("expected observe, got {other:?}"),
        }

        for (flag, expected) in [
            ("logs", ObserveSource::Logs),
            ("events", ObserveSource::Events),
            ("stream", ObserveSource::Stream),
            ("workflow", ObserveSource::Workflow),
        ] {
            let cli = Cli::try_parse_from(["animus", "daemon", "observe", "--source", flag])
                .unwrap_or_else(|e| panic!("--source {flag} should parse: {e}"));
            match cli.command {
                Command::Daemon { command: DaemonCommand::Observe(args) } => {
                    assert_eq!(args.source, Some(expected), "--source {flag}");
                }
                other => panic!("expected observe, got {other:?}"),
            }
        }

        let wf = Cli::try_parse_from(["animus", "daemon", "observe", "--workflow", "WF-9", "--limit", "5"])
            .expect("workflow + limit parses");
        match wf.command {
            Command::Daemon { command: DaemonCommand::Observe(args) } => {
                assert_eq!(args.workflow.as_deref(), Some("WF-9"));
                assert_eq!(args.limit, 5);
            }
            other => panic!("expected observe, got {other:?}"),
        }
    }

    #[test]
    fn daemon_observe_rejects_unknown_source() {
        let error = Cli::try_parse_from(["animus", "daemon", "observe", "--source", "bogus"])
            .expect_err("unknown --source should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
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
        assert!(matches!(cli.command, Command::Status { failures: 3 }));
    }

    #[test]
    fn parses_status_failures_flag() {
        let cli =
            Cli::try_parse_from(["animus", "status", "--failures", "10"]).expect("status --failures should parse");
        assert!(matches!(cli.command, Command::Status { failures: 10 }));
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
    fn rejects_removed_self_command_tree() {
        for argv in [vec!["animus", "self"], vec!["animus", "self", "update"]] {
            let error =
                Cli::try_parse_from(argv.clone()).expect_err("the `self` group was folded into top-level `update`");
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "argv {argv:?}");
        }
    }

    #[test]
    fn top_level_update_accepts_force_and_prerelease_folded_from_self_update() {
        let cli = Cli::try_parse_from(["animus", "update", "--force", "--prerelease", "--yes"])
            .expect("update --force --prerelease should parse");
        match cli.command {
            Command::Update(args) => {
                assert!(args.force);
                assert!(args.prerelease);
                assert!(args.yes);
                assert!(!args.check);
            }
            _ => panic!("expected update command"),
        }
    }

    #[test]
    fn rejects_removed_project_command_tree() {
        for argv in [vec!["animus", "project"], vec!["animus", "project", "list"], vec!["animus", "project", "create"]]
        {
            let error = Cli::try_parse_from(argv.clone()).expect_err("the `project` group was removed");
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "argv {argv:?}");
        }
    }

    #[test]
    fn rejects_removed_git_verbs() {
        for argv in [
            vec!["animus", "git", "status"],
            vec!["animus", "git", "commit"],
            vec!["animus", "git", "push"],
            vec!["animus", "git", "pull"],
            vec!["animus", "git", "branches"],
            vec!["animus", "git", "repo", "get"],
            vec!["animus", "git", "repo", "init"],
            vec!["animus", "git", "repo", "clone"],
            vec!["animus", "git", "worktree", "create"],
            vec!["animus", "git", "worktree", "get"],
            vec!["animus", "git", "worktree", "remove"],
            vec!["animus", "git", "worktree", "pull"],
            vec!["animus", "git", "worktree", "push"],
            vec!["animus", "git", "worktree", "sync"],
            vec!["animus", "git", "worktree", "sync-status"],
        ] {
            let error = Cli::try_parse_from(argv.clone()).expect_err("removed git verb should reject");
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "argv {argv:?}");
        }
    }

    #[test]
    fn keeps_supported_git_verbs() {
        Cli::try_parse_from(["animus", "git", "repo", "list"]).expect("git repo list should parse");
        Cli::try_parse_from(["animus", "git", "worktree", "list", "--repo", "current"])
            .expect("git worktree list should parse");
        Cli::try_parse_from(["animus", "git", "worktree", "prune", "--repo", "current"])
            .expect("git worktree prune should parse");
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
        let cli =
            Cli::try_parse_from(["animus", "queue", "enqueue", "--subject-id", "TASK-123", "--workflow-ref", "ops"])
                .expect("queue enqueue command should parse");

        match cli.command {
            Command::Queue { command: QueueCommand::Enqueue(args) } => {
                assert_eq!(args.subject_id.as_deref(), Some("TASK-123"));
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
        let cli =
            Cli::try_parse_from(["animus", "workflow", "run", "animus.task/standard", "--subject-id", "task:TASK-123"])
                .expect("workflow run should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::Run(args) } => {
                assert_eq!(args.pipeline.as_deref(), Some("animus.task/standard"));
                assert_eq!(args.subject_id.as_deref(), Some("task:TASK-123"));
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

    /// v0.6 breaking cleanup: the `-i` short alias on `workflow resume`
    /// was retired. Only the canonical `--id` long form (and the
    /// `--workflow-id` domain-prefixed form) parse now.
    #[test]
    fn workflow_resume_rejects_retired_short_i_flag() {
        let error = Cli::try_parse_from(["animus", "workflow", "resume", "-i", "wf-abc-123"])
            .expect_err("workflow resume -i was retired in v0.6 and must fail to parse");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);

        // The canonical long form keeps working.
        let cli_long = Cli::try_parse_from(["animus", "workflow", "resume", "--id", "wf-xyz"])
            .expect("workflow resume --id must continue to parse");
        match cli_long.command {
            Command::Workflow { command: WorkflowCommand::Resume(args) } => {
                assert_eq!(args.id, "wf-xyz");
                assert!(!args.force, "force should default to false");
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
    fn pack_info_is_primary_and_inspect_alias_is_retired() {
        let cli = Cli::try_parse_from(["animus", "pack", "info", "--pack-id", "animus.task"])
            .expect("pack info should parse");
        match cli.command {
            Command::Pack { command: PackCommand::Info(args) } => {
                assert_eq!(args.pack_id.as_deref(), Some("animus.task"));
            }
            other => panic!("expected pack info, got {other:?}"),
        }
        let error = Cli::try_parse_from(["animus", "pack", "inspect", "--pack-id", "animus.task"])
            .expect_err("pack inspect was retired in v0.6 and must fail to parse");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
        let help = subcommand_help(&["pack"]);
        assert!(help_lists_subcommand(&help, "info"), "pack help must list `info`:\n{help}");
        assert!(
            !help_lists_subcommand(&help, "inspect"),
            "pack help must not list the retired `inspect` verb:\n{help}"
        );
    }

    #[test]
    fn skill_info_is_primary_and_show_alias_is_retired() {
        let cli = Cli::try_parse_from(["animus", "skill", "info", "--name", "alpha"]).expect("skill info should parse");
        match cli.command {
            Command::Skill { command: SkillCommand::Info(args) } => assert_eq!(args.name, "alpha"),
            other => panic!("expected skill info, got {other:?}"),
        }
        let error = Cli::try_parse_from(["animus", "skill", "show", "--name", "alpha"])
            .expect_err("skill show was retired in v0.6 and must fail to parse");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
        let help = subcommand_help(&["skill"]);
        assert!(help_lists_subcommand(&help, "info"), "skill help must list `info`:\n{help}");
        assert!(!help_lists_subcommand(&help, "show"), "skill help must not list the retired `show` verb:\n{help}");
    }

    #[test]
    fn flavor_info_is_primary_and_describe_alias_is_retired() {
        let cli =
            Cli::try_parse_from(["animus", "flavor", "info", "--name", "default"]).expect("flavor info should parse");
        match cli.command {
            Command::Flavor { command: FlavorCommand::Info(args) } => assert_eq!(args.name, "default"),
            other => panic!("expected flavor info, got {other:?}"),
        }
        let error = Cli::try_parse_from(["animus", "flavor", "describe", "--name", "default"])
            .expect_err("flavor describe was retired in v0.6 and must fail to parse");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
        let help = subcommand_help(&["flavor"]);
        assert!(help_lists_subcommand(&help, "info"), "flavor help must list `info`:\n{help}");
        assert!(
            !help_lists_subcommand(&help, "describe"),
            "flavor help must not list the retired `describe` verb:\n{help}"
        );
    }

    #[test]
    fn mcp_serve_parses_actor_json_for_per_user_scoping() {
        // WU-G: the workflow runner relays the authenticated run's actor to the
        // per-agent `animus mcp serve` child via `--actor-json`.
        let actor_json = r#"{"user_id":"alice","claims":["admin"],"tenant_id":"acme"}"#;
        let cli = Cli::try_parse_from(["animus", "mcp", "serve", "--actor-json", actor_json])
            .expect("mcp serve --actor-json should parse");
        match cli.command {
            Command::Mcp { command: McpCommand::Serve(args) } => {
                assert_eq!(args.actor_json.as_deref(), Some(actor_json));
                assert_eq!(args.agent_id, None);
                assert_eq!(args.workflow_id, None);
            }
            other => panic!("expected mcp serve, got {other:?}"),
        }
        // Bare `mcp serve` carries no actor (global scope).
        let cli = Cli::try_parse_from(["animus", "mcp", "serve"]).expect("bare mcp serve should parse");
        match cli.command {
            Command::Mcp { command: McpCommand::Serve(args) } => assert_eq!(args.actor_json, None),
            other => panic!("expected mcp serve, got {other:?}"),
        }
    }

    #[test]
    fn workflow_run_parses_actor_json_for_transport_scoping() {
        let actor_json = r#"{"user_id":"alice","claims":["admin"],"tenant_id":"acme"}"#;
        let cli = Cli::try_parse_from([
            "animus",
            "workflow",
            "run",
            "--subject-id",
            "task:TASK-1",
            "--actor-json",
            actor_json,
        ])
        .expect("workflow run --actor-json should parse");
        match cli.command {
            Command::Workflow { command: WorkflowCommand::Run(args) } => {
                assert_eq!(args.actor_json.as_deref(), Some(actor_json));
            }
            other => panic!("expected workflow run, got {other:?}"),
        }
        // No flag => None => global scope.
        let cli = Cli::try_parse_from(["animus", "workflow", "run", "--subject-id", "task:TASK-1"])
            .expect("bare workflow run should parse");
        match cli.command {
            Command::Workflow { command: WorkflowCommand::Run(args) } => assert_eq!(args.actor_json, None),
            other => panic!("expected workflow run, got {other:?}"),
        }
    }

    #[test]
    fn workflow_config_get_and_validate_parse_actor_json() {
        let actor_json = r#"{"user_id":"bob","claims":[]}"#;
        let cli = Cli::try_parse_from(["animus", "workflow", "config", "get", "--actor-json", actor_json])
            .expect("workflow config get --actor-json should parse");
        match cli.command {
            Command::Workflow { command: WorkflowCommand::Config { command: WorkflowConfigCommand::Get(args) } } => {
                assert_eq!(args.actor_json.as_deref(), Some(actor_json));
            }
            other => panic!("expected workflow config get, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["animus", "workflow", "config", "validate", "--actor-json", actor_json])
            .expect("workflow config validate --actor-json should parse");
        match cli.command {
            Command::Workflow {
                command: WorkflowCommand::Config { command: WorkflowConfigCommand::Validate(args) },
            } => assert_eq!(args.actor_json.as_deref(), Some(actor_json)),
            other => panic!("expected workflow config validate, got {other:?}"),
        }
        // No flag => global-only resolution.
        let cli = Cli::try_parse_from(["animus", "workflow", "config", "get"])
            .expect("bare workflow config get should parse");
        match cli.command {
            Command::Workflow { command: WorkflowCommand::Config { command: WorkflowConfigCommand::Get(args) } } => {
                assert_eq!(args.actor_json, None)
            }
            other => panic!("expected workflow config get, got {other:?}"),
        }
    }

    #[test]
    fn chat_send_parses_actor_json_distinct_from_as_user() {
        let actor_json = r#"{"user_id":"carol","claims":["admin"]}"#;
        let cli =
            Cli::try_parse_from(["animus", "chat", "send", "hello", "--as-user", "carol", "--actor-json", actor_json])
                .expect("chat send --actor-json should parse");
        match cli.command {
            Command::Chat { command: ChatCommand::Send(args) } => {
                assert_eq!(args.actor_json.as_deref(), Some(actor_json));
                assert_eq!(args.as_user.as_deref(), Some("carol"));
            }
            other => panic!("expected chat send, got {other:?}"),
        }
        // No actor flag => None (global authz scope) even with --as-user set.
        let cli = Cli::try_parse_from(["animus", "chat", "send", "hello", "--as-user", "carol"])
            .expect("bare chat send should parse");
        match cli.command {
            Command::Chat { command: ChatCommand::Send(args) } => {
                assert_eq!(args.actor_json, None);
                assert_eq!(args.as_user.as_deref(), Some("carol"));
            }
            other => panic!("expected chat send, got {other:?}"),
        }
    }

    #[test]
    fn output_read_is_primary_and_run_alias_is_retired() {
        let cli =
            Cli::try_parse_from(["animus", "output", "read", "--run-id", "RUN-1"]).expect("output read should parse");
        match cli.command {
            Command::Output { command: OutputCommand::Read(args) } => {
                assert_eq!(args.run_id.as_deref(), Some("RUN-1"));
                assert_eq!(args.workflow_id, None);
            }
            other => panic!("expected output read, got {other:?}"),
        }
        let error = Cli::try_parse_from(["animus", "output", "run", "--run-id", "RUN-1"])
            .expect_err("output run was retired in v0.6 and must fail to parse");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
        let help = subcommand_help(&["output"]);
        assert!(help_lists_subcommand(&help, "read"), "output help must list `read`:\n{help}");
        assert!(!help_lists_subcommand(&help, "run"), "output help must not list the retired `run` verb:\n{help}");
    }

    #[test]
    fn output_read_accepts_workflow_id_but_not_both_or_neither() {
        let cli = Cli::try_parse_from(["animus", "output", "read", "--workflow-id", "WF-1"])
            .expect("output read --workflow-id should parse");
        match cli.command {
            Command::Output { command: OutputCommand::Read(args) } => {
                assert_eq!(args.run_id, None);
                assert_eq!(args.workflow_id.as_deref(), Some("WF-1"));
            }
            other => panic!("expected output read, got {other:?}"),
        }
        Cli::try_parse_from(["animus", "output", "read", "--run-id", "RUN-1", "--workflow-id", "WF-1"])
            .expect_err("--run-id and --workflow-id must conflict");
        Cli::try_parse_from(["animus", "output", "read"]).expect_err("one of --run-id/--workflow-id is required");
    }

    #[test]
    fn output_decisions_parses_run_id_or_workflow_id() {
        let cli = Cli::try_parse_from(["animus", "output", "decisions", "--run-id", "RUN-9"])
            .expect("output decisions --run-id should parse");
        match cli.command {
            Command::Output { command: OutputCommand::Decisions(args) } => {
                assert_eq!(args.run_id.as_deref(), Some("RUN-9"));
                assert_eq!(args.workflow_id, None);
            }
            other => panic!("expected output decisions, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["animus", "output", "decisions", "--workflow-id", "WF-9"])
            .expect("output decisions --workflow-id should parse");
        match cli.command {
            Command::Output { command: OutputCommand::Decisions(args) } => {
                assert_eq!(args.workflow_id.as_deref(), Some("WF-9"));
            }
            other => panic!("expected output decisions, got {other:?}"),
        }
        Cli::try_parse_from(["animus", "output", "decisions"]).expect_err("one of --run-id/--workflow-id is required");
    }

    #[test]
    fn history_search_since_parses_durations_and_conflicts_with_started_after() {
        let cli = Cli::try_parse_from(["animus", "history", "search", "--since", "7d"])
            .expect("history search --since 7d should parse");
        match cli.command {
            Command::History { command: HistoryCommand::Search(args) } => {
                assert_eq!(args.since, Some(7 * 86_400));
            }
            other => panic!("expected history search, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["animus", "history", "search", "--since", "30m"])
            .expect("history search --since 30m should parse");
        match cli.command {
            Command::History { command: HistoryCommand::Search(args) } => assert_eq!(args.since, Some(30 * 60)),
            other => panic!("expected history search, got {other:?}"),
        }
        let cli =
            Cli::try_parse_from(["animus", "history", "search", "--since", "90"]).expect("bare numbers mean seconds");
        match cli.command {
            Command::History { command: HistoryCommand::Search(args) } => assert_eq!(args.since, Some(90)),
            other => panic!("expected history search, got {other:?}"),
        }
        Cli::try_parse_from(["animus", "history", "search", "--since", "5y"])
            .expect_err("unknown duration units must be rejected");
        Cli::try_parse_from([
            "animus",
            "history",
            "search",
            "--since",
            "7d",
            "--started-after",
            "2026-06-01T00:00:00Z",
        ])
        .expect_err("--since must conflict with --started-after");
        let cli = Cli::try_parse_from([
            "animus",
            "history",
            "search",
            "--started-after",
            "2026-06-01T00:00:00Z",
            "--started-before",
            "2026-06-02T00:00:00Z",
        ])
        .expect("RFC3339 flags must keep working");
        match cli.command {
            Command::History { command: HistoryCommand::Search(args) } => {
                assert_eq!(args.started_after.as_deref(), Some("2026-06-01T00:00:00Z"));
                assert_eq!(args.started_before.as_deref(), Some("2026-06-02T00:00:00Z"));
                assert_eq!(args.since, None);
            }
            other => panic!("expected history search, got {other:?}"),
        }
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

    /// v0.6 breaking cleanup: the `--force` visible alias for `--yes` on
    /// `workflow prune` / `workflow delete` was retired. Only `--yes`
    /// applies the deletion now.
    #[test]
    fn workflow_prune_and_delete_reject_retired_force_alias() {
        let prune = Cli::try_parse_from(["animus", "workflow", "prune", "--force"])
            .expect_err("workflow prune --force was retired in v0.6 and must fail to parse");
        assert_eq!(prune.kind(), ErrorKind::UnknownArgument);

        let delete = Cli::try_parse_from(["animus", "workflow", "delete", "--run-id", "wf-1", "--force"])
            .expect_err("workflow delete --force was retired in v0.6 and must fail to parse");
        assert_eq!(delete.kind(), ErrorKind::UnknownArgument);

        let prune_yes = Cli::try_parse_from(["animus", "workflow", "prune", "--yes"])
            .expect("workflow prune --yes must keep parsing");
        match prune_yes.command {
            Command::Workflow { command: WorkflowCommand::Prune(args) } => assert!(args.yes),
            other => panic!("expected workflow prune, got {other:?}"),
        }

        let delete_yes = Cli::try_parse_from(["animus", "workflow", "delete", "--run-id", "wf-1", "--yes"])
            .expect("workflow delete --yes must keep parsing");
        match delete_yes.command {
            Command::Workflow { command: WorkflowCommand::Delete(args) } => {
                assert_eq!(args.run_id, "wf-1");
                assert!(args.yes);
            }
            other => panic!("expected workflow delete, got {other:?}"),
        }
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

        let respond = Cli::try_parse_from(["animus", "approval", "respond", "--request-id", "confirm-1", "--approve"])
            .expect("approval respond should parse");
        match respond.command {
            Command::Approval { command: ApprovalCommand::Respond(args) } => {
                assert_eq!(args.request_id, "confirm-1");
                assert!(args.approve);
                assert!(!args.reject);
                assert!(args.approved().expect("--approve resolves"));
            }
            other => panic!("expected approval respond, got {other:?}"),
        }

        let reject = Cli::try_parse_from(["animus", "approval", "respond", "--request-id", "confirm-1", "--reject"])
            .expect("approval respond --reject should parse");
        match reject.command {
            Command::Approval { command: ApprovalCommand::Respond(args) } => {
                assert!(!args.approved().expect("--reject resolves"));
            }
            other => panic!("expected approval respond, got {other:?}"),
        }

        // --approve and --reject are mutually exclusive (clap group).
        Cli::try_parse_from(["animus", "approval", "respond", "--request-id", "confirm-1", "--approve", "--reject"])
            .expect_err("--approve and --reject must not be combinable");

        // Neither flag: parses, but `approved()` errors rather than
        // silently rejecting (the footgun this replaces).
        let neither = Cli::try_parse_from(["animus", "approval", "respond", "--request-id", "confirm-1"])
            .expect("respond without a decision still parses");
        match neither.command {
            Command::Approval { command: ApprovalCommand::Respond(args) } => {
                assert!(args.approved().is_err(), "omitting the decision must error, not silently reject");
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
    fn git_confirm_alias_is_retired() {
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
        .expect_err("git confirm was retired in v0.6 and must fail to parse");
        assert_eq!(request.kind(), ErrorKind::InvalidSubcommand);

        let respond = Cli::try_parse_from(["animus", "git", "confirm", "respond", "--request-id", "confirm-1"])
            .expect_err("git confirm respond must fail to parse");
        assert_eq!(respond.kind(), ErrorKind::InvalidSubcommand);

        let outcome = Cli::try_parse_from(["animus", "git", "confirm", "outcome", "--request-id", "confirm-1"])
            .expect_err("git confirm outcome must fail to parse");
        assert_eq!(outcome.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn approval_group_renders_in_top_level_help_and_git_confirm_is_gone() {
        let top_level = Cli::try_parse_from(["animus", "--help"]).expect_err("help short-circuits parsing");
        assert_eq!(top_level.kind(), ErrorKind::DisplayHelp);
        let top_level_help = top_level.to_string();
        assert!(top_level_help.contains("approval"), "top-level help should list the approval group");

        let git_help = Cli::try_parse_from(["animus", "git", "--help"]).expect_err("help short-circuits parsing");
        assert_eq!(git_help.kind(), ErrorKind::DisplayHelp);
        let git_help_text = git_help.to_string();
        assert!(
            !git_help_text.lines().any(|line| line.split_whitespace().next() == Some("confirm")),
            "git help must not list the retired confirm subcommand"
        );

        let confirm_help =
            Cli::try_parse_from(["animus", "git", "confirm", "--help"]).expect_err("help short-circuits parsing");
        assert_eq!(confirm_help.kind(), ErrorKind::InvalidSubcommand, "retired alias must not answer --help");
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
    fn subject_update_accepts_title_flag() {
        // TASK-192: `animus subject update --title` must parse and land in
        // SubjectUpdateArgs so a subject can be renamed. Before the fix clap
        // rejected `--title` as an unknown argument.
        let cli = Cli::try_parse_from([
            "animus", "subject", "update", "--kind", "task", "--id", "TASK-1", "--title", "New name",
        ])
        .expect("subject update --title should parse");
        match cli.command {
            Command::Subject { command: SubjectCommand::Update(args) } => {
                assert_eq!(args.title.as_deref(), Some("New name"));
                assert_eq!(args.id, "TASK-1");
            }
            other => panic!("expected subject update command, got {other:?}"),
        }
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
