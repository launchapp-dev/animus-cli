# Changelog - v0.5.1

## Release Date
2026-06-03

## Overview

v0.5.1 ships **8 surface-shrinking moves** that close every v0.6 item flagged during the v0.5 series and rationalize the surfaces v0.5 introduced. Four parallel sub-agents (ALPHA, BETA, GAMMA, DELTA) landed cleanly via worktree isolation; codex caught 2 P1s in the merged result that fold-in rounds had missed; both fixed. Net: ~10 days of deferred work shipped in one cycle, reviewer-clean.

This release follows the **"clean smaller surfaces"** coding philosophy: prefer surface-shrink over feature-grow, replace mocks with concrete impls, collapse dual implementations, delete dead paths.

---

## Architectural changes

### Recording layer — production-grade replay (ALPHA, items 1+4+6)

- **Real `DurableStoreClient`.** Replaced `MockDurableStore` with `DbosDurableStoreClient` — the production-facing impl that discovers the `durable_store` plugin via `orchestrator_plugin_host::discover_by_kind`, runs the standard initialize handshake with `project_binding`, and maps the fence trait to `durable/begin_workflow_run` / `durable/begin_step` / `durable/commit_step` / `durable/abandon_step` / `durable/query_run`. `MockDurableStore` relegated to `#[cfg(test)] mod test_doubles`. The load-bearing differentiation is now live: durable workflow retries consume the prior decision instead of re-calling the model.
- **Out-of-boundary tool side-effect re-assertion (Option B).** New `DecisionEvent::ToolSideEffect { name, assertion }` variant with typed `SideEffectAssertion::{FileExists, FileAbsent, Unknown}` (`#[serde(other)]` on `Unknown` so v0.6 can add variants). Replay never re-executes side-effecting tools; instead, `drive_replay` asserts the recorded post-condition. Mismatches raise `ReplayDivergenceError`, surfaced to the operator as `AgentRunEvent::Error`. Preserves determinism while catching divergence.
- **Long-term decision-log compaction.** New `sweeper` module: archives older than 24h get `zstd`-compressed to `.jsonl.bak.zst`; archives older than 7 days (configurable via `ANIMUS_DECISION_LOG_EXPIRY_DAYS`) get deleted. `ReplaySource::open` transparently decompresses. Wired into the daemon's `startup_cleanup` block — no new background task.

### Reattach completeness — Unix + Windows on one abstraction (BETA, items 2+7)

- **`decisions.jsonl` auto-discovery on reattach.** `AgentSpawnRecord` gains `decisions_jsonl_path: Option<String>` and `last_consumed_offset: u64` (both `#[serde(default)]` for backward compatibility). New `replay_gap_from_spawn_record(record, ...)` auto-derives the path. The daemon pre-allocates `agent_session_id` via the new `ANIMUS_AGENT_RUN_ID` env var; older runners that ignore it still work — gap-replay degrades to `Ok(None)`.
- **Cross-platform reattach via `interprocess`.** Collapsed the cfg(unix)-only daemon socket code AND the runner's Unix-only `set_write_timeout` hack into ONE cross-platform implementation. Unix uses domain sockets; Windows uses named pipes; both round-trip through `local_socket_name_for` which auto-detects via a path-separator predicate. `try_reattach` signature changed from `&Path` to `&str`. `ReattachConnection` no longer `cfg(unix)`-gated.
- **Bounded runner broadcast.** Per-reader writer thread fed by a `mpsc::sync_channel(256)`. A wedged reader trips `try_send → Full → drop` instantly, never blocking the runner's emit path. Cross-platform replacement for the Unix-only `set_write_timeout` hack.

### Queue protocol — head-of-line preemption (GAMMA, item 3)

- **`animus-queue-protocol` v0.3.0:** `QueueLeaseRequest` gains `exclude_subjects: Option<Vec<SubjectId>>` (backward-compatible default `None`). The queue plugin skips entries whose subject is excluded; they stay in `Pending` for a later lease.
- **`animus-queue-default` v0.3.0:** implements the filter; 4 new tests including a HoL regression guard.
- **ao-cli daemon:** snapshots `process_manager.active_subject_ids()` and passes them as `exclude_subjects` at lease time. **Deleted the `release_pending`-as-HoL-workaround code path** (~40 LOC removed). `call_queue_release_pending` retained in `plugin_clients.rs` for two real use cases: (1) within-batch duplicate subjects in one lease batch (different `workflow_refs`), (2) genuine "leased but cannot handle" scenarios.

### Cleanups + setsid (DELTA, items 5+8)

- **`config_context` sparse-override gap closed.** New `builtin_runtime_config()` `OnceLock` helper; fallback wired into `phase_capabilities`, `phase_output_contract`, `phase_decision_contract`, `phase_output_json_schema`. Cross-contamination guards prevent custom `output_contract` from inheriting builtin `output_json_schema` and vice versa. The Phase B codex P2 deferral is closed.
- **`current_ao_command` sibling-binary lookup removed.** Dropped the sibling discovery from `runtime_contract`; the canonical surface is the `memory_mcp_stdio_command` init-extension (round-1 fold-in). `ProcessManager::spawn_workflow_runner` exports `ANIMUS_HOST_CLI_PATH=current_exe()` so the daemon's direct-spawn path works without a sibling binary.
- **Manual-phase ignored test deleted.** `approve_manual_phase_continues_non_terminal_workflow` — its assertion exercised the deleted in-tree workflow runner; coverage moved to pack conformance. Test count regularized at 1917 (was 1900 v0.5.0, 11 ignored now from 14).
- **Full setsid hardening.** New scoped `#[allow(unsafe_code)] set_session_id_on_spawn` helper in `process_manager.rs` (one function, ~10 LOC, one-line WHY comment). Replaces `process_group(0)` with the stronger `setsid()` — runner survives terminal hangups too, not just SIGTERM to the daemon pgid. `libc` dep added. New test asserts new POSIX session.

