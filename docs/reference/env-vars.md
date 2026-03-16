# Environment Variables Reference

Complete inventory of all environment variables read by the AO CLI workspace, with migration targets.

---

## Migration Target Legend

| Target | Description |
|---|---|
| `workflow-yaml` | Migrate to a field in `.ao/workflows/*.yaml` (e.g., `phase.runtime.*`) |
| `daemon-config` | Migrate to `.ao/state/agent-runtime-config.v2.json` or daemon config JSON |
| `cli-flag` | Migrate to a named CLI flag on the relevant `ao` subcommand |
| `runtime-contract` | Keep as env var; document as a stable runtime contract for CI/automation |
| `system` | OS-provided system variable; not owned by AO |
| `test-only` | Used only in integration/e2e test scaffolding; not a user-facing API |
| `internal` | Set and read internally within the same process; not a user-facing API |
| `delete` | Zombie var — set but never read, or superseded; should be removed |

---

## Configuration & Paths

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_CONFIG_DIR` | `protocol`, `oai-runner` | Global AO configuration directory override | `~/.ao` or `.ao` (project-local) | `runtime-contract` — keep; equivalent CLI flag already exists as `--config-dir` |
| `AO_RUNNER_CONFIG_DIR` | `orchestrator-core` | Runner configuration directory override | Falls back to `AO_CONFIG_DIR` | `runtime-contract` — keep; useful for test isolation |
| `HOME` | `oai-runner`, `orchestrator-config` | User home directory for config path resolution | OS-provided | `system` |
| `PATHEXT` | `orchestrator-core` (doctor) | Windows executable extensions for binary lookup | `.EXE;.CMD;.BAT;.COM` | `system` |

---

## Agent Runner Control

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_SKIP_RUNNER_START` | `orchestrator-cli`, `orchestrator-core` | When `1`/`true`/`yes`, ignores runner startup failure (used in testing) | `false` | `test-only` — document as test-scaffolding; consider replacing with `--skip-runner` CLI flag |
| `AO_RUNNER_SCOPE` | `orchestrator-cli` | Internal runner scope label (`global` or project-scoped); set/restored via `EnvVarGuard` | None | `internal` — set programmatically around IPC calls; not user-facing |
| `AO_MAX_AGENTS` | `orchestrator-cli` | Maximum concurrent agent count in daemon pool | Pool size from daemon config | `delete` — set via `std::env::set_var` and `command.env()` in `runtime_daemon.rs` but **never read** from the environment; actual pool size comes from `DaemonStartConfig.max_agents` |
| `AO_RUNNER_BUILD_ID` | `orchestrator-core`, `agent-runner` | Build ID stamp for runner version tracking (compile-time via `option_env!`) | None | `internal` — compile-time injection; keep as-is |
| `AGENT_RUNNER_TOKEN` | `orchestrator-core`, `workflow-runner-v2` | IPC authentication token between CLI and runner process | None | `delete` — token removed from protocol; references in `runner_helpers.rs` are dead cleanup code |

---

## Phase Execution

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_PHASE_RUN_ATTEMPTS` | `workflow-runner-v2` | Number of phase execution retry attempts (clamped 1–10) | `3` | `workflow-yaml` — `phase.runtime.max_attempts` already exists in `WorkflowPhaseRuntimeSettings` |
| `AO_PHASE_MAX_CONTINUATIONS` | `workflow-runner-v2` | Max phase continuations before forcing completion (clamped 0–10) | `3` | `workflow-yaml` — `phase.runtime.max_continuations` already exists in `WorkflowPhaseRuntimeSettings` |
| `AO_PHASE_TIMEOUT_SECS` | `orchestrator-cli` | Phase execution timeout in seconds | None | `delete` — set via `EnvOverrideGuard` in `daemon_run.rs` but **never read** from environment; zombie var. Timeout is passed via `WorkflowPhaseRuntimeSettings.timeout_secs` |

---

## Daemon Automation Flags (Zombie Vars)

These five variables are **set** via `EnvOverrideGuard` in `daemon_run.rs:111–115` from CLI args but are **never read** from the environment by any downstream code. They are zombie variables.

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_AUTO_MERGE_ENABLED` | `orchestrator-cli` | Enable automatic branch merging | `false` | `delete` — set but never read; value already lives in `DaemonRunArgs.scheduler.auto_merge` |
| `AO_AUTO_PR_ENABLED` | `orchestrator-cli` | Enable automatic PR creation | `false` | `delete` — set but never read; value lives in `DaemonRunArgs.scheduler.auto_pr` |
| `AO_AUTO_COMMIT_BEFORE_MERGE` | `orchestrator-cli` | Auto-commit before merge | `false` | `delete` — set but never read; value lives in `DaemonRunArgs.scheduler.auto_commit_before_merge` |
| `AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE` | `orchestrator-cli` | Auto-prune worktrees after merge | `false` | `delete` — set but never read; value lives in `DaemonRunArgs.scheduler.auto_prune_worktrees_after_merge` |

