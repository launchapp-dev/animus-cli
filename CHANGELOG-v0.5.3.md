# Changelog - v0.5.3

## Release Date
2026-06-03

## Overview

v0.5.3 is a pure surface-shrink release. **Workspace shrinks from 17 → 12 crates** across v0.5.2 + v0.5.3 (the in-tree `oai-runner` extraction lived in v0.5.2; the `orchestrator-git-ops` delete + three folds land in v0.5.3 as the cleanup wave). The kernel is now mostly **CLI + daemon + plugin-host + core + config + a small support tail**.

Per Sami's coding philosophy ("clean smaller surfaces are easier and smaller surfaces that can be made bulletproof or hot swapped"), every change here is **net-shrink, no behavior change**.

---

## Surface-shrinks landed

### Delete `orchestrator-git-ops` (1472 LOC)
The crate had **zero Rust consumers** — only its own `Cargo.toml` referenced it. The docs claimed it "owns git automation helpers" but no code imported it; actual worktree management lives in `animus-workflow-runner-default` (out-of-tree plugin) and the daemon dispatch path. Pure delete.

### Extract in-tree `oai-runner` (5483 LOC)
`crates/oai-runner/` was the standalone OpenAI-compatible agentic runner binary. `launchapp-dev/animus-provider-oai-agent` v0.1.3 already ships the same code wrapped in the stdio plugin protocol, plus a separate `animus-oai-runner` release archive. The in-tree crate became redundant when the plugin was published.

- Pinned `animus-provider-oai-agent` v0.1.3 in `default-install.json` + `plugin_registry::DEFAULT_OAI_AGENT_PLUGINS` (constant already existed).
- New `resolve_oai_runner_binary()` helper in `orchestrator-core::runtime_contract`: searches `$ANIMUS_OAI_RUNNER_BIN` → `~/.animus/plugins/animus-provider-oai-agent/bin/animus-oai-runner` → legacy flat path → `$PATH`.
- `agent-runtime-config.v2.json` cleared the hardcoded `"executable": "animus-oai-runner"` so the resolver kicks in.
- Stripped `animus-oai-runner` from `.cargo/config.toml`, `Dockerfile`, `.github/workflows/release.yml`, `scripts/install.sh` (legacy archive support kept for upgraders).

**Multi-binary plugin install (closes the v0.5.2 OAI gap):** `animus plugin install` now follows a `[[binaries]]` array in the plugin's `plugin.toml` to install secondary binaries from sibling release archives. `animus-provider-oai-agent` v0.1.4 declares both `animus-provider-oai-agent` (primary, the stdio shell) and `animus-oai-runner` (the agent loop driver); a single `animus plugin install launchapp-dev/animus-provider-oai-agent` now lands both binaries in `~/.animus/plugins/`. Backward compat: plugins without the `[[binaries]]` section keep the legacy single-binary install behavior. Uninstall removes all declared binaries together; `plugins.yaml` records the full set via a new `binaries: [...]` field on the registry entry.

### Fold `orchestrator-providers` → `orchestrator-core::subject_adapter` (2006 LOC)
The crate was misnamed (not "providers" — provider plugins live out-of-tree). 73% of it was `subject_adapter.rs` (subject backend routing trait + impl); 11% was the git project adapter; 9% was the builtin project adapter. Only `orchestrator-core` depended on it. Folded as a sub-module:

- `subject_adapter.rs` → `crates/orchestrator-core/src/subject_adapter/adapter.rs`
- `git.rs` → `crates/orchestrator-core/src/subject_adapter/git.rs`
- `builtin.rs` → `crates/orchestrator-core/src/subject_adapter/builtin.rs`
- `lib.rs` → `crates/orchestrator-core/src/subject_adapter/mod.rs`

2 callers updated: `orchestrator-core` itself + `animus-runtime-shared::ensure_execution_cwd`. **Known v0.6+ follow-up:** `subject_adapter::TaskServiceApi` and `services::TaskServiceApi` now coexist in `orchestrator-core` (identically named); disambiguation via `as Provider...` aliases preserved. Worth a rename pass.

### Fold `orchestrator-store` → `orchestrator-core::store` (180 LOC)
Trivial — 180 LOC of persistence primitives, smaller than most files in the kernel. Folded into `orchestrator-core/src/store/mod.rs`. `tempfile` dep promoted from dev-only to prod in `orchestrator-core` (the store helpers use it in production paths). 2 callers updated: `orchestrator-core` + `animus-runtime-shared`.

