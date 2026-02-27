# TASK-048 Requirements: Requirement Category/Type Backfill

## Scope

Backfill missing `category` and `type` metadata for requirement records `REQ-007` through `REQ-024` in `.ao/requirements/generated/`.

This task is metadata-only and does not change requirement IDs, titles, descriptions, acceptance criteria, priorities, status, links, or timestamps.

## Constraints

- Use existing canonical category set: `documentation`, `usability`, `runtime`, `integration`, `quality`, `release`, `security`.
- Use existing canonical type set: `product`, `functional`, `non-functional`, `technical`.
- Keep assignments deterministic based on each requirement's dominant intent.
- Keep changes scoped to `REQ-007` through `REQ-024` only.

## Acceptance Criteria

- `REQ-007` through `REQ-024` each have non-null `category`.
- `REQ-007` through `REQ-024` each have non-null `type`.
- Assigned values are from the canonical sets above.
- No other requirement fields are modified.

## Deterministic Assignment Matrix

| Requirement | Category | Type | Rationale |
| --- | --- | --- | --- |
| REQ-007 | quality | technical | Dependency policy and CI guardrails are engineering-quality controls. |
| REQ-008 | integration | technical | JSON envelope compatibility defines external integration contracts. |
| REQ-009 | security | functional | Confirmation and preview safeguards are user-facing safety behaviors. |
| REQ-010 | runtime | non-functional | Trace completeness is a runtime observability quality attribute. |
| REQ-011 | usability | product | Primary operator console experience defines product UX direction. |
| REQ-012 | integration | technical | Typed SDK and API contract focus on server/client integration boundaries. |
| REQ-013 | usability | functional | Planning flows are explicit user-facing UI capabilities. |
| REQ-014 | usability | functional | Operational control-center interactions are user-facing workflow features. |
| REQ-015 | runtime | functional | Run telemetry and artifact inspection are runtime-centric operator features. |
| REQ-016 | security | functional | High-risk action protections are user-facing security controls. |
| REQ-017 | usability | non-functional | Accessibility and responsiveness are UX quality attributes. |
| REQ-018 | release | technical | CI/release gates and rollback discipline are release engineering controls. |
| REQ-019 | quality | non-functional | Structured diagnostics and observability improve operational quality. |
| REQ-020 | documentation | technical | Architecture graph/task linkage is technical documentation structure. |
| REQ-021 | usability | functional | Interactive TUI introduces operator-facing CLI interaction capabilities. |
| REQ-022 | runtime | technical | Concurrency-safe persistence is a runtime state-management mechanism. |
| REQ-023 | quality | technical | Deduplication and shared type unification are codebase quality work. |
| REQ-024 | security | technical | IPC auth and safe defaults are runtime security hardening mechanisms. |
