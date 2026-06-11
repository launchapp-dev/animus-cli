use protocol::CLI_SCHEMA_ID;
use std::process::Command;

#[test]
fn help_includes_top_level_usage() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("animus");
    let output = Command::new(binary).arg("--help").output()?;
    assert!(output.status.success(), "help command should succeed");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Animus — the spirit that drives your agents"), "help output should include CLI title");
    assert!(stdout.contains("Usage: animus [OPTIONS] <COMMAND>"), "help output should include usage line");
    assert!(stdout.contains("status"), "help output should include status command");
    Ok(())
}

#[test]
fn help_surfaces_command_descriptions_for_core_groups() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("animus");

    let top_level_help = Command::new(&binary).arg("--help").output()?;
    assert!(top_level_help.status.success(), "top-level help should succeed");
    let top_level_stdout = String::from_utf8(top_level_help.stdout)?;
    assert!(
        top_level_stdout.contains("List, get, create, and update subjects"),
        "top-level help should describe subject command"
    );
    assert!(
        top_level_stdout.contains("Run and control workflow execution"),
        "top-level help should describe workflow command"
    );
    assert!(
        top_level_stdout.contains("Show a unified project status dashboard"),
        "top-level help should describe status command"
    );
    assert!(
        top_level_stdout.contains("Search, install, update, and publish versioned skills"),
        "top-level help should describe skill command"
    );
    assert!(
        top_level_stdout.contains("Initialize an Animus project from a template"),
        "top-level help should describe init command"
    );

    let workflow_help = Command::new(&binary).args(["workflow", "--help"]).output()?;
    assert!(workflow_help.status.success(), "workflow help should succeed");
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

    let workflow_checkpoints_prune_help =
        Command::new(&binary).args(["workflow", "checkpoints", "prune", "--help"]).output()?;
    assert!(workflow_checkpoints_prune_help.status.success(), "workflow checkpoints prune help should succeed");
    let workflow_checkpoints_prune_stdout = String::from_utf8(workflow_checkpoints_prune_help.stdout)?;
    assert!(
        workflow_checkpoints_prune_stdout.contains("Retain at most this many checkpoints per phase."),
        "workflow checkpoints prune help should explain keep-last retention"
    );
    assert!(
        workflow_checkpoints_prune_stdout.contains("Additionally prune checkpoints older than this age in hours."),
        "workflow checkpoints prune help should explain age-based retention"
    );

    let workflow_prune_help = Command::new(&binary).args(["workflow", "prune", "--help"]).output()?;
    assert!(workflow_prune_help.status.success(), "workflow prune help should succeed");
    let workflow_prune_stdout = String::from_utf8(workflow_prune_help.stdout)?;
    assert!(
        workflow_prune_stdout.contains("--older-than <DAYS>"),
        "workflow prune help should explain age-based retention"
    );
    assert!(
        workflow_prune_stdout.contains("--keep-last <COUNT>"),
        "workflow prune help should explain keep-last retention"
    );
    assert!(
        workflow_prune_stdout.contains("dry-run preview"),
        "workflow prune help should explain that --yes is required to delete"
    );

    let workflow_delete_help = Command::new(&binary).args(["workflow", "delete", "--help"]).output()?;
    assert!(workflow_delete_help.status.success(), "workflow delete help should succeed");
    let workflow_delete_stdout = String::from_utf8(workflow_delete_help.stdout)?;
    assert!(
        workflow_delete_stdout.contains("--run-id <RUN_ID>"),
        "workflow delete help should document the run id flag"
    );

    let skill_help = Command::new(&binary).args(["skill", "--help"]).output()?;
    assert!(skill_help.status.success(), "skill help should succeed");
    let skill_stdout = String::from_utf8(skill_help.stdout)?;
    assert!(skill_stdout.contains("search"), "skill help should include search command");
    assert!(skill_stdout.contains("install"), "skill help should include install command");
    assert!(skill_stdout.contains("update"), "skill help should include update command");
    assert!(skill_stdout.contains("publish"), "skill help should include publish command");

    let init_help = Command::new(&binary).args(["init", "--help"]).output()?;
    assert!(init_help.status.success(), "init help should succeed");
    let init_stdout = String::from_utf8(init_help.stdout)?;
    assert!(init_stdout.contains("--template <TEMPLATE_ID>"), "init help should explain bundled template selection");
    assert!(init_stdout.contains("--path <PATH>"), "init help should explain local template loading");
    assert!(init_stdout.contains("--non-interactive"), "init help should explain non-interactive mode");

    Ok(())
}

