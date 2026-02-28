use std::process::Command;

#[test]
fn help_includes_top_level_usage() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");
    let output = Command::new(binary).arg("--help").output()?;
    assert!(output.status.success(), "help command should succeed");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("Agent Orchestrator CLI"),
        "help output should include CLI title"
    );
    assert!(
        stdout.contains("Usage: ao [OPTIONS] <COMMAND>"),
        "help output should include usage line"
    );
    assert!(
        stdout.contains("tui"),
        "help output should include tui command"
    );
    Ok(())
}

#[test]
fn help_surfaces_command_descriptions_for_core_groups() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");

    let top_level_help = Command::new(&binary).arg("--help").output()?;
    assert!(
        top_level_help.status.success(),
        "top-level help should succeed"
    );
    let top_level_stdout = String::from_utf8(top_level_help.stdout)?;
    assert!(
        top_level_stdout.contains("Draft and refine project vision artifacts"),
        "top-level help should describe vision command"
    );
    assert!(
        top_level_stdout.contains("Generate or execute task plans from requirements"),
        "top-level help should describe execute command"
    );
    assert!(
        top_level_stdout.contains("Record and inspect review decisions and handoffs"),
        "top-level help should describe review command"
    );
    assert!(
        top_level_stdout.contains("Search, install, update, and publish versioned skills"),
        "top-level help should describe skill command"
    );

    let workflow_help = Command::new(&binary)
        .args(["workflow", "--help"])
        .output()?;
    assert!(
        workflow_help.status.success(),
        "workflow help should succeed"
    );
    let workflow_stdout = String::from_utf8(workflow_help.stdout)?;
    assert!(
        workflow_stdout.contains("List and inspect workflow checkpoints"),
        "workflow help should describe checkpoints command"
    );
    assert!(
        workflow_stdout.contains("Manage workflow phase definitions"),
        "workflow help should describe phases command"
    );
    assert!(
        workflow_stdout.contains("Read and update workflow state machine configuration"),
        "workflow help should describe state-machine command"
    );

    let workflow_checkpoints_prune_help = Command::new(&binary)
        .args(["workflow", "checkpoints", "prune", "--help"])
        .output()?;
    assert!(
        workflow_checkpoints_prune_help.status.success(),
        "workflow checkpoints prune help should succeed"
    );
    let workflow_checkpoints_prune_stdout =
        String::from_utf8(workflow_checkpoints_prune_help.stdout)?;
    assert!(
        workflow_checkpoints_prune_stdout
            .contains("Retain at most this many checkpoints per phase."),
        "workflow checkpoints prune help should explain keep-last retention"
    );
    assert!(
        workflow_checkpoints_prune_stdout
            .contains("Additionally prune checkpoints older than this age in hours."),
        "workflow checkpoints prune help should explain age-based retention"
    );

    let web_serve_help = Command::new(&binary)
        .args(["web", "serve", "--help"])
        .output()?;
    assert!(
        web_serve_help.status.success(),
        "web serve help should succeed"
    );
    let web_serve_stdout = String::from_utf8(web_serve_help.stdout)?;
    assert!(
        web_serve_stdout.contains("Host interface to bind the web server"),
        "web serve help should explain --host"
    );
    assert!(
        web_serve_stdout.contains("Serve API endpoints only without static assets"),
        "web serve help should explain --api-only"
    );

    let skill_help = Command::new(&binary).args(["skill", "--help"]).output()?;
    assert!(skill_help.status.success(), "skill help should succeed");
    let skill_stdout = String::from_utf8(skill_help.stdout)?;
    assert!(
        skill_stdout.contains("search"),
        "skill help should include search command"
    );
    assert!(
        skill_stdout.contains("install"),
        "skill help should include install command"
    );
    assert!(
        skill_stdout.contains("update"),
        "skill help should include update command"
    );
    assert!(
        skill_stdout.contains("publish"),
        "skill help should include publish command"
    );

    Ok(())
}

