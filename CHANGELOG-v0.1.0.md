# Release v0.1.0

**Release Date:** 2026-03-21  
**Previous Version:** v0.0.18  
**Commits:** 841 since v0.0.18

---

## Features

### Core Architecture
- **Two-stage dispatch system:** Work-planner → triage → implementation pipeline
- **AI-driven release decisions:** Evaluate commit significance before cutting releases
- **Auto-release triggers:** Work-planner triggers releases when 10+ PRs merged since last tag

### Model & Provider Support
- **Groq integration:** Add Groq as OpenAI-compatible provider in oai-runner
- **IonRouter integration:** Add IonRouter as OpenAI-compatible provider
- **GPT-4.1 support:** Track GPT-4.1 and GPT-4.1 Nano as cost-efficient OpenAI model alternatives
- **Model upgrades:** Upgrade MiniMax to M2.7, Codex to GPT-5.4

### oai-runner v2
- Production-grade structured output with json_schema validation
- Async execution and context management
- Enhanced API client, tool executor, file operations
- Circuit breaker integration with fallback routing
- Per-token pricing data and cost-aware model routing
- Token tracking and cost reporting
- Increased default max_turns from 50 to 200

### Workflow & Agent Improvements
- **Session Resume:** Reliability and error recovery for interrupted workflows
- **Process Lifecycle:** Zombie reaping and signal handling for orphaned processes
- **Workflow events subscription:** Real-time phase progress on WorkflowDetailPage
- **Agent profiles:** Add save_agent_profile mutation with editable AgentProfilesPage form
- **GraphQL checkpoint detail:** Expose workflow checkpoint detail query
- **Saas-template workflow port:** Command phases, rework, rebase, sync capabilities

### Installation & Distribution
- **Install script:** Cross-platform installation with `curl -fsSL https://get.ao-0.1.0.launchapp.dev/ao.sh | sh`
- **Binary optimization:** 52MB → 16MB with strip, LTO, codegen-units=1, opt-level=z
- **Embedded web UI:** Clean stale builds, 18MB → 1.7MB embedded
- **Bundled packs:** Embed bundled packs in binary with fixed CARGO_MANIFEST_DIR path

---

## Fixes

### Critical Stability Fixes
- **Process leak resolution:** Multiple iterations fixing agent-runner process leak escalation (up to 139 processes)
- **Daemon stability:** Daemon stability fixes for production workloads
- **Failing tests:** Fixed 3+ failing daemon_run integration tests

### oai-runner Fixes
- Fix retry implementation: loop bounds, guard unification, delay cleanup, 5xx classification
- Fix model routing defaults
- Fix standard-minimax workflow failures (0% success rate)
- Standardize task MCP input structs to use `id` field name

### Work-planner Fixes
- Fix work-planner MCP crash: explicitly set mcp_servers to ao-only
- Fix routing guard and reconciler exit=1 transient failure handling
- Auto-detect and re-route failing model pipelines

### Install Script Fixes
- Fix macOS Sequoia: ad-hoc codesign compatibility
- Fix version detection with awk for cross-platform support
- Fix trap cleanup for proper signal handling

### Other Fixes
- Fix Codex model ID: gpt-5.4 not gpt-5.4-codex
- Fix 3 regression tests: HOME isolation, duplicate workflow, legacy parse order
- Fix cleanup phase missing cwd_mode: task_root
- Fix ops_queue test: remove duplicate workflow push

---

## Improvements

### Model Routing
- **Smart routing:** Balance features→Sonnet, bugfix/refactor→Codex, UI→Gemini
- **Fallback chains:** Automatic rate limit failover across all model phases
- **Structured output detection:** Auto-detect provider structured output support (json_schema vs json_object)

### Developer Experience
- **Phase timeout enforcement:** Add timeouts to prevent runaway phases
- **Phase output parsing:** Strict validation and provenance tracking
- **Circuit breaker jitter:** Improved retry backoff with typed error classification
- **Multi-owner team:** 6 POs, 2 architects, 2 researchers, master reviewer

### MCP Server Additions
- GitHub, sequential-thinking, memory MCP servers
- brave-search, sentry, filesystem MCP servers
- context7 for library docs, rust-docs for crate API lookup
- Add git and gh to tools_allowlist for command phases

### CI/CD
- **Cargo caching:** Add Cargo caching to Rust CI workflows
- **cargo-nextest:** Integrate for improved async test execution
- **Exit code handling:** Allow test/lint exit code 1 to trigger rework

### Documentation
- Update CLI reference with `queue` and `pack` commands
- Fix tool counts in agents.md (~68 → ~70)
- Fix mcp-tools.md Definition Tools count (3 → 5)
- Document ao.output.phase-outputs
- Document replace_linked_architecture_entities in ao.task.update

---

## Requirements Completed

- REQ-002: Track GPT-4.1 and GPT-4.1 Nano as cost-efficient OpenAI model alternatives
- REQ-003: Add `queue` command to CLI reference docs
- REQ-004: Add `pack` command to CLI reference docs
- REQ-005: Update CLI reference summary table count
- REQ-006: Fix Definition Tools count in mcp-tools.md header
- REQ-007: Fix ao.output.* count in agents.md overview table
- REQ-008: Fix ao.runner.* count in agents.md overview table
- REQ-009: Fix total tool count in agents.md header

---

## Breaking Changes

None - this is a backward-compatible minor release.

---

## Migration Guide

No migration required. Existing workflows continue to function without changes.