---

## Tool-Specific Configuration

### Global (all CLI tools)

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_AI_CLI_EXTRA_ARGS_JSON` | `workflow-runner-v2` | JSON array of extra args appended to every AI CLI invocation | None | `workflow-yaml` — `phase.runtime.extra_args` in `WorkflowPhaseRuntimeSettings` |
| `AO_AI_CLI_EXTRA_ARGS` | `workflow-runner-v2` | Space-delimited extra args; fallback when `_JSON` variant is unset | None | `workflow-yaml` — same as above |

### Claude

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_CLAUDE_BYPASS_PERMISSIONS` | `workflow-runner-v2` | Pass `--dangerously-skip-permissions` to claude CLI | `true` (enabled when unset) | `daemon-config` — agent profile capability flag, e.g., `capabilities.bypass_permissions` |
| `AO_CLAUDE_EXTRA_ARGS_JSON` | `workflow-runner-v2` | JSON array of extra args for claude invocations | None | `workflow-yaml` — `phase.runtime.extra_args` |
| `AO_CLAUDE_EXTRA_ARGS` | `workflow-runner-v2` | Fallback space-delimited extra args for claude | None | `workflow-yaml` — same as above |

### Codex (additional flags)

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_CODEX_REASONING_EFFORT` | `orchestrator-cli` (`shared/runner.rs:480`) | Codex reasoning effort level (`low`/`medium`/`high`) injected via CLI arg | None | `workflow-yaml` — `phase.runtime.reasoning_effort` or agent profile |

### Codex

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_CODEX_WEB_SEARCH` | `workflow-runner-v2` | Enable web search for Codex CLI | `true` | `workflow-yaml` — `phase.runtime.web_search` in `WorkflowPhaseRuntimeSettings` |
| `AO_CODEX_NETWORK_ACCESS` | `workflow-runner-v2` | Enable network access for Codex CLI | `true` | `workflow-yaml` — `phase.runtime.network_access` in `WorkflowPhaseRuntimeSettings` |
| `AO_CODEX_EXTRA_ARGS_JSON` | `workflow-runner-v2` | JSON array of extra args for codex invocations | None | `workflow-yaml` — `phase.runtime.extra_args` |
| `AO_CODEX_EXTRA_ARGS` | `workflow-runner-v2` | Fallback space-delimited extra args for codex | None | `workflow-yaml` — same as above |
| `AO_CODEX_EXTRA_CONFIG_OVERRIDES_JSON` | `workflow-runner-v2` | JSON array of `key=value` codex config overrides | None | `workflow-yaml` — `phase.runtime.codex_config_overrides` in `WorkflowPhaseRuntimeSettings` |
| `AO_CODEX_EXTRA_CONFIG_OVERRIDES` | `workflow-runner-v2` | Fallback space-delimited codex config overrides | None | `workflow-yaml` — same as above |

### Gemini

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_GEMINI_EXTRA_ARGS_JSON` | `workflow-runner-v2` | JSON array of extra args for gemini invocations | None | `workflow-yaml` — `phase.runtime.extra_args` |
| `AO_GEMINI_EXTRA_ARGS` | `workflow-runner-v2` | Fallback space-delimited extra args for gemini | None | `workflow-yaml` — same as above |

### OpenCode

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_OPENCODE_EXTRA_ARGS_JSON` | `workflow-runner-v2` | JSON array of extra args for opencode invocations | None | `workflow-yaml` — `phase.runtime.extra_args` |
| `AO_OPENCODE_EXTRA_ARGS` | `workflow-runner-v2` | Fallback space-delimited extra args for opencode | None | `workflow-yaml` — same as above |

