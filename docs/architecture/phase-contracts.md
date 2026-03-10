# Phase Contracts

## Purpose

This document defines the target design for phase output contracts in AO.

The goal is to keep YAML as the only authored workflow surface while giving the
runtime enough structure to validate phase output, assemble prompts, and pass
deterministic context between phases.

This is an architecture target, not a claim that every part of the design is
fully implemented today.

## Core Decision

Every phase should emit the same universal decision envelope:

- `verdict`
- `reason`
- `confidence`
- `risk`
- `evidence`

Phase-specific output should then extend that envelope with additional
phase-local fields defined in YAML.

Users should not manage standalone JSON schema files.

## Why This Model

AO currently needs three things at once:

1. One stable lifecycle contract the workflow engine can rely on.
2. Enough flexibility for projects to define phase-specific output surfaces.
3. Strong validation and prompt generation without exposing schema internals to
   workflow authors.

The universal envelope solves the first problem. YAML-defined extra fields solve
the second. Runtime-compiled contracts solve the third.

## Universal Phase Envelope

Every phase should produce these core fields:

| Field | Meaning |
|---|---|
| `verdict` | Workflow control signal: `advance`, `rework`, `fail`, or `skip` |
| `reason` | Short explanation for the verdict |
| `confidence` | Numeric confidence in the decision |
| `risk` | Risk level for proceeding: `low`, `medium`, `high` |
| `evidence` | Concrete evidence supporting the verdict |

This envelope is the stable runtime protocol. It allows the workflow engine,
monitoring, MCP retrieval, and future automation to reason about any phase in a
uniform way.

## YAML Defines Extra Fields

YAML should define only the extra fields beyond the universal envelope.

Each phase-local field should carry:

- `type`
- `required`
- `description`
- optional `enum`
- optional `items` for arrays

The field descriptions are important. They are the main user-facing way to
explain what the phase must emit.

Example target shape:

```yaml
phase_definitions:
  triage:
    emits: decision
    fields:
      skip_reason:
        type: string
        required: false
        description: "When verdict is skip, use already_done, duplicate, no_longer_valid, or out_of_scope."
      recommended_task_status:
        type: string
        required: false
        enum: [done, cancelled]
        description: "When verdict is skip, use done for already_done, otherwise cancelled."

  unit-test:
    emits: decision
    fields:
      exit_code:
        type: number
        required: true
        description: "Process exit code from the test command."
      failing_tests:
        type: array
        required: false
        description: "Names of failing tests when verdict is rework."
        items:
          type: string
      suggested_fix_focus:
        type: string
        required: false
        description: "Short summary of what the next repair phase should focus on."
```

## Runtime Responsibilities

The runtime should provide reusable primitives. It should not hardcode
workflow-specific product logic.

The runtime owns:

- built-in base contract types in Rust
- parsing YAML field declarations
- compiling effective phase contracts in memory
- validating final phase output against those contracts
- persisting structured phase artifacts
- injecting prior artifacts into later prompts

The runtime should not require users to create or edit JSON schema files.

## Effective Contract Compilation

The runtime should combine:

1. a built-in Rust-backed base contract for the universal envelope
2. YAML-defined field declarations for the current phase

That produces an effective phase contract in memory.

The effective contract is then used for both:

- prompt assembly
- final output validation

This gives AO one source of truth for what a phase is supposed to emit.

## Validation Model

AO should validate every final phase output against the effective contract.

The first version does not need full arbitrary JSON Schema authoring in YAML.
It only needs practical workflow-author features:

- basic type checks
- required fields
- enum validation
- array item shapes
- human-readable validation errors

If richer validation is needed later, AO can grow it internally without changing
the authored YAML surface.

## Prompt Assembly

Prompt assembly should be derived from the same contract metadata.

That means:

- core envelope fields are always required
- YAML field descriptions are rendered into the phase instructions
- later phases can declare which prior artifacts they consume
- prompts and validation remain aligned

This avoids the current failure mode where prompt expectations and runtime
validation drift apart.

Workflow authors should not paste hardcoded JSON examples into phase
`system_prompt` text. The runtime should inject the final structured output
shape from the compiled phase contract so authored YAML only needs to describe
phase behavior and field semantics.

## Structured Artifacts

The universal envelope is the control plane for every phase. AO should also
persist structured artifacts such as:

- `phase_diagnostics`
- `repair_plan`
- phase-local result objects

These artifacts should be workflow-local, durable, and retrievable without raw
log parsing.

That enables:

- diagnosis and fix loops
- workflow-local context retrieval
- better workflow monitor views
- better AO MCP tools

## Triage Example

Triage should still emit the universal envelope, but it can add fields like:

- `skip_reason`
- `recommended_task_status`

Example:

```json
{
  "verdict": "skip",
  "reason": "Task is obsolete because the planning facade is now the only supported path.",
  "confidence": 0.93,
  "risk": "low",
  "evidence": [
    {
      "kind": "code_search",
      "description": "Legacy direct planning entrypoints are hidden and replaced by workflow-backed commands."
    }
  ],
  "skip_reason": "no_longer_valid",
  "recommended_task_status": "cancelled"
}
```

The workflow lifecycle can continue to treat `skip` deterministically. The
project-specific interpretation of why a task is obsolete remains a matter of
prompting and context, not hardcoded rules.

## Command Phase Example

Command phases should also participate in the same contract model.

For example, `unit-test` should not only persist raw stdout/stderr. It should
emit the universal envelope plus phase-local fields such as:

- `exit_code`
- `failing_tests`
- `failure_category`

Example:

```json
{
  "verdict": "rework",
  "reason": "Workspace tests failed after the config cutover.",
  "confidence": 0.98,
  "risk": "medium",
  "evidence": [
    {
      "kind": "test_failure",
      "description": "5 tests still expect legacy config paths."
    }
  ],
  "exit_code": 101,
  "failing_tests": [
    "services::operations::ops_workflow::tests::legacy_path_is_rejected"
  ],
  "failure_category": "stale_test_expectation"
}
```

This makes deterministic gate failures much easier to route into diagnosis and
repair phases.

## Internal Libraries

The preferred implementation approach is:

- `serde` and `serde_yaml` for YAML parsing
- Rust types for the built-in base contracts
- `schemars` for internal schema generation and metadata
- optionally `jsonschema` later for richer in-memory validation

The important constraint is that these remain internal implementation details.
Workflow authors should still only work with YAML.

## Relationship to Other Requirements

| Requirement | Relationship |
|---|---|
| `REQ-031` | Workflow power stays in YAML; phase contracts become another YAML-defined workflow primitive |
| `REQ-032` | Structured workflow memory and phase contracts share the same artifact model |
| `REQ-045` | Workflow-local retrieval surfaces expose the universal envelope and phase-local fields |
| `REQ-036` | Authored config remains YAML-only |

## Practical Rule of Thumb

When designing a phase:

1. Assume every phase must end with a `verdict`.
2. Keep the universal envelope stable.
3. Put project-specific semantics into YAML field descriptions.
4. Let the runtime validate and persist the results.
5. Do not make users manage separate schema files.
