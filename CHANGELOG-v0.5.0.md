# Changelog - v0.5.0

## Release Date
2026-06-02

## Overview

v0.5.0 ships the **kernel + flavors** architecture: a Rust daemon kernel plus a curated bundle of out-of-tree plugins that own workflow execution, queues, durable steps, and memory. The daemon's role narrows to scheduling + plugin orchestration; everything else is plugin-replaceable.

This release is the culmination of 5 fold-in rounds, **120 codex review passes**, and a substantial cargo-graph reshuffle. The architectural surface area is materially smaller and the plugin ecosystem is materially larger.

---

## Architecture: kernel + flavors

The daemon is the kernel. Plugins implement four new roles at the protocol level:

- **`workflow_runner`** — runs workflow phases ([animus-workflow-runner-default](https://github.com/launchapp-dev/animus-workflow-runner-default) is the reference)
- **`queue`** — durable subject dispatch queue ([animus-queue-default](https://github.com/launchapp-dev/animus-queue-default))
- **`durable_store`** — Postgres-backed step fence ([animus-step-durable-dbos](https://github.com/launchapp-dev/animus-step-durable-dbos))
- **`memory_store`** — semantic memory with graph scopes ([animus-memory-zep](https://github.com/launchapp-dev/animus-memory-zep))

The daemon refuses to start when any required role is unsatisfied. `animus daemon preflight` reports missing roles with actionable `animus plugin install ...` commands. `animus daemon start --auto-install` installs recommended defaults on the fly.

See [`docs/architecture/kernel-and-flavors.md`](docs/architecture/kernel-and-flavors.md) for the product/discipline architecture.

---

## ✨ Features

### Four new plugin kinds at the protocol level (animus-protocol v0.5.1)

- New `animus-workflow-runner-protocol` with `workflow/execute`, `workflow/run_phase` (now carries `mcp_config`), `workflow/events` subscription, `paused`/`pending` wire vocabulary.
- New `animus-queue-protocol` with atomic `queue/lease` + `queue/release_pending(entry_id, reason)` (Assigned → Pending transition that returns work to the queue without canceling it).
- New `animus-durable-store-protocol` with `step/begin` + `step/complete` + `step/abort` + diagnostics; PRIOR_ERROR is terminal for idempotency_key across plugin restarts.
- New `animus-memory-store-protocol` with semantic `memory/put` / `memory/get` / `memory/search` over graph-scoped storage.
- Extended `animus-plugin-protocol` with `init_extensions.project_binding` + `memory_mcp_stdio_command` so plugins can reach the daemon's MCP sidecar.

### Generic plugin authoring shell (animus-plugin-runtime v0.2.0)

- New `Plugin::new()` builder + `register_method!` macro that any plugin kind can use to skip rolling its own stdio dispatch loop. Notifier handle for fan-out; `MethodContext` with `keep_cancellation()` for streaming subscriptions; multi-line frame parsing; protocol-major compatibility check; shutdown drains in-flight requests.

### Workflow execution

- **Daemon-restart-survivable agent reattach.** The runner subprocess now binds a Unix socket as a server; the daemon connects as a client and can reconnect after restart. `process_group(0)` isolates the runner from daemon SIGTERM. Stalled-reader 100ms write timeout prevents the runner from blocking on dead daemons. Multi-daemon simultaneous attach via broadcaster.
- **LLM-decision recording at the agent-runner level.** Every prompt, response chunk, tool call, and tool result is captured to `~/.animus/<scope>/runs/<run_id>/decisions.jsonl`. Three durability modes (`FlushOnly`, `FsyncPerEvent`, `FsyncEveryN(8)` default). Replay-from-log mode (`ANIMUS_REPLAY_SESSION` env var) for tests/dev determinism — tool-result replay prevents re-execution of side-effecting tools.
- **Daemon startup orphan-scan.** Detects in-flight agents from prior daemon runs via per-agent JSON records. PID-identity verification via `ps -p` matches recorded binary basename, so PID reuse doesn't produce false orphans.
- **Daemon-side reattach gap reconstruction.** Race-safe tail reader replays decisions.jsonl events emitted during the daemon gap to subscribers.
- **Workflow-runner v0.4.1** consumes `mcp_config` in `workflow/run_phase`; emits `paused` / `pending` wire vocabulary.

### Subject backends

- All subject operations route through `SubjectRouter` to installed `subject_backend` plugins. The in-tree `InTreeTaskSubjectBackend` / `InTreeRequirementsSubjectBackend` adapters and their kill-switch env vars (`ANIMUS_DAEMON_DISABLE_BUILTIN_TASK_ADAPTER`, etc.) are gone. Install `launchapp-dev/animus-subject-default` (kind=task) and `animus-subject-requirements` (kind=requirement).

### Daemon control surface

- Queue verbs over the daemon's control socket (CLI → daemon over Unix socket → queue plugin RPC) now forward to the queue plugin instead of returning `NotSupported`. Anchored reorder (Front/Back/Before/After), idempotent enqueue, NotFound/InvalidRequest/Unavailable error mapping.

### Configuration

- Workflow YAML supports `${VAR}` env-var interpolation for non-secret config (URLs, team IDs, feature flags), with `${VAR:-default}` and `${VAR:?error}` fallback shapes; substitution happens before YAML parsing.

### Plugin kill-switches

- `ANIMUS_DAEMON_DISABLE_TRIGGERS=1` — skip trigger plugin supervisor (interrupts in-progress restart backoff)
- `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1` — skip subject plugin discovery
- `ANIMUS_PROVIDER_DISABLE_PLUGIN` removed — there is no in-tree provider fallback anymore

---

## 🧹 Cleanup

### Workflow runtime deduplicated

- New crate [`launchapp-dev/animus-runtime-shared`](https://github.com/launchapp-dev/animus-runtime-shared) v0.1.1 — shared workflow runtime modules consumed by both the daemon and the workflow_runner plugin. Single source of truth; the duplicated ~2K LOC between in-tree and the plugin is gone.
- `crates/workflow-runner-v2/` **deleted entirely** (~14k LOC). 21 ao-cli consumer files migrated to `animus_runtime_shared::*`.
- Plugin v0.4.1 consumes the published shared crate via git tag; no path-dep release blocker.

### In-tree fallbacks removed

- Daemon-side workflow runner + queue fallback paths surgically removed (~1055 LOC). Plugins are hard-required by preflight; the daemon refuses to start without them.
- Subject type duplicate removed; in-tree `animus-subject-protocol` mirror crate deleted; CI protocol-drift gate retired.

### Kind-based plugin discovery

- `orchestrator_plugin_host::discover_by_kind(project_root, kind)` filters installed plugins by manifest `plugin_kind`. The daemon's workflow runner resolver does kind-based discovery first, falling back to hard-coded binary names for legacy installs.

---

## 🔧 Hardening

### Durable store (animus-step-durable-dbos v0.2.0)

- **Cross-IPC idempotency:** NFC normalization + control-char rejection + line-separator-before-trim + 512-byte cap. Legacy-spelling lookup bridge with `array_position` sort prevents stale-row shadowing.
- **Schema sync:** `_animus_durable_store_schema` table records expected version; on-startup validation; forward canonicalization migration. `durable/diagnostics` JSON-RPC method exposes drift + row counts.
- **Reservation expiry:** 30s sweeper reclaims orphaned reservations; late commits hard-fail with `RESERVATION_EXPIRED`.
- **PRIOR_ERROR is terminal** for idempotency_key across plugin restarts; tested with 5-way concurrent replay race.

### Queue (animus-queue-default v0.2.0)

- Atomic `queue/lease` path adopted by the daemon. `queue/release_pending(entry_id, reason)` returns Assigned → Pending without canceling work.

### Memory (animus-memory-zep)

- Exhaustive `memory/get` via `graph.episode.getByGraphId` (no longer bounded-page search-based; misses under heavy-episode scopes are fixed).
- Metadata-round-trip `scopeFromGraphId` with filter-leak guard.

---

## 📊 Cumulative numbers

- **120 codex review passes** across the v0.5 arc: 6 protocol-design + 38 Wave 1–4 implementation + 76 across 5 P2 fold-in rounds. Every released artifact ended at 0 P1.
- **18 standalone plugin repos** under [`launchapp-dev`](https://github.com/launchapp-dev) — protocol, four providers, five subject backends, two transports, web UI, two triggers, log storage, conformance testkit, release automation, plugin template, shared runtime, and four reference plugins for the new plugin kinds.
- **443 plugin-side tests** across the seven v0.5 artifacts (queue, Zep, DBOS, workflow_runner, runtime-shared, plugin-runtime, animus-protocol workspace).
- **1880 ao-cli tests passing**, 6 pre-existing testkit-binary failures (unrelated).

---

## 📦 Workspace inventory

The ao-cli workspace shrinks from 18 → 17 members in v0.5 (`workflow-runner-v2` deleted, `animus-runtime-shared` added):

`agent-runner`, `animus-plugin-protocol`, `animus-plugin-runtime`, `animus-runtime-shared`, `oai-runner`, `orchestrator-cli`, `orchestrator-config`, `orchestrator-core`, `orchestrator-daemon-runtime`, `orchestrator-git-ops`, `orchestrator-logging`, `orchestrator-notifications`, `orchestrator-plugin-host`, `orchestrator-providers`, `orchestrator-session-host`, `orchestrator-store`, `protocol`.

---

## 🚧 v0.6 roadmap (deferred from v0.5)

- Real `DurableStoreClient` wiring against the DBOS plugin RPC (v0.5 shipped the trait + `MockDurableStore`).
- Daemon-side per-agent `decisions.jsonl` auto-discovery on reattach (v0.5 shipped the explicit-path primitive).
- Out-of-boundary tool side-effect re-assertion on replay.
- Long-term decision-log compaction (zstd compression + 7-day expiry of completed logs).
- Queue protocol — Assigned head-of-line blocking under FIFO + low headroom.
- `config_context::RuntimeConfigContext::load` sparse-override gap.
- `runtime_contract::current_ao_command` sibling-binary lookup for standalone plugin installs.
- Re-architect `approve_manual_phase_continues_non_terminal_workflow` against the plugin path.
- Subject `SubjectDispatchExt` cleanup.
- `workflow/events/poll` real-time streaming (protocol spec deferral).
- Full `setsid` hardening (currently blocked by workspace `deny(unsafe_code)` lint).
- Windows reattach via named pipes (`cfg(unix)` only today).

---

## ⚠️ Breaking changes

- **`workflow_runner` and `queue` plugins are now required.** The daemon refuses to start without them. Run `animus plugin install-defaults` ahead of upgrade, or pass `animus daemon start --auto-install`.
- **In-tree `animus-workflow-runner` binary is deleted.** The release archive no longer ships it. The daemon now spawns `animus-workflow-runner-default` (the plugin binary). Upgrade installer preserves the legacy binary if a v0.4.x archive shipped it, but new installs go straight to the plugin.
- **`workflow-runner-v2` crate is deleted.** Out-of-tree consumers of its public modules should migrate to [`launchapp-dev/animus-runtime-shared`](https://github.com/launchapp-dev/animus-runtime-shared) which holds the same surface.
- **Daemon control socket `queue/*`** is now routed through the queue plugin instead of in-tree handlers. Behavior is the same; some error-envelope shapes differ slightly.

---

## Upgrade path

```bash
animus daemon stop                                                     # graceful shutdown
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus plugin install-defaults --include-subjects --include-transports # add workflow_runner + queue if missing
animus daemon preflight                                                # verify all required roles present
animus daemon start --autonomous
```