---

## Feature Flags

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_ALLOW_NON_EDITING_PHASE_TOOL` | `workflow-runner-v2` | When `true`, disables write-capability enforcement — allows gemini/non-editing tools on all phase types without redirecting to claude fallback | `false` | `daemon-config` — agent profile capability or phase target policy field |
| `AO_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE` | `orchestrator-git-ops` | When `false`, disables automatic runner rebuild after main-branch update | `true` | `daemon-config` — daemon config JSON field |
| `AO_MCP_SCHEMA_DRAFT` | `agent-runner` | Set to `07`/`draft07`/`draft-07`/`draft_07` to use JSON Schema Draft-07 for MCP tool input schemas | Standard (not draft07) | `daemon-config` — MCP server config field in `workflow-config.v2.json` |

---

## MCP & Agent Process

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_MCP_ENDPOINT` | `agent-runner` | MCP server endpoint URL injected into agent subprocess environment | None | `internal` — injected by supervisor per-agent; not user-facing |

---

## User Identity

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_ASSIGNEE_USER_ID` | `orchestrator-cli` | Task assignee user ID (highest priority, overrides `AO_USER_ID`) | None | `cli-flag` — `ao task create --assignee <id>` |
| `AO_USER_ID` | `orchestrator-cli` | Current user ID for task operations (fallback when `AO_ASSIGNEE_USER_ID` is unset) | None | `cli-flag` — global `--user-id` flag or user config file |

---

## Notifications

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `AO_NOTIFY_WEBHOOK_URL` | `orchestrator-notifications` | Webhook endpoint URL for task/workflow event notifications | None (required if notifications enabled) | `runtime-contract` — keep; secret value inappropriate for config files |
| `AO_NOTIFY_BEARER_TOKEN` | `orchestrator-notifications` | Bearer token for webhook authorization header | None (optional) | `runtime-contract` — keep; secret value inappropriate for config files |
| `AO_NOTIFY_MISSING_URL` | `orchestrator-cli` | Internal sentinel var used to test missing-URL error path in notification tests | None | `test-only` / `internal` |

---

## System & Platform

| Env Var | Crate(s) | Controls | Default | Migration Target |
|---|---|---|---|---|
| `PATH` | `orchestrator-core`, `agent-runner`, `orchestrator-cli`, `orchestrator-daemon-runtime` | System executable search path for binary lookup | OS-provided | `system` |
| `CLAUDECODE` | `workflow-runner-v2` | Indicates process is running inside Claude Code; `claude` CLI refuses to start when set | None | `system` — unset before daemon start; document workaround in ops guide |

---

## Dynamic Provider Credentials

These are user-configured strings, not static variable names. The env var name is read from provider config, not hardcoded.

| Pattern | Crate(s) | Controls | Migration Target |
|---|---|---|---|
| `<jira.api_token_env>` | `orchestrator-providers` | Jira API token; env var name set in provider config field `api_token_env` | `runtime-contract` — keep; secret value |
| `<gitlab.token_env>` | `orchestrator-providers` | GitLab personal access token; env var name set in provider config field `token_env` | `runtime-contract` — keep; secret value |
| `<linear.api_key_env>` | `orchestrator-providers` | Linear API key; env var name set in provider config field `api_key_env` | `runtime-contract` — keep; secret value |

---

## Legacy / Deprecated

| Env Var | Crate(s) | Controls | Status | Migration Target |
|---|---|---|---|---|
| `AGENT_ORCHESTRATOR_CONFIG_DIR` | `orchestrator-core`, `orchestrator-cli` | Legacy config directory (older name for `AO_CONFIG_DIR`) | Deprecated — actively cleared via `EnvVarGuard::set(None)` before most IPC calls | `delete` — remove reads; callers already clear it |
| `AGENT_RUNNER_TOKEN` | `orchestrator-core`, `orchestrator-cli` | Legacy IPC auth token between CLI and runner | Removed from protocol; only save/restore dead code remains in `runner_helpers.rs` | `delete` — remove all `env::var("AGENT_RUNNER_TOKEN")` and `set_var`/`remove_var` calls |

---

## Test-Only Variables

| Env Var | Crate(s) | Controls | Migration Target |
|---|---|---|---|
| `AO_E2E_SESSION_CONTINUATION` | `orchestrator-cli` (tests) | Gate for enabling the session continuation E2E test | `test-only` — keep in test scaffolding |
| `AO_E2E_PROJECT_ROOT` | `orchestrator-cli` (tests) | Project root override for E2E tests | `test-only` |
| `AO_E2E_TOOLS` | `orchestrator-cli` (tests) | Comma-separated tool list to exercise in E2E tests (default: `claude`) | `test-only` |
| `AO_E2E_TIMEOUT` | `orchestrator-cli` (tests) | Agent timeout in seconds for E2E tests (default: `120`) | `test-only` |
| `AO_TEST_ARGS_CAPTURE` | `agent-runner` (`session_process.rs:425`) | Path to file where test spy captures CLI tool args before invocation | `test-only` |
| `AO_TEST_ENV_CAPTURE` | `agent-runner` (`session_process.rs:425`) | Path to file where test spy captures env vars passed to AI CLI | `test-only` |

---

## Allowed-Through Env Vars (Agent Sandbox)

The agent sandbox in `agent-runner/src/sandbox/env_sanitizer.rs` allows these additional variables to pass through to agent subprocesses:

**Explicit allow-list:** `PATH`, `HOME`, `USER`, `SHELL`, `LANG`, `LC_ALL`, `TMPDIR`, `TERM`, `COLORTERM`, `SSH_AUTH_SOCK`, `CLAUDE_CODE_SETTINGS_PATH`, `CLAUDE_API_KEY`, `CLAUDE_CODE_DIR`

**Prefix wildcards:** `AO_*`, `XDG_*`

**Note:** API keys (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`) are **not** in the explicit allow-list and must be present under an `AO_*`-prefixed alias or via provider config `*_env` fields.