### Fold `orchestrator-session-host` → `orchestrator-plugin-host::session` (2406 LOC)
The crate's own lib.rs docstring said _"Host-side glue that wires the upstream SessionBackend trait to the Animus STDIO plugin protocol"_ — literally plugin-host glue. All 5 source files (incl. `plugin_supervisor.rs` and `session_backend_resolver.rs`) folded under `crates/orchestrator-plugin-host/src/session/`.

- 6 import sites updated across `agent-runner`, `orchestrator-cli`, `orchestrator-daemon-runtime`.
- `animus-session-backend`, `orchestrator-logging`, `async-trait`, `uuid` (+ `rt-multi-thread` tokio feature) absorbed into plugin-host's deps.
- Re-exports inside `session/mod.rs` preserve the old top-level surface; consumers use `orchestrator_plugin_host::session::...` (no top-level plugin-host re-export — keeps the namespace boundary visible).

---

## Codex round 1 P2 fix (main loop)

- **`docs/architecture/crate-map.md`** claimed "13 crates" and still listed `orchestrator-session-host` as a current crate. **Fixed:** "12 crates" + removed the stale row; consolidated the description into the `orchestrator-plugin-host` row.

Codex round 1: 0 P1, 1 P2 (fixed inline).

---

## Numbers

- **Workspace members:** v0.5.0: 17 → v0.5.1: 17 → v0.5.2: 15 → **v0.5.3: 12**.
- **Net LOC delta on main vs v0.5.2:** ~−1700 (the folds are roughly LOC-neutral as moves; the wins come from deleted Cargo.toml + README files and tightened doc tables).
- **ao-cli tests:** **1774 passing** (down from 1917 at v0.5.1; the delta reflects the deleted in-tree oai-runner tests + the deleted store + providers test modules). 6 pre-existing testkit-binary failures unchanged. 11 ignored.
- **Codex rounds on main merge:** 1 → 0 P1, 1 P2 (fixed inline).

---

## Architectural shape after v0.5.3

The remaining 12 crates, grouped:

**Core (5):** `orchestrator-cli`, `orchestrator-core` (with the folded `subject_adapter` + `store` modules), `orchestrator-config`, `orchestrator-daemon-runtime`, `agent-runner`

**Plugin foundation (4):** `orchestrator-plugin-host` (with the folded `session` module), `animus-plugin-protocol`, `animus-plugin-runtime`, `animus-runtime-shared`

**Support (3):** `orchestrator-notifications`, `orchestrator-logging`, `protocol`

That's the "glue" shape Sami's coding philosophy was after — the kernel is CLI + daemon + plugin host + core. Everything else is a plugin (out-of-tree) or shared infrastructure consumed by both kernel and plugins.

---

## 🚧 v0.6 carry-forwards from this release

- **Multi-binary plugin install.** Today `animus plugin install` is single-binary; the OAI agent plugin ships two binaries (`animus-provider-oai-agent` + `animus-oai-runner`). v0.6 should add multi-binary install support OR `animus-oai-runner` should be published as its own repo.
- **Subject adapter trait rename.** `subject_adapter::TaskServiceApi` / `services::TaskServiceApi` collide in `orchestrator-core` post-fold. The `as Provider...` aliases work today; a clean rename would close the disambiguation surface.
- **Standalone `launchapp-dev/animus-runtime-shared`** repo needs Cargo.toml updated when this ships: drop the `orchestrator-store` git dep — `fsync_rename` now lives under `orchestrator_core::store::fsync_rename`.
- **`orchestrator-notifications` extraction** (1.5k LOC) — could be a `notifier` plugin (the symmetric inverse of triggers). Not done in v0.5.3.
- **`agent-runner` extraction** to a runner plugin (10.5k LOC) — biggest single-crate move; would mirror the `workflow-runner-v2` → `animus-workflow-runner-default` swap in v0.5.1. Architectural cleanup; deferred.

---

## Upgrade

```bash
animus daemon stop
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus plugin install-defaults --include-subjects --include-transports
# OAI users: also install the agent plugin
animus plugin install launchapp-dev/animus-provider-oai-agent
animus daemon preflight
animus daemon start --autonomous
```
