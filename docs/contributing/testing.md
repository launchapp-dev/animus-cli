# Testing Guide

## Running Tests

Run all workspace tests:

```bash
cargo test --workspace
```

Run tests for a specific crate:

```bash
cargo test -p protocol
cargo test -p orchestrator-core
cargo test -p orchestrator-cli
```

Run a single test by name:

```bash
cargo test -p orchestrator-cli -- rust_only_dependency_policy
```

## Unit Tests

Unit tests live in `#[cfg(test)]` modules within the source files they test. They use the `InMemoryServiceHub` for isolated testing without filesystem access.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn task_creation_sets_backlog_status() {
        let hub = InMemoryServiceHub::new();
        let task = hub.tasks().create(TaskCreateInput {
            title: "Test task".into(),
            ..Default::default()
        }).await.unwrap();
        assert_eq!(task.status, TaskStatus::Backlog);
    }
}
```

The `protocol` crate provides test utilities behind the `test-utils` feature flag, including `EnvVarGuard` for safely setting/unsetting environment variables in tests.

## Integration Tests

Integration tests live in `crates/orchestrator-cli/tests/` and exercise the CLI as a subprocess:

| Test File | What It Tests |
|-----------|--------------|
| `cli_smoke.rs` | Basic CLI invocation and help output |
| `cli_e2e.rs` | End-to-end task and workflow operations |
| `cli_json_contract.rs` | JSON envelope schema stability |
| `workflow_state_machine_e2e.rs` | Workflow state machine transitions |
| `setup_doctor_e2e.rs` | Setup and doctor command behavior |
| `cli_skill_lifecycle.rs` | Skill registration and execution |
| `session_continuation_e2e.rs` | Session continuation across CLI invocations |
| `rust_only_dependency_policy.rs` | Dependency policy enforcement |

## Test Harness

The `CliHarness` (defined in `crates/orchestrator-cli/tests/support/test_harness.rs`) provides a convenient wrapper for testing CLI commands:

- Creates a temporary project directory for isolation
- Runs `ao` commands as subprocesses
- Parses JSON output and validates envelope contracts
- Provides `run_json_ok()` for commands expected to succeed and `run_json_err()` for expected failures

Example usage:

```rust
let harness = CliHarness::new()?;
let result = harness.run_json_ok(&["task", "list"])?;
assert_success_envelope(&result);
```

The harness validates the `ao.cli.v1` envelope contract:
- `schema` field matches `CLI_SCHEMA_ID`
- `ok` is `true` for success, `false` for errors
- Success envelopes include `data`
- Error envelopes include `error.code`, `error.exit_code`, and `error.message`

## InMemoryServiceHub for Isolated Tests

The `InMemoryServiceHub` stores all state in memory, implementing the same `ServiceHub` trait as the production `FileServiceHub`. This means unit tests exercise real business logic without touching the filesystem:

- No temp directory cleanup needed
- Tests run in parallel without interference
- Fast execution (no I/O overhead)

## CI Workflows

The project uses several GitHub Actions workflows:

| Workflow | File | Purpose |
|----------|------|---------|
| Rust Workspace CI | `rust-workspace-ci.yml` | Build, test, and lint all crates |
| Dependency Policy | `rust-only-dependency-policy.yml` | Verify no prohibited dependencies |
| Web UI CI | `web-ui-ci.yml` | Build and test web UI assets |
| Release | `release.yml` | Build release binaries and publish |
