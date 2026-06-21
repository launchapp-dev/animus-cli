# `config_source` plugin role — kernel rewiring IMPL-PLAN

This file describes the **kernel rewiring** the main session must do to consume
`animus-config-protocol`. The protocol crate itself (this directory) is already
written and additive. None of the changes below have been made — this is the
file-by-file plan, grounded in line regions verified against the worktree at
authoring time. **Verify line numbers before editing; the source moves.**

The crate could not be `cargo check`ed in the sandbox. First step for the main
session:

```bash
cargo check -p animus-config-protocol
cargo test -p animus-config-protocol
cargo run -p animus-config-protocol --bin animus-config-protocol-export-schema
```

Iterate on any schemars/serde issues before starting the rewiring.

---

## 0. What this crate already provides

- `PLUGIN_KIND_CONFIG_SOURCE = "config_source"`, `METHOD_CONFIG_LOAD`,
  `METHOD_CONFIG_VALIDATE`, `NOTIFICATION_CONFIG_CHANGED`,
  `CAPABILITY_CONFIG_WATCH`, `CONFIG_MODEL_SCHEMA_ID = "animus.workflow-config.v2"`,
  `CONFIG_MODEL_VERSION = 2`.
- `ConfigModel { schema, version, config: serde_json::Value }` — the canonical
  model. `config` is **opaque JSON** shaped like the kernel's `WorkflowConfig`.
  See "Design note" below for why it is `Value` and not a typed mirror.
- `ConfigLoadRequest { project_root, repo_scope }`,
  `ConfigLoadResponse { config: ConfigModel, cache_token: CacheToken }`.
- `CacheToken { version: String, external_inputs: bool }` — the cache contract
  (RFC §3 caching + open question 5).
- `ConfigChangedEvent { version: Option<String> }` — the watch notification.
- `ConfigValidateRequest/Response` + `ConfigDiagnostic` + `DiagnosticSeverity` —
  optional source-side pre-check.

### Design note: why `ConfigModel::config` is `serde_json::Value`

The kernel's `WorkflowConfig` (`crates/orchestrator-config/src/workflow_config/types.rs:809`)
and its ~40 nested types (`WorkflowDefinition`, `PhaseExecutionDefinition`,
`AgentProfileOverlay`, `McpServerDefinition`, `WorkflowSchedule`,
`WorkflowTrigger`, `DaemonConfig`, ...) derive only `Serialize/Deserialize`,
**not `schemars::JsonSchema`**, and `orchestrator-config` does not depend on
`schemars`. Mirroring them into the protocol crate, or adding `JsonSchema`
across that whole tree, is invasive kernel work that is explicitly out of scope
for the additive slice. Carrying the model as `Value` keyed by `schema` /
`version` keeps the wire contract stable and matches RFC open question #1
(leaning: keep `WorkflowConfig` internal; expose a stable envelope). If/when the
main session wants a typed wire model, replace `config: Value` with a strongly
typed `ConfigModel` body — but that is a follow-up, not this slice.

On the kernel side, deserialize after an admit check:

```rust
let model: animus_config_protocol::ConfigModel = /* from config/load */;
if !model.is_compatible() { /* error: incompatible config model schema/version */ }
let config: WorkflowConfig = serde_json::from_value(model.config)?;
```

---

## 1. Workspace wiring (ALREADY DONE in this slice)

- `Cargo.toml`: added `crates/animus-config-protocol` to `[workspace].members`
  and `animus-config-protocol = { path = ... }` to `[workspace.dependencies]`.
- The main session adds `animus-config-protocol.workspace = true` to the
  `[dependencies]` of each crate that needs it (at minimum
  `orchestrator-config`, `orchestrator-core`, and whichever crate hosts the
  `ConfigSourceClient` — see §3). `orchestrator-config` will also need
  `serde_json` (already present) for the `Value` <-> `WorkflowConfig` bridge.

---

## 2. Preflight: add `RequiredRole::ConfigSource`

File: `crates/orchestrator-core/src/plugin_preflight/mod.rs`

