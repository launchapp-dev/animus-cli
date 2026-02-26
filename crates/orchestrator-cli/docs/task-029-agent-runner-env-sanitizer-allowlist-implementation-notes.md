# TASK-029 Implementation Notes: Agent Runner Env Sanitizer Allowlist

## Purpose
Define the concrete code change plan to satisfy TASK-029 without widening
environment exposure beyond the required contract.

## Current State Summary
Current sanitizer behavior:
- uses a static explicit allowlist in
  `crates/agent-runner/src/sandbox/env_sanitizer.rs`,
- includes `GOOGLE_API_KEY` but not `GEMINI_API_KEY`,
- does not support prefix-based forwarding for `AO_*` or `XDG_*`,
- has minimal tests (`PATH` only).

## Change Surface

### 1) Env Sanitizer Contract Update
Primary file:
- `crates/agent-runner/src/sandbox/env_sanitizer.rs`

Planned updates:
- keep the existing explicit allowlist model,
- add missing explicit entries:
  - `GEMINI_API_KEY`
  - `TERM`
  - `COLORTERM`
  - `SSH_AUTH_SOCK`
- add prefix allowlist support:
  - `AO_`
  - `XDG_`

Implementation approach:
- retain explicit-key lookup loop for fixed keys,
- add an environment iteration path for prefix matches (`env::vars()`),
- insert matched entries into the sanitized map.

### 2) Test Coverage Expansion
Primary file:
- `crates/agent-runner/src/sandbox/env_sanitizer.rs` (`#[cfg(test)]` module)

Planned tests:
- forwards `GEMINI_API_KEY` and `GOOGLE_API_KEY` when set,
- forwards `TERM`, `COLORTERM`, `SSH_AUTH_SOCK` when set,
- forwards representative `AO_*` and `XDG_*` variables,
- rejects unrelated sensitive variables not in allowlist/prefixes.

## Constraints to Preserve During Implementation
- Default-deny remains in place for non-matching keys.
- No changes to supervisor lifecycle, IPC, or process-builder contracts.
- No direct edits to `.ao` state files.

## Risk Notes
- `AO_*` forwarding may include variables not consumed by the child CLI.
  - Mitigation: scope remains prefix-based per task requirement; no additional
    non-`AO_` prefix expansion.
- Prefix iteration could accidentally include undesired keys if prefix checks are
  incorrect.
  - Mitigation: exact `starts_with("AO_")` and `starts_with("XDG_")` checks
    only.

## Validation Targets
- `cargo test -p agent-runner env_sanitizer`
- optional broader check if needed after implementation:
  - `cargo test -p agent-runner`
