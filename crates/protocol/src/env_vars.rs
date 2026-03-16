/// Environment variable constants for the AO CLI workspace.
///
/// This module provides named constants for every `AO_*` environment variable
/// read or written by the workspace. Using constants prevents typo-bugs at
/// compile time and gives every crate a single import path for env var names.
///
/// See `docs/reference/env-vars.md` for the full inventory including migration
/// targets and deprecation status.

// ── Configuration & Paths ─────────────────────────────────────────────────

/// Global AO configuration directory override (`~/.ao` or `.ao` by default).
/// Migration target: `runtime-contract` — keep; CLI flag `--config-dir` already exists.
pub const AO_CONFIG_DIR: &str = "AO_CONFIG_DIR";

/// Runner configuration directory override. Falls back to `AO_CONFIG_DIR`.
/// Migration target: `runtime-contract` — keep; useful for test isolation.
pub const AO_RUNNER_CONFIG_DIR: &str = "AO_RUNNER_CONFIG_DIR";

// ── Agent Runner Control ───────────────────────────────────────────────────

/// When `1`/`true`/`yes`, ignores runner startup failure. Used in testing.
/// Migration target: `test-only`.
pub const AO_SKIP_RUNNER_START: &str = "AO_SKIP_RUNNER_START";

/// Internal runner scope label (`global` or project-scoped). Set/restored via guard.
/// Migration target: `internal` — not user-facing.
pub const AO_RUNNER_SCOPE: &str = "AO_RUNNER_SCOPE";

/// Build ID stamp for runner version tracking (compile-time via `option_env!`).
/// Migration target: `internal` — compile-time injection; keep as-is.
pub const AO_RUNNER_BUILD_ID: &str = "AO_RUNNER_BUILD_ID";

// ── Phase Execution ────────────────────────────────────────────────────────

/// Number of phase execution retry attempts (clamped 1–10, default 3).
/// Migration target: `workflow-yaml` — `phase.runtime.max_attempts`.
pub const AO_PHASE_RUN_ATTEMPTS: &str = "AO_PHASE_RUN_ATTEMPTS";

/// Max phase continuations before forcing completion (clamped 0–10, default 3).
/// Migration target: `workflow-yaml` — `phase.runtime.max_continuations`.
pub const AO_PHASE_MAX_CONTINUATIONS: &str = "AO_PHASE_MAX_CONTINUATIONS";

// ── Daemon Automation Flags (zombie — set but never read) ──────────────────

/// ZOMBIE: set from `DaemonRunArgs` but never read from env downstream.
/// Migration target: `delete`.
pub const AO_AUTO_MERGE_ENABLED: &str = "AO_AUTO_MERGE_ENABLED";

/// ZOMBIE: set from `DaemonRunArgs` but never read from env downstream.
/// Migration target: `delete`.
pub const AO_AUTO_PR_ENABLED: &str = "AO_AUTO_PR_ENABLED";

/// ZOMBIE: set from `DaemonRunArgs` but never read from env downstream.
/// Migration target: `delete`.
pub const AO_AUTO_COMMIT_BEFORE_MERGE: &str = "AO_AUTO_COMMIT_BEFORE_MERGE";

/// ZOMBIE: set from `DaemonRunArgs` but never read from env downstream.
/// Migration target: `delete`.
pub const AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE: &str = "AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE";

/// ZOMBIE: set from `DaemonRunArgs.scheduler.phase_timeout_secs` but never read.
/// Migration target: `delete` — timeout passed via `WorkflowPhaseRuntimeSettings`.
pub const AO_PHASE_TIMEOUT_SECS: &str = "AO_PHASE_TIMEOUT_SECS";

// ── Tool-Specific Configuration ────────────────────────────────────────────

/// JSON array of extra args appended to every AI CLI invocation.
/// Migration target: `workflow-yaml` — `phase.runtime.extra_args`.
pub const AO_AI_CLI_EXTRA_ARGS_JSON: &str = "AO_AI_CLI_EXTRA_ARGS_JSON";

