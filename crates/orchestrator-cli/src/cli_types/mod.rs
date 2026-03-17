mod agent_types;
mod architecture_types;
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
mod qa_types;
mod requirements_types;
mod review_types;
mod root_types;
mod runner_types;
mod setup_types;
mod shared_types;
mod skill_types;
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
pub(crate) use git_types::*;
pub(crate) use history_types::*;
pub(crate) use mcp_types::*;
pub(crate) use model_types::*;
pub(crate) use output_types::*;
pub(crate) use pack_types::*;
pub(crate) use queue_types::*;

pub(crate) use project_types::*;
pub(crate) use qa_types::*;
pub(crate) use requirements_types::*;
pub(crate) use review_types::*;
pub(crate) use root_types::*;
pub(crate) use runner_types::*;
pub(crate) use setup_types::*;
pub(crate) use shared_types::*;
pub(crate) use skill_types::*;
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
    fn workflow_checkpoints_prune_rejects_zero_keep_last_per_phase() {
        let error = Cli::try_parse_from([
            "ao",
            "workflow",
            "checkpoints",
            "prune",
            "--id",
            "WF-1",
            "--keep-last-per-phase",
            "0",
        ])
        .expect_err("zero keep-last-per-phase should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--keep-last-per-phase"));
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
    fn parses_pack_pin_command() {
        let cli = Cli::try_parse_from([
            "ao",
            "pack",
            "pin",
            "--pack-id",
            "ao.review",
            "--version",
            "=0.2.0",
            "--source",
            "installed",
            "--disable",
        ])
        .expect("pack pin should parse");

        match cli.command {
            Command::Pack { command: PackCommand::Pin(args) } => {
                assert_eq!(args.pack_id, "ao.review");
                assert_eq!(args.version.as_deref(), Some("=0.2.0"));
                assert_eq!(args.source.as_deref(), Some("installed"));
                assert!(args.disable);
            }
            _ => panic!("expected pack pin command"),
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
    fn parses_queue_reorder_subject_ids() {
        let cli = Cli::try_parse_from(["ao", "queue", "reorder", "--subject-id", "TASK-2", "--subject-id", "TASK-1"])
            .expect("queue reorder command should parse");

        match cli.command {
            Command::Queue { command: QueueCommand::Reorder(args) } => {
                assert_eq!(args.subject_ids, vec!["TASK-2", "TASK-1"]);
            }
            _ => panic!("expected queue reorder command"),
        }
    }

    #[test]
    fn parses_requirements_execute_command() {
        let cli = Cli::try_parse_from(["ao", "requirements", "execute", "--id", "REQ-101", "--id", "REQ-102"])
            .expect("requirements execute should parse");

        match cli.command {
            Command::Requirements { command: RequirementsCommand::Execute(args) } => {
                assert_eq!(args.requirement_ids, vec!["REQ-101", "REQ-102"]);
                assert!(args.start_workflows);
            }
            _ => panic!("expected requirements execute command"),
        }
    }

    #[test]
    fn parses_requirements_list_filters_and_sort() {
        let cli = Cli::try_parse_from([
            "ao",
            "requirements",
            "list",
            "--status",
            "draft",
            "--priority",
            "must",
            "--category",
            "runtime",
            "--type",
            "technical",
            "--tag",
            "backend",
            "--linked-task-id",
            "TASK-123",
            "--search",
            "cache",
            "--sort",
            "updated_at",
            "--limit",
            "20",
            "--offset",
            "5",
        ])
        .expect("requirements list should parse");

        match cli.command {
            Command::Requirements { command: RequirementsCommand::List(args) } => {
                assert_eq!(args.status.as_deref(), Some("draft"));
                assert_eq!(args.priority.as_deref(), Some("must"));
                assert_eq!(args.category.as_deref(), Some("runtime"));
                assert_eq!(args.requirement_type.as_deref(), Some("technical"));
                assert_eq!(args.tag, vec!["backend".to_string()]);
                assert_eq!(args.linked_task_id.as_deref(), Some("TASK-123"));
                assert_eq!(args.search.as_deref(), Some("cache"));
                assert_eq!(args.sort.as_deref(), Some("updated_at"));
                assert_eq!(args.limit, Some(20));
                assert_eq!(args.offset, 5);
            }
            _ => panic!("expected requirements list command"),
        }
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
            "--sort",
            "updated_at",
        ])
        .expect("task list command should parse");

        match cli.command {
            Command::Task { command: TaskCommand::List(args) } => {
                assert_eq!(args.task_type.as_deref(), Some("feature"));
                assert_eq!(args.status.as_deref(), Some("in-progress"));
                assert_eq!(args.priority.as_deref(), Some("high"));
                assert_eq!(args.assignee_type.as_deref(), Some("human"));
                assert_eq!(args.tag, vec!["api".to_string()]);
                assert_eq!(args.linked_requirement.as_deref(), Some("REQ-123"));
                assert_eq!(args.linked_architecture_entity.as_deref(), Some("ARCH-42"));
                assert_eq!(args.search.as_deref(), Some("critical path"));
                assert_eq!(args.sort.as_deref(), Some("updated_at"));
            }
            _ => panic!("expected task list command"),
        }
    }

    #[test]
    fn parses_workflow_list_filters_and_sort() {
        let cli = Cli::try_parse_from([
            "ao",
            "workflow",
            "list",
            "--status",
            "running",
            "--workflow-ref",
            "default",
            "--task-id",
            "TASK-123",
            "--phase-id",
            "implementation",
            "--search",
            "retry",
            "--sort",
            "started_at",
            "--limit",
            "10",
            "--offset",
            "2",
        ])
        .expect("workflow list should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::List(args) } => {
                assert_eq!(args.status.as_deref(), Some("running"));
                assert_eq!(args.workflow_ref.as_deref(), Some("default"));
                assert_eq!(args.task_id.as_deref(), Some("TASK-123"));
                assert_eq!(args.phase_id.as_deref(), Some("implementation"));
                assert_eq!(args.search.as_deref(), Some("retry"));
                assert_eq!(args.sort.as_deref(), Some("started_at"));
                assert_eq!(args.limit, Some(10));
                assert_eq!(args.offset, 2);
            }
            _ => panic!("expected workflow list command"),
        }
    }

    #[test]
    fn parses_task_create_with_single_linked_requirement() {
        let cli = Cli::try_parse_from([
            "ao",
            "task",
            "create",
            "--title",
            "Traceability task",
            "--linked-requirement",
            "REQ-123",
        ])
        .expect("task create should parse linked requirement");

        match cli.command {
            Command::Task { command: TaskCommand::Create(args) } => {
                assert_eq!(args.linked_requirement, vec!["REQ-123".to_string()]);
            }
            _ => panic!("expected task create command"),
        }
    }

    #[test]
    fn parses_task_create_with_repeated_linked_requirements() {
        let cli = Cli::try_parse_from([
            "ao",
            "task",
            "create",
            "--title",
            "Traceability task",
            "--linked-requirement",
            "REQ-123",
            "--linked-requirement",
            "REQ-456",
        ])
        .expect("task create should parse repeated linked requirements");

        match cli.command {
            Command::Task { command: TaskCommand::Create(args) } => {
                assert_eq!(args.linked_requirement, vec!["REQ-123".to_string(), "REQ-456".to_string()]);
            }
            _ => panic!("expected task create command"),
        }
    }

    #[test]
    fn task_stats_parses_stale_threshold_override() {
        let cli = Cli::try_parse_from(["ao", "task", "stats", "--stale-threshold-hours", "72"])
            .expect("task stats command should parse");

        match cli.command {
            Command::Task { command: TaskCommand::Stats(args) } => {
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
    fn task_rebalance_priority_parses_apply_and_overrides() {
        let cli = Cli::try_parse_from([
            "ao",
            "task",
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
        .expect("task rebalance-priority should parse");

        match cli.command {
            Command::Task { command: TaskCommand::RebalancePriority(args) } => {
                assert_eq!(args.high_budget_percent, 15);
                assert_eq!(args.essential_task_id, vec!["TASK-001".to_string()]);
                assert_eq!(args.nice_to_have_task_id, vec!["TASK-009".to_string()]);
                assert!(args.apply);
                assert_eq!(args.confirm.as_deref(), Some("apply"));
            }
            _ => panic!("expected task rebalance-priority command"),
        }
    }

    #[test]
    fn task_rebalance_priority_rejects_budget_above_100() {
        let error = Cli::try_parse_from(["ao", "task", "rebalance-priority", "--high-budget-percent", "101"])
            .expect_err("budget above 100 should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--high-budget-percent"));
        assert!(message.contains("between 0 and 100"));
    }

    #[test]
    fn parses_task_assign() {
        let cli =
            Cli::try_parse_from(["ao", "task", "assign", "--id", "TASK-001", "--assignee", "alice", "--type", "human"])
                .expect("task assign should parse");

        match cli.command {
            Command::Task { command: TaskCommand::Assign(args) } => {
                assert_eq!(args.id, "TASK-001");
                assert_eq!(args.assignee, "alice");
                assert_eq!(args.assignee_type.as_deref(), Some("human"));
            }
            _ => panic!("expected task assign command"),
        }

        let cli = Cli::try_parse_from([
            "ao",
            "task",
            "assign",
            "--id",
            "TASK-001",
            "--assignee",
            "coder",
            "--agent-role",
            "coder",
        ])
        .expect("task assign with --agent-role should parse");

        match cli.command {
            Command::Task { command: TaskCommand::Assign(args) } => {
                assert_eq!(args.id, "TASK-001");
                assert_eq!(args.assignee, "coder");
                assert_eq!(args.agent_role.as_deref(), Some("coder"));
            }
            _ => panic!("expected task assign command with agent role"),
        }
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
            Command::Workflow { command: WorkflowCommand::Phase { command: WorkflowPhaseCommand::Approve(args) } } => {
                assert_eq!(args.id, "WF-001");
                assert_eq!(args.phase, "testing");
                assert_eq!(args.note, "gate approved");
            }
            _ => panic!("expected workflow phase approve command"),
        }
    }

    #[test]
    fn parses_workflow_phase_reject_from_workflow_module() {
        let cli = Cli::try_parse_from([
            "ao",
            "workflow",
            "phase",
            "reject",
            "--id",
            "WF-002",
            "--phase",
            "testing",
            "--note",
            "gate rejected",
        ])
        .expect("workflow phase reject should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::Phase { command: WorkflowPhaseCommand::Reject(args) } } => {
                assert_eq!(args.id, "WF-002");
                assert_eq!(args.phase, "testing");
                assert_eq!(args.note, "gate rejected");
            }
            _ => panic!("expected workflow phase reject command"),
        }
    }

    #[test]
    fn parses_workflow_prompt_render_for_ad_hoc_subjects() {
        let cli = Cli::try_parse_from([
            "ao",
            "workflow",
            "prompt",
            "render",
            "--title",
            "Release Preview",
            "--phase",
            "implementation",
            "--input-json",
            "{\"ticket\":\"REL-9\"}",
            "--var",
            "release_name=Mercury",
        ])
        .expect("workflow prompt render should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::Prompt { command: WorkflowPromptCommand::Render(args) } } => {
                assert_eq!(args.title.as_deref(), Some("Release Preview"));
                assert_eq!(args.phase.as_deref(), Some("implementation"));
                assert_eq!(args.input_json.as_deref(), Some("{\"ticket\":\"REL-9\"}"));
                assert_eq!(args.vars, vec!["release_name=Mercury".to_string()]);
            }
            _ => panic!("expected workflow prompt render command"),
        }
    }

    #[test]
    fn parses_workflow_prompt_render_for_existing_workflow() {
        let cli =
            Cli::try_parse_from(["ao", "workflow", "prompt", "render", "--workflow-id", "WF-123", "--all-phases"])
                .expect("workflow prompt render should parse");

        match cli.command {
            Command::Workflow { command: WorkflowCommand::Prompt { command: WorkflowPromptCommand::Render(args) } } => {
                assert_eq!(args.workflow_id.as_deref(), Some("WF-123"));
                assert!(args.all_phases);
                assert!(args.vars.is_empty());
            }
            _ => panic!("expected workflow prompt render command"),
        }
    }
}
