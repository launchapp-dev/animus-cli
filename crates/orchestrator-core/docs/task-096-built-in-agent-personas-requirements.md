# TASK-096 Requirements: Built-In EM, PO, and SWE Personas

## Phase
- Workflow phase: `requirements`
- Workflow ID: `c99daa66-b692-4740-9b07-041addbfd76b`
- Task: `TASK-096`

## Objective
Define three built-in agent personas in `agent-runtime-config.v2` and bind
workflow phases to role-appropriate personas:
- `em` (Engineering Manager): prioritization, queue management, scheduling
- `po` (Product Owner): requirements, acceptance criteria, deliverable review
- `swe` (Software Engineer): implementation, testing, code review

## Current Baseline Audit

| Surface | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Built-in agent profiles | `crates/orchestrator-core/config/agent-runtime-config.v2.json` | only `default` and `implementation` profiles exist | required `em` / `po` / `swe` personas are missing |
| Hardcoded built-in fallback | `crates/orchestrator-core/src/agent_runtime_config.rs` (`hardcoded_builtin_agent_runtime_config`) | fallback mirrors only `default` / `implementation` | fallback would drift from JSON if personas are added only in config file |
| Phase-to-agent mapping | same config + fallback | most phases use `default`; `implementation` phase uses `implementation` | standard pipeline not mapped to PO/SWE role intent |
| Persona schema expressiveness | `AgentProfile` in `agent_runtime_config.rs` | runtime fields are prompt/tool/model/overrides; no role/capabilities/tool-policy shape in current code | role capabilities + MCP policy need TASK-092 schema support (dependency) |
| Runtime prompt composition | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs` | phase prompt prepends profile system prompt and phase directive | persona prompts are directly impactful once profiles are mapped |

## Dependency and Preconditions
- `TASK-092` is the declared dependency for this task and introduces extended
  persona fields (`role`, `mcp_servers`, `tool_policy`, `skills`,
  `capabilities`).
- If `TASK-092` is not yet merged at implementation time, this task must either:
  - include the minimal schema additions required for persona fields, or
  - be explicitly re-blocked pending dependency completion.

## Problem Statement
The default runtime configuration does not currently include EM/PO/SWE personas
or phase bindings aligned to those roles. As a result, workflow execution cannot
differentiate management, product, and engineering behavior by persona.

## Decision for Implementation
- Add built-in `em`, `po`, and `swe` profiles with distinct prompts and
  role-specific metadata.
- Map phase `agent_id` values so standard pipeline execution uses:
  - `requirements` -> `po`
  - `implementation` -> `swe`
  - `code-review` -> `swe`
  - `testing` -> `swe`
- Keep persona metadata consistent in both:
  - checked-in built-in JSON (`config/agent-runtime-config.v2.json`)
  - hardcoded fallback (`hardcoded_builtin_agent_runtime_config`)
- Keep scope limited to built-in persona definitions and phase mapping, not full
  MCP policy-engine enforcement.

## Scope
In scope for implementation after this requirements phase:
- Add `em`, `po`, and `swe` profiles to built-in runtime config.
- Populate each profile with:
  - tailored `system_prompt`,
  - role-appropriate MCP tool access patterns,
  - role-specific capability flags.
- Update phase definitions to use PO/SWE personas for standard pipeline phases.
- Keep fallback and JSON built-ins aligned.
- Add/adjust tests for built-in profile existence + phase mapping.

Out of scope:
- New CLI commands for managing personas.
- Inter-agent messaging features.
- Custom project MCP server registration and merge logic.
- Broad MCP tool-policy enforcement redesign beyond already-supported behavior.
- Manual edits to `/.ao/*.json`.

## Constraints
- Preserve schema/version (`ao.agent-runtime-config.v2`, version `2`) unless
  dependency work explicitly changes it.
- Maintain deterministic behavior when loading runtime config from JSON or
  hardcoded fallback.
- Keep changes focused to persona definitions and phase mapping only.
- Avoid regressions in existing runtime phase execution contracts.

## Functional Requirements

### FR-01: Built-In Persona Presence
- Built-in runtime config must define `agents.em`, `agents.po`, and `agents.swe`.
- Each persona must include non-empty `description` and `system_prompt`.

### FR-02: Role-Specific Capability Flags
- Each new persona must include explicit capability flags aligned to role intent.
- Capability flags must differentiate PO from SWE and EM from both.

### FR-03: Role-Specific MCP Tool Access Patterns
- Each new persona must include AO MCP tool access pattern metadata reflecting
  least-privilege role boundaries.
- Patterns must at minimum separate:
  - product/requirements operations (`po`),
  - engineering implementation/testing operations (`swe`),
  - scheduling/prioritization/coordination operations (`em`).

### FR-04: Standard Pipeline Persona Mapping
- Built-in phase config must map:
  - `requirements` phase to `po`
  - `implementation`, `code-review`, `testing` phases to `swe`

### FR-05: Built-In/Fallback Parity
- Persona definitions and phase mappings must be kept consistent between:
  - `config/agent-runtime-config.v2.json`
  - hardcoded fallback in `agent_runtime_config.rs`

### FR-06: Backward Compatibility
- Existing consumers that still reference legacy profile IDs (`default`,
  `implementation`) must not fail validation unexpectedly.
- Any compatibility aliasing strategy must be explicit and tested.

### FR-07: Regression Coverage
- Tests must assert new persona IDs exist and phase mappings resolve to expected
  agent IDs under built-in defaults.

## Acceptance Criteria
- `AC-01`: Built-in runtime config contains `em`, `po`, and `swe` personas with
  non-empty prompts and descriptions.
- `AC-02`: `requirements` phase resolves to agent `po`.
- `AC-03`: `implementation`, `code-review`, and `testing` phases resolve to
  agent `swe`.
- `AC-04`: Persona definitions are present and equivalent in both checked-in
  JSON built-in config and hardcoded fallback built-in config.
- `AC-05`: Runtime config validation passes for updated built-ins.
- `AC-06`: Targeted tests in `orchestrator-core` pass for persona presence and
  phase-agent resolution.

## Testable Acceptance Checklist
- `T-01`: Add/adjust unit tests in
  `crates/orchestrator-core/src/agent_runtime_config.rs` for built-in persona
  IDs and phase mappings.
- `T-02`: Validate `implementation` phase now resolves to `swe`.
- `T-03`: Run targeted tests:
  - `cargo test -p orchestrator-core agent_runtime_config::tests -- --nocapture`
- `T-04`: If compatibility aliases are kept, test alias IDs still validate.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| Persona existence | unit assertions against `builtin_agent_runtime_config()` |
| Phase mapping correctness | phase-agent ID assertions for `requirements` / `implementation` / `code-review` / `testing` |
| Built-in parity | compare built-in JSON-loaded config behavior vs hardcoded fallback behavior in tests |
| Regression safety | targeted `orchestrator-core` test run |

## Implementation Notes Input (Next Phase)
Primary expected change targets:
- `crates/orchestrator-core/config/agent-runtime-config.v2.json`
  - add `em`/`po`/`swe`; update phase `agent_id` bindings.
- `crates/orchestrator-core/src/agent_runtime_config.rs`
  - mirror persona + phase changes in hardcoded fallback; update tests.
- `crates/orchestrator-core/src/services/tests.rs` (if fixtures assert old
  profile IDs)
  - update fixture assumptions for phase-agent mapping.
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs` (only if
  extended persona fields need to be surfaced in JSON output views).

## Deterministic Deliverables for Implementation Phase
- Built-in EM/PO/SWE persona definitions in runtime config.
- Standard pipeline phases mapped to PO/SWE by default.
- Built-in JSON/fallback parity retained.
- Tests covering persona presence and phase mapping.
