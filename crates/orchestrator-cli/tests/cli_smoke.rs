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
    Ok(())
}

#[test]
fn planning_vision_get_returns_cli_json_envelope() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let binary = assert_cmd::cargo::cargo_bin!("ao");
    let output = Command::new(binary)
        .args([
            "--json",
            "--project-root",
            temp.path().to_str().expect("utf-8 temp path"),
            "planning",
            "vision",
            "get",
        ])
        .output()?;

    assert!(
        output.status.success(),
        "planning vision get should succeed for empty project root"
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        payload.get("schema").and_then(|value| value.as_str()),
        Some("ao.cli.v1")
    );
    assert_eq!(
        payload.get("ok").and_then(|value| value.as_bool()),
        Some(true)
    );
    assert!(
        payload.get("data").is_some(),
        "json envelope should contain data field"
    );
    Ok(())
}
