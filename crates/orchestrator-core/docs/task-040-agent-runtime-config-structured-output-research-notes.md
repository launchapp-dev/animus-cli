# TASK-040 Research Notes: `agent_runtime_config` Structured-Output Test Mismatch

## Scope
- Workflow: `e36089f0-be43-4548-a937-0dc3b0bd90f6`
- Task: `TASK-040` (`in-progress`)
- Phase: `research`
- Objective:
  - explain why `builtin_defaults_mark_review_as_structured_output` fails
  - determine whether to fix test expectations or runtime logic
  - provide implementation-ready plan with validation steps

## AO State Evidence (2026-02-27)
- Active task metadata exists in canonical project root and matches the prompt:
  - `/path/to/animus/.animus/tasks/TASK-040.json`
- Worktree-local AO snapshot is older (tasks present through `TASK-036` only):
  - `.animus/tasks/`
  - `.animus/index/ao-cli-1222ef9c4f94/tasks/index.json`
- Assumption for implementation phase: use task prompt + canonical task file as source of truth for `TASK-040`, and avoid mutating `.ao` state manually in this worktree.

## Reproduction Evidence
- Failing test reproduces exactly with:
  - `cargo test -p orchestrator-core builtin_defaults_mark_review_as_structured_output -- --nocapture`
- Panic output:
  - `assertion failed: !config.is_structured_output_phase("implementation")`
  - `crates/orchestrator-core/src/agent_runtime_config.rs:1238`

## Code Evidence

### 1) Structured-output classifier returns `true` for schema/contract phases
- `is_structured_output_phase` short-circuits to `true` when either output contract or output schema exists:
  - `crates/orchestrator-core/src/agent_runtime_config.rs:433`
  - `crates/orchestrator-core/src/agent_runtime_config.rs:434`
  - `crates/orchestrator-core/src/agent_runtime_config.rs:435`

### 2) Built-in `implementation` phase explicitly declares contract + schema
- Hardcoded fallback config defines `implementation` with:
  - `output_contract.kind = "implementation_result"`
  - `required_fields = ["commit_message"]`
  - `output_json_schema` requiring `kind` + `commit_message`
  - `crates/orchestrator-core/src/agent_runtime_config.rs:652`
  - `crates/orchestrator-core/src/agent_runtime_config.rs:664`
- Checked-in built-in JSON config mirrors the same:
  - `crates/orchestrator-core/config/agent-runtime-config.v2.json:111`
  - `crates/orchestrator-core/config/agent-runtime-config.v2.json:116`
  - `crates/orchestrator-core/config/agent-runtime-config.v2.json:121`

### 3) Runtime phase execution consumes contract/schema for policy enforcement
- Phase execution injects output schema/contract into runtime policy:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs:606`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs:607`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs:626`
- Contract/schema validation gates completed phase outcome:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs:1166`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs:1193`

### 4) The failing test expectation contradicts current built-in semantics
- Same test module already asserts `implementation` has an output JSON schema:
  - `crates/orchestrator-core/src/agent_runtime_config.rs:1231`
- Next test asserts `implementation` is *not* structured-output:
  - `crates/orchestrator-core/src/agent_runtime_config.rs:1235`
  - `crates/orchestrator-core/src/agent_runtime_config.rs:1238`

## Root Cause
- The test assertion is stale/inconsistent with current config + classifier logic.
- Under current semantics, `implementation` is structured-output by definition because it has an explicit contract/schema.

## Decision Analysis
- Option A: change `is_structured_output_phase` to exclude `implementation`.
  - Risk: diverges from explicit per-phase contract/schema semantics.
  - Risk: may silently weaken contract-driven behavior expected by runtime execution.
- Option B (recommended): update test expectations to match current semantics.
  - Preserves contract-first interpretation and runtime behavior.
  - Minimal, deterministic change isolated to tests.

## Build-Ready Implementation Plan (Next Phase)
1. Update `builtin_defaults_mark_review_as_structured_output` assertions:
- keep `code-review` assertion
- change `implementation` expectation to `true`
2. Add/retain a negative control assertion for a phase without schema/contract (e.g., `testing`) to preserve non-structured coverage.
3. Run targeted validation:
- `cargo test -p orchestrator-core builtin_defaults_mark_review_as_structured_output -- --nocapture`
- optional confidence check: `cargo test -p orchestrator-core agent_runtime_config::tests -- --nocapture`

## Risks And Mitigations
- Risk: semantic ambiguity between keyword-based fallback (`contains("review")`) and explicit contract/schema criteria.
- Mitigation: in follow-up task, consider splitting classifier into explicit mode helpers (`requires_structured_result` vs heuristic review/audit tagging) if downstream call sites need distinction.

- Risk: drift between built-in JSON and hardcoded fallback config.
- Mitigation: add parity regression coverage in a follow-up test suite (not required to unblock TASK-040).

## External Blockers
- None. First-party code and task artifacts are sufficient to implement safely in the next phase.