/// Space-delimited extra args; fallback when `AO_AI_CLI_EXTRA_ARGS_JSON` is unset.
/// Migration target: `workflow-yaml` — same as above.
pub const AO_AI_CLI_EXTRA_ARGS: &str = "AO_AI_CLI_EXTRA_ARGS";

/// Pass `--dangerously-skip-permissions` to claude CLI (enabled when unset).
/// Migration target: `daemon-config` — agent profile capability flag.
pub const AO_CLAUDE_BYPASS_PERMISSIONS: &str = "AO_CLAUDE_BYPASS_PERMISSIONS";

/// JSON array of extra args for claude invocations.
/// Migration target: `workflow-yaml` — `phase.runtime.extra_args`.
pub const AO_CLAUDE_EXTRA_ARGS_JSON: &str = "AO_CLAUDE_EXTRA_ARGS_JSON";

/// Fallback space-delimited extra args for claude.
/// Migration target: `workflow-yaml` — same as above.
pub const AO_CLAUDE_EXTRA_ARGS: &str = "AO_CLAUDE_EXTRA_ARGS";

/// Codex reasoning effort level (`low`/`medium`/`high`) injected via CLI arg.
/// Migration target: `workflow-yaml` — `phase.runtime.reasoning_effort`.
pub const AO_CODEX_REASONING_EFFORT: &str = "AO_CODEX_REASONING_EFFORT";

/// Enable web search for Codex CLI (default `true`).
/// Migration target: `workflow-yaml` — `phase.runtime.web_search`.
pub const AO_CODEX_WEB_SEARCH: &str = "AO_CODEX_WEB_SEARCH";

/// Enable network access for Codex CLI (default `true`).
/// Migration target: `workflow-yaml` — `phase.runtime.network_access`.
pub const AO_CODEX_NETWORK_ACCESS: &str = "AO_CODEX_NETWORK_ACCESS";

/// JSON array of extra args for codex invocations.
/// Migration target: `workflow-yaml` — `phase.runtime.extra_args`.
pub const AO_CODEX_EXTRA_ARGS_JSON: &str = "AO_CODEX_EXTRA_ARGS_JSON";

/// Fallback space-delimited extra args for codex.
/// Migration target: `workflow-yaml` — same as above.
pub const AO_CODEX_EXTRA_ARGS: &str = "AO_CODEX_EXTRA_ARGS";

/// JSON array of `key=value` codex config overrides.
/// Migration target: `workflow-yaml` — `phase.runtime.codex_config_overrides`.
pub const AO_CODEX_EXTRA_CONFIG_OVERRIDES_JSON: &str = "AO_CODEX_EXTRA_CONFIG_OVERRIDES_JSON";

/// Fallback space-delimited codex config overrides.
/// Migration target: `workflow-yaml` — same as above.
pub const AO_CODEX_EXTRA_CONFIG_OVERRIDES: &str = "AO_CODEX_EXTRA_CONFIG_OVERRIDES";

/// JSON array of extra args for gemini invocations.
/// Migration target: `workflow-yaml` — `phase.runtime.extra_args`.
pub const AO_GEMINI_EXTRA_ARGS_JSON: &str = "AO_GEMINI_EXTRA_ARGS_JSON";

/// Fallback space-delimited extra args for gemini.
/// Migration target: `workflow-yaml` — same as above.
pub const AO_GEMINI_EXTRA_ARGS: &str = "AO_GEMINI_EXTRA_ARGS";

/// JSON array of extra args for opencode invocations.
/// Migration target: `workflow-yaml` — `phase.runtime.extra_args`.
pub const AO_OPENCODE_EXTRA_ARGS_JSON: &str = "AO_OPENCODE_EXTRA_ARGS_JSON";

/// Fallback space-delimited extra args for opencode.
/// Migration target: `workflow-yaml` — same as above.
pub const AO_OPENCODE_EXTRA_ARGS: &str = "AO_OPENCODE_EXTRA_ARGS";

// ── Feature Flags ──────────────────────────────────────────────────────────