1. **Enum** (`enum RequiredRole`, currently lines ~152-164): add a
   `ConfigSource` variant alongside `WorkflowRunner` / `Queue`.
2. **`RequiredRole::label`** (match arm block, ~169-175): add
   `RequiredRole::ConfigSource => "config_source".to_string()`.
3. **`PluginPreflightSpec::daemon_default`** (~194-214): push
   `RequiredRole::ConfigSource` into `required_roles`, and add
   `("config_source".to_string(), default_config_source_repo())` to
   `auto_install_defaults`.
4. **Kind constant**: near `PLUGIN_KIND_WORKFLOW_RUNNER` / `PLUGIN_KIND_QUEUE`
   (lines ~20-22, currently local `const`s in this file), add
   `const PLUGIN_KIND_CONFIG_SOURCE: &str = "config_source";` — or import it
   from `animus_config_protocol::PLUGIN_KIND_CONFIG_SOURCE` (preferred, single
   source of truth).
5. **`InstalledPluginSummary::is_config_source`** (~321-327, next to
   `is_workflow_runner` / `is_queue`): add
   `pub fn is_config_source(&self) -> bool { self.plugin_kind == PLUGIN_KIND_CONFIG_SOURCE }`.

File: `crates/orchestrator-core/src/plugin_preflight/runner.rs`

6. **`role_satisfied` match** (the `RequiredRole::*` arms, ~67-72): add
   `RequiredRole::ConfigSource => installed.iter().any(|p| p.is_config_source())`.
7. **The fix-command match** (the second `RequiredRole::*` match, ~84-99): add
   a `RequiredRole::ConfigSource => { ... }` arm mirroring the `WorkflowRunner`
   / `Queue` arms (uses `install_target_for("config_source")`).

File: `crates/orchestrator-core/src/plugin_registry.rs`

8. Add `DEFAULT_CONFIG_SOURCE_PLUGINS: &[(&str, &str)] =
   &[("launchapp-dev/animus-config-yaml", "v0.1.0")];` next to
   `DEFAULT_QUEUE_PLUGINS` (line 34) / `DEFAULT_WORKFLOW_RUNNER_PLUGINS`
   (line 30). Tag is a placeholder until the plugin is published.

File: `crates/orchestrator-core/src/plugin_preflight/mod.rs`

9. Add `pub fn default_config_source_repo() -> String` mirroring
   `default_queue_repo()` (lines 132-135), reading the first
   `DEFAULT_CONFIG_SOURCE_PLUGINS` entry via `format_repo_spec`.

File: `crates/orchestrator-core/src/plugin_preflight/tests.rs` — add a test
asserting `ConfigSource` is in `daemon_default().required_roles` and that an
installed `config_source` plugin satisfies it.

> **Rollout note (RFC §6 phase v0.6.x-a):** Do NOT add `ConfigSource` to
> `daemon_default()` until the kernel load path can actually call the plugin
> (§3) OR a built-in/in-tree config source still satisfies the role. Otherwise
> every existing daemon fails preflight on upgrade. Sequence: land the role enum
> + `is_config_source()` + discovery first (inert), keep the in-tree YAML path
> as the default implementation, and only flip `daemon_default()` +
> `auto_install_defaults` once `animus-config-yaml` exists and the load path
> resolves it (phase v0.6.x-b).

---

## 3. The config load path swap

File: `crates/orchestrator-config/src/workflow_config/loading.rs`

The cut is in `load_workflow_config_with_metadata` (lines **49-157**). Today:

- Line 50: `let yaml_sources = super::collect_project_yaml_workflow_sources(project_root)?;`
- Lines 115-120: `super::compile_yaml_sources_with_base(&config, &yaml_sources)?`
  then `merge_yaml_into_config`.

The **source-acquisition legs** that move behind a `ConfigSourceClient`:

- `collect_project_yaml_workflow_sources` (defined in
  `yaml_compiler.rs:37`) — the YAML scan.
- `compile_yaml_sources_with_base` (`yaml_compiler.rs:71`) — interpolate +
  parse into a `WorkflowConfig`.