#[test]
fn help_surfaces_accepted_values_and_confirmation_guidance(
) -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");

    let task_help = Command::new(&binary)
        .args(["task", "list", "--help"])
        .output()?;
    assert!(task_help.status.success(), "task list help should succeed");
    let task_stdout = String::from_utf8(task_help.stdout)?;
    assert!(
        task_stdout.contains("feature|bugfix|hotfix|refactor|docs|test|chore|experiment"),
        "task list help should enumerate task type values"
    );
    assert!(
        task_stdout.contains(
            "backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled"
        ),
        "task list help should enumerate status values"
    );
    assert!(
        task_stdout.contains("critical|high|medium|low"),
        "task list help should enumerate priority values"
    );

    let requirements_update_help = Command::new(&binary)
        .args(["requirements", "update", "--help"])
        .output()?;
    assert!(
        requirements_update_help.status.success(),
        "requirements update help should succeed"
    );
    let requirements_update_stdout = String::from_utf8(requirements_update_help.stdout)?;
    assert!(
        requirements_update_stdout.contains("must|should|could|wont|won't"),
        "requirements update help should enumerate priority values"
    );
    assert!(
        requirements_update_stdout.contains("draft|refined|planned|in-progress|in_progress|done"),
        "requirements update help should enumerate status values"
    );
    assert!(
        requirements_update_stdout
            .contains("documentation|usability|runtime|integration|quality|release|security"),
        "requirements update help should enumerate category values"
    );
    assert!(
        requirements_update_stdout.contains(
            "product|functional|non-functional|nonfunctional|non_functional|technical|other"
        ),
        "requirements update help should enumerate type values"
    );

    let workflow_cancel_help = Command::new(&binary)
        .args(["workflow", "cancel", "--help"])
        .output()?;
    assert!(
        workflow_cancel_help.status.success(),
        "workflow cancel help should succeed"
    );
    let workflow_cancel_stdout = String::from_utf8(workflow_cancel_help.stdout)?;
    assert!(
        workflow_cancel_stdout.contains("Confirmation token; must match --id."),
        "workflow cancel help should explain confirmation token"
    );
    assert!(
        workflow_cancel_stdout
            .contains("Preview cancellation payload without mutating workflow state."),
        "workflow cancel help should explain dry-run mode"
    );

    let task_create_help = Command::new(&binary)
        .args(["task", "create", "--help"])
        .output()?;
    assert!(
        task_create_help.status.success(),
        "task create help should succeed"
    );
    let task_create_stdout = String::from_utf8(task_create_help.stdout)?;
    assert!(
        task_create_stdout
            .contains("When provided, values in this payload override individual CLI flags."),
        "task create help should explain --input-json precedence"
    );

    let git_push_help = Command::new(&binary)
        .args(["git", "push", "--help"])
        .output()?;
    assert!(
        git_push_help.status.success(),
        "git push help should succeed"
    );
    let git_push_stdout = String::from_utf8(git_push_help.stdout)?;
    assert!(
        git_push_stdout.contains("Force push (destructive and requires --confirmation-id)."),
        "git push help should explain destructive --force behavior"
    );
    assert!(
        git_push_stdout
            .contains("Approved confirmation id required for destructive git operations."),
        "git push help should explain --confirmation-id semantics"
    );

    let git_worktree_remove_help = Command::new(&binary)
        .args(["git", "worktree", "remove", "--help"])
        .output()?;
    assert!(
        git_worktree_remove_help.status.success(),
        "git worktree remove help should succeed"
    );
    let git_worktree_remove_stdout = String::from_utf8(git_worktree_remove_help.stdout)?;
    assert!(
        git_worktree_remove_stdout
            .contains("Preview command payload without changing repository state."),
        "git worktree remove help should explain --dry-run behavior"
    );

    let git_worktree_prune_help = Command::new(&binary)
        .args(["git", "worktree", "prune", "--help"])
        .output()?;
    assert!(
        git_worktree_prune_help.status.success(),
        "git worktree prune help should succeed"
    );
    let git_worktree_prune_stdout = String::from_utf8(git_worktree_prune_help.stdout)?;
    assert!(
        git_worktree_prune_stdout.contains(
            "Delete remote branches for pruned worktrees when branch metadata is available."
        ),
        "git worktree prune help should explain --delete-remote-branch behavior"
    );
    assert!(
        git_worktree_prune_stdout
            .contains("Approved confirmation id required before pruning worktrees."),
        "git worktree prune help should explain --confirmation-id semantics"
    );
    assert!(
        git_worktree_prune_stdout
            .contains("Preview prune actions without changing repository state."),
        "git worktree prune help should explain prune-specific --dry-run behavior"
    );

    Ok(())
}

