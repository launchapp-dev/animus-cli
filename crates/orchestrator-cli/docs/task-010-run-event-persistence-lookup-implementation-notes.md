# TASK-010 Implementation Notes: Run/Event Persistence and Lookup Traceability

## Purpose
Translate TASK-010 requirements into concrete, deterministic test coverage that
proves runner persistence and CLI lookup/read paths remain aligned for
repository-scoped runtime directories.

## Non-Negotiable Constraints
- Keep changes scoped to Rust crates in this repository.
- Preserve current runtime behavior and command contracts.
- Keep tests isolated from host AO runtime state.
- Do not mutate `.ao/*.json` manually.

## Proposed Change Surface

### Runner persistence traceability
- `crates/agent-runner/src/runner/event_persistence.rs`
  - extend `#[cfg(test)]` coverage to assert canonical scoped runtime run path
    expectations for persisted `events.jsonl` and `json-output.jsonl`.
  - keep existing unsafe run-id no-op behavior coverage.

### Shared run directory traceability
- `crates/orchestrator-cli/src/shared.rs` (shared test module)
  - add focused tests for `run_dir` default path derivation under scoped runtime
    root.
  - assert `base_override` precedence remains deterministic and unchanged.

### Agent status fallback traceability
- `crates/orchestrator-cli/src/shared/parsing.rs`
  - add tests for `read_agent_status` reading canonical scoped `events.jsonl`
    and surfacing `events_path`.
  - include representative event streams (`Started`, `Error`, `Finished`) to
    validate fallback status derivation.

### Output lookup traceability and compatibility
- `crates/orchestrator-cli/src/services/operations/ops_output.rs`
  - add tests for `run_dir_candidates` precedence and `get_run_jsonl_entries`
    deterministic behavior.
  - add tests for legacy path fallback behavior and invalid run-id rejection.
  - add precedence assertion when both canonical and legacy run dirs exist.

## Deterministic Test Strategy
- Use per-test temp roots (`tempfile::TempDir`) for project/config/home
  isolation.
- Use env guards + lock for process-global env keys when tests set:
  - `HOME`
  - `XDG_CONFIG_HOME`
  - `AO_CONFIG_DIR`
- Build fixture JSONL files directly with deterministic lines and timestamps.
- Avoid long waits, process spawns, or network dependencies for this task.

## Implementation Sequence
1. Add/extend runner persistence unit tests for canonical path traceability.
2. Add shared-layer tests for `run_dir` alignment and override precedence.
3. Add `read_agent_status` fallback traceability tests.
4. Add `ops_output` lookup precedence/fallback/validation tests.
5. Run targeted crate tests, then broader crate-level passes.

## Risks and Mitigations
- Risk: flaky path assertions due to host-specific home/config behavior.
  - Mitigation: force temp HOME/config env in tests and assert suffix/shape.
- Risk: duplicate path derivation logic drifting across crates.
  - Mitigation: cross-check runner and CLI path expectations in tests.
- Risk: regression in legacy lookup compatibility while strengthening canonical
  assertions.
  - Mitigation: include explicit legacy fallback tests in `ops_output`.

## Validation Targets
- `cargo test -p agent-runner event_persistence`
- `cargo test -p orchestrator-cli shared::tests`
- `cargo test -p orchestrator-cli --lib`
- `cargo test -p orchestrator-cli --test cli_smoke --test cli_e2e`