### Codex round 1 P1 fixes (main loop)

- **`daemon_task_dispatch.rs`:** GAMMA's within-batch dedupe deleted too much. When `queue/lease` returns multiple Pending entries for the same subject (different `workflow_refs`) in one batch, the duplicates should `release_pending`, not `completion(CANCELLED)`. **Fixed:** within-batch duplicates now release back to Pending; old-plugin fallback to `completion(CANCELLED)` only when `release_pending` returns method-not-found.
- **`dbos_client.rs`:** lock-inversion between `shutdown` and `ensure_workflow_run`. **Fixed:** both functions now take `host` BEFORE `init` (consistent order); the init reset happens inside the host critical section so a racing step can't re-init on the old host before shutdown swaps it.

Codex round 3: 0 P1 / 0 P2 / 0 P3 on the merged + fixed v0.5.1 state.

---

## Surface-shrink wins (deletes ranked by impact)

- ALPHA: `MockDurableStore` moved out of production API into `#[cfg(test)]`.
- BETA: `cfg(unix)`-only socket code AND `set_write_timeout` hack collapsed into one `interprocess`-backed impl.
- BETA: `build_record` deleted (canonical constructor is `build_record_with_decisions`).
- BETA: `try_reattach` signature `&Path → &str` — drops path-vs-name confusion.
- GAMMA: ~40 LOC `release_pending`-as-HoL-workaround DELETED from daemon dispatch (net −19 LOC in ao-cli for this item).
- GAMMA: protocol change is one parameter extension on existing `QueueLeaseRequest`, not a new method.
- DELTA: `approve_manual_phase_continues_non_terminal_workflow` (`#[ignore]`'d, dead test) DELETED.
- DELTA: 2 `TODO(codex-p2)` markers DELETED (`config_context` + `runtime_contract`).
- DELTA: `runtime_contract::current_ao_command` sibling-binary discovery DELETED.

---

## Out-of-tree releases

- **[`animus-protocol`](https://github.com/launchapp-dev/animus-protocol)** → `v0.5.3` (commit `6b114186`) — `animus-queue-protocol` 0.2.0 → 0.3.0 with `exclude_subjects`.
- **[`animus-queue-default`](https://github.com/launchapp-dev/animus-queue-default)** → `v0.3.0` (commit `5f12e7db`) — implements `exclude_subjects` filter.

---

## 📊 Numbers

- **Total LOC delta on main:** +2978 / −878 across 25 files (most additions are tests + new modules).
- **ao-cli tests:** 1917 passing (was 1900 at v0.5.0), 6 pre-existing testkit-binary failures (unchanged), 11 ignored (was 14 — net −3 from DELTA's cleanup).
- **Codex rounds:** 3 rounds on the final merged main, ending 0 P1 / 0 P2 / 0 P3. (Sub-agents could not run codex inside worktrees; main loop ran it.)
- **Out-of-tree plugin test totals:** +4 in animus-queue-default (now 25), +2 in animus-queue-protocol (now 5).
- **Surface-shrinks:** 9 explicit deletes (see "Surface-shrink wins" above).

---

## ⚠️ Breaking changes

- **`animus-queue-protocol` 0.2.0 → 0.3.0** — new optional field on `QueueLeaseRequest`. Backward-compatible at the wire level (old clients omitting the field work identically to today), but the type signature changed. Third-party plugins consuming the protocol must bump their dep.
- **`animus-queue-default` v0.2.0 → v0.3.0** — install with `animus plugin install launchapp-dev/animus-queue-default@v0.3.0` or rerun `animus plugin install-defaults`. The daemon's `default-install.json` pin is bumped automatically.
- **`AgentSpawnRecord`** gains two fields. Older record files (without the fields) parse cleanly via `#[serde(default)]`; older daemons reading newer records ignore unknown fields. The wire is forward-compatible.
- **`current_ao_command` sibling-binary fallback removed.** Standalone plugin deployments that relied on a sibling `animus` binary now must use the `memory_mcp_stdio_command` init-extension or the `ANIMUS_HOST_CLI_PATH` env var (which the daemon sets automatically).

---

## 🚧 v0.6 roadmap (small carry-forwards)

- Live-socket `last_consumed_offset` write-back (BETA gap): events between gap-replay and crash get re-emitted on next reattach (duplicates per affected workflow).
- Built-in tool classification for side-effect assertions (ALPHA gap): producers currently opt in per-call.
- Side-effect kinds beyond `file_exists`/`file_absent` (process exit codes, network endpoint reachability, env-var equality).
- Graceful per-orphan shutdown for `ReattachConnection` (drop currently detaches but doesn't kill the reader thread).
- DBOS step_query 3-RPC cold path consolidation (perf, not correctness).
- Built-in `archive_decision_log` siblings check for `.bak.zst` collisions (low-probability re-archive case).

---

## Upgrade

```bash
animus daemon stop
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus plugin install-defaults --include-subjects --include-transports
animus daemon preflight
animus daemon start --autonomous
```
