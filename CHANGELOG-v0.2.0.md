# Changelog - v0.2.0

## Release Date
2026-03-21

## Overview
Major release with 985 commits since v0.1.0. Includes two-stage dispatch architecture, oai-runner production hardening, multi-model routing with failover chains, daemon stability fixes, process leak fixes, web UI control center, and cross-platform install script.

---

## ✨ Features

### Two-Stage Dispatch
- **Work-planner triage pipeline**: New two-stage dispatch flow — work-planner triages tasks before routing to implementation phases.
- **AI-driven release decisions**: Release decisions now evaluate commit significance, requirements, task stats, open PRs, and regressions before cutting releases.

### Multi-Model Routing
- **Failover model chains**: Add fallback model chains for rate limit failover across all model phases.
- **Auto-detect structured output support**: Providers auto-negotiate between `json_schema` and `json_object` fallback.
- **Provider exhaustion failover**: Treat `authentication_error` as provider exhaustion to trigger failover chain.
- **Auto-detect and re-route failing pipelines**: Reconciler detects and re-routes failing model pipelines automatically.
- **Model routing updates**: Codex GPT-5.4, MiniMax M2.7, Gemini for UI/UX tasks, Kimi K2.5 for medium-complexity tasks.
- **Cost optimization**: Research phases route to `gemini-2.5-flash-lite` for reduced API costs.

### oai-runner v2
- **Production-grade structured output**: Async execution, context management, and robust structured output handling.
- **Enhanced API client**: Improved tool executor and file operations from daemon PRs.
- **Retry implementation**: Fixed loop bounds, guard unification, delay cleanup, and 5xx classification (TASK-1090).
- **Increased max turns**: Default `max_turns` raised from 50 to 200.

### Daemon & Runtime
- **Notification connector framework**: Retries and dead-lettering for notification delivery.
- **Task state-change events**: Emit daemon events and ingest notification delivery failures.
- **Auto-merge and auto-commit**: Configurable daemon behavior for merge and commit automation.
- **Workflow state persistence**: Handle skill command dispatch and persist daemon workflow state.
- **File locking**: Add file locking to `core-state.json` persistence.

### CLI & Developer Experience
- **Setup wizard**: Interactive setup wizard with `doctor --fix` remediation.
- **Skill lifecycle commands**: Deterministic skill lifecycle with lockfile resolution.
- **Install script**: Cross-platform install script with cross-publish to public `launchapp-dev/ao` repo.
- **Polished CLI help**: Standardized actionable validation errors and improved help text.
- **Confirmations and dry-run**: Hardened destructive command confirmations and dry-run behavior.
- **Version command**: Exposed `ao version` command.

### Web UI
- **Task/workflow control center**: Full task and workflow control center interface.
- **Planning workspace**: Vision and requirement CRUD/refine flows.
- **Telemetry pipeline**: Redaction policy, correlation IDs, and failed-action diagnostics panel.
- **Template picker**: Template picker for new workflow creation.
- **Accessibility**: Focus flow, responsive baselines, and route performance baselines.
- **High-risk action safeguards**: Confirmation, dry-run preview, and audit feedback for high-risk daemon actions.

### Architecture & Infrastructure
- **Architecture graph**: Architecture graph and task entity linkage.
- **Scoped runtime state**: Runs and logs moved to global scoped `~/.ao` paths.
- **Repo-scoped worktrees**: Self-hosting docs and repo-scoped worktree support.
- **Dependency policy guardrail**: Rust-only dependency policy guardrail test.
- **CI workflow**: GitHub release workflow for version tags/branches.
- **MCP servers**: Added context7, GitHub, sequential-thinking, and memory MCP servers.

---

## 🐛 Fixes

### Process Leaks
- **[CRITICAL] Agent-runner process leak** (TASK-1071): Fixed escalation from 134 to 139 orphaned processes — track native session backend PIDs in orphan tracker.
- **Zombie process reaping**: Reap zombies after kill in session backends (TASK-750).
- **Process leak reaping**: Daemon stability fixes for process leak reaping (v0.0.13).

### Daemon Stability
- **Pool semaphore enforcement**: Fixed broken utilization enforcement causing 233% utilization.
- **Task reconciler**: Auto-unblock tasks blocked by transient workflow runner failures.
- **Work-planner MCP crash**: Fixed crash by explicitly setting `mcp_servers` to ao-only for planner and reconciler.
- **Reconciler exit=1 handling**: Fixed Rust routing guard and reconciler exit=1 transient failure handling.

### Model & Routing
- **Model routing defaults**: Fixed model routing defaults, updated Codex and MiniMax model IDs.
- **Codex model ID**: Corrected to `gpt-5.4` (not `gpt-5.4-codex`).
- **Removed failing routes**: Removed standard-glm/quick-fix-glm (0% success rate).
- **Disabled standard-kimi**: Extended reconciler to re-route escalated oai-runner workflows.

### CLI & Runtime
- **Cleanup phase cwd**: Fixed cleanup phase missing `cwd_mode: task_root`.
- **Commit handling**: Skip commit gracefully when agent already committed or no changes needed.
- **IPC token auth**: Enforce agent-runner IPC token authentication and remove dev token fallback.
- **Env sanitizer allowlist**: Fixed incomplete env sanitizer allowlist in agent-runner.
- **Rollback validation**: Hardened rollback validation checkouts for arbitrary refs.
- **macOS Sequoia**: Ad-hoc codesign in install script for macOS Sequoia compatibility.

---

## 🔧 Improvements

- **Binary optimization**: Release binary reduced from 52MB to 16MB with strip, LTO, codegen-units=1, opt-level=z.
- **Embedded assets**: Clean stale web UI builds — 18MB to 1.7MB embedded.
- **Bundled packs**: Embed bundled packs in binary instead of hardcoded `CARGO_MANIFEST_DIR` path.
- **Agent profiles**: Backfill empty `system_prompt` in agent profiles before validation.
- **Orphan cleanup tool**: Added `ao.runner.orphans-cleanup` MCP tool.
- **Standardized task MCP inputs**: All task MCP input structs use `id` field name.
- **Multi-owner agent team**: 6 POs, 2 architects, 2 researchers, master reviewer.

---

## 📦 Breaking Changes

- Version bump from 0.0.13 to 0.2.0 reflects the scope of changes since v0.1.0.
- Runtime state now lives under `~/.ao/<repo-scope>/` — legacy readers preserved for compatibility.
- Removed Tauri references from code and docs.
- Default `AO_CLAUDE_BYPASS_PERMISSIONS` set to `false`.