- The legacy-JSON rejection guards (lines 53-66) — these move into
  `animus-config-yaml` (so the "JSON config no longer supported" guidance still
  fires from the YAML plugin), OR stay kernel-side as a transitional guard.
- The external-inputs / cache-bypass logic (`sources_have_external_inputs`,
  lines 226-274; `build_workflow_cache_input`, 205-224) — replaced by the
  plugin-returned `CacheToken` (`version` keys the cache, `external_inputs`
  bypasses it). See §4.

The legs that **STAY kernel-side, unchanged**:

- Pack overlay merge: `build_installed_pack_workflow_config_base` (184-189),
  the `registry.entries_for_source(...)` loops (98-106, 122-130) calling
  `load_pack_workflow_overlay` + `merge_yaml_into_config`.
- `merge_yaml_into_config` (`yaml_compiler.rs:155`).
- `validate_workflow_config_with_project_root` (line 132).
- Agent-runtime derivation: `merge_workflow_runtime_overlay`
  (`agent_runtime_config.rs:1903`).
- State-machine compilation + the disk cache (`crate::cache::*`).

### Proposed shape

Introduce a `ConfigSourceClient` (new module — recommend
`crates/orchestrator-config/src/config_source_client.rs`, or in
`orchestrator-core` if plugin-host deps don't belong in `orchestrator-config`;
note `orchestrator-config` does not currently depend on
`orchestrator-plugin-host`, so **putting the client in `orchestrator-core` and
passing the resulting `WorkflowConfig` + `CacheToken` down into
`orchestrator-config` is the lower-coupling option**). The client:

1. `discover_by_kind(project_root, PLUGIN_KIND_CONFIG_SOURCE)` —
   `orchestrator_plugin_host::discover_by_kind` (signature at
   `crates/orchestrator-plugin-host/src/discovery.rs:693`). Pattern to copy:
   `build_runner_command_from_dispatch.rs:73` (workflow_runner discovery) and
   `notifier_dispatcher.rs:92` (notifier discovery + spawn + RPC).
2. Spawn the plugin via `PluginHost` + `PluginSpawnOptions` and issue a
   `config/load` JSON-RPC request with `ConfigLoadRequest { project_root,
   repo_scope }`.
3. Deserialize `ConfigLoadResponse`, run `ConfigModel::is_compatible()`, then
   `serde_json::from_value::<WorkflowConfig>(model.config)`.
4. Return `(WorkflowConfig, CacheToken)`.

Then `load_workflow_config_with_metadata` replaces lines 50 + 115-120: instead
of scanning/compiling YAML in-tree, it calls the client to get the base
`WorkflowConfig`, then runs the **unchanged** pack-overlay merge on top, then
validate + cache as today.

> **Transition (phase v0.6.x-a, RFC §6):** keep the in-tree YAML path as the
> default `ConfigSourceClient` implementation (an enum: `InTree(yaml)` vs
> `Plugin(discovered)`), so nothing breaks before `animus-config-yaml` is
> extracted. Flip the default to plugin-resolution in v0.6.x-b.

---

## 4. Caching against the plugin token

File: `crates/orchestrator-config/src/workflow_config/loading.rs` (cache legs)
and `crates/orchestrator-config/src/cache.rs` (`WorkflowCacheInput`).

- Today `build_workflow_cache_input` (205-224) hashes YAML bytes + pack inputs;
  `sources_have_external_inputs` (226-261) decides cache bypass via
  `content_references_external_inputs` (263-274).
- Under the RFC: key the cache on `CacheToken::version` (combined with the
  pack-input hash, which stays kernel-side since packs stay kernel-side), and
  bypass the cache when `CacheToken::external_inputs` is true. The pack-overlay
  inputs still feed `WorkflowCacheInput` so editing a pack still invalidates.
- The `content_references_external_inputs` substring scan moves into
  `animus-config-yaml` (it computes `external_inputs` for its returned
  `CacheToken`). Non-YAML sources compute their own (Postgres: probe for
  `${...}`-bearing string columns, or just always set `external_inputs = false`
  if values are pre-resolved).