#[test]
fn help_uses_explicit_value_names_and_repeatable_flag_guidance(
) -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");

    let task_update_help = Command::new(&binary)
        .args(["task", "update", "--help"])
        .output()?;
    assert!(
        task_update_help.status.success(),
        "task update help should succeed"
    );
    let task_update_stdout = String::from_utf8(task_update_help.stdout)?;
    assert!(
        task_update_stdout.contains("--id <TASK_ID>"),
        "task update help should use explicit TASK_ID value names"
    );
    assert!(
        task_update_stdout.contains("--linked-architecture-entity <ENTITY_ID>"),
        "task update help should expose ENTITY_ID value names"
    );
    assert!(
        task_update_stdout.contains(
            "Replace all linked architecture entities with the provided --linked-architecture-entity values."
        ),
        "task update help should explain replace behavior"
    );

    let requirements_create_help = Command::new(&binary)
        .args(["requirements", "create", "--help"])
        .output()?;
    assert!(
        requirements_create_help.status.success(),
        "requirements create help should succeed"
    );
    let requirements_create_stdout = String::from_utf8(requirements_create_help.stdout)?;
    assert!(
        requirements_create_stdout.contains("--acceptance-criterion <TEXT>"),
        "requirements create help should expose repeatable criterion value names"
    );
    assert!(
        requirements_create_stdout
            .contains("Optional source describing where this requirement originated."),
        "requirements create help should explain source"
    );

    let workflow_run_help = Command::new(&binary)
        .args(["workflow", "run", "--help"])
        .output()?;
    assert!(
        workflow_run_help.status.success(),
        "workflow run help should succeed"
    );
    let workflow_run_stdout = String::from_utf8(workflow_run_help.stdout)?;
    assert!(
        workflow_run_stdout.contains("--pipeline-id <PIPELINE_ID>"),
        "workflow run help should expose pipeline id value names"
    );

    let task_list_help = Command::new(&binary)
        .args(["task", "list", "--help"])
        .output()?;
    assert!(
        task_list_help.status.success(),
        "task list help should succeed"
    );
    let task_list_stdout = String::from_utf8(task_list_help.stdout)?;
    assert!(
        task_list_stdout.contains("--assignee-type <ASSIGNEE_TYPE>"),
        "task list help should expose assignee type value names"
    );
    assert!(
        task_list_stdout.contains(
            "Match tasks that include all provided tags. Repeat to require multiple tags."
        ),
        "task list help should explain repeatable tags"
    );

    Ok(())
}

#[test]
fn version_subcommand_supports_json_output() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");
    let output = Command::new(binary).args(["--json", "version"]).output()?;
    assert!(output.status.success(), "version command should succeed");

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        payload.get("schema").and_then(|value| value.as_str()),
        Some("ao.cli.v1")
    );
    assert_eq!(
        payload
            .pointer("/data/binary")
            .and_then(|value| value.as_str()),
        Some("ao")
    );
    assert!(
        payload
            .pointer("/data/version")
            .and_then(|value| value.as_str())
            .is_some(),
        "version payload should include data.version"
    );
    Ok(())
}

#[test]
fn invalid_arguments_include_usage_and_help_hint() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");
    let output = Command::new(binary)
        .args(["task", "list", "--bogus"])
        .output()?;

    assert!(
        !output.status.success(),
        "unknown argument should produce a failing exit code"
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("unexpected argument '--bogus' found"),
        "stderr should identify the unexpected argument"
    );
    assert!(
        stderr.contains("Usage: ao task list [OPTIONS]"),
        "stderr should include command usage"
    );
    assert!(
        stderr.contains("For more information, try '--help'."),
        "stderr should include a hint to use --help"
    );

    Ok(())
}

#[test]
fn help_includes_workflow_monitor_command() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("ao");
    let output = Command::new(&binary).arg("--help").output()?;
    assert!(output.status.success(), "help command should succeed");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("workflow-monitor"),
        "help output should include workflow-monitor command"
    );
    assert!(
        stdout.contains("Live workflow phase monitor"),
        "help output should describe workflow-monitor command"
    );

    let monitor_help = Command::new(&binary)
        .args(["workflow-monitor", "--help"])
        .output()?;
    assert!(
        monitor_help.status.success(),
        "workflow-monitor help should succeed"
    );
    let monitor_stdout = String::from_utf8(monitor_help.stdout)?;
    assert!(
        monitor_stdout.contains("--refresh-interval"),
        "workflow-monitor help should explain --refresh-interval"
    );
    assert!(
        monitor_stdout.contains("--buffer-lines"),
        "workflow-monitor help should explain --buffer-lines"
    );
    assert!(
        monitor_stdout.contains("--workflow-id"),
        "workflow-monitor help should explain --workflow-id"
    );

    Ok(())
}
