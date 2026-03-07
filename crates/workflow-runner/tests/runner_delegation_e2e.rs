use std::process::Command;
use tempfile::TempDir;

fn runner_binary() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin!("ao-workflow-runner").to_path_buf()
}

#[test]
fn runner_emits_lifecycle_events_on_stderr() {
    let temp_dir = TempDir::new().unwrap();

    let output = Command::new(runner_binary())
        .arg("execute")
        .arg("--task-id")
        .arg("TASK-TEST")
        .arg("--project-root")
        .arg(temp_dir.path())
        .output()
        .expect("runner should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("runner_start"),
        "should emit runner_start event; stderr: {stderr}"
    );
    assert!(
        stderr.contains("runner_complete"),
        "should emit runner_complete event; stderr: {stderr}"
    );
    assert!(
        stderr.contains("TASK-TEST"),
        "events should include task id; stderr: {stderr}"
    );
}

#[test]
fn runner_fails_with_nonzero_exit_for_missing_task() {
    let temp_dir = TempDir::new().unwrap();

    let output = Command::new(runner_binary())
        .arg("execute")
        .arg("--task-id")
        .arg("TASK-NONEXISTENT")
        .arg("--project-root")
        .arg(temp_dir.path())
        .output()
        .expect("runner should execute");

    assert!(
        !output.status.success(),
        "runner should fail when task does not exist"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"exit_code\":1"),
        "completion event should contain exit code 1; stderr: {stderr}"
    );
}

#[test]
fn runner_includes_pipeline_in_events() {
    let temp_dir = TempDir::new().unwrap();

    let output = Command::new(runner_binary())
        .arg("execute")
        .arg("--task-id")
        .arg("TASK-PIPE")
        .arg("--pipeline")
        .arg("custom-pipeline")
        .arg("--project-root")
        .arg(temp_dir.path())
        .output()
        .expect("runner should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("custom-pipeline"),
        "events should include pipeline; stderr: {stderr}"
    );
}

#[test]
fn runner_requires_subject_argument() {
    let temp_dir = TempDir::new().unwrap();

    let output = Command::new(runner_binary())
        .arg("execute")
        .arg("--project-root")
        .arg(temp_dir.path())
        .output()
        .expect("runner should execute");

    assert!(
        !output.status.success(),
        "runner should fail when no subject is provided"
    );
}

#[test]
fn runner_does_not_delegate_to_subprocess() {
    let source = include_str!("../src/main.rs");
    assert!(
        !source.contains("TokioCommand"),
        "main.rs must not delegate via TokioCommand"
    );
    assert!(
        !source.contains("resolve_ao_binary"),
        "main.rs must not contain resolve_ao_binary"
    );
}
