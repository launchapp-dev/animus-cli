# Release v0.0.18

**Release Date:** 2026-03-21  
**Commits:** 841 changes since v0.0.17

---

## Features

### Agent Orchestration
- **Two-stage dispatch:** Work-planner → triage → implementation pipeline for better task routing
- **Real-time workflow progress:** Add workflow_events subscription for real-time phase progress in WorkflowDetailPage
- **Agent profile management:** Add save_agent_profile mutation with editable AgentProfilesPage form
- **Session validation:** Validate session ID before dispatching resume to CLI backend

### Cost Management
- **Per-provider cost calculation:** oai-runner now tracks per-provider costs with real-time cost reporting

### Developer Experience
- **Orphan cleanup tool:** New `ao.runner.orphans-cleanup` MCP tool for cleaning up orphaned runner processes
- **Multi-owner agent team:** 6 POs, 2 architects, 2 researchers, master reviewer for comprehensive coverage
- **Rustfmt CI expansion:** Expanded cargo-check matrix to all 16 workspace crates

### Model Routing
- **AI-driven release decisions:** Evaluate commit significance before cutting releases
- **Work-planner release triggers:** Automatic releases when 10+ PRs merged since last tag
- **DeepSeek routing:** Fix DeepSeek model routing to route deepseek/* models to oai-runner

---

## Fixes

### Process Management
- **Runner connection failures:** Fix pool_size mismatch causing TASK-683 to persist
- **Agent-runner process leaks:** Multiple fixes resolving leaks from 36, 65, 66, and 139 processes
- **Stale lock race:** Fix stale lock race in agent-runner
- **Session backend PIDs:** Track native session backend PIDs in orphan tracker

### Test Infrastructure
- **10 recurring failing tests:** Fix in daemon_run, runtime_project_task, shared::runner, shared::parsing
- **daemon_run tests:** Fix 3 failing tests in daemon_run integration

### Workflow & Routing
- **Task-reconciler:** Auto-unblock tasks blocked by transient workflow runner failures
- **Auto-detect and re-route:** Automatically detect and re-route failing model pipelines
- **Failover chain:** Treat authentication_error as ProviderExhaustion to trigger failover

### Documentation
- **Documentation fixes:** Multiple corrections to agents.md, mcp-tools.md, cli/index.md, README.md

### CI/CD
- **Rustfmt formatting:** Multiple fixes to unblock CI on release branches
- **Rustfmt continue-on-error:** Proper error handling in CI pipelines
- **Codex model ID:** Fixed gpt-5.4 not gpt-5.4-codex
- **Work-planner MCP crash:** Explicitly set mcp_servers to ao-only for planner and reconciler

### Installation
- **macOS Sequoia:** Fix ad-hoc codesign in install script
- **Cross-platform version detection:** Use awk for better compatibility
- **Trap cleanup:** Fix install script trap cleanup

---

## Improvements

### Binary Optimization
- **Release binary size:** Optimized from 52MB → 16MB with strip, LTO, codegen-units=1, opt-level=z
- **Web UI build size:** Cleaned stale builds, reduced from 18MB → 1.7MB embedded
- **Bundled packs:** Embed bundled packs in binary — fixed hardcoded CARGO_MANIFEST_DIR path

### Model Upgrades
- **MiniMax upgrade:** Upgraded to M2.7 — near-Opus quality at $0.30/$1.20 per M tokens
- **Codex GPT-5.4:** Use Codex GPT-5.4 for PR review, code review, reconciler, workflow optimizer
- **Kimi K2.5:** Promoted to medium-complexity tasks — near-Opus coding performance

### Routing Strategy
- **Balance routing:** features→Sonnet, bugfix/refactor→Codex, UI→Gemini
- **Rate limit management:** Route most tasks to Codex GPT-5.4 — 2x rate limits until April 2nd
- **Disabled routes:** Remove standard-glm/quick-fix-glm and standard-kimi — 0% success rate

### Daemon Stability
- **Log spam reduction:** Suppress daemon tick escalation log spam for recurring reset messages
- **Stale web UI builds:** Set emptyOutDir=true for cleaner builds
- **.mcp.json path:** Switch from debug to release binary path

### Release Pipeline
- **Release decision logic:** Consider requirements, task stats, open PRs, and regressions
- **Version enforcement:** Bump version to 0.0.11 and enforce version in release pipeline
- **Cross-publish:** Add install script and cross-publish releases to public launchapp-dev/ao repo

---

## Dependencies

- Updated Cargo.lock for v0.0.11, v0.0.12
- Multiple dependency updates via standard updates

---

## Contributors

This release includes contributions from both human developers and AI agents working together to ship a more stable, performant Agent Orchestrator.