---

## Migration Summary

| Target | Count | Variables |
|---|---|---|
| `delete` | 8 | `AO_AUTO_MERGE_ENABLED`, `AO_AUTO_PR_ENABLED`, `AO_AUTO_COMMIT_BEFORE_MERGE`, `AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE`, `AO_PHASE_TIMEOUT_SECS`, `AO_MAX_AGENTS`, `AGENT_RUNNER_TOKEN`, `AGENT_ORCHESTRATOR_CONFIG_DIR` |
| `workflow-yaml` | 10 | `AO_PHASE_RUN_ATTEMPTS`, `AO_PHASE_MAX_CONTINUATIONS`, `AO_AI_CLI_EXTRA_ARGS*`, `AO_CLAUDE_EXTRA_ARGS*`, `AO_CODEX_WEB_SEARCH`, `AO_CODEX_NETWORK_ACCESS`, `AO_CODEX_EXTRA_ARGS*`, `AO_CODEX_EXTRA_CONFIG_OVERRIDES*`, `AO_GEMINI_EXTRA_ARGS*`, `AO_OPENCODE_EXTRA_ARGS*` |
| `daemon-config` | 4 | `AO_CLAUDE_BYPASS_PERMISSIONS`, `AO_ALLOW_NON_EDITING_PHASE_TOOL`, `AO_AUTO_REBUILD_RUNNER_ON_MAIN_UPDATE`, `AO_MCP_SCHEMA_DRAFT` |
| `cli-flag` | 3 | `AO_ASSIGNEE_USER_ID`, `AO_USER_ID`, `AO_CODEX_REASONING_EFFORT` |
| `runtime-contract` | 5 | `AO_CONFIG_DIR`, `AO_RUNNER_CONFIG_DIR`, `AO_NOTIFY_WEBHOOK_URL`, `AO_NOTIFY_BEARER_TOKEN`, provider credential vars |
| `internal` | 3 | `AO_RUNNER_SCOPE`, `AO_MCP_ENDPOINT`, `AO_RUNNER_BUILD_ID` |
| `test-only` | 7 | `AO_SKIP_RUNNER_START`, `AO_E2E_*` (4 vars), `AO_TEST_ARGS_CAPTURE`, `AO_TEST_ENV_CAPTURE`, `AO_NOTIFY_MISSING_URL` |
| `system` | 4 | `HOME`, `PATH`, `PATHEXT`, `CLAUDECODE`, `USER`/`USERNAME` |

See also: [Configuration Reference](configuration.md), [Workflow YAML Schema](workflow-yaml.md).
