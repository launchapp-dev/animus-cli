# TASK-029 Requirements: Agent Runner Env Sanitizer Allowlist Completion

## Phase
- Workflow phase: `requirements`
- Workflow ID: `bc4cb8b7-9b2b-4c23-9240-88823371b242`
- Task: `TASK-029`

## Objective
Define the implementation contract to complete the `agent-runner` environment
sanitizer allowlist so required provider/runtime variables are forwarded to
child AI CLI processes without dropping critical configuration.

## Existing Baseline Audit

| Surface | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Env sanitizer allowlist | `crates/agent-runner/src/sandbox/env_sanitizer.rs` | static explicit list for base shell vars and selected provider vars | missing `GEMINI_API_KEY`, terminal/TTY vars, `SSH_AUTH_SOCK`, and prefix-based `AO_`/`XDG_` forwarding |
| Gemini API key forwarding | `env_sanitizer.rs` | `GOOGLE_API_KEY` is already present | `GEMINI_API_KEY` is not forwarded, causing Gemini CLI auth failures in sanitized runs |
| AO runtime config forwarding | `env_sanitizer.rs` + `runner/supervisor.rs` | isolated explicit insertions (`AO_MCP_ENDPOINT`) exist in supervisor | general `AO_*` configuration passthrough is absent from sanitizer contract |
| Regression coverage | `env_sanitizer.rs` tests | only checks that `PATH` exists in sanitized env | no targeted assertions for allowlist additions or denylist behavior |

## Scope
In scope for implementation after this requirements phase:
- Expand environment sanitizer support for:
  - explicit keys: `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `TERM`, `COLORTERM`,
    `SSH_AUTH_SOCK`
  - prefix-based keys: `AO_*`, `XDG_*`
- Preserve existing explicit allowlist entries and current sanitizer behavior.
- Add deterministic unit tests in `env_sanitizer.rs` for positive and negative
  coverage.

Out of scope for this task:
- Reworking runner process lifecycle or IPC protocol behavior.
- Introducing a full environment passthrough mode.
- Changing CLI/runtime contract schemas.

## Constraints
- Keep sanitizer default-deny: only explicit keys and approved prefixes pass.
- Do not log or persist environment values in test output or runtime logs.
- Keep behavior cross-platform:
  - `SSH_AUTH_SOCK` may be absent on systems without SSH agent usage.
  - `XDG_*` passthrough must remain optional and non-failing when unset.
- Preserve existing successful provider flows (`OPENAI_API_KEY`,
  `ANTHROPIC_API_KEY`, Claude settings vars).

## Functional Requirements

### FR-01: Gemini Credential Forwarding
- Sanitized environments must include `GEMINI_API_KEY` when set.
- `GOOGLE_API_KEY` support must remain intact.

### FR-02: AO Runtime Configuration Forwarding
- Sanitized environments must include any source environment variable whose name
  starts with `AO_`.

### FR-03: XDG Directory Forwarding
- Sanitized environments must include any source environment variable whose name
  starts with `XDG_`.

### FR-04: Terminal and SSH Agent Context
- Sanitized environments must include `TERM`, `COLORTERM`, and
  `SSH_AUTH_SOCK` when present.

### FR-05: Regression Coverage
- Unit tests must verify:
  - allow behavior for new explicit keys and prefix keys,
  - retention of existing critical keys,
  - deny behavior for unrelated variables (for example `AWS_SECRET_ACCESS_KEY`).

## Acceptance Criteria
- `AC-01`: `GEMINI_API_KEY` and `GOOGLE_API_KEY` are forwarded by
  `sanitize_env()` when set.
- `AC-02`: `sanitize_env()` forwards `AO_*` variables from parent environment.
- `AC-03`: `sanitize_env()` forwards `XDG_*` variables from parent environment.
- `AC-04`: `sanitize_env()` forwards `TERM`, `COLORTERM`, and `SSH_AUTH_SOCK`
  when present.
- `AC-05`: Existing explicit allowlist keys remain forwarded as before.
- `AC-06`: Unit tests cover positive and negative sanitizer behavior for the new
  contract.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-04`, `AC-05` | `agent-runner` unit tests in `env_sanitizer.rs` for explicit key forwarding |
| `AC-02`, `AC-03` | `agent-runner` unit tests for prefix-based forwarding (`AO_`, `XDG_`) |
| `AC-06` | targeted crate test run: `cargo test -p agent-runner env_sanitizer` |

## Deterministic Deliverables for Implementation Phase
- Updated allowlist logic in `env_sanitizer.rs` supporting explicit keys plus
  `AO_`/`XDG_` prefixes.
- Expanded sanitizer tests proving required pass-through and deny behavior.