/// When `true`, disables write-capability enforcement for non-editing tools.
/// Migration target: `daemon-config` — agent profile capability flag.
pub const AO_ALLOW_NON_EDITING_PHASE_TOOL: &str = "AO_ALLOW_NON_EDITING_PHASE_TOOL";

/// When `false`, disables automatic runner rebuild after main-branch update.
/// Migration target: `daemon-config` — daemon config JSON field.
pub const AO_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE: &str = "AO_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE";

/// Set to `07`/`draft07`/`draft-07`/`draft_07` to use JSON Schema Draft-07.
/// Migration target: `daemon-config` — MCP server config field.
pub const AO_MCP_SCHEMA_DRAFT: &str = "AO_MCP_SCHEMA_DRAFT";

// ── MCP & Agent Process ────────────────────────────────────────────────────

/// MCP server endpoint URL injected into agent subprocess environment.
/// Migration target: `internal` — injected by supervisor per-agent.
pub const AO_MCP_ENDPOINT: &str = "AO_MCP_ENDPOINT";

// ── User Identity ──────────────────────────────────────────────────────────

/// Task assignee user ID (highest priority, overrides `AO_USER_ID`).
/// Migration target: `cli-flag` — `ao task create --assignee <id>`.
pub const AO_ASSIGNEE_USER_ID: &str = "AO_ASSIGNEE_USER_ID";

/// Current user ID for task operations (fallback when `AO_ASSIGNEE_USER_ID` is unset).
/// Migration target: `cli-flag` — global `--user-id` flag or user config file.
pub const AO_USER_ID: &str = "AO_USER_ID";

// ── Notifications ──────────────────────────────────────────────────────────

/// Webhook endpoint URL for task/workflow event notifications.
/// Migration target: `runtime-contract` — keep; secret value.
pub const AO_NOTIFY_WEBHOOK_URL: &str = "AO_NOTIFY_WEBHOOK_URL";

/// Bearer token for webhook authorization header.
/// Migration target: `runtime-contract` — keep; secret value.
pub const AO_NOTIFY_BEARER_TOKEN: &str = "AO_NOTIFY_BEARER_TOKEN";

/// Internal sentinel used to test missing-URL error path in notification tests.
/// Migration target: `test-only` / `internal`.
pub const AO_NOTIFY_MISSING_URL: &str = "AO_NOTIFY_MISSING_URL";

// ── Test-Only Variables ────────────────────────────────────────────────────

/// Gate for enabling the session continuation E2E test.
pub const AO_E2E_SESSION_CONTINUATION: &str = "AO_E2E_SESSION_CONTINUATION";

/// Project root override for E2E tests.
pub const AO_E2E_PROJECT_ROOT: &str = "AO_E2E_PROJECT_ROOT";

/// Comma-separated tool list for E2E tests (default: `claude`).
pub const AO_E2E_TOOLS: &str = "AO_E2E_TOOLS";

/// Agent timeout in seconds for E2E tests (default: `120`).
pub const AO_E2E_TIMEOUT: &str = "AO_E2E_TIMEOUT";

/// Path to file where test spy captures CLI tool args before invocation.
pub const AO_TEST_ARGS_CAPTURE: &str = "AO_TEST_ARGS_CAPTURE";

/// Path to file where test spy captures env vars passed to AI CLI.
pub const AO_TEST_ENV_CAPTURE: &str = "AO_TEST_ENV_CAPTURE";

// ── Legacy / Deprecated ────────────────────────────────────────────────────

/// DEPRECATED: legacy config dir name. Actively cleared before IPC calls.
/// Migration target: `delete` — remove reads; callers already clear it.
pub const AGENT_ORCHESTRATOR_CONFIG_DIR: &str = "AGENT_ORCHESTRATOR_CONFIG_DIR";

/// DEPRECATED: legacy IPC auth token. Removed from protocol.
/// Migration target: `delete` — remove all reads, set_var, and remove_var calls.
pub const AGENT_RUNNER_TOKEN: &str = "AGENT_RUNNER_TOKEN";