#[test]
fn help_surfaces_accepted_values_and_confirmation_guidance() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("animus");

    let workflow_cancel_help = Command::new(&binary).args(["workflow", "cancel", "--help"]).output()?;
    assert!(workflow_cancel_help.status.success(), "workflow cancel help should succeed");
    let workflow_cancel_stdout = String::from_utf8(workflow_cancel_help.stdout)?;
    assert!(
        workflow_cancel_stdout.contains("Confirmation token; must match --id."),
        "workflow cancel help should explain confirmation token"
    );
    assert!(
        workflow_cancel_stdout.contains("Preview cancellation payload without mutating workflow state."),
        "workflow cancel help should explain dry-run mode"
    );

    let git_push_help = Command::new(&binary).args(["git", "push", "--help"]).output()?;
    assert!(git_push_help.status.success(), "git push help should succeed");
    let git_push_stdout = String::from_utf8(git_push_help.stdout)?;
    assert!(
        git_push_stdout.contains("Force push (destructive and requires --confirmation-id)."),
        "git push help should explain destructive --force behavior"
    );
    assert!(
        git_push_stdout.contains("Approved confirmation id required for destructive git operations."),
        "git push help should explain --confirmation-id semantics"
    );

    let git_worktree_remove_help = Command::new(&binary).args(["git", "worktree", "remove", "--help"]).output()?;
    assert!(git_worktree_remove_help.status.success(), "git worktree remove help should succeed");
    let git_worktree_remove_stdout = String::from_utf8(git_worktree_remove_help.stdout)?;
    assert!(
        git_worktree_remove_stdout.contains("Preview command payload without changing repository state."),
        "git worktree remove help should explain --dry-run behavior"
    );

    let git_worktree_prune_help = Command::new(&binary).args(["git", "worktree", "prune", "--help"]).output()?;
    assert!(git_worktree_prune_help.status.success(), "git worktree prune help should succeed");
    let git_worktree_prune_stdout = String::from_utf8(git_worktree_prune_help.stdout)?;
    assert!(
        git_worktree_prune_stdout
            .contains("Delete remote branches for pruned worktrees when branch metadata is available."),
        "git worktree prune help should explain --delete-remote-branch behavior"
    );
    assert!(
        git_worktree_prune_stdout.contains("Approved confirmation id required before pruning worktrees."),
        "git worktree prune help should explain --confirmation-id semantics"
    );
    assert!(
        git_worktree_prune_stdout.contains("Preview prune actions without changing repository state."),
        "git worktree prune help should explain prune-specific --dry-run behavior"
    );

    Ok(())
}

#[test]
fn help_uses_explicit_value_names_and_repeatable_flag_guidance() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("animus");

    let workflow_run_help = Command::new(&binary).args(["workflow", "run", "--help"]).output()?;
    assert!(workflow_run_help.status.success(), "workflow run help should succeed");
    let workflow_run_stdout = String::from_utf8(workflow_run_help.stdout)?;
    assert!(
        workflow_run_stdout.contains("[PIPELINE]"),
        "workflow run help should expose the pipeline positional argument"
    );
    assert!(
        workflow_run_stdout.contains("--task-id <TASK_ID>"),
        "workflow run help should expose the explicit TASK_ID value name"
    );
    assert!(
        workflow_run_stdout.contains("When provided, values in this payload override individual CLI flags."),
        "workflow run help should explain --input-json precedence"
    );

    Ok(())
}

#[test]
fn version_subcommand_supports_json_output() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("animus");
    let output = Command::new(binary).args(["--json", "version"]).output()?;
    assert!(output.status.success(), "version command should succeed");

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(payload.get("schema").and_then(|value| value.as_str()), Some(CLI_SCHEMA_ID));
    assert_eq!(payload.pointer("/data/binary").and_then(|value| value.as_str()), Some("animus"));
    assert!(
        payload.pointer("/data/version").and_then(|value| value.as_str()).is_some(),
        "version payload should include data.version"
    );
    Ok(())
}

#[test]
fn invalid_arguments_include_usage_and_help_hint() -> Result<(), Box<dyn std::error::Error>> {
    let binary = assert_cmd::cargo::cargo_bin!("animus");
    let output = Command::new(binary).args(["queue", "list", "--bogus"]).output()?;

    assert!(!output.status.success(), "unknown argument should produce a failing exit code");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("unexpected argument '--bogus' found"), "stderr should identify the unexpected argument");
    assert!(stderr.contains("Usage: animus queue list [OPTIONS]"), "stderr should include command usage");
    assert!(stderr.contains("For more information, try '--help'."), "stderr should include a hint to use --help");

    Ok(())
}