---

## 5. Hot-reload via `config/changed`

File: daemon scheduler / control loop —
`crates/orchestrator-cli/src/services/runtime/runtime_daemon/` (the same area
that owns config hot-reload today) and
`crates/orchestrator-daemon-runtime/src/control/`.

- When the discovered config source advertises `CAPABILITY_CONFIG_WATCH`,
  subscribe to its `config/changed` stream (model the subscription on the
  `subject/watch` consumer; the notifier dispatcher in
  `notifier_dispatcher.rs` is the closest in-tree spawn-and-stream example).
- On `ConfigChangedEvent`: if `event.version` is `Some` and equals the
  last-compiled `CacheToken::version`, skip; else re-run `config/load` +
  recompile, then broadcast the existing `phase_*`/`workflow_*` config-reload
  events.
- When the plugin lacks `config_watch`: fall back to the existing interval
  heartbeat / explicit reload — no regression for YAML (which has no watcher
  today either; it recompiles on file-change/tick).

---

## 6. Default extraction: `animus-config-yaml` (separate repo, phase v0.6.x-b)

Out-of-tree at `launchapp-dev/animus-config-yaml`, path-deped locally until
published (the `animus-runtime-shared` precedent). It depends on
`animus-config-protocol` + `animus-plugin-runtime` and implements the
`config/*` family. The following move OUT of `orchestrator-config` into it:

- `workflow_config/yaml_compiler.rs` — `collect_project_yaml_workflow_sources`
  (37), `compile_yaml_sources_with_base` (71), `compile_yaml_sources_with_base_inner`
  (86). **Keep `merge_yaml_into_config` (155) in the kernel** — it is the
  compiler, not the parser.
- `workflow_config/env_interp.rs` — `${VAR}` / `${secret.X}` interpolation.
- `workflow_config/yaml_parser.rs`, `yaml_types.rs`, `yaml_diagnostic.rs`,
  `yaml_scaffold.rs`.
- `workflow_config/builtins.rs` — the YAML base (`builtin_workflow_config_base`)
  if the plugin owns the built-in starter; otherwise the kernel keeps a minimal
  `runtime_workflow_config_base` (loading.rs:191) for the empty case.
- The legacy-JSON rejection errors (loading.rs:53-66).

`merge_yaml_into_config` signature (`yaml_compiler.rs:155`) takes two
`WorkflowConfig`s — it stays kernel-side and is fed the plugin's returned model
as `base`. Confirm no other in-tree caller of the moved functions remains
(grep `collect_project_yaml_workflow_sources` — `loading.rs:36,50` and
`ensure_workflow_config_compiled` are the current callers; both route through
the new client after the swap).

---

## 7. Secrets / env interpolation (RFC §3, open question 4)

Interpolation moves into `animus-config-yaml`. The kernel's secret-resolver
hook (`install_workflow_secret_resolver` — grep for it; it lives in the
secret-resolution path) is exposed to the config plugin via the existing host
secret-passing path (the same mechanism that resolves keychain entries for
plugin spawns). Non-YAML sources own their own credentials. Decide whether to
add a host-side `secret/resolve` hook to the protocol so any source can request
resolution by name — **deferred; not in this crate.** Flag for v0.6.x-c.

---

## 8. Files to touch — checklist

- [ ] `Cargo.toml` — DONE (member + workspace dep).
- [ ] `crates/orchestrator-core/src/plugin_preflight/mod.rs` — enum, label,
      `daemon_default`, kind const, `is_config_source`, `default_config_source_repo`.
- [ ] `crates/orchestrator-core/src/plugin_preflight/runner.rs` — two match arms.
- [ ] `crates/orchestrator-core/src/plugin_registry.rs` — `DEFAULT_CONFIG_SOURCE_PLUGINS`.
- [ ] `crates/orchestrator-core/src/plugin_preflight/tests.rs` — preflight test.
- [ ] New `ConfigSourceClient` (orchestrator-core preferred; or orchestrator-config).
- [ ] `crates/orchestrator-config/src/workflow_config/loading.rs` — swap the
      acquisition legs (50, 115-120), keep pack/validate/cache legs.
