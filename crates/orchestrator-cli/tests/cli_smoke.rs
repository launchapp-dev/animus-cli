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
