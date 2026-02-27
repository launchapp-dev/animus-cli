# TASK-096 Implementation Notes: Built-In EM/PO/SWE Persona Mapping

## Purpose
Translate requirements into a minimal-risk implementation plan that introduces
built-in EM/PO/SWE personas and binds standard pipeline phases to PO/SWE.

## Chosen Strategy
- Treat this task as a config-and-fallback parity update:
  - update checked-in built-in JSON config,
  - update hardcoded fallback config,
  - update tests for new persona IDs and phase mappings.
- Keep scope tightly bounded to persona definitions and phase `agent_id`
  bindings; do not expand into full MCP policy-engine work.

## Non-Negotiable Constraints
- No manual edits to `/.ao/*.json`.
- Preserve deterministic runtime behavior across JSON and hardcoded built-ins.
- Keep schema/version compatibility unless dependency work requires otherwise.
- Avoid unrelated workflow or scheduler refactors.

## Proposed Change Surface

### 1) Built-In Persona Definitions (Primary)
- File: `crates/orchestrator-core/config/agent-runtime-config.v2.json`
- Add `agents.em`, `agents.po`, `agents.swe`.
- Populate each with:
  - role-specific description and system prompt,
  - role-specific capabilities metadata,
  - role-specific AO MCP tool access pattern metadata.
- Keep legacy IDs (`default`, `implementation`) either:
  - as explicit compatibility aliases, or
  - migrated with tests and all call sites updated in the same change.

### 2) Phase-to-Persona Mapping
- File: `crates/orchestrator-core/config/agent-runtime-config.v2.json`
- Update built-in phase definitions:
  - `requirements` -> `po`
  - `implementation` -> `swe`
  - `code-review` -> `swe`
  - `testing` -> `swe`
- Evaluate UI/UX phases (`ux-research`, `wireframe`, `mockup-review`) and map
  explicitly only if required by accepted scope; otherwise preserve current
  mapping.

### 3) Hardcoded Fallback Parity
- File: `crates/orchestrator-core/src/agent_runtime_config.rs`
- Mirror JSON persona additions and phase mapping changes in
  `hardcoded_builtin_agent_runtime_config()`.
- Ensure fallback remains valid if checked-in JSON fails to parse/validate.

### 4) Test Updates
- File: `crates/orchestrator-core/src/agent_runtime_config.rs` tests
- Update/extend assertions:
  - persona IDs exist (`em`, `po`, `swe`),
  - implementation-phase mapping resolves to `swe`,
  - requirements-phase mapping resolves to `po`,
  - structured-output behavior remains unchanged for implementation phase.

### 5) Optional Surface (Only if Needed)
- File: `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
- If persona metadata fields were extended and should be visible in CLI config
  output, update mapping structs so JSON output includes new fields.

## Sequencing Plan
1. Update built-in JSON config with persona definitions and phase mappings.
2. Mirror the same changes in hardcoded fallback config.
3. Update/extend core unit tests for persona presence and mapping behavior.
4. Run targeted `orchestrator-core` tests and address regressions.

## Risks and Mitigations
- Risk: JSON and fallback drift.
  - Mitigation: mirror edits in both surfaces and add tests that assert expected
    runtime behavior from built-in defaults.
- Risk: breaking legacy profile references (`default`, `implementation`).
  - Mitigation: keep compatibility aliases or migrate all references and tests
    atomically in one change.
- Risk: over-scoping into policy-engine enforcement.
  - Mitigation: treat MCP policy patterns as persona metadata in this task.

## Validation Targets
- `cargo test -p orchestrator-core agent_runtime_config::tests -- --nocapture`
- `cargo test -p orchestrator-core -- --nocapture` (recommended full package
  sweep if touched tests pass)