- [ ] `crates/orchestrator-config/src/cache.rs` + loading cache legs — key on
      `CacheToken`.
- [ ] daemon hot-reload loop — subscribe to `config/changed`.
- [ ] `crates/orchestrator-cli/config/default-install.json` — add config_source
      to the curated defaults (so `install-defaults` installs it).
- [ ] Docs: `docs/reference/configuration.md`, `docs/reference/workflow-yaml.md`,
      `docs/architecture/kernel-and-flavors.md` (config sourcing now a role),
      and `CLAUDE.md` preflight/required-role prose.
- [ ] Separate repo `launchapp-dev/animus-config-yaml` (phase v0.6.x-b).

---

## 9. Open questions (carried from RFC §7, with this slice's stance)

1. **Canonical model exactness.** This slice carries `config` as opaque
   `Value` keyed by `animus.workflow-config.v2`, deliberately NOT forking
   `WorkflowConfig`. If a typed wire model is wanted later, it is a follow-up
   that requires adding `JsonSchema` across the kernel type tree (or a hand-
   written mirror) — significant, intentionally deferred.
2. **Single vs. multiple config sources.** This crate models a **single**
   source (one `config/load`). Pack overlays remain the one kernel-side merge
   exception. Composition (YAML + Postgres + API by priority) is not modeled.
3. **Pack overlays: kernel leg or a config source?** This slice keeps packs as
   a kernel merge leg layered on top of `config/load` (lower churn). Not
   modeled as a config source.
4. **Secret interpolation for non-YAML sources.** No host-side `secret/resolve`
   method is in the protocol yet. Each source owns credentials; YAML's
   interpolation moves into `animus-config-yaml`. Revisit in v0.6.x-c.
5. **Cache-token contract.** Resolved here as `CacheToken { version,
   external_inputs }` — structured, not a bare string, to cover the
   "external inputs changed but bytes didn't" hazard `sources_have_external_inputs`
   handles today. Confirm `version` granularity is sufficient for Postgres
   (`max(updated_at)`) and ETag sources.
6. **CLI authoring verbs that write YAML** (`animus workflow phases add`,
   `upsert_generated_workflow_phase` / `write_workflow_config` at loading.rs:178)
   — out of scope for the read path. They become source-specific (no-op/error
   for read-only sources) or route through a future `config/write` optional
   method. Flag for v0.6.x-c.
7. **Compiled-artifact inspection** (`animus workflow config get`) stays pointed
   at the kernel's compiled output, independent of source. No change needed.

### Additional open questions surfaced during this slice

8. **Where does `ConfigSourceClient` live?** `orchestrator-config` does not
   depend on `orchestrator-plugin-host` today. Putting plugin discovery/spawn in
   `orchestrator-core` (which already depends on the host) and passing the
   resulting `WorkflowConfig` + `CacheToken` into `orchestrator-config`'s
   compile path avoids a new dependency edge from config → plugin-host. Confirm
   the layering with the main session before wiring.
9. **`repo_scope` source of truth.** `ConfigLoadRequest.repo_scope` is
   `Option<String>`. Confirm the kernel computes the same `repo-scope` string it
   uses for scoped state (`protocol::scoped_state_root` / repository_scope) and
   passes it verbatim, so a Postgres source selects the right rows.
10. **Plugin-protocol RPC plumbing.** This crate defines only the `config/*`
    payload types; it does NOT define how they ride the `animus-plugin-protocol`
    `RpcRequest`/`RpcResponse` envelope. The client/server wrap these in the
    standard JSON-RPC envelope (params = `ConfigLoadRequest`, result =
    `ConfigLoadResponse`), exactly as subject backends wrap `SubjectFilter` etc.
    No `async_trait` server-side trait is provided here (subject protocol bundles
    one via `animus-subject-protocol`); decide whether `animus-config-yaml`
    wants a `ConfigSource` trait + a `config_source_main` runtime helper in a
    follow-up `animus-plugin-runtime` addition, or hand-rolls dispatch.
```
