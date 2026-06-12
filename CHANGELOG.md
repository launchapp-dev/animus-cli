# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.5.14] - 2026-06-12

**The 9.5 campaign release: budgets that enforce, skills that supercharge any
agent, a forensic chain that never dead-ends, and a smaller, honest surface.**
Driven by an 8-surface product-owner review (baseline 6.2/10, re-reviewed at
7.6 mid-campaign); every fix below was independently verified against the
review findings. BREAKING items are marked.

### Added

- Audited trust-on-first-use (TOFU) for plugin installs. The trusted-orgs
  store (`~/.animus/trusted-orgs.yaml`, `$ANIMUS_TRUSTED_ORGS`) now records
  per-entry audit metadata: `trusted_at` (RFC3339), `decided_by`
  (`interactive-prompt` | `yes` | `allow-org` | `built-in`), and
  `first_plugin` (the `owner/repo` whose install first triggered the
  prompt). The loader still accepts the legacy bare-string format for
  back-compat; new grants always write the rich record.
- `animus plugin revoke-trust <ORG>` removes an org from the TOFU
  allowlist and records a `revoked_at` tombstone so re-trusting re-prompts
  while the audit trail survives. The built-in `launchapp-dev` anchor
  cannot be revoked (errors with an explanation). Emits a
  `trust_org_revoked` audit event.
- `animus plugin trust list` (`--json`) shows current and revoked orgs
  with their `trusted_at` / `revoked_at` timestamps and how each grant was
  decided.
- Successful release-source installs now surface an `org_trust` block
  (`org` + `trusted_at` + `decided_by`) in the install JSON envelope and
  the `plugin_install` audit line, so `plugin install --json` can answer
  "when did we trust this org?".
- Active flavor persistence: a successful `animus plugin install-defaults
  --flavor <name>` (or `animus flavor install <name>`) now records the
  selected flavor project-locally in `.animus/plugin-scope.yaml` under an
  `active_flavor:` key. The daemon's flavor-only plugin scope resolver
  (`resolve_flavor_plugins` in `plugin_preflight_wiring.rs`) and the CLI's
  `animus plugin list` / `animus plugin scope show` read that selection back
  via `orchestrator_core::flavor::active_flavor_id_in`, so a non-default
  flavor's plugins are admitted by scoped discovery instead of being filtered
  out against `flavors/default.toml`. Selecting `default` again clears the
  persisted key. An unknown persisted name (no `flavors/<name>.toml` on disk)
  logs a warning and falls back to unrestricted (`mode: all`) scope — it
  never fail-closes discovery to an empty set. The selection merges into any
  existing scope file, preserving the operator's `mode` / `allow` / `extras`.
  `animus flavor current` with no `--name` now probes the persisted active
  flavor (was always `default`) and reports a `source` field
  (`flag` | `persisted` | `default`); `animus plugin scope show` adds
  `active_flavor` + `active_flavor_source`. Closes the codex-flagged TODO in
  `build_install_defaults_targets`.

- Cost attribution by provider and model. Per-phase cost records now
  carry a `provider` (sourced from the phase session checkpoint) and a
  `model` (already captured from run metadata events), so operators can
  answer "how much did claude vs gemini spend" and "which model drove
  the spike". New grouped breakdown views: `animus cost summary --by
  provider|model`, `animus cost workflow <run-id> --by provider|model|
  phase`, and `animus cost top --by model` (a cross-run model
  leaderboard). Each grouped row reports total tokens, total USD, and
  the group's percentage of the grand total (token share when no USD is
  reported); phases without attribution fold into an `unknown` bucket.
  The grouped views attribute live workflows only — archived `history`
  rows lack per-phase detail and are excluded (`cost workflow --by` is
  rejected for archived runs; they land under `unknown` in `cost top
  --by model`). `cost trends` is intentionally left workflow-level (no
  provider/model split) since per-bucket attribution across history
  would require a `HistorySummary` storage migration. New JSON envelope
  schemas: `animus.cost.summary.breakdown.v1`,
  `animus.cost.workflow.breakdown.v1`, `animus.cost.top.models.v1`. The
  additive `PhaseCost.provider` field deserializes old cost-state files
  unchanged.
- `animus status --failures <N>` controls how many recent workflow
  failures the dashboard lists (default 3), applied to both the human
  and `--json` output.
- One observability entry point: `animus daemon observe`. PO review found five
  log/event verbs (`daemon logs`, `daemon events`, `daemon stream`,
  `logs tail`, `events tail`) with no front-door — distinct data sources with
  no signpost for which answers which question. `daemon observe` is a routing
  front-door, not a new data path: `--follow` delegates to `daemon stream
  --pretty`; `--source <logs|events|stream|workflow>` routes to the specific
  existing handler; `--since <dur>` merges daemon events + logs chronologically
  across a recent window, labeling each line's source; `--workflow <id>` scopes
  surfaces that support filtering. Bare `daemon observe` prints a data-source
  matrix (verb | data source | live? | filters | when to use) plus the last
  ~20 merged lines. `--json` returns `{matrix, recent}` (bare) or `{lines}`
  (window/source). Every branch reuses the existing `events`/`logs`/`stream`
  readers — zero duplicated data paths.
- Skill application is now VERIFIABLE end to end. `animus output
  phase-outputs` grew a human-readable per-phase view (verdict, reason,
  commit summary) with a Skills block showing requested vs applied
  (source scope + contribution kinds: prompt / tool_policy / mcp_servers /
  args / env / codex_config / model / timeout / capabilities) vs missing
  (not found — check `animus skill list`) skills; `--json` keeps the
  raw envelope and now carries compact persisted skill records.
  `PersistedPhaseOutput` gained optional `requested_skills` /
  `resolved_skills` / `applied_skills` fields (old outputs deserialize
  unchanged; skill-less outputs serialize byte-identically), populated via
  the new `persist_phase_output_with_metadata` once the out-of-tree
  `animus-workflow-runner-default` pin bumps to pass its
  `PhaseExecutionMetadata` through. Typo'd skill names also stopped being
  a silent no-op at authoring time: explicit workflow-YAML `skills:`
  declarations (`phases.<id>.skills`, `agents.<id>.skills`) that do not
  resolve against the project's skill sources WARN (never error) on the
  compile stderr and in the `warnings` array of `animus workflow config
  validate` / `compile`; implicit builtin agent-profile defaults (pack
  names absent until the pack installs) are exempt, matching the runtime's
  explicit/implicit split.
- Pack dependencies are now real: `animus pack install` resolves and installs
  the pack's declared non-optional `[[dependencies]]` after the requested pack
  (semver-aware skip of already-installed versions, recursion into deps of
  deps capped at depth 5 with cycle detection, same source-resolution path as
  the parent — `ANIMUS_INIT_PACK_SOURCE_DIR` offline override, marketplace
  registry, or the pinned `default-install.json` GitHub release). Optional
  dependencies print as suggestions; a failing dependency never aborts the
  parent install and surfaces the manual install command. New flags:
  `--no-deps` skips dependency resolution, `--dry-run` prints the full
  dependency closure (and plugin requirements) without installing.
- New `[[requires_plugins]]` pack-manifest section (`repo`, optional `tag`,
  informational `role`, `optional`, `reason`): packs can declare the Animus
  plugins their workflows need. `animus pack install` checks each entry
  against the installed-plugin registry; missing required plugins prompt for
  install in an interactive terminal (default yes), install non-interactively
  with `--install-plugins`, and otherwise print the exact
  `animus plugin install <repo>@<tag>` commands — never silent.
  `animus pack info` now reports both pack dependencies and required plugins
  with installed/missing status. Additive compat: packs WITHOUT the section
  keep loading on older animus versions; packs USING it require this version
  (pair with `compatibility.animus_core = ">=0.5.14"`).
- First-class **project-scoped plugin installation**: `animus plugin install
  --project` (and the same flag on `uninstall` and `update`) targets the
  project-local triple — binary `<project>/.animus/plugins/`, registry
  `<project>/.animus/plugins.yaml`, lockfile `<project>/.animus/plugins.lock`
  — with the identical sha256 / cosign / TOFU / fail-closed-lockfile
  pipeline as global installs. `--project` conflicts with `--plugin-dir`.
  Project entries flow into `animus plugin list` (new `SCOPE` column +
  `scope` JSON field), `plugin status` (via discovery), `plugin outdated`
  (rows carry `scope: global|project`; the project registry is merged in),
  `plugin update --project`, and `plugin lock verify`, which now sweeps BOTH
  lockfile roots by default (global entries against the global dir, project
  entries against the project dir with a global-dir fallback for legacy
  entries). Discovery gains a project-registry tier
  (`<project>/.animus/plugins.yaml`) so official names outside the
  dir-scanned `animus-plugin-*`/`animus-provider-*` prefixes (e.g.
  `animus-subject-default`, `animus-queue-default`) resolve when installed
  project-scoped. Discovery precedence fix: the project-local tier now also
  outranks the explicit registry yaml (which every global install writes),
  so a project install truly shadows a same-named global install; the hidden
  global binary is surfaced in `plugin list` as a `shadowed` entry
  (`shadowed by project install`). `animus init` and project-scoped installs
  write a `.animus/.gitignore` covering `plugins/` so binaries stay out of
  version control while the committable project lockfile pins the repo's
  plugin set. See `docs/reference/configuration.md#project-vs-global-plugin-installs`.
- `animus plugin install-defaults --flavor <name>` (default: `default`):
  the flavor manifest (`flavors/<name>.toml`, with a binary-bundled fallback
  for `default`) is now the source of truth for the install plan. Everything
  the manifest marks `required` installs in one command — provider, subject
  backends (task + requirement), transport, workflow runner, and queue —
  covering every daemon-preflight role, so the canonical first run no longer
  needs a second `--include-subjects` pass. New `--include-recommended` flag
  installs the manifest's `recommended` set (extra providers, web UI, GraphQL
  transport, more subject backends). Unknown flavor names error instead of
  silently falling back.
- Phase-level `skills:` now actually reach workflow phase agents. The daemon
  resolves the union of each phase's `skills:` list and the executing agent
  profile's `skills:` at dispatch time — using the same scoped skill sources
  and trust rules as the ad-hoc `--skill` path (project > user > installed >
  agent-host prompt-only, with agent-host structural fields stripped at
  load) — and ships the resolved definitions to the workflow runner as an
  additive `ANIMUS_PHASE_SKILLS_JSON` spawn-env payload
  (`animus.phase-skills.v1`; older runners ignore it, and a newer runner
  paired with an older daemon resolves locally with the same rules via the
  new `animus_runtime_shared::phase_skills` module). Activation gating
  (`activation.tools` / `activation.models`) is applied runner-side against
  the actually selected tool/model; applied skills inject prompt fragments,
  tool policy, launch args/env, and skill-declared MCP servers (joining the
  phase contract like `phase_mcp_bindings`; unknown server names warn and
  are skipped). Missing skill names are a loud dispatch-log warning plus a
  `missing` metadata record — never a hard failure. Phase execution
  metadata now records `requested_skills`, `resolved_skills`, and
  `applied_skills` truthfully, and `animus workflow prompt render` previews
  the skill-injected prompt. Runtime enforcement in dispatched workflows
  lands when the out-of-tree `animus-workflow-runner-default` pin bumps to
  consume the payload.
- Skills now apply FULLY on the ad-hoc paths (`animus agent run` and
  `animus chat send`), not just their MCP servers + tool policy. The
  selected `--skill`'s prompt `prefix`/`suffix`/`directives` wrap the
  outgoing prompt and `prompt.system` rides the session `system_prompt`
  (an explicit `--context-json system_prompt` comes first); `extra_args`
  and `codex_config_overrides` are grafted onto the runtime contract's
  `cli.launch` block — the same mechanism workflow phases use — and skill
  `env` rides the session request env; the skill's `model` preference and
  `timeout_secs` apply when no explicit value is given. Precedence for
  every field:
  explicit CLI flags / context-json > skill > defaults; a caller-supplied
  `--runtime-contract-json` disables skill application entirely. On
  `animus chat send` the skill binds once per send invocation and applies
  per turn; launch-affecting skills force full-history replay instead of
  native session resume so launch flags reach every turn's provider
  process. New shared helpers in `animus-runtime-shared`
  (`apply_skill_prompt_to_body`, `merge_skill_system_prompt`,
  `skill_directives_section`, `inject_cli_extra_args_list`,
  `inject_codex_config_overrides_list`) back both the workflow and ad-hoc
  paths.

### Changed

- `animus flavor install` is now manifest-driven: it delegates to
  `animus plugin install-defaults --flavor <name>` instead of hard-coding
  `--include-subjects --include-transports`, and gains
  `--include-recommended`. `animus flavor current` drift and the install plan
  now share one required-set function (`FlavorManifest::required_plugins`),
  so they cannot disagree.
- `flavors/default.toml`: `animus-subject-requirements` moved from
  `subjects.recommended` to `subjects.required` — daemon preflight requires
  the `subject_kind:requirement` role, so it belongs in the required set.
- `animus plugin install-defaults` without flags now installs the flavor's
  required set (claude provider, subject-default, subject-requirements,
  transport-http, workflow-runner-default, queue-default) instead of all five
  providers; recommended providers install via `--include-recommended`. The
  legacy `--include-subjects` / `--include-transports` flags keep working and
  add the recommended slice of those sections.
- Daemon/`daemon preflight` failure output is composition-aware: when more
  than one required role is missing, the message also prints the single
  composed fix (`animus plugin install-defaults --flavor default --yes`)
  alongside the per-role install commands.
### Fixed

- Workflow lifecycle controls no longer leave the bound task in unexplained
  ghost state. `animus workflow cancel` now syncs the task to `cancelled`
  (unless already terminal), matching the daemon-side execution-fact
  projection and the orphan reconciler, so a CLI-local cancel can no longer
  strand the task in-progress forever. `animus workflow pause` annotates the
  task's `blocked_reason` with `paused by workflow <id>` (informational only
  — task status and the `paused` flag are untouched) so `animus subject get`
  explains the stall, and `animus workflow resume` clears exactly that
  annotation without clobbering genuine failure reasons. Resetting a subject
  with `animus subject status ... --status ready` now prints an
  `unstuck: cleared ...` line naming the `paused` / `blocked_*` flags the
  transition cleared.

### Changed

- `animus daemon health` now leads its human output with a one-line verdict:
  `healthy: true`, `healthy: true (paused)` (paused runtime is a deliberate
  operator action, not a failure), or `healthy: false`. The verdict is
  `false` when the daemon is down/crashed, a critical subsystem is failing,
  or any plugin reports unhealthy (including supervisor-disabled plugins);
  transitional degraded states stay `true`. The same boolean is added
  additively as a `healthy` key to `animus daemon health --json` and the
  `animus.daemon.health` MCP tool on the live control-wire path (the offline
  snapshot already carried it).

- **BREAKING:** `animus daemon start` now always starts the daemon as a
  detached background process. Previously it silently fell through to the
  foreground `daemon run` path unless `--autonomous` was passed. The
  `--autonomous` flag remains accepted as a hidden, deprecated no-op (it now
  just names the default), so existing `daemon start --autonomous` automation
  keeps working unchanged. Use `animus daemon run` for a foreground
  (dev/debug) daemon. `daemon start` success output now reports the daemon
  pid and the background log path, and starting while a daemon is already
  running is idempotent for all invocations (previously the flag-less form
  errored). `animus daemon restart` restarts into detached mode, matching
  the new default.
- **BREAKING**: `animus daemon metrics` is now a subcommand group. Bare
  `animus daemon metrics` keeps the live observability display (counters,
  gauges, histograms; `--watch / --interval-secs / --pretty` unchanged),
  and the opt-in anonymous usage telemetry controls moved under it as
  `animus daemon metrics {status, enable, disable, flush, cleanup}`.

### Fixed

- `animus daemon metrics` no longer hard-errors when the daemon is offline.
  The telemetry subcommands already worked offline; the bare display did not.
  Now bare `animus daemon metrics` with no daemon prints `daemon not running;
  live metrics unavailable` plus the offline telemetry status summary and exits
  `0` (`--json` returns `{"daemon_running": false, "telemetry": {...}}`). The
  hard "daemon required" behavior is retained only for `--watch`, where a live
  dashboard is meaningless without a running daemon.

### Removed

- **BREAKING**: the `animus project` command group (`list`, `active`,
  `get`, `create`, `set-active`, `rename`, `archive`, `remove`). The
  project registry was a CLI-only facade with no daemon, workflow, or MCP
  consumers — the daemon routes purely via git-root + repo-scope. The
  underlying `ProjectServiceApi` trait, its `InMemoryServiceHub` /
  `FileServiceHub` impls, the `project_shared` helper module, and the
  `ServiceHub::projects()` method were deleted from `orchestrator-core`.
  No aliases (model/runner removal precedent).
- **BREAKING**: the `animus self` command group. Its only verb,
  `self update`, folded into the canonical top-level `animus update`:
  `--check` replaces `--check-only`, and `--force` / `--prerelease` are
  folded in alongside the existing `--yes` / `--channel`. No aliases.
- **BREAKING**: trimmed `animus git` to inspection-only. Kept:
  `git repo list`, `git worktree list`, `git worktree prune`. Removed:
  `git repo {get,init,clone}`, `git branches`, `git status`,
  `git commit`, `git push`, `git pull`, and
  `git worktree {create,get,remove,pull,push,sync,sync-status}`.
  Agent-driven merge/PR/commit behavior lives in the out-of-tree workflow
  runner plugin via `post_success.merge`, not these in-tree handlers;
  there were never any `animus.git.*` MCP tools, so no MCP surface
  changed. `git worktree prune` keeps its `prune_worktrees` approval gate.
  No aliases.
- **BREAKING**: the top-level `animus metrics` command group. Its five
  telemetry verbs live under `animus daemon metrics` now; there are no
  aliases (v0.4 removal precedent). The `metrics` value also left the
  telemetry `command_group` tag enum — `animus daemon metrics ...`
  invocations record as `daemon`.
- **BREAKING (alias retirement)**: the legacy CLI aliases added during
  the v0.5.13 conventions convergence were removed with no replacement
  aliases (matching the v0.4 no-alias precedent) — `pack inspect` (use
  `pack info`), `skill show` (use `skill info`), `flavor describe` (use
  `flavor info`), `output run` (use `output read`), `project load` (use
  `project set-active`), the hidden `animus git confirm {request, respond,
  outcome}` subcommand (use `animus approval`), the `-i` short flag on
  `workflow resume` (use `--id` / `--workflow-id`), and the `--force`
  visible alias for `--yes` on `workflow prune` / `workflow delete`.

## [0.5.13] - 2026-06-11

**Human-in-the-loop agents, MCP pass-down on every provider, an event-driven
scheduler, and two deep bugfix waves.** Agents can now ask questions and
request approvals mid-run (blocking or suspend-and-resume), per-agent MCP
servers reach all four providers on every run path, the daemon wakes on
events instead of polling, and ~60 verified bugs were fixed across the
workflow engine, config compiler, plugin host, and OAuth broker.

### Added

- Agent interactions (human-in-the-loop): blocking `animus.agent.ask` and
  `animus.agent.request_approval` MCP tools with per-agent `approval_policy`
  (`auto_allow`/`auto_deny` globs + default), an `animus agent interactions`
  inbox, management-gated `animus.interactions.*` tools (`animus mcp serve
  --management`), and notifier push on escalations. Workflow phases use
  suspend/resume: the tool suspends, the workflow pauses, and answering
  resumes the provider session with the decision as feedback. With protocol
  v0.1.13.5, claude enforces approvals natively via
  `--permission-prompt-tool`; codex/gemini/opencode get a prompt preamble.
  Design: docs/architecture/agent-interactions-protocol.md.
- Per-agent MCP pass-down to the providers themselves: the resolved server
  set rides the provider RPC as `extras.mcp_servers` for `animus agent run`
  and `animus chat` on all four providers (protocol v0.1.13.4 transports),
  and on the workflow path via workflow-runner v0.4.3. Secret-bearing
  servers (all OAuth flows) are rewritten to `animus-mcp-proxy` stdio
  entries — tokens never reach CLI configs or argv.
- `permission_mode`: agent-profile field, phase `runtime:` override
  (phase > profile), and `--permission-mode` on `animus agent run` /
  `animus chat send`; forwarded to claude/codex/gemini native flags.
- `animus mcp serve` gains `--agent-id` / `--workflow-id` identity pins and
  `--management` (inbox tools agent-injected servers cannot access).
- Event-driven scheduler: `daemon/nudge` control message (sent fire-and-forget
  by subject/queue write paths), cron-deadline wakes (schedules fire on time
  instead of on the next tick), and completion wakes. `--interval-secs` is now
  a fallback heartbeat, not the dispatch latency.
- `animus daemon restart`; `animus plugin update --restart-daemon` is now real
  (was a documented placeholder).
- `workflow-failed` and `task-blocked` notifier events (once per transition).
- Compile/validate warnings for declared-but-unenforced workflow YAML
  (`UNENFORCED_RULES` registry), surfaced in `workflow config validate` JSON.
- `daemon health`/`status` report `runtime_paused` (+ since-when) and
  per-plugin supervisor state (restart counts, disabled-until).
- `animus pack uninstall` and `animus skill uninstall` (`--dry-run`, `--json`,
  reference guards).
- `animus workflow prune` (dry-run by default, terminal-status only) and
  `animus workflow delete --run-id` for run-history retention.
- Bulk queue ops: `hold`/`release`/`drop` take multiple ids or `--all --yes`;
  MCP queue tools gain `subject_ids[]`.
- `animus plugin outdated` (installed vs recommended vs latest, `--offline`);
  registry fetch falls back to stale cache with an age warning, retries with
  backoff, and explains 429s.
- `animus init` reaches a runnable state: installs the recommended packs
  (interactive default-yes / `--install-packs`), suggests `animus secret`
  migration for detected env-var API keys, and prints the next command.
- `animus approval {request, respond, outcome}` group (formerly `git confirm`,
  which remains a hidden alias).
- `animus.cli.v1` envelope contract tests per command family and a documented
  CLI conventions section.

### Changed

- Typed exit codes for `subject`, `cost`, `events`, and `plugin`: invalid
  input exits 2, not-found 3, unavailable (missing plugin / network) 5 —
  previously all exit 1. Scripts matching on exit 1 for these cases must
  update.
- Verb renames, aliases-first (old names still work, hidden): `pack info`,
  `skill info`, `flavor info`, `output read`, `project set-active`. Workflow
  commands accept `--workflow-id` aliases; `workflow prune --older-than`
  accepts unit suffixes (`30d`, `12h`).
- `animus plugin status` absorbs provider health (aggregate
  `provider_plugins_healthy` + per-provider install state); `animus doctor`
  absorbs orphaned-CLI-process detection (`--fix` prunes dead tracker entries
  only; live PIDs get a manual suggestion).
- Provider/runner default-install pins: claude v0.2.7, codex v0.2.8,
  gemini/opencode v0.2.6 (all on protocol v0.1.13.5), workflow-runner v0.4.3
  (first cosign-signed runner release; `animus plugin install` verifies it).
- `animus daemon events` is one-shot by default; `--follow` (bare,
  `=true`/`false`) opts into streaming. Previously follow defaulted on, so
  scripted `daemon events --limit N` never exited.
- Explicit queue entries (`animus queue enqueue`) drain even when
  `daemon.auto_run_ready` is false — the flag now gates only ready-task
  auto-dispatch.
- `animus output artifacts`/`download` read the scoped runtime artifacts
  root (`~/.animus/<scope>/artifacts/`) first, project-local legacy second.

### Removed

- `animus model` command group (zero call sites) and `animus runner` group +
  the `animus.runner.*` MCP tools (absorbed as above).
- The never-executed daemon git policy: `auto_merge`, `auto_pr`,
  `auto_commit_before_merge`, `auto_prune_worktrees` config/flags/YAML keys,
  and the zero-caller `GitProvider`/`BuiltinGitProvider`. Merge/PR behavior
  lives in workflow `post_success.merge` (workflow-runner plugin). Old
  pm-config.json files still load.
- The no-op `idle_timeout_secs` knob, the dead `ANIMUS_RUNNER_CONFIG_DIR` and
  `ANIMUS_RUNNER_SCOPE` env vars, and the dead `--runner-scope` flag.
- Dead crate leftovers (`agent-runner`, `oai-runner`), the orphaned
  `runner_helpers.rs`, stale agent-runner doctor checks, and ~15 unused
  dependencies.

### Fixed

- Workflow execution: async `animus workflow run` and `workflow resume` now
  spawn a real detached workflow_runner (previously they bootstrapped a
  Running record nothing executed; the reconciler zombie-cancelled it ~90s
  later and set the task Cancelled). Resumed workflows register a pid guard
  so the orphan reconciler leaves them alone; manually-claimed in-progress
  tasks are no longer re-blocked every tick (`status_changed_at`-aware
  reconciliation); in-progress tasks with no workflow self-heal to Ready.
- Secrets: `workflow phases upsert` round-trips unresolved sources — resolved
  env/keychain/`${secret.*}` values can no longer land in committable
  generated overlays; keychain-resolved values are redacted from parse
  diagnostics; `${...}` inside YAML comments is no longer interpolated.
- Lifecycle engine: all ~20 remaining state-machine `.expect()` panic sites
  fail gracefully (merge-conflict + phase-approve no longer panics the
  daemon); custom state machines are validated for executor-required
  transitions; the duplicate-workflow guard actually works on the file hub.
- Triggers/schedules: file-watcher events survive failed spawns (baseline no
  longer advances on dispatch failure); fresh schedules fire via a
  horizon-bounded catch-up instead of requiring a tick inside the exact cron
  minute; the first file appearing under a watch dispatches.
- Concurrency: agent memory/message appends, OAuth token refresh, chat
  sends (per-conversation), and plugin lockfile saves are cross-process
  locked; SQLite multi-entity mutations are transactional; checkpoint
  numbering is collision-safe.
- OAuth: `invalid_grant` evicts the cached refresh chain and retries the env
  seed in the same call; re-minted seeds bypass stale caches (value-hash
  fingerprints); corrupt keychain credentials degrade to unauthenticated
  instead of bricking `animus mcp auth status`/`logout`.
- Plugin host: a request racing reader teardown can no longer hang a daemon
  dispatch task forever; subject routing has a response deadline; broken
  `flavors/default.toml` fails closed loudly and consistently across
  `plugin list`, `daemon preflight` (exit 2, `--auto-install` withheld), and
  daemon startup.
- Repository scope: clones on unmounted volumes no longer get their state
  adopted by same-origin siblings (mount-aware reclaim heuristic +
  fast-path ownership validation) — ends silent cross-clone workflow.db
  sharing.
- Observability: `daemon events --follow` and `daemon stream` no longer lose
  records across log rotation (dev/ino cursor drains the rotated file
  first); `chat list`/`search` skip stray directories instead of failing;
  a chat stream ending without a terminal event is an error, not a
  silently-successful empty turn.
- `init` template prompt no longer hangs on non-TTY stdin (and detects EOF).
- Stale help text (hardcoded version markers, internal jargon) and misleading
  config docs: `.animus/config.json` is self-update config only; daemon
  runtime settings live in the scoped `daemon/pm-config.json`. Scheduler
  timing model and previously undocumented `ANIMUS_*` env vars documented.
- Pack/skill uninstall tests pin HOME (no more real `~/.animus` pollution or
  parallel flakes).


## [0.5.12] - 2026-06-09

**Agents reach Animus + project MCP servers, author skills, and tune
thinking effort — plus MCP OAuth.** This release makes the agent surface
genuinely usable: an ad-hoc agent now gets the MCP servers its profile/skill
declares, can create skills, and OAuth-protected MCP servers work through a
local proxy.

### Added
- **Per-agent MCP wiring** — `animus chat` and `animus agent run` now give an
  agent the MCP servers its **profile/skill declares** (a trading agent gets
  trading servers, a marketing agent gets marketing servers), resolved by name
  against the project `mcp_servers` map. Flags: `--agent`, `--skill`,
  `--mcp-server <name>` (add more), `--no-animus-mcp` (drop the built-in).
  Materializes the per-agent set into the cwd `.mcp.json` the CLI tool reads,
  with secrets stripped. OAuth servers route through `animus-mcp-proxy`.
- **MCP OAuth** — `animus mcp auth/auth-status/auth-logout` + `animus-mcp-proxy`
  (stdio bridge), built on rmcp 1.7's auth engine with OS-keychain token
  storage. Discovery + DCR + auth_code/PKCE + loopback callback. Least-privilege
  scope default + preview/confirm before the browser + `--dry-run`/`--yes`.
- **`animus.skill.create` / `animus.skill.update`** MCP tools — agents author
  project-scoped skills to `.animus/skills/<name>.yaml` (validated, overwrite-
  guarded, round-trip-verified). Joins the existing `skill.list/get/search`.
- **`--reasoning-effort low|medium|high`** on `animus agent run` + `chat send`
  (and a `reasoning_effort` agent/phase config field) — codex
  `-c model_reasoning_effort`, claude `--effort`.
- **`animus chat search <query>`** — grep conversation transcripts across the
  scope (case-insensitive, `--limit`, `--case-sensitive`).

### Fixed
- Chat `tool_result` streams the full tool **output** + correct tool name.
- MCP built-in tool count published in the docs synced to 78.

### Provider compatibility (run `animus plugin update`)
- codex / opencode / gemini providers stream tool/command activity correctly
  against current CLI versions; codex/claude pick up `--reasoning-effort`.

## [0.5.11] - 2026-06-08

**Chat streaming fidelity + the full provider matrix.** `animus chat send
--stream --json` now streams *everything the agent does* — and all four
provider tools (claude / codex / opencode / gemini) were brought up to
current-CLI compatibility so their tool/command activity streams too.

### Fixed
- **chat `tool_result` streams the tool output.** Previously a tool result
  emitted only `success: bool`, dropping the actual content. It now carries
  the provider's full output (command stdout, file contents, structured
  JSON) so a consumer sees what each tool returned, not just that it ran.
- **chat `tool_result` reports the human tool name.** When a provider put a
  tool_use id (e.g. `toolu_…`) in the result's name field, the stream now
  correlates it back to the preceding `tool_call`'s name.

### Provider compatibility (ship separately — run `animus plugin update`)
The provider plugins evolved past their parsers as the upstream CLIs
changed. All three are released at **v0.2.4** on the stable `v0.1.13.2`
protocol lineage:
- **codex v0.2.4** — codex emits sandbox shell runs as `command_execution`
  stream items (not `function_call`); these were dropped. Now mapped to
  `tool_call` + `tool_result`, so codex streams its commands.
- **opencode v0.2.4** — opencode v1.2 wraps frames as `{type, part}` with
  tool details under `part.state`; the parser read the old flat keys and
  produced `unknown_tool`. Now parses the part shape into `tool_call` +
  `tool_result` (old format still handled).
- **gemini v0.2.4** — gemini CLI refused headless runs in an "untrusted"
  directory (exit 55). The provider now sets `GEMINI_CLI_TRUST_WORKSPACE=true`
  on spawn so `animus chat --tool gemini` works without interactive pre-trust.

All four providers verified end-to-end through `animus chat` against official
signed builds.

## [0.5.10] - 2026-06-08

**Animus Chat (CLI-first).** Multi-turn conversations through the CLI,
streamed as JSON, built entirely on the **existing** provider plugins
(claude/codex/gemini) — no new plugin, no new API key.

### Added
- **`animus chat new|send|get|list`** — durable multi-turn conversations.
  `send --stream --json` emits one `ChatStreamEvent` JSON object per line
  to stdout (turn_started / text_delta / tool_call / tool_result /
  metadata / turn_completed) — the surface to build chat apps on.
- **`animus cost conversation <id>`** — per-conversation token + USD rollup,
  folding each turn's usage (matches the existing workflow cost aggregator).
- **Provider-owned continuity** — each turn is strictly resume XOR replay,
  never both: with a live provider `session_id` the prompt is just the new
  message (the CLI tool's native session carries history); with no session
  Animus replays its stored transcript. Single full-history retry on a
  stale-session error. No double-counted context.
- **Conversation store** at `~/.animus/<repo-scope>/chat/<id>/`
  (`meta.json` continuity pointer + append-only `messages.jsonl`),
  path-traversal-guarded. User message persists before the provider call
  (crash safety); the provider's native `session_id` is captured per turn.
- Reuses `SessionBackendResolver` (the `animus agent run` path), so the
  wrapped CLI tool reaches Animus's MCP server via `mcp_endpoint` — the
  model can call `animus.*` tools mid-conversation.

### Notes
- Direct-API `chat_provider` plugins (own the turn structure, intercept
  tool use) and the OpenAI-compat HTTP surface are deferred to v0.5.11.
  The turn loop is factored behind a `TurnProducer` seam so that path is
  additive. See [docs/architecture/animus-chat.md](docs/architecture/animus-chat.md).

## [0.5.9] - 2026-06-08

The **performance + scoping** wave. Five parallel agents cut the
compound cost that multiplies across Tauri polling, agent automation,
and daemon ticks. Headline: `animus daemon status` 3052ms → ~800ms,
`animus status` 11.1s → 3.1s, warm 30-plugin discovery 3000ms → ~4ms.

### Added
- **`animus plugin cache clear|list`** — inspect/wipe the sha256-keyed
  on-disk manifest cache (`~/.animus/cache/manifests/<sha>.json`). Cache
  invalidation is automatic since install/upgrade rewrites the binary sha.
  `ANIMUS_DISABLE_MANIFEST_CACHE=1` kill-switch.
- **`animus plugin scope show|set|reset`** — per-project plugin scoping via
  `.animus/plugin-scope.yaml`. Three modes (`all`, `flavor-only`,
  `allowlist`); `flavor-only` is the default when a flavor manifest exists.
  Discovery, preflight, and the plugin-status registry iterate the scoped
  set instead of every binary in `~/.animus/plugins/`.
- **`--no-cache`** global flag + `ANIMUS_DISABLE_{CI,WORKFLOW}_CACHE=1` /
  `ANIMUS_CI_CACHE_TTL_SECS` env knobs for the new hot-path caches.
- **`ANIMUS_DISABLE_PROJECT_ROOT_CACHE=1`** kill-switch for the new
  in-process `resolve_project_root` cache.

### Performance
- **sha256-keyed plugin manifest cache + parallel cold probes** — warm
  30-plugin discovery drops from ~3s to ~4ms; cold-cache probes run
  `min(8, num_cpus)`-parallel instead of serial. Fixes the root cause of
  slow `daemon status` / `plugin list` / `init` on many-plugin installs.
- **daemon status fast-path** — `load_daemon_status_snapshot_fast` skips the
  SQLite queued-task count + provider scan that `load_daemon_health_snapshot`
  does. `animus daemon status` no longer pays ~3.3s of health-snapshot cost.
- **MCP daemon tools route in-process** — `animus.daemon.status/health/agents`
  no longer `current_exe()`-spawn a CLI subprocess per call, killing the
  Tauri 3-call refresh penalty (~3× full CLI startup per refresh).
- **single-socket ControlClient** — `try_connect` + `call_raw` reuse one
  AF_UNIX connect instead of two.
- **provider-healthy check is a directory walk** — `provider_plugins_healthy_for`
  walks `~/.animus/plugins/animus-provider-*` instead of running full plugin
  discovery (the original v0.5.8.1 fix, now alongside the cache).
- **CI status cache** (60s TTL, per-repo) skips the `gh run list` subprocess —
  `animus status` ~8s faster. **workflow YAML compile cache** keyed by source
  mtime+sha. **1s in-process daemon health cache** for multi-call flows.
- **CLI per-invocation cuts** — `resolve_project_root` in-process cache,
  `gh --version` memoized, update-check end grace 750ms → 50ms, end-of-run
  metrics flush 2s → 200ms, keychain installer short-circuit.

## [0.5.8] - 2026-06-08

This wave is the **observability + reliability** + **secrets** + **state**
release. Eight parallel agents shipped one cohesive set:

- **Project secrets via OS keychain**: `animus secret set/get/list/rm/import-env/export-env`
  with manifest-filtered injection into plugin processes and `${VAR}` workflow YAML fallback.
- **RBAC small core**: Principal + policy.rbac config + chokepoint #1 +
  peer-cred + `animus auth whoami` + global `--as <principal>`.
- **Observability**: `animus events tail` + `animus plugin status`.
- **Hot reload**: notify-based watcher + ArcSwap snapshot for
  `.animus/workflows.yaml` + `animus workflow config reload`.
- **Doctor --fix**: stale daemon pid, orphan worktrees, zombie phase
  sessions, merged agent branches.
- **State export/import**: versioned `animus.state.export.v1` tar.zst
  with MANIFEST.json + safe-by-default import.
- **Workflow YAML diagnostics**: rustc-style file:line:col + did-you-mean
  for parse errors.
- **Self-update**: `animus update` top-level alias with atomic binary swap.
- **Plugin bulk-update**: `animus plugin update --all/--kind/--name`
  with diff preview and semver-aware "ahead of pin" detection.
- **Workflow YAML quality-of-life**: `worktree: true|false` boolean
  shorthand alongside the string short-form and long-form map.
- **Plugin rename verb** + multi-kind subject backend collision check +
  `name_override` round-trip through discovery.

### Added (v0.5.8)

- **`animus plugin rename <PLUGIN_NAME> --to <NEW_KIND>`** — post-install
  rename of a plugin's `installed_kind` in `plugins.lock`. Reuses the
  v0.5.7 install pipeline's collision check, auto-increment behavior, and
  invalid-character validation; only the lockfile's `installed_kind` slot
  changes (the on-disk binary, manifest, and `native_kind` stay put).
  `--force` auto-increments past a collision (`requirement` -> `requirement-2`)
  instead of failing. Emits an `animus.plugin.rename.v1` envelope.
- **`Principal` type + `policy.rbac` config (small core).** Introduces
  `orchestrator_core::principal::Principal` (User / Daemon /
  ServiceAccount) and a `RbacMode` enum (`single-user` default,
  `enforce` opt-in). Single-user installs are bit-identical — the
  permission hook is a no-op unless `enforce` is set.
- **`~/.animus/principals.yaml` auto-bootstrap.** On first run the
  daemon writes a one-principal default file mapping the current OS
  user to an `admin`-roled `local` principal. Existing files are
  never overwritten (collision guard from the design doc).
- **Chokepoint #1: control-dispatch permission hook.** Every
  control RPC routes through `check_principal_can`. Under enforce,
  a hardcoded role table allows `admin: *` and `viewer: *.read`;
  everything else fails closed with `permission_denied`.
- **Peer-cred resolution on the Unix socket (`cfg(unix)`).** The
  accept loop calls `getpeereid` (BSD/macOS) or `SO_PEERCRED` (Linux)
  and resolves the peer UID to an OS username and a declared
  `PrincipalEntry`. Honors the design doc §"Identity sources"
  resolution chain.
- **`animus auth whoami` + global `--as <principal>` flag.** The
  CLI surfaces the effective principal and propagates `--as`
  honor-system via a `$/setPrincipal` notification at every
  control connection. Warns loudly on use.
- **Audit-log shim.** `AuditActor::Principal { id, kind }` carries
  typed principal fields alongside the legacy `actor` string —
  additive; existing readers parse unchanged.
- v0.6 deferrals: per-tenant scope migration, per-principal secret
  routing, plugin-host trust changes, and chokepoints #2-#4. See
  [`docs/architecture/multi-tenant-rbac-v0.5.5.md`](docs/architecture/multi-tenant-rbac-v0.5.5.md).

### Fixed (v0.5.8)

- **Multi-kind subject backend collision detection** — the install
  pipeline's `rename_eligible_native_kind` previously returned only the
  FIRST exact `subject_kind:*` capability, so a single binary declaring
  both `task` and `requirement` slipped past collision checks on the
  secondary kind. v0.5.8 introduces `all_rename_eligible_native_kinds`
  and a secondary-kind collision pass in `compute_kind_assignment`. A
  multi-kind backend now refuses to install when ANY of its declared
  exact kinds collides with an existing install — the lockfile records
  one `installed_kind` per plugin, so a secondary collision cannot be
  auto-incremented and must be surfaced. Closes `TODO(codex-p2 round-4
  v0.5.7)` in `ops_plugin.rs`.
- **`--name <NAME>` install override round-trips through discovery** —
  `plugins.yaml` gains a `name_override` field that records the
  install-time `--name` value. `PluginDiscovery::discover_configured`
  now uses `name_override` as the canonical `DiscoveredPlugin::name`
  when set, so the daemon's `SubjectRouter` alias map (keyed by
  `plugin.name` from discovery) finds the lockfile entry (also keyed
  under the override) instead of dropping the alias. Without this
  fix, a plugin installed as `--name task-archive` would re-register
  under its manifest name on next daemon start and lose its rename.
  Closes `TODO(codex-p2 round-4 v0.5.7)` in `subject_dispatch.rs`.

## [0.5.5] - 2026-06-07

### Documentation

- docs(workflow-yaml): document `schedules`, `triggers`, and `daemon` top-level blocks — features have shipped but had no authoring documentation. Added per-type config tables for `file_watcher`, `webhook`, `github_webhook`, and `plugin` triggers, runtime semantics for cron schedules (missed runs not replayed; `run_count` vs `missed_count`), and the full `DaemonConfig` field surface. Cross-linked from `docs/reference/configuration.md`.

### Added (v0.5.5)

- **`worktree:` block on workflows and phases.** Workflow YAML now accepts
  `worktree: { mode: auto|required|skip, cleanup, base_ref }` at the
  workflow level and (with short-form `worktree: skip` support) at the
  per-phase level. `mode: required` fails-fast if a worktree can't be
  created; `mode: skip` keeps the phase in the project root; phase-level
  values always override the workflow-level default. Replaces the
  previously implicit, always-on worktree creation in the workflow
  runner.
- **`secrets:` block + `${secret.<name>}` interpolation.** Workflow YAML
  now declares logical secret names mapped to process env vars:

  ```yaml
  secrets:
    linear_token:
      env: LINEAR_API_TOKEN
      required: true
  mcp_servers:
    linear:
      env: { LINEAR_API_TOKEN: "${secret.linear_token}" }
  ```

  Resolution happens at config-compile time. Required-but-unset env
  vars and references to undeclared keys fail the compile with the
  YAML file path and 1-based line number. The compiled
  `workflow-config.v2.json` contains the resolved string — plugins
  consume the same scalar shape they always did.
- **Sensitive-interpolation lint.** When a workflow YAML interpolates
  `${VAR}` whose name matches `TOKEN|KEY|SECRET|PASSWORD`
  (case-insensitive) *outside* the `secrets:` block or a `*_env:`
  declaration field, the compiler emits a warning to stderr suggesting
  the author move it under `secrets:`. The lint is a warning, not an
  error — trusted workflows may have legitimate uses.
- **Trigger plugin authoring story.** New `animus plugin scaffold trigger <name>`
  subcommand emits a minimal, offline, self-contained Cargo starter for a
  custom `trigger_backend` plugin (Cargo.toml, plugin.toml, src/main.rs,
  README.md, .gitignore) pinned against `launchapp-dev/animus-protocol`
  v0.5.5 by default. Unlike `animus plugin new`, the scaffolder works
  without network access and ships a working `initialize` + `trigger/watch`
  + `trigger/ack` + `health/check` skeleton.
- **`examples/triggers/fswatch/`** — reference filesystem-watch trigger
  plugin that watches a glob list and emits `trigger/event` notifications
  on file modification. Compiles standalone; not a workspace member.
- **`docs/guides/authoring-trigger-plugins.md`** — walks the trigger
  protocol surface, daemon supervisor lifecycle, manifest format,
  scaffold usage, fswatch walkthrough, and debugging.

### Fixed

- **Trigger plugins now receive workflow YAML `config` blocks.** The
  daemon's `TriggerSupervisor` was previously sending
  `TriggerWatchParams::default()` for every trigger plugin, so the
  `triggers[].config` block authors wrote in workflow YAML never
  reached the plugin. The supervisor now forwards every enabled
  `type: plugin` trigger as `TriggerWatchParams.config.triggers[]`;
  each plugin filters the list for entries it understands.
### Added (v0.5.5)

- **Kernel-level OAuth broker for HTTP-transport MCP servers.** HTTP MCP
  entries (`mcp_servers.<name>` with `transport: http` + `url:`) can now
  carry an `oauth:` block declaring one of three flows
  (`client_credentials`, `refresh_token`, `manual_bearer`). The daemon
  resolves a bearer token at runtime-contract assembly time, caches it
  under `~/.animus/<repo-scope>/mcp-oauth-cache/<server>.json` (0600 perms
  on Unix, 60s skew margin), and injects `Authorization: Bearer <token>`
  into the `/mcp/additional_servers/<name>/headers` map alongside `url`
  and `transport`. Credentials are read from env vars named via `*_env`
  pointers; tokens never appear in YAML or logs. Validation rejects
  `oauth:` on stdio transports and surfaces actionable errors when
  required env vars or `token_url` are missing. See
  [docs/reference/workflow-yaml.md](docs/reference/workflow-yaml.md#http-transport-with-oauth-v055)
  for the full shape.
### Added

- **`feat(workflow)`: basic eval framework for phase quality gates
  (YAML + runner library; runtime enforcement deferred).** Phase
  definitions can declare an `evals:` block listing `command` and
  `llm_judge` checks. The runner library (`animus_runtime_shared::evals`)
  executes them, scores against `pass_threshold`, and routes failures to
  `rework` (re-execute the phase, up to `max_reworks`) or `block` (pause
  for manual approval). Each check emits an `animus.eval.v1` record
  (check_id, kind, passed, duration_ms, exit_code, output_excerpt) ready
  for the workflow decision log.

  **Runtime status:** the YAML/config/validator/runner ship in this
  release. The workflow-runner call site that invokes `run_evals` between
  phase output and phase advance lives in
  `launchapp-dev/animus-workflow-runner-default` (out-of-tree per the
  v0.5.1 fold-in) and is pending its next release. Until that pin bumps,
  configured `evals:` blocks parse and validate but a phase still
  advances regardless of check results — author/test now, do not yet
  rely on it as a production trust gate. See
  [docs/reference/workflow-yaml.md#evals-experimental--runtime-enforcement-deferred](docs/reference/workflow-yaml.md#evals-experimental--runtime-enforcement-deferred)
  for the YAML surface.
### Added (v0.5.5 cost governance)

- **Workflow + phase budget caps in YAML.** New `budget:` block on
  `WorkflowDefinition` and on rich phase entries declares
  `max_tokens`, `max_cost_usd`, and `on_exceed: pause|fail|warn`.
  Workflow caps subsume phase caps; phase rework resets the per-phase
  counter. Validated by `validate_workflow_config`.
- **Cost aggregator over existing `AgentRunEvent::Metadata` events.**
  `crates/orchestrator-cli/src/services/cost/` folds vendor-reported
  cost + token counts into a per-(workflow_run_id, phase_id) rollup
  persisted to `~/.animus/<scope>/cost-state.v1.json` (schema
  `animus.cost-state.v1`). When the provider omits `cost`, a small
  per-model USD rate table estimates it (see
  `docs/reference/configuration.md#cost-governance-and-model-rates-v055`).
- **`animus cost` CLI.** `summary [--since 24h]`, `workflow <ID>`,
  `top [--by tokens|cost] [--limit N]`, `trends [--window day|week|month]`.
  All commands honor `--json`.
- **Budget-exceeded decision records.** When a cap is crossed,
  `cap_check::check_caps` produces a `BudgetExceededRecord` (schema
  `animus.budget-exceeded.v1`) intended for `decisions.jsonl`. The
  workflow runner hook that translates these into actual workflow
  pauses lands in the `launchapp-dev/animus-workflow-runner-default`
  plugin in a follow-up release; this commit ships the data, schemas,
  and read surface.

## [0.5.4] - 2026-06-05

### Removed (v0.5.4 surface-shrink)

- **`agent-runner` sidecar deleted.** The standalone `crates/agent-runner/`
  binary (~8 kLOC) was removed entirely. `animus agent {run, status, cancel,
  control}` now talks directly to provider plugins via
  `orchestrator_plugin_host::session::SessionBackendResolver` (the same
  surface the daemon and tests have used since v0.4.12). The Unix-domain
  socket bridge, shared-secret auth handshake, runner lockfile, and the
  in-process supervisor in `orchestrator-core::services::runner_helpers`
  were deleted at the same time. The published
  `launchapp-dev/animus-protocol::animus-agent-runner-protocol` crate is
  bumped to v0.1.1 and marked deprecated.
- **`animus runner restart-stats` removed.** It read daemon-event records
  for a sidecar that no longer exists.

### Changed (v0.5.3)

- **`animus runner health`** now reports one entry per discovered provider
  plugin (`plugin_name`, `provider_tool`, `binary_path`, `installed`)
  alongside daemon health. The rendered output adds
  `provider_plugins_healthy: bool`; `runner_connected` is still emitted on
  the wire as `false` for back-compat.
- **`DaemonHealth`** gains `provider_plugins_healthy: bool`. The legacy
  `runner_connected: bool` and `runner_pid: Option<u32>` fields are kept
  for wire back-compat and now always serialize as `false` / `None`.
- **`animus agent control`** returns `unavailable` (exit code 5) under the
  v0.5.3 provider-only model — there is no in-process pool to address; the
  CLI emits an actionable error rather than a silent success.

### Added

- **`feat(cli)`: configurable auto-update via `animus self update`.** Adds GitHub
  release polling on CLI startup with four modes (`off` / `notify` / `prompt` /
  `auto`), a configurable ISO-8601 `check_interval` (default `P1D`), channel
  filtering (`stable` / `prerelease`), and an atomic same-directory binary swap.
  Env overrides: `ANIMUS_AUTO_UPDATE_MODE` (mode), `ANIMUS_AUTO_UPDATE_DISABLE`
  (short-circuit to off). The new `animus self update` command supports
  `--check-only`, `--force`, `--prerelease`, and `--yes`. The startup check is
  fire-and-forget; the subcommand it runs alongside is never blocked by network
  latency.
- **`feat(cli)`: opt-in anonymous metrics with first-run prompt (v0.5.3).**
  Adds a hard-opt-in event-counter telemetry surface. First-run prompt on
  `animus init` and `animus daemon start`; persisted under
  `.animus/config.json`. Events are bounded enums (`workflow_started`,
  `workflow_completed`, `plugin_installed`, `daemon_started`, `error_hit`,
  `cli_invoked`, `update_applied`) with closed tag enums — no code, no
  file paths, no repo names, no prompts, no credentials reach the
  payload. Disabled by default if no TTY is available. Kill switch:
  `ANIMUS_METRICS_DISABLE=1`. New CLI surface: `animus metrics {status,
  enable, disable, flush}`. Default endpoint
  `https://metrics.animus.dev/v1/events` is a placeholder pending
  product confirmation.
- **`feat(config)`: agent profiles now accept `system_prompt_file` to load the
  system prompt from an external UTF-8 file.** The path resolves relative to
  the source YAML's parent directory (absolute paths supported). The contents
  are inlined verbatim into `system_prompt` at config-compile time, so the
  compiled `workflow-config.v2.json` stays self-contained and daemon runtime
  behavior is unchanged. `system_prompt` and `system_prompt_file` are mutually
  exclusive; missing files, non-UTF-8 contents, and files larger than 1 MiB
  fail compile with the resolved path in the error message.
- **`feat(config)`: workflow YAML `mcp_servers:` entries can declare HTTP MCP
  servers via `transport: http` + `url:`.** Validation enforces exactly one
  transport per entry (http requires `url:` with an `http://`/`https://`
  scheme; stdio requires `command:`), rejects mixed shapes, and the URL flows
  through `/mcp/additional_servers` so agents reach remote MCPs without a
  bridge script. Env-var interpolation works on `url:` and `command:` fields.

### Added (v0.5.7)

- **Plugin kind translator**: `subject_backend` and `provider`
  plugin collisions are now solved daemon-side instead of requiring each
  plugin to be parametric. Every install carries a user-facing
  `installed_kind` recorded in `.animus/plugins.lock` that may differ
  from the plugin-native `native_kind`. The `SubjectRouter` rewrites
  outbound methods (`<installed_kind>/<verb>` → `<native_kind>/<verb>`)
  and inbound top-level `kind` fields in responses, so plugins stay
  unmodified. Install pipeline auto-increments on `subject_kind:*` /
  `provider_tool:*` collisions (`task` → `task-2` → `task-3`); explicit
  override via the new `animus plugin install --as-kind <KIND>` flag.
  Preflight summarization consults `plugins.lock` so renamed plugins
  report against their `installed_kind`. New `animus plugin doctor`
  subcommand shows every preflight role with its installed claimants
  and flags duplicates explicitly. See
  [docs/architecture/plugin-kind-translator-v0.5.7.md](docs/architecture/plugin-kind-translator-v0.5.7.md)
  for the architecture and the explicit deferral of nested-kind rewriting.

## [0.4.19] - 2026-05-29

Follow-up hotfix to v0.4.18. The previous release wired the subject
plugin fallback into `FileServiceHub` and the CLI dispatch helpers,
but the parallel `InMemoryServiceHub` paths were still wired without
a plugin fallback, and plugin discovery only scanned the project-local
install dir — global installs under `~/.animus/plugins/` required a
manual `$ANIMUS_PLUGIN_DIR` override or a symlink to be picked up.

### Fixed

- **`fix(services)`: `InMemoryServiceHub::subject_resolver()` and
  `project_adapter()` now activate `.with_plugin_fallback()` when a
  project root is supplied** via the new
  `InMemoryServiceHub::with_project_root()` builder, matching the
  production `FileServiceHub` behavior so plugin-backed subject
  resolution stays symmetric between the two hub variants. Three
  regression tests assert that the InMemoryServiceHub skips the
  plugin fallback when no project_root is set, engages it when one
  is supplied, and that `FileServiceHub` continues to engage it.
- **`fix(plugin-host)`: plugin discovery now scans
  `~/.animus/plugins/` unconditionally** instead of only when
  `$ANIMUS_PLUGIN_DIR` is set. This restores the canonical
  precedence chain: explicit `plugins.yaml` registry entries first,
  then project-local `<project_root>/.animus/plugins/`, then the
  global install dir (`$ANIMUS_PLUGIN_DIR` override or
  `~/.animus/plugins/` default), then `$ANIMUS_PLUGIN_PATH`, then
  optionally `$PATH`. Duplicate plugin names dedupe to the first
  source that produced them, so project-local installs continue to
  override global ones deterministically. Four new discovery tests
  cover the global-only discovery, project-local-vs-global
  precedence, both-locations co-existence, and `$ANIMUS_PLUGIN_DIR`
  overriding the default install location.

## [0.4.18] - 2026-05-29

Hotfix release. Real-world reproduction in a v0.4.x project hit a
migration gap that wasn't covered by the v0.4.12-through-v0.4.16
audit waves: `animus workflow run` could not dispatch against any
task or requirement subject backed by a plugin (not by an in-tree
adapter — which v0.4.12 deleted).

### Fixed

- **`fix(workflow-run)`: subject context now falls through to installed
  `subject_backend` plugins on both sync + daemon dispatch paths.**
  `SubjectAdapterRegistry::resolve_subject_context` previously returned
  the in-tree `BuiltinTaskSubjectAdapter`'s "task not found" error
  without consulting the configured `plugin_fallback`, and the CLI
  dispatch helpers (`resolve_workflow_run_dispatch{,_from_input}`)
  called `hub.tasks().get()` / `hub.planning().get_requirement()`
  directly without ever routing plugin-owned subjects through any
  `subject_backend` plugin. Both paths now retry via the configured
  `SubjectFallback`/`hub.subject_resolver()` chain so plugin-owned
  task and requirement subjects dispatch correctly.
  - `PluginSubjectFallback` probes both canonical (`animus.task`) and
    bare (`task`) kind aliases across candidate plugins, canonical
    first to avoid nondeterministic shadowing under HashMap iteration.
  - `ensure_execution_cwd` recognizes plugin-resolved contexts via a
    new `SUBJECT_ATTR_PLUGIN_RESOLVED` attribute marker, so in-tree
    tasks keep their managed worktrees while plugin-resolved tasks
    fall back to `project_root`.
  - 9 new unit tests in `subject_adapter.rs::tests` covering both
    the resolve + ensure_execution_cwd paths, including the marker
    semantics.
  - 4 codex review rounds; final clean. P1 catches: broad fallback
    masked worktree errors, `task=None` heuristic misclassified
    in-tree tasks after `.take()`, plugin probe missed the bare-kind
    alias, HashMap iteration order could shadow canonical plugin.

## [0.4.17] - 2026-05-28

Ecosystem expansion — all reference packs and SDKs extracted to standalone
`launchapp-dev/animus-*` repos so agencies + community can fork
individually. Six communication-channel trigger plugins shipped to enable
end-to-end automation loops (inbound + outbound) for Slack, Discord,
WhatsApp, Telegram, Email, and SMS.

### Changed

- **`chore`: extract `packs/*` and `sdk/*` to standalone launchapp-dev
  repos.** All 7 reference packs and both SDKs (TS + Python) are now their
  own public repos at launchapp-dev, all tagged v0.1.0:
  - `launchapp-dev/animus-plugin-sdk-ts`
  - `launchapp-dev/animus-plugin-sdk-py`
  - `launchapp-dev/animus-pack-customer-support`
  - `launchapp-dev/animus-pack-marketing-outreach`
  - `launchapp-dev/animus-pack-sales-pipeline`
  - `launchapp-dev/animus-pack-engineering-backlog`
  - `launchapp-dev/animus-pack-recruiting-pipeline`
  - `launchapp-dev/animus-pack-organization-meetings`
  - `launchapp-dev/animus-pack-ecommerce-fulfillment`
  In-tree `packs/<name>/` and `sdk/<lang>/` replaced with redirect READMEs +
  index files at `packs/README.md` and `sdk/README.md`. Stubs will be
  removed once `animus pack install` + marketplace discovery are wired up.

### Added (ecosystem repos — out of tree)

- **Communication channel trigger plugins** under `launchapp-dev/`. Each is
  a dual-role plugin: `trigger_backend` for inbound events + custom RPC
  methods for outbound replies, so workflow phases can route fully
  end-to-end through a single plugin. All Node 20+ TS except Slack (Rust,
  existed; enhanced with outbound).
  - `animus-trigger-slack` v0.2.0 — Socket Mode inbound + `chat.postMessage` / `chat.postEphemeral` / `send_dm` outbound (Rust)
  - `animus-trigger-discord` v0.1.0 — gateway intents (mentions + DMs) + `send_channel_message` / `send_dm` / `send_embed` (Node TS, discord.js)
  - `animus-trigger-whatsapp` v0.1.1 — Cloud API webhook (incl. signature validation) + `send_text` / `send_template` / `send_media` (Node TS)
  - `animus-trigger-telegram` v0.1.0 — polling + webhook modes + `send_message` / `send_photo` / `edit_message_text` / `answer_callback_query` / `set_chat_action` (Node TS, grammy)
  - `animus-trigger-email` v0.1.1 — IMAP IDLE inbound (mailparser) + nodemailer SMTP outbound with correctly-threaded replies (In-Reply-To + References) (Node TS)
  - `animus-trigger-sms-twilio` v0.1.0 — Twilio webhook with `X-Twilio-Signature` validation + `sms/send` / `sms/send_mms` (Node TS)

### Fixed

- **`fix(protocol-spec)`: `trigger/event` wire shape was nested in `spec.md`
  but flat in the host runtime.** The host's `trigger_supervisor.rs:289`
  deserializes `notification.params` directly into the flat `TriggerEvent`
  struct (`event_id`, `trigger_id`, `payload`, `action_hint`, `subject_id`,
  `subject_kind`). The spec was showing `{ id, event: {...} }` which never
  matched any version of the host code. Spec.md §7.3 + §11.1 rewritten to
  match the deployed wire shape. **Deeper drift flagged for v0.5
  follow-up**: `animus-trigger-protocol` crate + `animus-plugin-runtime::drive_trigger_stream`
  in the launchapp-dev/animus-protocol repo still emit the old nested shape;
  existing Rust trigger plugins built on `trigger_backend_main` may be
  silently dropped by the host today.

### Internal

- Wire-shape mismatch caught + fixed in 2 of the comm plugins
  (whatsapp v0.1.0 → v0.1.1, email v0.1.0 → v0.1.1) that initially followed
  the stale spec. Other 3 Node plugins (discord, telegram, sms-twilio) and
  the slack enhancement all verified against `trigger_supervisor.rs:289`
  source-of-truth and shipped with flat shape from v0.1.0. - 2026-05-28

P3 housekeeping complete + **vertical pack expansion** (6 new reference
packs) + the **Python plugin SDK**. The audit backlog is empty through
P3. The workflow-engine framing now has 7 reference packs spanning
engineering, customer support, marketing outreach, sales pipeline,
recruiting, organization meetings, and ecommerce fulfillment.

### Added (this release)

- **`feat(sdk-py)`: Python plugin SDK skeleton** at `sdk/python/animus_plugin_sdk/`.
  NDJSON-RPC stdio wire, handshake, `define_plugin(...)`, role Protocols,
  PEP 561 `py.typed`, mypy strict + ruff clean. 1609 src + 544 test LOC.
  subject_backend role fully wired (same scope as the v0.4.14 TS SDK).
- **`feat(packs)`: 6 new vertical reference packs** under `packs/`:
  marketing-outreach (4-phase prospect triage), engineering-backlog
  (6-phase impl+codex-review+test+PR), sales-pipeline (5-phase BANT
  qualification), recruiting-pipeline (screening + debrief), organization-meetings
  (per-meeting + weekly rollup), ecommerce-fulfillment (process-order +
  handle-return).

### Fixed (this release)

- **`fix(packs)`: namespace phase IDs to avoid cross-pack collisions** in
  the daemon's global `phase_definitions` map. `flag_for_review` was used
  by 3 packs; `enrich` by 2. Renamed `ticket_*` / `prospect_*` / `lead_*`.

### Internal (this release)

- **`chore(workflow-runner)`: rename binary `ao-workflow-runner` → `animus-workflow-runner`**
  with 3 layers of back-compat (install-time symlink + Dockerfile symlink
  + daemon resolver fallback). Migration doc at `docs/migration/v0.4-to-v0.5.md`.
- **`chore`: sweep stale `ao.*` references from user-facing strings** —
  76 files. MCP `ao://project/*` resource URIs still advertised for client
  back-compat.
- **`chore`: strict clippy workspace-clean** — 20 violations fixed. CI
  now runs clippy with `-D warnings` + `cargo check --workspace --all-targets`.
  Added `protocol_drift_subject` CI gate alongside plugin-protocol.

### Internal

- **`feat(protocol)`: add `TransportBackend` + `WebUi` variants to `PluginKind`.**
  The typed `PluginKind` enum in `animus-plugin-protocol` previously only
  modeled `provider` / `subject_backend` / `task_backend` /
  `trigger_backend` / `log_storage_backend` / `custom`, so first-party
  `transport_backend` (consumed by `ops_web.rs`) and `web_ui` (legacy
  `partition_transport_plugins` path) plugins landed in
  `PluginKind::Other(...)` for any caller using the typed helper.
  Adds `PLUGIN_KIND_TRANSPORT_BACKEND` / `PLUGIN_KIND_WEB_UI` constants
  with matching enum variants, round-trip serde tests, and an updated
  `protocol_drift` allowlist. Wire shape is unchanged. Follow-up: bump
  the upstream `launchapp-dev/animus-protocol` crate to mirror the new
  variants and remove the allowlist entries.
### Changed

- **`chore(workflow-runner)`: rename binary `ao-workflow-runner` → `animus-workflow-runner` with back-compat (P3 housekeeping).**
  Closes the v0.4.0 known-follow-up that left the legacy `ao-*` bin name in
  place. The `[[bin]]` entry in `crates/workflow-runner-v2/Cargo.toml` now
  emits `animus-workflow-runner`; `Dockerfile`, `scripts/install.sh`, and
  `.github/workflows/release.yml` all bundle the new name. Back-compat:
  - The daemon's runner resolver (`build_runner_command_from_dispatch.rs`)
    probes `animus-workflow-runner` first and falls back to
    `ao-workflow-runner`. Each name is resolved via `current_exe` sibling
    first, then `PATH` search, so a daemon in `~/.cargo/bin/` still finds
    a runner that lives in `~/.local/bin/`. A `tracing::warn!` line
    nudges operators to reinstall when the legacy binary is selected.
  - `scripts/install.sh` creates a back-compat `ao-workflow-runner`
    symlink alongside the new binary and also accepts already-published
    archives that ship the legacy filename (mapping `ao-workflow-runner`
    onto the new name during extraction). `Dockerfile` adds the same
    symlink inside the image. v0.4.x daemon PIDs still spawning the
    legacy name keep working until they're restarted on the new lookup.
  - Migration steps live at `docs/migration/v0.4-to-v0.5.md`.
### Fixed

- **`chore(lint)`: strict clippy now green workspace-wide (audit J4).**
  Resolved every `-D warnings` violation `cargo animus-lint-strict` surfaced —
  `unnecessary_filter_map`, `derivable_impls`, `single_char_pattern`,
  `while_let_on_iterator`, `chars_last_cmp`, `if_same_then_else`,
  `io_other_error`, `default_constructed_unit_structs`, `vec_init_then_push`,
  `manual_assert`, `useless_vec`, `useless_conversion`,
  `unwrap_or_else_on_some`, `nonminimal_bool`, `err_expect`, plus six
  `#[ignore]`-without-reason entries that already had explanatory comments
  above them. The intentional std-`Mutex`-across-`.await` patterns in the
  doctor / plugin-tools / e2e test modules get a scoped
  `#[allow(clippy::await_holding_lock)]` with a note: the contended
  resource is the process env, not the lock.

### Changed

- **`ci(workspace)`: gate clippy on `-D warnings` + check the whole
  workspace in one cached job (audit J5).** Adds `-D warnings` to
  `cargo clippy` in `rust-workspace-ci.yml` so any future regression
  fails the PR. Replaces the four-package `cargo-check` matrix with a
  single `cargo check --workspace --all-targets` job — one cache reuse
  instead of four, no manual sync against `docs/guides/ci-cd.md`.
- **`ci(drift)`: subject-protocol drift now gated alongside
  plugin-protocol (audit J6).** Adds `protocol_drift_subject` to
  `crates/orchestrator-plugin-host/tests/protocol_drift.rs` and exports
  `ANIMUS_STANDALONE_SUBJECT_PATH` from `protocol-drift.yml` so any
  divergence between the in-tree `animus-subject-protocol` and the
  standalone `launchapp-dev/animus-protocol/animus-subject-protocol`
  fails the build. The existing standalone-ahead entries are documented
  in the test's `expected_drift` allowlist for the next re-sync to drain.

## [0.4.15] - 2026-05-28

Second audit-remediation release. Lands the 8 remaining P2 findings the
external audit surfaced after v0.4.14 — MCP scoping, project-local
plugin discovery in worktrees, logging scope unification,
log_storage_backend actually dispatched, schedule/trigger budget split,
skill path migration, and agent-host skill trust hardening. Plus
docs-agent cleanup that accumulated across v0.4.13 → v0.4.15.

### Fixed

- **`fix(mcp)`: per-project plugin registry cache (audit H1).** `AoMcpServer`
  used to memoize a single `PluginRegistry` for the lifetime of the server.
  An MCP `animus.plugin.call` against `/repo/a` would warm the cache, and a
  later call against `/repo/b` would silently reuse `/repo/a`'s discovered
  plugin set — running the wrong binary. The cache is now a
  `HashMap<PathBuf, Arc<Mutex<PluginRegistry>>>` keyed by the canonical
  `project_root` (`canonicalize_lossy`). When no override is supplied, the
  server's `default_project_root` is canonicalized and used as the key —
  there is no separate sentinel. Install/uninstall/marketplace-update all
  clear the entire cache so the next call rediscovers freshly mutated
  binaries.
- **`fix(mcp)`: plumb project_root + force_rewrite_lockfile through MCP
  install/uninstall (audit H2).** `animus.plugin.install` and
  `animus.plugin.uninstall` used to pass `project_root: None` to
  `run_plugin_install` / `run_plugin_uninstall`, so the install-time
  lockfile + audit log silently fell through to `~/.animus/plugins.lock`
  instead of the project-local `.animus/plugins.lock`. Both tools now
  accept an optional `project_root` field (defaulting to the server's
  configured root) and forward it. `animus.plugin.install` also accepts
  `force_rewrite_lockfile: bool` to match the CLI's v0.4.14 G2 fail-closed
  escape hatch. Tool descriptions updated.
- **`fix(session)`: Carry `project_root` through `SessionRequest` so worktree-bound tasks find project-local provider plugins.** When a task ran inside a managed worktree (`~/.animus/<scope>/worktrees/...`), `spawn_session_process` discovered provider plugins via `Path::new(cwd)` and dropped `project_root` from the outgoing `SessionRequest`. Plugins installed at `<project_root>/.animus/plugins/animus-provider-*` therefore disappeared the moment a workflow switched into its worktree, breaking the plugin author guide's project-local-plugin promise. The supervisor now plumbs `project_root` through to `spawn_session_process` / `build_session_request`, the resolver scans `<project_root>/.animus/plugins/` instead of `<cwd>/.animus/plugins/`, and the field rides over the JSON-RPC `agent/run` payload to the plugin. Negative-cased test in `agent-runner` proves the fix.
- **`fix(logging)`: delegate `Logger::for_project`/`for_run`/`logs_dir` to `protocol::scoped_state_root` for canonical scope isolation.** `orchestrator-logging` had its own weaker scope resolver that scanned `~/.animus` for directories starting with the raw basename and compared `.project-root` to the raw input string with no canonicalization. Callers passing a relative path, a symlinked path, or a not-yet-created scope fell back to `<project>/.animus/logs/`, contradicting `docs/reference/data-layout.md` which puts logs at `~/.animus/<repo-scope>/logs/events.jsonl`. It also missed the G1 v0.4.14 hardening, so two clones of the same git origin could collide. Replaced the local resolver with `protocol::scoped_state_root(project_root)` — same canonical-basename hashing, same-origin collision guard, same moved-clone reclaim path. Added regression tests covering noncanonical input, distinct same-origin clones, and end-to-end `for_project`/`for_run` writes landing under `~/.animus/<scope>/logs/`. (audit P2 cleanup)
- **daemon**: actually dispatch `log_storage_backend` plugin for
  `log_storage/store` + `log_storage/query` (P2 audit follow-up). When a
  `log_storage_backend` plugin is installed the daemon now spawns +
  handshakes it at startup via `PluginSpawnOptions::for_manifest`
  (matching subject/trigger/health), installs a process-global
  `LogStorageHandle`, and forwards every `DaemonEventLog::append` record
  to the plugin as a `log_storage/store` request (the
  `animus-log-storage-protocol` wire shape) while still writing the
  in-tree `daemon-events.jsonl` (tee policy — preserves existing
  `daemon events` / MCP `daemon.events` poll readers). The `daemon/logs`
  control endpoint now issues `log_storage/query` against the plugin
  instead of reading the in-tree file, so `animus logs tail` reflects the
  installed backend. Plugin failures degrade to a `tracing::warn!`
  rather than killing the daemon.
- **`fix(scheduler)`: shared decrementing tick budget across schedule + trigger dispatch.**
  Audit P2 finding: `run_project_tick` previously passed the SAME headroom value to the
  schedule hook and the trigger hook, letting each path spend the full pool independently.
  With pool cap N=5, schedule could commit 3 dispatches and trigger would then drain 5
  webhook events — `ProcessManager` refused the over-budget spawns, leaving schedules
  marked `last_run` and webhook events dropped without runners. Now both hooks share a
  `&mut TickBudget` (claim via `try_take`, release on non-capacity spawn failure); failed
  schedules route through new `project_schedule_dispatch_missed` which increments a new
  `ScheduleRunState.missed_count` and leaves `last_run` untouched so the schedule re-fires
  on the next tick; webhook events are peeked from the queue and only popped after the
  spawn succeeds. (`run_project_tick.rs`, `default_project_tick_driver.rs`,
  `trigger_dispatch.rs`, `execution_projection.rs`, `dispatch_support.rs`,
  `schedule_state.rs`)
- **`fix(skill)`: project + user skill resolution honors the v0.4 `.animus/`
  naming contract.** The resolver and `animus skill install --path` previously
  read and wrote `.ao/skills/` even though every doc told users to drop SKILL.md
  under `.animus/skills/`. Result: skills placed at the documented location
  were silently undiscovered. `project_markdown_skills_dir`,
  `user_markdown_skills_dir`, and the matching YAML-definition dirs
  (`project_skills_dir`, `user_skills_dir`) in
  `crates/orchestrator-config/src/skill_scoping.rs` now resolve under
  `.animus/`. The MCP tool description for `animus.skill.list` and the
  `cli_skill_lifecycle` regression test both flipped to the new layout.
  Operators with files still under `.ao/skills/` OR
  `.ao/config/skill_definitions/` get a single one-shot `warning:` line per
  process per legacy path pointing at the new location; running
  `animus skill migrate-from-ao` moves entries for both the markdown skills
  dir and the YAML skill-definitions dir, drops a `.migrated-from-ao` marker
  in each destination so the warning stops, and refuses to clobber non-empty
  targets. (audit I1)

### Security

- **`fix(skill)`: agent-host skill stripper also clears `model` and
  `timeout_secs`.** `strip_structural_fields_for_agent_host` in
  `crates/orchestrator-config/src/skill_scoping.rs` enforces the trust
  boundary for SKILL.md files discovered under `~/.claude/skills/`,
  `~/.codex/skills/`, etc. The strip list previously covered
  `tool_policy`, `extra_args`, `env`, `mcp_servers`, `adapters`,
  `codex_config_overrides`, and `capabilities`, but left `model` and
  `timeout_secs` flowing through. A hostile SKILL.md could therefore force
  the runner onto a cheaper / less-capable model (or claim a model the
  workspace doesn't have access to) and monopolize the runner with an
  arbitrarily long timeout. Both fields are now stripped, and a new
  field-coverage test (`agent_host_strip_covers_every_runtime_field`)
  enumerates every serializable `SkillDefinition` field and fails the build
  if a new field is added without an explicit allowlist / strip-list
  classification. `docs/architecture/skill-system.md` updated to match.
  (audit I2)

## [0.4.14] - 2026-05-27

Audit remediation release. External audit + parallel review across v0.3.2 →
v0.4.13 surfaced 30+ findings (security, durability, JSON contract,
control-protocol parity, release pipeline). This release lands ~20 of them
across 14 merge commits + adds the TypeScript plugin SDK skeleton and the
first non-coding reference pack (customer support).

### Added

- **`feat(sdk-ts)`: TypeScript plugin SDK skeleton** at `sdk/typescript/animus-plugin-sdk/`.
  Wire (NDJSON-RPC over stdio), handshake, `definePlugin({ kind, impl, ... })` entrypoint.
  subject_backend role fully wired (list/get/create/update/status/next); provider,
  trigger_backend, transport_backend, log_storage_backend roles export type-only
  interfaces and throw on dispatch (to prevent host routing to unwired plugins).
  28 vitest tests.
- **`feat(sdk-ts)`: TypeScript types codegen from JSON Schemas.** New `scripts/codegen.mjs`
  generates `src/types/{plugin,subject}-protocol.ts` (32 types total) from
  `schemas/animus-{plugin,subject}-protocol/_all.json` via `json-schema-to-typescript`.
  `pnpm run codegen:check` enforces no-drift. Enums render as
  `"known" | (string & {})` to preserve autocomplete + accept arbitrary
  strings for the Rust `Other(String)` forward-compat case.
- **`feat(examples)`: hello-world TS plugin** at `examples/plugin-hello-ts/`.
  ~60-line `src/index.ts`, complete README install workflow, bash launcher,
  local-install script. The reference for what "plugin authoring in TypeScript"
  looks like end-to-end.
- **`feat(packs)`: first-party customer-support reference pack** at
  `packs/customer-support/`. 4-phase workflow (classify → draft_response →
  summarize_for_human → flag_for_review), 5 realistic sample tickets, full
  README + architecture + customizing docs, idempotent setup script.
  Proves the workflow-engine framing for a non-coding vertical and serves
  as the agency starter fork for support automation.

### Fixed

- **`fix(cli)`: emit JSON envelope for clap argparse failures when `--json` is set.** `Cli::parse()` exits the process directly on bad argv, bypassing every downstream `emit_cli_error` call. `main` now pre-scans argv for `--json`, switches to `Cli::try_parse()`, and on parse error emits an `animus.cli.v1` invalid_input envelope with `error.details.stage = "parse"` plus the raw clap text under `details.raw`. Non-JSON callers keep clap's pretty-printed help text unchanged. Exit code stays 2. (audit Fix 3)
- **`fix(cli)`: `animus web serve --json` returns JSON envelope when no transport plugins installed.** `bail_with_install_help` wrote multi-line human help to stderr and called `std::process::exit(2)`, never giving the JSON envelope path a chance to fire. Replaced with `missing_transport_plugins_error(json)` which routes through `CliError(InvalidInput)` — JSON callers get a single-line message plus `error.details.install_command` and `error.details.individual_plugins`; humans still see the multi-line help. (audit Fix 1)
- **`fix(cli)`: MCP tool error payloads now read the stderr envelope.** The production `build_tool_error_payload` and `batch_item_error_from_result` only checked `stdout_json`, so a properly-emitted `animus.cli.v1` error envelope on stderr was silently dropped. A test-only helper (`build_cli_error_payload`) handled stderr correctly, so the test suite was green against the wrong helper. Production helpers now share a `pick_envelope_error` that prefers `stderr_json` (canonical error channel per `docs/reference/json-envelope.md`) over `stdout_json`. Added production-path regression tests. (audit Fix 2)
- **`fix(cli)`: `workflow run --sync --json` keeps stderr silent.** Phase/progress emitters in `ops_workflow::execute` accepted `_json` but ignored it, spraying ANSI-colored progress to stderr in `--json` mode. Now gated on `if json { return }` so the JSON envelope on stdout is the entire user-facing surface. (audit Fix 4)
- **`fix(cli)`: `init --walkthrough --json` no longer prompts in TTY.** The `interactive` flag was computed from TTY detection alone, so a Guided walkthrough in a TTY would block on `prompt_yes_no` even when `--json` was set (silent hang for scripted callers). `interactive` now requires `!json`, so the JSON envelope path is the entire surface in JSON mode. (audit Fix 5)
- **`fix(durability)`: propagate phase-checkpoint write failures in dispatch
  to preserve the crash-replay invariant.** Three failure points in
  `run_workflow_phase_attempt` (`crates/workflow-runner-v2/src/phase_executor.rs`)
  previously logged and continued: the pending-checkpoint write before
  runner dispatch, the post-dispatch flip to `Running`, and the terminal
  `Completed`/`Failed` mutation. Recovery scans `Running` checkpoints to
  shield in-flight phases across daemon restart; dispatching without a
  durable checkpoint risked silently losing the work AND double-dispatching
  side-effecting phases on a re-tick. All three now return explicit
  errors using typed sentinels: `DispatchRetryableError` for the
  pre-runner case (no side-effecting work happened yet — operator
  triage can distinguish via the `dispatch_retry` event discriminator)
  and `TerminalCheckpointError` for the post-runner cases. The
  sentinels are matched by the agent retry/failover loop, which
  REFUSES to redispatch a phase on those errors even when the I/O
  message overlaps the transient-runner classifier (eg "connection
  timed out" on a network-storage fsync) — this prevents the agent
  loop from re-running a phase whose side effects have already
  executed. All four cases terminally fail the workflow phase so
  downstream daemon reconciliation surfaces the failure correctly and
  orphan recovery skips the run; automatic next-tick retry is left for
  a follow-up because it would require scheduler changes outside this
  PR. Added a per-thread fault-injection seam
  (`phase_session::test_fault`) with 5 regression tests covering each
  failure mode plus the typed-sentinel detection.
- **`fix(durability)`: persist failure now fails the phase instead of
  silently advancing.** `workflow_execute.rs` previously did
  `let _ = persist_phase_output(...)` after a successful phase and then
  advanced workflow state — a persist failure would leave the workflow
  ahead of its durable completion marker. The resumed-agent path in
  `daemon_run.rs` already treated this same failure as fatal. The
  normal-execution path now calls `fail_current_phase` when persistence
  fails (with a `persist_failed` phase_status discriminator so operators
  can distinguish from real phase failures), preventing the workflow from
  ever advancing past a phase whose completion oracle isn't on disk.
  Added a fault-injection seam (`phase_output::test_fault`) plus a
  behavioral test that the workflow's `current_phase_index` does not
  change when persist fails and the workflow ends in
  `WorkflowStatus::Failed`.
- **`fix(durability)`: graceful drain of subprocess workflow_events on
  shutdown AND normal-lifecycle process completion.**
  `SubprocessEventPipe::shutdown` previously aborted the reader task
  unconditionally, which could drop the final batch of events the runner
  emitted right before exiting (writer flushed bytes into the socket
  buffer; reader had not yet consumed them when abort fired). The reader
  loop now responds to a shutdown notification by entering a bounded-wait
  drain pass (50 ms accept-queue probe + unbounded EOF read, capped
  overall by the 250 ms `SHUTDOWN_DRAIN_DEADLINE`) so a runaway plugin
  still cannot stall daemon shutdown. The deadline path now keeps the
  `JoinHandle` borrowed via `&mut task` across the timeout so a leaked
  reader is actually `abort()`-ed on fallback (dropping a `JoinHandle`
  only detaches the task). `ProcessManager::check_running` now takes and
  awaits `event_pipe.shutdown()` on the normal-completion, timeout, and
  probe-error paths so the drain runs in the production lifecycle, not
  only on explicit pipe-shutdown. New regression test writes 3 events,
  closes the writer, immediately calls shutdown, and asserts all 3
  events reach the broadcaster.
### Security

- **`fix(session-host)`: enforce provider plugin env allowlist via the
  plugin manifest at the RPC layer.** `PluginSessionBackend::dispatch`
  was forwarding every key in `SessionRequest.env_vars` straight through
  to the plugin's spawn-time `env_allowlist`, copying them into the
  per-call RPC `env` param AND embedding the full merged launch env
  under `extras.runtime_contract.cli.launch.env`, bypassing the
  manifest gate documented in `docs/guides/plugin-author-guide.md` § 9.
  A provider plugin whose manifest declared no `env_required` could
  still receive the runner's sanitized launch env wholesale (e.g.
  `OPENAI_API_KEY` reaching a Claude plugin that never asked for it).
  Fix intersects request env keys against
  (`PLUGIN_BASE_ENV_ALLOWLIST` ∪ `manifest.env_required`) at the
  dispatch boundary and applies the SAME filter to all three leak
  channels — spawn allowlist, RPC `env` param, and runtime_contract
  launch env. Six new regression tests cover empty-manifest scrubbing,
  manifest-declared pass-through, the RPC-param surface, and the
  runtime_contract surface (including no-op behaviour when the path
  is absent or wrong-shape).

### Docs

- **`docs(security)`: align signature-policy default text with code
  (`Warn`, not `Strict`).** Several places — the `PolicyMode` /
  `PolicyMode::default_for_install` rustdoc, the `SignaturePolicy::default_install`
  rustdoc, `effective_policy_mode` in `ops_plugin.rs`, the v0.4.12 entry
  in `CHANGELOG.md`, and the "v0.4.12 temporary default" section in
  `docs/reference/security.md` — claimed v0.4.13 would flip the
  install-time default back to `Strict`. The code at
  `signature_verifier.rs::default_for_install` continues to return
  `Warn` intentionally; the docs were stale. Text now reflects the
  shipped behaviour and the recommended `--signature-policy strict`
  opt-in for production. No behaviour change.
- **`fix(web)`: drive transport protocol lifecycle (`transport/start` +
  `transport/shutdown`) per spec.** `animus web serve` / `animus web open`
  only called `initialize` and (optionally) `transport/info` on installed
  `transport_backend` plugins, then waited in the foreground. The
  animus-protocol v0.1.13 spec (§13) requires the host to issue
  `transport/start` with a `TransportConfig` after `initialize` so the
  plugin can bind its listener, and `transport/shutdown` before process
  exit so it can drain in-flight requests. The ops_web path now drives
  the full lifecycle: after `initialize` it sends `transport/start` with
  the control socket path + project root, and the new
  `PluginHost::shutdown_transport()` helper issues `transport/shutdown`
  (with a bounded 5 s timeout) before the generic `shutdown` RPC. Legacy
  plugins that pre-date the lifecycle (current launchapp-dev
  `animus-transport-http`, `animus-transport-graphql`, and
  `animus-web-ui` bind inside `initialize`) respond with
  METHOD_NOT_FOUND / METHOD_NOT_SUPPORTED and are handled gracefully
  with a deprecation warning so the v0.4.x web surface keeps working
  while the ecosystem upgrades. New plugin-host tests in
  `tests/concurrency.rs` assert the spec ordering
  (`initialize` → work → `transport/shutdown` → `shutdown`) and the
  legacy METHOD_NOT_FOUND swallow.
- **`fix(workflow)`: plumb `--var KEY=VALUE` through the control wire.**
  `animus workflow run --task-id ... --var FOO=bar --json` was silently
  dropping the user-supplied vars when the daemon was running because the
  control-path `WorkflowRunRequest` was sent with `params: Default::default()`.
  Vars now round-trip via `params["vars"]` and reach
  `WorkflowRunInput::with_vars` on the daemon side, matching the local path.
  Same fix applies to `workflow_execute` over the control wire.
- **`fix(queue)`: stop silently swapping `--workflow-ref` for the project
  default when the daemon is running.** `animus queue enqueue --task-id ...
  --workflow-ref custom --json` was routing through the wire-side
  `queue/enqueue` that only carries `task_id + priority`, then the daemon
  loaded the default workflow ref. Now the CLI degrades to the local path
  whenever `--workflow-ref` is set so the user's requested workflow is
  honored. (Wire-side fix needs a new `workflow_ref` field on
  `QueueEnqueueRequest`, deferred until the external protocol crate gains it.)
- **`fix(plugin)`: stop silently ignoring `--include-system-path` over the
  control wire.** `animus plugin list/info/ping/call --include-system-path
  --json` was routing through the daemon (which hardcodes
  `include_system_path: false`), dropping the flag. The CLI now stays on the
  local discovery path when the flag is set.
- **`fix(daemon preflight)`: exit non-zero when required plugins are
  missing.** `animus daemon preflight` was printing `"ok": false` inside the
  payload but exiting 0 with `"ok": true` on the outer envelope —
  contradicting `docs/getting-started/installation.md`'s contract and
  silently passing CI gates. Now exits 2 (invalid_input) when required roles
  are missing, exit 1 (internal) on transient plugin-discovery failures, and
  exit 0 only when all required roles are satisfied. The error envelope
  carries the actionable `animus plugin install ...` fix message.
- **`fix(daemon preflight)`: surface plugin-discovery failures instead of
  swallowing them.** The standalone preflight previously used
  `discover_installed_plugins(...).unwrap_or_default()`, masking real
  discovery errors (broken install index, IO failures) as "no plugins
  installed". Now propagates discovery errors as exit code 1 with a clear
  message, distinct from "ran successfully and found gaps".
- **`fix(init walkthrough)`: don't ship the first-contact daemon in a
  degraded state.** `animus init --walkthrough --auto-start` was installing
  providers-only and then booting the daemon with `--skip-preflight`,
  hiding missing subject/transport backends from the operator. Now installs
  the full required-role set (`--include-subjects --include-transports`)
  AND drops `--skip-preflight` from the daemon spawn so any leftover gaps
  surface as actionable preflight errors before the daemon boots.
- **`fix(protocol)` [P1]: isolate scopes by canonical root even when origin
  matches.** `protocol::scoped_state_root` first hashes the canonical project
  root into a `<repo-name>-<12 hex>` scope, but if that directory did not yet
  exist it fell back to `find_existing_scope_by_origin`, which would return any
  scope sharing the same git remote URL. Two distinct clones of the same repo
  could therefore alias onto one scope and silently share `workflow.db`, logs,
  worktrees, and state. The fallback now compares the candidate scope's
  recorded `.project-root` marker against the requested canonical root and only
  adopts the existing directory when (a) no marker is present (legacy
  unmigrated scopes) or (b) the recorded path no longer canonicalizes (the
  user moved the repo on disk). Two regression tests cover the same-origin
  isolation guarantee and the moved-clone adoption path.
### Security

- **`fix(plugin-install)`: fail closed on corrupt plugin lockfile; add
  `--force-rewrite-lockfile` escape hatch.** `animus plugin install` and
  `animus plugin install-defaults` previously caught `PluginLockfile::load_default`
  errors with a `tracing::warn!` and continued with an empty in-memory
  lockfile that was then saved over the corrupt file on success. This was
  fail-OPEN against the tamper/audit boundary: an attacker who corrupted
  `.animus/plugins.lock` (or who hand-edited it incorrectly) could
  trigger a fresh install with no integrity checks and silently lose the
  recorded `sha256` history. The install path now REFUSES with an
  actionable error that names the corrupt path, surfaces the underlying
  loader error chain, and documents two remediation paths: (1) restore
  the lockfile from version control / backup, or (2) re-run with
  `--force-rewrite-lockfile` to discard and rebuild. Path (2) emits an
  explicit `warn!` noting integrity history was reset. Flag is wired
  through both `install` and `install-defaults`; MCP and control-plane
  install surfaces default to fail-closed (no override). New tests cover
  the refusal and the override paths.
- **`fix(daemon-health)`: apply manifest `env_required` to plugin probes.**
  `daemon_health()` in `crates/orchestrator-daemon-runtime/src/control/dispatch.rs`
  was probing each plugin with `PluginSpawnOptions::default()`, which scrubs
  env down to the base allowlist and never forwards manifest-declared vars.
  As a result, provider plugins that need credentials (e.g. `OPENAI_API_KEY`,
  `ANTHROPIC_API_KEY`, `LINEAR_API_TOKEN`) were spawned without them during
  the probe and reported false-unhealthy even when the daemon environment
  carried the keys. The probe now builds spawn options via
  `PluginSpawnOptions::for_manifest(&plugin.manifest.env_required, ...)` —
  the same path used by `subject_dispatch.rs` and
  `schedule/trigger_supervisor.rs`. When a `required = true` var is missing
  from the daemon environment we emit a `daemon_health.probe` warn so the
  operator can correlate the unhealthy row with the missing secret. (3 new
  unit tests covering the present / missing / no-env-declared shapes.)
### Security

- **Log redaction now scrubs secret-keyed JSON metadata, not just
  regex-matched values.** Previously, `redact_json_value` recursed into
  `meta` objects and only ran the content regex against string *values*,
  so a payload like `meta({"api_key":"sk_live_..."})` persisted the raw
  secret to `events.jsonl` because the bare value did not match any
  `key=value` content pattern. The recursion now also checks each JSON
  object key against a default secret-key set (`api_key`, `apikey`,
  `token`, `access_token`, `refresh_token`, `id_token`, `secret`,
  `client_secret`, `password`, `passwd`, `pwd`, `authorization`,
  `bearer`, `private_key`, `signing_key`, `x-api-key`). Matching is
  case-insensitive and treats `_` and `-` as equivalent, and the list
  also includes no-separator variants (`privatekey`, `signingkey`,
  `accesstoken`, etc.) so camelCase keys such as `privateKey`,
  `signingKey`, `accessToken` are redacted as well as snake_case and
  kebab-case forms (`X-API-Key`, `access-token`, `Authorization`). The
  value-content regex path is unchanged and still fires for plain
  strings such as `"description":"my api_key=sk_live_abc was leaked"`.
  Override the secret-key list with the new `ANIMUS_LOG_REDACT_KEYS`
  environment variable (comma-separated; replaces defaults). Documented
  in `docs/reference/observability.md`. (P2 from external audit.)

## [0.4.13] - 2026-05-27

Operational hardening of the v0.4.12 plugin extraction. Several pieces of
infrastructure shipped in v0.4.12 (signature verifier, process quota,
durability fsync, doctor diagnostics) but were not actually invoked on
the user path; v0.4.13 wires them up. Plus the v0.4.13 W-bundle ships
plugin lockfile, audit log, subprocess workflow_events back-channel,
runtime quotas, and an onboarding overhaul (`animus init`, `animus doctor`).

### Security

- **`fix(installer)`: invoke keyless cosign TrustedPublisher verifier on
  install + close cross-repo SAN attack.** The keyless cosign rewrite
  (v0.4.12 commit 518c0d9e) landed `verify_plugin_install` in
  `orchestrator-plugin-host::signature_verifier` but `animus plugin install`
  was still routing through a local `verify_with_cosign` with a weak
  `^https://github.com/<owner>/<repo>/.+` regex that would have accepted a
  bundle signed by `launchapp-dev/animus-subject-linear` against an install
  of `launchapp-dev/animus-provider-claude` (cross-repo SAN attack). Fix
  delegates to the host verifier with the TrustedPublisher regex pinned
  per-install to the specific repo segment, and re-applies the
  trusted-signers allowlist on the host verdict. 6 new tests including
  the attack-shape regression.

### Features

- **`feat(durability bundle)`: plugin lockfile + audit log + subprocess
  workflow_events back-channel + runtime quotas.** `.animus/plugins.lock`
  pins `sha256(artifact)` and `sha256(signature_bundle)`; install appends,
  upgrade refuses on mismatch without `--force`. `~/.animus/<scope>/audit.jsonl`
  is an append-only audit log of plugin installs / signatures / quota
  denials with 10 MB rotation. Subprocess phases now receive workflow events
  via a per-run Unix-domain-socket back-channel
  (`$TMPDIR/animus-event-pipes/<daemon-pid>/<subject>-<hex>.sock`) instead
  of relying on direct broadcaster handles. `RuntimeQuotas` caps trigger
  backlog (1000), subscriber memory (10 MB/sub), plugin process count (50),
  workflow concurrency (10).
- **`feat(plugin-host)`: enforce process quota via `ProcessSlotFactory` at
  spawn site.** The `PluginProcessSlot` RAII guard from the W-bundle is now
  actually acquired in `host.rs::spawn_with_options`. Wiring uses a trait
  (`ProcessSlotFactory`) the daemon installs at startup to avoid a circular
  dep between plugin-host and daemon-runtime. Spawn returns
  `ProcessSlotError::Exhausted` when at cap; the slot is released eagerly
  on shutdown.
- **`feat(durability)`: fsync session checkpoints + phase markers + task
  state writes (W3).** All atomic writes now call `File::sync_all()` +
  `fsync_rename()` so a crash mid-write cannot leave torn state.
- **`feat(daemon)`: defer `ProcessManager` broadcaster lookup to spawn time
  so subprocess workflow events actually fire.** The lookup was happening
  at construction time, before `run_daemon` installed the broadcaster,
  always returning `None`.

### Onboarding

- **`feat(onboarding)`: `animus init` interactive walkthrough + hello-world
  workflow template.** First-run experience: detects whether plugins are
  installed, offers `--auto-install`, drops a `.animus/` skeleton with a
  bundled `hello-world.yaml` workflow you can run immediately.
- **`feat(onboarding)`: `animus doctor` polish — 8 actionable diagnostic
  categories.** Replaces the previous monolithic health output with
  category-scoped checks (daemon socket, plugin install state, signature
  cache, audit log presence, etc.) each with concrete remediation steps.

### Fixes

- **`fix(web)`: partition transport plugins by `$ui/web` capability so
  `animus web open` picks the UI instead of an API transport.** Previously
  `ops_web.rs` keyed off `plugin_kind = "web_ui"`, but no installed plugin
  declares that kind — `animus-web-ui` ships as `transport_backend` like
  the API transports, so the UI bucket was empty and `transport-http` won
  the sort. The browser opened the API endpoint instead of the UI.
  Fix: `ops_web.rs` now scans each transport plugin's manifest
  `capabilities` (which `extra_capabilities` flattens into at discovery
  time) and treats any plugin advertising `$ui/web` as the UI surface.
  `animus web open` opens the UI URL when one is installed and falls back
  to the highest-priority API transport with a warning explaining how to
  remediate. `animus web serve` prints `UI:` and `API (<name>):` on their
  own labelled lines and includes `ui_url` / `api_url` / `serves_ui` in
  the JSON envelope.

  Follow-up required in `launchapp-dev/animus-web-ui` v0.1.2: declare
  `"$ui/web"` in its manifest via the v0.1.13 `extra_capabilities`
  extension point. Until that plugin ships, machines with only the
  default transports installed still open the API endpoint and get the
  warning above.
- **`fix(doctor)`: avoid nested tokio runtime in daemon RPC check.**
  `probe_daemon_health` was constructing a current-thread `Runtime` and
  calling `block_on` while already inside the `#[tokio::main]` runtime
  driving `handle_doctor`, panicking with "Cannot start a runtime from
  within a runtime" whenever the daemon control socket was present and
  `--skip-subprocess` was off. Fix converts the probe to async and
  propagates `.await` through `run_all_checks`.
- **`fix(durability)`: skip marker writes for `--phase` runs + canonicalize
  provider alias on restart resume.** Two follow-ups from codex round 8:
  the `--phase` filter path now uses `persist_phase_output_without_marker`
  so partial workflow runs do not advance the workflow marker, and
  restart-resume now normalizes `oai-runner` / `animus-oai-runner` →
  `oai` before looking up the resume target.
- **`fix(init)`: create `.animus/` before `install-defaults` + suppress
  child JSON on parent stdout.** Two codex round-9 P2s in the new
  `animus init --walkthrough`. (1) The plugin install was running before
  the template copy created `.animus/`, so `PluginLockfile::default_path`
  fell back to `~/.animus/plugins.lock`; subsequent `animus plugin lock
  list/verify` then looked at the now-existing project-local lockfile
  and missed the walkthrough's entries. Fix creates `.animus/` first so
  the lockfile location is stable. (2) `animus init --walkthrough --json`
  was passing `--json` to the child install / daemon subprocesses while
  inheriting their stdout, producing multiple JSON documents on the
  parent stdout. Fix pipes (and discards) child stdout when `json=true`,
  inherits in human mode so progress still streams.
- **`fix(workflow-runner-v2)`: unify plugin_pack_fixture tests with the
  crate-wide state serializer.** `plugin_pack_fixture_tests` had its own
  local `env_lock()` mutex while every other HOME-mutating test used
  `crate::test_env::scoped_state_serializer`. Parallel runs raced on
  `HOME` between the two groups, surfacing intermittently as
  `agent_state::tests::memory_delete_entry_by_id_removes_only_matching_entry`
  failures depending on which test got HOME first. Fix routes the 4
  plugin_pack_fixture tests through the shared serializer so a single
  mutex covers all HOME mutators. `cargo test -p workflow-runner-v2`
  (default parallel) now passes 120 tests with no flakes.
- **`fix(daemon)`: enforce default workflow concurrency from `RuntimeQuotas`
  + bind event-pipe synchronously.** Two daemon-runtime correctness
  fixes from user codex round 9: (1) `RuntimeQuotas.workflow_concurrency_max`
  (default 10) was documented but `ProcessManager::new()` only honored
  the env var; the cap was effectively unbounded for typical operators.
  `daemon_run` now installs `RuntimeQuotas::from_env()` before
  constructing `ProcessManager`, which seeds its spawn cap from the
  quota struct. `schedule_headroom(pool_size, active)` now caps the
  effective pool at `min(pool_size, runtime_quota)` so scheduler and
  trigger paths never select more work than the spawn site will accept.
  Without this, oversized `pool_size` configs would silently consume
  schedules and drain webhook events without runners ever starting.
  (2) `SubprocessEventPipe::bind` spawned an async bind task and waited
  on a `std::sync::mpsc::Receiver` from the calling thread, which could
  deadlock a current-thread runtime and stall a multi-thread worker.
  New `bind_sync` binds the socket synchronously on the calling thread
  and spawns only the reader task on the current Tokio runtime.

### Documentation

- **`docs`: refresh plugin version refs after registry bumps.** Installation,
  upgrading, web-dashboard, and CLI reference docs now match
  `plugin_registry.rs` constants. `plugin_types.rs` help text cross-references
  the registry instead of hard-coded versions to prevent future drift.

### Internal

- **`chore(cleanup)`: drop dead code, dedupe audit test, fix duplicate
  test attr, remove unused imports.** 5 → 0 workspace build warnings.
- **`chore(scripts)`: add `dispatch-wave.sh` helper for worktree-isolated
  parallel agent dispatch.** Standardizes the `git worktree add` +
  per-agent branch + merge / cleanup flow for parallel sub-agent waves.
  Not on the user path — used by the maintainer when driving multi-agent
  cleanup waves like this release.

## [0.4.12] - 2026-05-24

Closes the v0.4 arc. Finishes plugin extraction (web stack, subject adapters,
all in-tree provider mirrors, and `llm-cli-wrapper` are gone), ships the
durability items that were carrying `// TODO: v0.5` notes, and turns the
plugin host into a first-class component of daemon startup with preflight +
auto-install. Approximately 11.5K lines of code removed from this repo; the
replacement code lives in 18 standalone plugin repositories under
`launchapp-dev/`.

### Features

- **`feat(daemon preflight + auto-install)`: plugin presence check on
  startup.** New `animus daemon preflight` standalone subcommand prints the
  current installed-vs-required matrix; `animus daemon start` /
  `animus daemon run` now run the same check before booting. Default posture
  is refuse-to-start when any required role is unsatisfied — the error
  surfaces the exact `animus plugin install ...` command to remediate.
  `--auto-install` installs the daemon's recommended default plugin for any
  missing role from `launchapp-dev` releases before continuing.
  `--skip-preflight` is the escape hatch for dev iteration. JSON envelope
  is `animus.daemon.preflight.v1`.
- **`feat(plugin install-defaults)`: bulk install for the standard 5
  providers.** New `animus plugin install-defaults` installs the
  claude / codex / gemini / opencode / oai provider plugins at v0.2.1.
  Flags: `--include-oai-agent` (oai-agent v0.1.1),
  `--include-subjects` (default / requirements / linear / sqlite / markdown
  subject backends), `--include-transports` (transport-http +
  transport-graphql + web-ui). Uses the same install pipeline as
  `animus plugin install`, so signature verification and integrity checks
  apply uniformly.
- **`feat(daemon workflow events)`: workflow/events ControlClient
  subscription.** Daemon-side broadcaster now emits `phase_started` /
  `phase_completed` / `workflow_completed` / `workflow_failed`
  notifications on `workflow/events`. Subscribers can filter by
  `workflow_id` or by kinds. Closes the matching v0.5 deferral.

### Durability

- **`feat(notifications log)`: per-run JSONL.** Each run now writes
  `~/.animus/<scope>/runs/<id>/notifications.jsonl` with
  `{ seq, ts, phase, ... }` rows. 100 MB rotation; partial trailing
  lines are dropped on replay. UI clients can reconnect and replay from any
  `seq`.
- **`feat(session checkpointing + auto-resume)`: per-phase
  `<phase>.session.json` holds `{ provider, session_id, status }`. On
  daemon restart, the scheduler attempts `provider.resume_agent` through
  the plugin that originally ran the phase. Failures land as `Blocked`
  with an explicit reason rather than silently re-running.
- **`feat(idempotency markers)`: new `idempotency: idempotent |
  sideeffecting | unknown` field on phases (default `unknown`). Crash
  recovery auto-retries `idempotent` phases; everything else lands as
  `Blocked` with an actionable hint. `animus workflow resume <id>
  --force` is the manual override.
- **`feat(atomic completion markers)`: `<phase>.completed` is now written
  via tmp → fsync → rename after `persist_phase_output`. Closes the
  "output exists, daemon crashed before state transition" race.
- **`feat(plugin supervisor)`: per-plugin restart budget of 3 in 60s with
  a 5-minute `Disabled` cooldown when exhausted. Death-like errors
  auto-retry once before propagating; structured JSON-RPC errors
  propagate immediately.
- **`refactor(plugin-host)`: typed `HostError::ConnectionLost` plus
  `classify(&HostError) -> RetryDecision` API replaces the previous
  string-substring death-like classifier.

### Refactor / Deletions

- **`refactor(web)`: delete in-tree web stack
  (`orchestrator-web-server` + `orchestrator-web-api` +
  `orchestrator-web-contracts`, ~9K LOC).** `animus web serve` and
  `animus web open` now discover installed `transport_backend` + `web_ui`
  plugins, spawn them, and open the browser. The standalone
  `launchapp-dev/animus-transport-{http,graphql}` v0.2.x and
  `launchapp-dev/animus-web-ui` v0.1.0 replace the in-tree
  implementation. `animus plugin install-defaults --include-transports`
  installs the standard set.
- **`refactor(subject backends)`: delete `inproc_subject_backend.rs`
  (~1000 LOC).** All subject operations route through the
  `SubjectRouter` to an installed plugin —
  `launchapp-dev/animus-subject-default` v0.1.1 for `kind=task`,
  `launchapp-dev/animus-subject-requirements` v0.1.6 for
  `kind=requirement` (with legacy JSON state compat), plus
  `launchapp-dev/animus-subject-{linear,sqlite,markdown}` v0.1.4 for
  the other kinds. The env vars
  `ANIMUS_DAEMON_DISABLE_BUILTIN_TASK_ADAPTER` and
  `ANIMUS_DAEMON_DISABLE_BUILTIN_REQUIREMENTS_ADAPTER` are now no-ops.
  Use `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1` to skip subject
  discovery entirely.
- **`refactor(llm-cli-wrapper)`: crate deleted (~5,882 LOC across the
  multi-phase retirement).** Session DTOs, native backends, the parser,
  and the error types now live in upstream `animus-session-backend`
  v0.1.10. The `cli/launch.rs` symbols moved into
  `agent-runner::runner::launch`. `lookup_binary_in_path` was inlined
  as `which::which` at the few remaining call sites.
  `CapabilityNotSupported` moved into `orchestrator-session-host::error`.
- **`refactor(providers)`: delete the 5 in-tree provider mirrors
  (`animus-provider-{claude,codex,gemini,opencode,oai}`).** The
  standalone `launchapp-dev/animus-provider-*` v0.2.1 repos are now the
  canonical implementations and ship streaming notifications.
- **`refactor(protocol mirror)`: delete in-tree
  `animus-provider-protocol` mirror (-408 LOC).** Now consumed from
  upstream via `animus-protocol` v0.1.10.

### Protocol

- `animus-protocol` pins bumped to v0.1.10. Path through v0.1.6
  (`animus-transport-protocol` crate +
  `transport_backend_main` entrypoint) → v0.1.7 (`AgentNotification` +
  `NotificationSink` + `ProviderBackend::run_agent_streaming`) →
  v0.1.8 (subject/delete wire verb + `ControlClient` for cross-process
  control) → v0.1.9 (subscription API: `subject_watch` /
  `daemon_events` / `daemon_logs_follow`) → v0.1.10
  (`ControlClient::workflow_events`).

### Tests

- All 14 pre-existing baseline test failures resolved: 4 rewritten
  against the current surface, 8 deleted (they referenced commands
  removed in v0.4.4), 1 marked `#[ignore]` because of a pre-existing
  race that is not in this release's scope, 1 already passes after the
  surrounding refactors.
- **New: `launchapp-dev/animus-plugin-testkit` v0.1.0** — conformance
  harness with 8 baseline scenarios validated end-to-end against
  `animus-provider-claude`.

### Breaking Changes

- **`animus web serve` no longer boots an in-process axum server.**
  Requires the transport + UI plugins installed first; run
  `animus plugin install-defaults --include-transports` or
  `animus daemon preflight` for the exact remediation command.
- **Daemon requires at least one provider plugin installed at start.**
  Use `animus daemon start --auto-install` or `animus plugin
  install-defaults` first. `--skip-preflight` is the dev escape hatch.
- **`ANIMUS_DAEMON_DISABLE_BUILTIN_TASK_ADAPTER` and
  `ANIMUS_DAEMON_DISABLE_BUILTIN_REQUIREMENTS_ADAPTER` are no-ops.**
  The in-tree subject adapters are gone; install the corresponding
  subject_backend plugin (see `--include-subjects`).

### Security

- **`refactor(security): cosign keyless OIDC verification replaces the
  key-based PEM trust path.`** Every `launchapp-dev/animus-*` release
  pipeline (`animus-transport-{graphql,http}`, `animus-web-ui`, and the
  six `animus-provider-*` repos) signs through GitHub Actions OIDC +
  Sigstore Fulcio + Rekor, never against a static signing key. The
  pre-v0.4.12 verifier in `crates/orchestrator-plugin-host/src/signature_verifier.rs`
  shelled out to `cosign verify-blob --key <PEM>` against a baked
  `LAUNCHAPP_DEV_COSIGN_PUBLIC_KEY_PEM` placeholder, which could never
  match a real keyless bundle. v0.4.12 rewrites the verifier to keyless:
  trust is now anchored on the per-publisher `identity_regex` + OIDC
  issuer combination held in `TrustedPublisher`. The built-in
  `TrustedPublisher::launchapp_dev()` matches
  `^https://github\.com/launchapp-dev/[^/]+/\.github/workflows/release\.yml@refs/tags/v.*`
  against issuer `https://token.actions.githubusercontent.com`, which is
  exactly the cert SAN that GitHub Actions OIDC bakes into the
  Fulcio-issued cert for every standardized release. Manual
  verification: `cosign verify-blob --certificate-identity-regexp <regex>
  --certificate-oidc-issuer <issuer> --bundle <.bundle> <artifact>` —
  see [`docs/reference/security.md`](docs/reference/security.md#manual-cosign-verification).
- **Removed:** the baked PEM constant `LAUNCHAPP_DEV_COSIGN_PUBLIC_KEY_PEM`,
  the seed-on-first-strict-install helper `seed_launchapp_dev_trusted_key`,
  the `~/.animus/trusted-keys/` lookup (`default_trusted_keys_dir`,
  `resolve_trusted_key_for`), and the `crates/orchestrator-plugin-host/trusted-keys/launchapp-dev.pub`
  file. The `dirs` crate dependency drops out of `orchestrator-plugin-host`
  alongside. **Operators can also delete** their local
  `~/.animus/trusted-keys/` directory, the GH org
  `COSIGN_PRIVATE_KEY` / `COSIGN_PASSWORD` secrets, and any
  `~/.animus/keys/launchapp-dev.{key,pub,password}` files — none of them
  are read by v0.4.12. Sigstore Fulcio + Rekor (built into the `cosign`
  binary) is now the only trust anchor.
- **CLI flag `--trust-key` is deprecated and a no-op.** Keyless cosign
  verification has no static public-key trust anchor, so the flag is
  retained only to avoid breaking existing install scripts. Passing it
  logs a deprecation warning and proceeds via the normal keyless path.
  Removal targeted for a future release.
- **`security(plugin install)`: install-time signature policy default is
  `warn`.** Now that the trust anchor is real (Fulcio + Rekor) rather
  than a placeholder PEM, `warn` records `signature_status` and logs a
  stderr warning on missing / invalid / untrusted signatures without
  failing the install. Operators wanting fail-closed enforcement opt
  in per-install via `animus plugin install --signature-policy strict
  <repo>`. See
  [`docs/reference/security.md`](docs/reference/security.md#install-time-default-warn).

## [0.4.11] - 2026-05-23

Cleanup + automation hardening. Lands the v0.4.10 Node 24 sweep across the
plugin matrix and promotes the release-automation tooling from "future
work" to "shipped".

### CI

- **`ci`: merged Node 24 PRs across 15 plugin repos (deadline 2026-06-02).**
  Each plugin repo's `chore/force-node24-actions` PR (PR #1, opened in
  v0.4.10) is now landed on `main`. 14 merged clean; `animus-protocol`
  was held back because the existing `cargo fmt` job is failing on a
  pre-existing drift in `animus-plugin-runtime/src/log.rs:241` unrelated
  to the Node 24 env-var change — flagged for v0.4.12.
- **`ci(release-automation)`: add compat-test / validate-manifests /
  check-signatures scripts.** The `launchapp-dev/animus-release-automation`
  repo (shipped in v0.4.10 as v0.1.0 with `matrix.sh` + `cascade.sh`)
  now ships three additional scripts: `compat-test.sh` runs the in-tree
  `orchestrator-plugin-host` `protocol_drift` contract test against each
  plugin's pinned `animus-plugin-protocol` tag; `validate-manifests.sh`
  validates every plugin's `plugin.toml` against `animus.plugin.v1`;
  `check-signatures.sh` audits cosign bundles on each plugin's latest
  release against the `trusted-signers.yaml` defaults. README updated to
  v0.1.1 with usage + dependency notes.

## [0.4.10] - 2026-05-22

Patch release. Picks up v0.4.9 deferrals plus the broader cleanup the user has been queuing — the long-deferred `workflows_list` wire migration (HTTP only), live per-plugin `health/check` RPC fan-out, log redaction wired into every emit site, the two pre-existing flaky tests fixed at the root cause, and a `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` sweep ahead of the 2026-06-02 GitHub Actions Node 20 cutover.

### Breaking Changes

- **`http(workflows_list)`: wire shape on `/api/v1/workflows`.** The HTTP endpoint now serves `WorkflowRunSummary` rows (`id`, `definition`, `status`, `subject_id`, `started_at`, `finished_at`) instead of the full `OrchestratorWorkflow`. Daemon-internal fields (`phases`, `machine_state`, `current_phase`, `checkpoint_metadata`, decision history) no longer leak through the public HTTP API. `status` is kebab-case (`in-progress`-style) on the wire; `Escalated` collapses to `Paused` because the wire status enum lacks the variant. GraphQL `workflows` and `workflowsPaginated` still serve the full `OrchestratorWorkflow` shape, so the embedded web UI is unaffected.

### Features

- **`feat(daemon health)`: live per-plugin `health/check` fan-out.** `daemon_health()` now spawns each discovered plugin one-shot, runs the `initialize` handshake, calls `health/check`, and shuts the plugin down — all under a 3s per-plugin deadline. Probes fire concurrently via `futures_util::future::join_all`, so the wall time is roughly one probe regardless of plugin count. Failures land as `Unhealthy` rows with the error string in `last_error`; the daemon's own status stays `Healthy` because plugin-side trouble is an observability concern, not a daemon-liveness one. A long-lived plugin host pool is still v0.5 work, but `daemon health` is not hot-path enough to need it.
- **`feat(orchestrator-logging)`: redaction wired into `Logger::write_entry`.** The v0.4.9 `redact_log_entry` building block moved from `orchestrator_daemon_runtime::control::log_redact` into `orchestrator_logging::log_redact` and is invoked from `Logger::write_entry` immediately before serialization. Every emit site that goes through the logger picks up redaction automatically; the old module path remains as a re-export so external callers stay source-compatible. New regression test (`write_entry_redacts_msg_in_persisted_line`) reads the persisted `events.jsonl` line and asserts the secret never reached disk.
- **`feat(workflows_list)`: `workflows_list_summary` API method.** New `WebApiService::workflows_list_summary(query)` returns `ListPage<WorkflowRunSummary>` — the projection helper `workflow_to_run_summary` maps local `OrchestratorWorkflow` rows onto the wire shape. The HTTP handler at `/api/v1/workflows` calls this method; the existing `workflows_list` is kept for GraphQL.

### Fixes

- **`fix(tests)`: `plugin_registry_path_falls_back_to_legacy_when_canonical_missing`.** The test was setting `ANIMUS_CONFIG_DIR` to a non-empty path while asserting that `default_config_path()` returns the legacy `~/.config/animus/plugins.yaml`. The `config_dir_overridden` guard in `default_config_path()` intentionally skips the legacy fallback when `ANIMUS_CONFIG_DIR` is set, so the test was actually deterministically failing — but ENV_GUARD poisoning and other tests interacting through the same mutex were masking it as a "flake". The fix explicitly clears `ANIMUS_CONFIG_DIR` for the duration of the test (using the existing `EnvVarGuard`) so the legacy-fallback branch is exercised.
- **`fix(tests)`: `install_succeeds_after_org_added_to_trusted` + 4 siblings.** All five trusted-orgs tests in `ops_plugin` were calling `std::env::set_var("ANIMUS_TRUSTED_ORGS", …)` and `remove_var` bare, with no serialization. Concurrent test threads racing on the same process-global env var were the documented flake source. Fix: new `TRUSTED_ORGS_ENV_GUARD` process-level mutex plus a small `ScopedEnv` RAII helper that restores the prior value on drop (panic-safe). All five tests now serialize through the mutex.

### Plugin Cascade

No plugin-repo cascade in v0.4.10 — `animus-protocol` stays pinned at v0.1.4. The wire shape adopted by item A already lives in v0.1.4, so no protocol bump was required.

### Ops / CI

- **`chore(ci)`: `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` set across all in-tree workflows.** GitHub Actions runners deprecate Node 20 on 2026-06-02; the env var forces JS-based actions onto Node 24 ahead of the cutover. Patched into all 8 `.github/workflows/*.yml` files as a top-level `env:` block.

### Carried Forward

- **Release automation across the 15-repo ecosystem.** Scoping was completed (lighter path: a separate `launchapp-dev/animus-release-automation` repo with a CLI tool that runs the version-pin matrix check + cascade-PR generation), but the implementation work was deferred so the v0.4.10 budget could land items A through F. Filed as the v0.4.11 / v0.5 starting point.

## [0.4.9] - 2026-05-22

Patch release. Picks up v0.4.8 deferrals plus a handful of originally-planned items: the plugin-repo cascade onto animus-protocol v0.1.4, daemon-health per-plugin enumeration, a log redaction layer, and a cleanup pass on the stale `AgentPool` references that have been confusing agent reports for several patches.

### Features

- **`feat(daemon health)`: per-plugin section.** `DaemonHealthResponse` now carries a populated `plugins: Vec<PluginHealth>` whenever the daemon's control wire serves the request: each discovered plugin contributes a `name` / `kind` / `status` row. Live `health/check` RPC fan-out per plugin is intentionally deferred — without a long-lived plugin host pool the daemon would have to one-shot every plugin process per health probe, which is too expensive for a frequently-polled endpoint. The CLI's human renderer prints a compact `plugins:` table when the wire path is taken; the JSON envelope passes the full wire shape through unchanged.
- **`feat(daemon-runtime)`: log redaction layer.** New `orchestrator_daemon_runtime::control::log_redact` module that scrubs secret-shaped values from `LogEntry.msg`, `LogEntry.meta` (recursively over nested JSON), `LogEntry.content`, and `LogEntry.error` before the entry is handed to a storage sink. Default pattern matches `api_key|apikey|api-key|password|token|secret|authorization` followed by `:` or `=` and a value; additional patterns can be appended via `ANIMUS_LOG_REDACT_PATTERNS` (comma-separated regex). Five unit tests cover the default patterns, custom patterns, nested JSON traversal, and the no-op path. The redactor is shipped as a public function — wiring it into every emit path is a v0.4.10 follow-up so the patch boundary stays bounded.

### Plugin Cascade

All 12 plugin repos were re-pinned to animus-protocol v0.1.4 and bumped to their next patch version. The cascade is git-additive: each repo got one `chore: bump to v0.1.x + pin animus-protocol v0.1.4` commit + a new tag, pushed to `main`.

| Repo                              | Old   | New   |
|-----------------------------------|-------|-------|
| animus-subject-linear             | 0.1.2 | 0.1.3 |
| animus-subject-sqlite             | 0.1.2 | 0.1.3 |
| animus-subject-markdown           | 0.1.2 | 0.1.3 |
| animus-subject-requirements       | 0.1.2 | 0.1.3 |
| animus-provider-claude            | 0.1.1 | 0.1.2 |
| animus-provider-codex             | 0.1.1 | 0.1.2 |
| animus-provider-gemini            | 0.1.1 | 0.1.2 |
| animus-provider-opencode          | 0.1.1 | 0.1.2 |
| animus-provider-oai               | 0.1.1 | 0.1.2 |
| animus-trigger-webhook            | 0.1.0 | 0.1.1 |
| animus-trigger-slack              | 0.1.0 | 0.1.1 |
| animus-log-storage-file           | 0.1.0 | 0.1.1 |

The triggers and log-storage repos previously pinned animus-protocol via `branch = "main"`; they now pin `tag = "v0.1.4"` explicitly so the wire surface is reproducible.

### Documentation

- **AgentPool references aligned with reality.** The CLI and daemon-runtime comment blocks claiming "AgentPool carries `#[allow(dead_code)]`" referred to a struct that no longer exists in the tree. Comments in `ops_agent/control_routing.rs` and `control/routing.rs` updated to state plainly: the wire surface stays pass-through because there is no daemon-side agent pool yet, and CLI callers degrade to the local in-process path.

### Deferred to v0.4.10

- **`web-api workflows_list`** continues to return `ListPage<OrchestratorWorkflow>`. Migrating to the wire `WorkflowListResponse` requires both a contract change (pagination model, status casing) and the dropped fields (`phases`, `machine_state`) propagated through the GraphQL surface and downstream typed tests. The web-api crate has stalled multiple agents on this exact migration; held back another patch to keep v0.4.9 from sprawling.
- **Log redaction at every emit site.** v0.4.9 lands the redactor as a public function plus tests. Calling it from `Logger::write_entry` (or from each emit builder) is the next step; the patch ships the building block without changing emit semantics yet.
- **Live `health/check` RPC fan-out per plugin.** Per-plugin status currently reports `Healthy` for any discoverable plugin. Real per-plugin health probes require a long-lived plugin host pool with per-process status caches.

## [0.4.8] - 2026-05-22

Wire + UX patch. Picks up the v0.4.7-deferred web-api work that needed an animus-protocol bump, ships the plugin-runtime structured log macros, and polishes the `animus subject` CLI for the single-backend project case.

### Features

- **`feat(protocol)`: bump animus-protocol to v0.1.4.** `WorkflowResumeRequest` carries an optional `feedback` field so approval-gated workflows can pass reviewer comments through resume. `QueueReorderRequest` accepts a multi-entry form (`subject_ids: Vec<String>`) alongside the single-id shape. `animus-plugin-runtime` ships a new `log` module with crate-level `info!`/`warn!`/`error!`/`debug!`/`trace!` macros that emit `log/entry` JSON-RPC notifications via the existing stdout pipe; each `*_main` entrypoint installs the global emitter automatically.
- **`feat(web-api)`: route `queue/reorder` and `workflow/resume` through the daemon control wire.** With the v0.1.4 wire surface in place, the web-api handlers prefer the daemon's control RPC, falling back to the local path only when the daemon isn't running. `workflow/resume` now carries the reviewer feedback string end-to-end.
- **`feat(cli)`: `animus subject` honors `default_subject_kind` from `.animus/config.json`.** New `--kind` becomes optional; falls back to the config default (which seeds to `"task"` for new projects). Operators can now `animus subject list` without re-typing `--kind task` every invocation. A missing config default *and* missing flag prints a helpful error listing the lookup precedence.
- **`feat(plugin-host)`: hierarchical kind matching in `SubjectRouter`.** Plugins may declare glob kinds like `task.*`; resolution does exact-match first, then longest matching glob prefix. Duplicate equal-prefix globs are rejected at registration time. Five new tests cover the precedence rules.
- **`feat(llm-cli-wrapper)`: typed `Error::CapabilityNotSupported`.** Callers can pattern-match on the typed variant instead of grepping `ExecutionFailed` strings; the cancel-routing path now emits the typed error when a plugin's handshake `capabilities.cancellation` is `false`.

### Removed

- **`test(cli)`: delete the stale `json_success_envelope_contract_is_stable` test.** It still drove `animus task stats`, which was removed in v0.4.4 cleanup, so it had been red on every run since.

### Deferred to v0.4.9

- **`web-api workflows_list`** continues to return `ListPage<OrchestratorWorkflow>` instead of the leaner wire `WorkflowListResponse`. The two shapes differ on pagination model (offset/total vs cursor), status casing (snake_case vs kebab-case), and the wire summary lacks `phases`, `machine_state`, and several other fields the handler currently exposes. Migration is a contract change across `paginated_success_response`, ETag computation, the GraphQL surface, and downstream typed tests.
- **`feat(daemon health)`: per-plugin health.** The brief asked for a `plugins:` section in `animus daemon health` that calls each installed plugin's `health/check` RPC. The plumbing change spans the daemon-runtime health snapshot, the control wire's `daemon/health` response, and CLI rendering — held back to keep the v0.4.8 surface bounded.
- **Plugin repo cascade (10 plugins).** Each subject/provider/trigger/log-storage plugin still pins animus-protocol v0.1.3. The v0.1.4 protocol crate is wire-additive so existing plugins continue to work; per-plugin re-pinning + retagging ships in v0.4.9.

## [0.4.7] - 2026-05-22

Wire-routing patch: the daemon's `daemon/logs` control method finally drives `animus logs tail`, and MCP gets the matching surface. Two of the three deferred web-api handlers stay deferred with v0.4.8-tagged notes; the cross-repo plugin runtime cascade ships in a separate window.

### Features

- **`feat(logs)`: route `animus logs tail` through the control wire when the daemon is up.** Adds `ControlClient::daemon_logs` (streaming consumer with a caller-side limit), wires the daemon's `daemon/logs` surface to read historical entries via the active `LogStorageDispatch`, and updates `ops_logs::handle_logs_tail` so the daemon-up branch no longer opens `events.jsonl` directly. Operators get a single transport regardless of which side is serving; the plugin live-tail path (long-lived `log_storage_backend` plugin host) stays deferred but the contract is now wire-first.
- **`feat(mcp)`: add `animus.logs.tail` tool.** Mirrors the `animus.subject.*` pattern — typed input struct, args builder, `run_tool` shell-out — so agents can pull the daemon's log tail through MCP without re-implementing the wire-fallback logic.

### Deferred to v0.4.8

- **`web-api workflows_list`** keeps returning `ListPage<OrchestratorWorkflow>`. Migrating to the leaner wire `WorkflowListResponse` needs a router + OpenAPI contract change and a GraphQL/web-UI follow-up.
- **`web-api queue_reorder`** stays on the local Vec-of-subject-ids path. Wire `QueueReorderRequest` is single-id + anchor + position; lifting requires a multi-id variant in animus-protocol v0.1.4.
- **`web-api workflows_resume` with feedback** stays local. `WorkflowResumeRequest` lacks a feedback field on the wire; adding it needs an animus-protocol v0.1.4 bump.
- **Plugin runtime `log::{info!, warn!, …}` macros + per-plugin cascade.** Building the `animus_plugin_runtime::log` macros and re-pinning the 10+ subject/provider/trigger/log-storage plugin repos onto v0.1.4 is a separate window.

## [0.4.6] - 2026-05-22

CI workflow cleanup. Two workflows have been failing on every commit since v0.4.1:

- `ci(protocol-drift)`: bump `STANDALONE_TAG` `v0.1.1` → `v0.1.3` to match the in-tree consumer. The workflow was trying to check out a tag that exists on `animus-protocol` but predated the wire-format expansions the in-tree crate now consumes, so the structural-drift assertion fired on every run.
- `ci(release)`: remove the `docker-publish` job. It pushed to `registry.fly.io/ao-daemon`, a registry slug that predates the `ao` → `animus` rename and no longer resolves. Can be re-added when there's a real registry target to point at; GitHub Release binary artifacts (the load-bearing publish path) are unaffected.

No Rust or CLI changes. Pure CI hygiene.

## [0.4.5] - 2026-05-22

Hotfix release. Two bugs in v0.4.4 + v0.4.3:

- fix(daemon-control): gate Unix-socket server on #[cfg(unix)]
  so Windows release CI compiles. v0.4.2/0.4.3/0.4.4 all failed
  the Windows build for the same reason. Windows now falls back
  to the in-process service path; named-pipe equivalent is a
  future enhancement.
- test: delete 18 cli_e2e tests that exercised v0.4.4-deleted
  commands (animus task / animus requirements lifecycles).
  Unit tests (321/321) cover the new SubjectBackend surface.

## [0.4.4] - 2026-05-21

Cleanup release that drops the legacy CLI command surfaces v0.4.3 made redundant. The in-tree `SubjectBackend` adapters (`InTreeTaskSubjectBackend`, `InTreeRequirementsSubjectBackend`) plus the external subject_backend plugin ecosystem cover the use cases through the unified `animus subject --kind <kind>` surface.

### Removed

- **`feat(cli)`: delete legacy `animus task` command tree.** Replaced by `animus subject --kind task` against the in-tree task adapter. Underlying `orchestrator-core::services::task*` services are untouched and continue to back both the subject adapter and the daemon's workflow runtime.
- **`feat(cli)`: delete legacy `animus requirements` command tree.** Replaced by `animus subject --kind requirement` against the in-tree requirements adapter. `orchestrator-core::services::requirements*` is preserved.
- **`feat(cli)`: delete legacy `animus cloud` command tree.** Animus-sync legacy surface; cloud sync now ships as an out-of-tree plugin.
- **`feat(cli)`: delete `animus setup` command.** `animus init` is the supported onboarding entry point.
- **`feat(cli)`: delete `animus now` command.** Overlaps with `animus status`; consolidate on the unified status dashboard.
- **`feat(cli)`: delete `animus errors` command tree.** Folded into `animus history`.
- **`feat(mcp)`: delete `ao_task_*` / `animus.task.*` MCP tool family.** Use `animus.subject.*` with `kind=task`.
- **`feat(mcp)`: delete `ao_requirements_*` / `animus.requirements.*` MCP tool family.** Use `animus.subject.*` with `kind=requirement`.
- Companion cleanup: removes the now-unused `shared/parsing.rs` task/requirement value parsers, the `cli_types/shared_types.rs` task/requirement help-text constants, and the `services/runtime/stale_in_progress.rs` summary helpers (only the deleted task handler consumed them).

### Preserved

- `orchestrator-core` services for tasks and requirements stay intact — both the new subject adapters and the daemon's workflow runtime continue to depend on them.
- All other CLI command groups (`daemon`, `agent`, `project`, `queue`, `workflow`, `history`, `git`, `skill`, `model`, `pack`, `plugin`, `runner`, `status`, `output`, `mcp`, `web`, `init`, `doctor`, `trigger`, `logs`, `subject`) are unchanged.

## [0.4.3] - 2026-05-21

Controller-as-plugin migration plus the daemon-wiring foundation that makes the v0.4.x plugin ecosystem operational. CLI, MCP, and WebAPI all route through the daemon's new Unix-socket control protocol when the daemon is running, falling back to in-process calls when it isn't.

### Features

- **`feat(daemon)`: Unix-socket control server + control protocol.** Daemon now exposes `~/.animus/<repo-scope>/control.sock` (0700 perms) speaking newline-delimited JSON-RPC 2.0 per [`animus-control-protocol` v0.1.3](https://github.com/launchapp-dev/animus-protocol/tree/v0.1.3/animus-control-protocol). 47 method constants across 7 groups (subject, plugin, daemon, workflow, agent, queue, project). Auto-starts at daemon launch; opt-out via `ANIMUS_DAEMON_DISABLE_CONTROL_SERVER=1`. Emits `DaemonRunEvent::ControlServerResolved` so operators can find the socket path in `events.jsonl`.
- **`feat(cli)`: every command tries the control socket first, falls back to local.** `animus plugin/daemon/workflow/queue/agent/subject` commands open the control socket when present and route through it; one-shot operations (no daemon) keep working via the existing in-process path. Preserves the `animus.cli.v1` JSON envelope on both paths.
- **`feat(mcp)`: tool surface routes through control protocol via CLI subprocess.** `ao_workflow_*`, `ao_queue_*`, `ao_plugin_*`, `ao_daemon_*`, and `ao_agent_*` MCP tools shell out to the migrated CLI, which routes via control when the daemon is up. Six new `animus.subject.*` MCP tools added (list, get, create, update, next, status) to mirror the CLI surface.
- **`feat(web-api)`: REST handlers route through control protocol (9 of 12 migrated).** `queue_list`, `queue_stats`, `queue_hold`, `queue_release`, `workflows_get`, `workflows_run`, `workflows_pause`, `workflows_resume`, `workflows_cancel` all try control first, fall back to direct `ServiceHub` calls. Deferred: `workflows_list`, `queue_reorder`, `workflows_resume` with feedback — wire-shape contract changes pending.
- **`feat(daemon-logs)`: LogStorageBackend plugin discovery + `animus logs tail`.** Daemon discovers installed `log_storage_backend` plugins at startup; if exactly one is found, it becomes the active log sink. Falls back to the in-tree `Logger` writing `events.jsonl` when no plugin is installed. New `animus logs tail [--plugin] [--level] [--since] [--follow] [--json]` CLI reads from whichever backend is active. Opt-out via `ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN=1`.
- **`feat(daemon-subject)`: SubjectBackend plugin discovery + `animus subject` CLI.** Daemon discovers installed `subject_backend` plugins, builds the `SubjectRouter` from their declared `subject_kinds`, rejects duplicate-kind claims at startup. Generic `animus subject list/get/create/update/next/status --kind <kind>` routes through whichever plugin owns that kind. Built-in `InTreeTaskSubjectBackend` + `InTreeRequirementsSubjectBackend` auto-register under `kind=task` and `kind=requirement` so legacy `animus task` and `animus requirements` keep working unchanged.

### Refactors

- **`refactor(control)`: relocate `ControlClient` to `orchestrator-daemon-runtime`.** Lived in `orchestrator-cli` originally; `orchestrator-web-api` couldn't depend on it (circular). Moved to the daemon-runtime crate which both already depend on. No behavior change; mechanical import updates across the CLI.
- **`refactor(plugin-runtime)`: subject dispatch accepts `<kind>/<verb>` method names.** Aligns the runtime's dispatch table with the daemon's `SubjectRouter` kind-keyed routing. Shipped as [`animus-protocol` v0.1.2](https://github.com/launchapp-dev/animus-protocol/tree/v0.1.2); subject plugin repos (linear, sqlite, markdown, requirements) cut v0.1.2 to consume it.

### Deferred to v0.4.x patches

- Legacy CLI command deletion (`Command::Task`, `Command::Requirements`, hidden `Review`, `Qa`) plus matching MCP tools (`ao_task_*`, `ao_requirements_*`). The in-tree task/requirements services keep working via the new SubjectBackend adapters; deletion is a follow-up cleanup commit. (Shipped in v0.4.4.)
- `workflows_list` web-api migration (router + OpenAPI contract change required)
- `queue_reorder` web-api migration (multi-id wire support needed)
- TTL eviction for the session-keyed host cache (deferred from the cancel-keep-host-alive MVP)
- AgentPool integration (still `allow(dead_code)`) — agent dispatch returns `NotSupported` over the control wire pending the daemon-side query surface

## [0.4.2] previously listed Unreleased features (now formally released)

- **`feat(plugin)`: cosign signature verification on plugin install.** `animus plugin install <owner/repo>` now checks for a [sigstore cosign](https://www.sigstore.dev) signature bundle (`<asset>.tar.gz.bundle`) alongside the binary asset. When present, the install verifies it via the `cosign` CLI (shell-out for v0.4.x; in-process `sigstore-rs` planned for v0.5+). Default mode: verify-if-present and record `signature_status: verified|unsigned` in `~/.animus/plugins.yaml`. New flags: `--require-signature` refuses install when no bundle is published or verification fails; `--skip-signature` bypasses verification entirely; `--trusted-signers <PATH>` points at a YAML allowlist (default `~/.animus/trusted-signers.yaml`). A FAILING signature always refuses install. The `cosign` binary is a soft dependency — when missing, installs degrade to `signature_status: unsigned` rather than failing. `animus plugin list` gains a `SIG` column surfacing the recorded status. Plugin release workflows (`launchapp-dev/animus-*`) ship signing as of plugin tag `v0.1.2+` via GitHub Actions OIDC (no secrets to manage). See [docs/architecture/plugin-signing.md](docs/architecture/plugin-signing.md).
- **`feat(workflow-config)`: `${VAR}` env-var interpolation in workflow YAML.** `.animus/workflows.yaml`, `.animus/workflows/*.yaml`, and pack-shipped workflow overlays now support shell-style `${VAR}`, `${VAR:-default}`, and `${VAR:?error}` interpolation. Substitution runs before YAML parsing so every string scalar — subject backend configs, provider tokens, MCP `env` blocks, phase env overrides — accepts the same syntax uniformly. Unset required vars fail fast with the YAML file path + line number. Use `$$` to embed a literal `$`. Recommended for non-secret config like team IDs, base URLs, and feature flags; credentials still belong in the daemon's process environment, not in YAML. See [docs/reference/configuration.md](docs/reference/configuration.md#workflow-yaml-interpolation-non-secret-config).

## [0.4.2] - 2026-05-17

Same-day follow-up to v0.4.1. Ships the full v0.4.0 plugin ecosystem: every
provider plugin promised by the v0.4.0 release is now live in its own
`launchapp-dev/animus-*` repository with green CI, alongside the protocol
crates, a public plugin scaffold template, and the first subject backend
(Linear). The in-tree CLI gains the matching `animus plugin install
<owner/repo>` and `animus plugin new` surfaces for one-command install +
bootstrap, and several rough edges in `animus init` and plugin discovery
caught during release verification are fixed.

### Plugin ecosystem (now shipped, was "in-flight" in v0.4.0 notes)

Eight standalone repositories under [launchapp-dev](https://github.com/launchapp-dev),
each tagged `v0.1.0` with green CI:

- [`animus-protocol`](https://github.com/launchapp-dev/animus-protocol) — 5-crate workspace publishing `animus-plugin-protocol`, `animus-subject-protocol`, `animus-provider-protocol`, `animus-plugin-runtime`, and `animus-session-backend`. CI workflow added.
- [`animus-plugin-template`](https://github.com/launchapp-dev/animus-plugin-template) — subject + provider scaffolds consumed by `animus plugin new`. CI workflow runs `cargo check` against both kinds on every push.
- [`animus-subject-linear`](https://github.com/launchapp-dev/animus-subject-linear) — Linear GraphQL `SubjectBackend` reference implementation. Credentials load leniently so `--manifest` probes work credential-free.
- [`animus-provider-claude`](https://github.com/launchapp-dev/animus-provider-claude) — Claude Code CLI wrapper provider.
- [`animus-provider-codex`](https://github.com/launchapp-dev/animus-provider-codex) — Codex CLI wrapper provider.
- [`animus-provider-gemini`](https://github.com/launchapp-dev/animus-provider-gemini) — Gemini CLI wrapper provider.
- [`animus-provider-opencode`](https://github.com/launchapp-dev/animus-provider-opencode) — OpenCode CLI wrapper provider.
- [`animus-provider-oai`](https://github.com/launchapp-dev/animus-provider-oai) — OpenAI-compatible HTTP provider. Lenient credential loading so `--manifest` works without a configured API key.

### Features

- **`animus plugin install <owner/repo>[@tag]`**: Install a plugin directly from a public GitHub release. The CLI resolves the latest release (or the supplied tag), downloads the matching architecture asset, verifies the published checksum, drops the binary into `~/.animus/plugins/`, and registers it in `~/.animus/plugins.yaml`. Mutually exclusive with `--path` / `--url`. New `--tag <TAG>` and `--latest` flags make scripted installs explicit. Local-file (`--path`) and direct-URL (`--url --sha256`) installs from v0.4.0 still work unchanged.
- **`animus plugin new --kind <subject|provider|trigger> --name <name>`**: Scaffold a brand-new plugin project from `launchapp-dev/animus-plugin-template`. Clones the template (or reads `--template-path <PATH>` for offline use), substitutes the per-kind/per-name variables, and writes a buildable Rust project to `./animus-<kind>-<name>/` (override with `--out-dir`). The output project is `cargo build`-clean from the first commit.
- **Plugin install dir is configurable**: New `--plugin-dir <PATH>` flag on `animus plugin install` / `uninstall`, plus the new `$ANIMUS_PLUGIN_DIR` env var, override the default `~/.animus/plugins/` location. Discovery scans the same directory automatically.
- **Plugin registry consolidated under `~/.animus/`**: The plugin registry file moves from `~/.config/animus/plugins.yaml` to `~/.animus/plugins.yaml`. The legacy path is still read transparently on first run; the next `animus plugin install` rewrites it to the new location. Two new helpers (`plugins_registry_path()` and `legacy_plugins_registry_path()`) are re-exported from `orchestrator-plugin-host` for tooling that needs to point at either.

### Fixes

- **`animus init` no longer emits legacy wrapper YAMLs alongside template files** (commit [`f7c85a11`](https://github.com/launchapp-dev/animus-cli/commit/f7c85a11), bug #22): `animus init --template task-queue` was writing both the registry's inline-content workflow files *and* three hardcoded wrapper YAMLs (`standard-workflow.yaml`, `hotfix-workflow.yaml`, `research-workflow.yaml`) because `FileServiceHub::new` ran the hardcoded scaffold inside `bootstrap_project_base_configs` *before* `write_template_files` had a chance to drop registry files. Reorder: write template files first, bootstrap second. The scaffold's `has_existing_yaml` short-circuit now sees the registry files and skips the wrappers. Also fixes an undercount of three in `apply.written_files` because the scaffold was writing outside the tracked path. Verified: a fresh `animus init --non-interactive --template task-queue` now produces 5 YAML files (was 8).
- **Plugin discovery surfaces failed-manifest probes instead of silent-dropping them** (commit [`8394a73`](https://github.com/launchapp-dev/animus-cli/commit/8394a73)): `discover_configured` and `scan_dir` were swallowing manifest-fetch errors, so installed plugins that failed `--manifest` invocation (the symptom: `animus-provider-oai` before the lenient-credential fix) silently disappeared from `animus plugin list` with no diagnostic. New `DiscoveryWarning` struct + `PluginDiscovery::discover_with_warnings()` API returns failed-probe plugins alongside successes, with the binary's stderr + exit code included. `tracing::warn!` fires at every drop site. `animus plugin list --json` gains a top-level `warnings` array; human output prints each warning to stderr. The `animus.plugin.list` MCP tool result gains a parallel `warnings` field. Regression tests cover failed `--manifest` probes for explicit-config plugins, missing configured binaries, and failed `scan_dir` manifest probes.
- **`animus-provider-oai --manifest` works credential-free** (shipped in `animus-provider-oai` v0.1.0): Manifest probes no longer require `OPENAI_API_KEY` to be set. The lenient-config loader returns a manifest with an `error` capability hint when credentials are missing, instead of exiting with code 1.
- **`animus-subject-linear --manifest` works credential-free** (shipped in `animus-subject-linear` v0.1.0): Same fix as the oai provider — credential validation deferred from `--manifest` to the first `subject/list` call.

### CI

- **`animus-protocol` repo now has CI**: `cargo check` + `cargo test` across the 5-crate workspace on every push.
- **`animus-plugin-template` repo now has CI**: Validates both the subject scaffold and the provider scaffold compile cleanly via `cargo check`.

### Docs

- **`README.md` + `CLAUDE.md` drop "extraction in progress" framing**: Both update to describe the shipped 22-crate workspace plus the 8 standalone plugin repositories under [launchapp-dev](https://github.com/launchapp-dev). The CLAUDE.md "Current Baseline" section now reflects v0.4.0 plugin extraction as complete rather than in flux.
- **`docs/migration/v0.3-to-v0.4.md` marked complete**: Migration guide now documents the actual shipped install surfaces (public-repo `owner/repo@tag`, local `--path`, URL `--url --sha256`, and scaffold via `animus plugin new`) rather than describing them as planned.
- **`docs/reference/cli/index.md`**: Adds the `animus plugin install <owner/repo>` and `animus plugin new` flag surfaces, the new `--plugin-dir` override, and the updated default registry path. Discovery-order line updated to point at `~/.animus/plugins.yaml`.
- **`docs/architecture/subject-backend-plugins.md`**: Resolves the v0.4.0 "open questions" section against the v0.1.0 protocol shape that actually shipped. Adds links to the live `animus-protocol` and `animus-subject-linear` repositories.

### Known follow-ups (deferred to v0.4.x)

- `[[bin]] name = "ao-workflow-runner"` in `crates/workflow-runner-v2/Cargo.toml` is still the legacy bin name (from v0.4.1 known-follow-ups list; not yet renamed).
- Release archive filenames still use the `ao-v0.4.X-` prefix; the release script's archive-naming convention still needs to be updated to `animus-v0.4.X-`.

## [0.4.1] - 2026-05-17

Same-day patch for issues caught during v0.4.0 release verification.

### Fixes
- **Plugin discovery config path renamed**: `default_config_path()` in `crates/orchestrator-plugin-host/src/discovery.rs:174` previously read `~/.config/ao/plugins.yaml` — a stale path the v0.4.0 hard rename missed. v0.4.1 reads from `~/.config/animus/plugins.yaml`. Users with a config at the old path need to `mv ~/.config/ao ~/.config/animus` on upgrade.
- **Docker image build broken upstream**: the v0.4.0 Release Binaries workflow's Docker step failed because OpenCode upstream changed its release-asset format from `opencode_linux_${ARCH}` (single binary) to `opencode-linux-${ARCH}.tar.gz` (tarball). Animus's `Dockerfile` now pulls the tarball and extracts the `opencode` binary out of it.
- **Docker stack finished rename**: `Dockerfile` and `docker-compose.yaml` both had leftover `ao` references the rename missed — `mkdir -p /root/.ao` → `mkdir -p /root/.animus`, compose service name `ao` → `animus`, image `ghcr.io/launchapp-dev/ao:latest` → `ghcr.io/launchapp-dev/animus:latest`, volume `ao-data:/root/.ao` → `animus-data:/root/.animus`, entrypoint `ao` → `animus`.

### Docs
- **Reference docs cover the v0.4.0 MCP surface**: `docs/reference/mcp-tools.md` now documents the `animus.skill.{list,get,search}` and `animus.memory.{get,list,append,clear}` families, including the `capabilities.memory: true` gating model. `docs/reference/cli/index.md` gets a "Selected Command Flags" section covering `animus init --update-registry`, the SHA256-required-with-URL plugin install rule, and the `--include-system-path` opt-in. `docs/reference/configuration.md` env var table expanded from 4 to ~30 entries. `docs/reference/data-layout.md` covers the new `~/.animus/packs/`, agent-host skill probe paths, and the resolution-paths table grew from 7 to 14 entries.
- **Narrative-docs `AO` / `ao` prose sweep**: 49 docs across `docs/{design,concepts,guides,architecture,contributing,reference,internals}/` plus the VitePress site config had bare project-name mentions of `AO` / `ao` that survived sub-agent A's code-focused rename. All now read `Animus`. Also folded into this pass: stale crate counts (`17-crate workspace` → `Rust-only Cargo workspace (around 20 crates)`), stale literal command examples (`ao status`, `ao now`, `ao git`, etc.), branch prefix `ao/<task-id>` → `animus/<task-id>` in `docs/concepts/worktrees.md` to match `crates/orchestrator-git-ops/src/daemon_git_worktree.rs`, and stale `PROJECT_ROOT` env var fallback in `docs/reference/cli/global-flags.md` per CLAUDE.md guidance.

### Known follow-ups (deferred to v0.4.x)
- `[[bin]] name = "ao-workflow-runner"` in `crates/workflow-runner-v2/Cargo.toml` is still the legacy bin name. Rename pending.
- Release archive filenames still use the `ao-v0.4.X-` prefix (e.g. `ao-v0.4.0-aarch64-apple-darwin.tar.gz`). The release script's archive-naming convention needs to be updated to `animus-v0.4.X-`.

## [0.4.0] - 2026-05-14

This is a major release and the v0.3.x → v0.4.0 break is intentionally large. Three commitments land together:

1. **One name across every surface.** The partial v0.3.x rebrand from AO to Animus is finished. There is no longer an `ao.*` namespace. Every MCP tool, environment variable, configuration directory, pack id, JSON envelope, plugin crate, and standalone plugin repository uses the `animus` name. No deprecation aliases. See the [naming contract](docs/architecture/naming-contract.md) and [migration guide](docs/migration/v0.3-to-v0.4.md).
2. **The architecture commits to plugin-first.** Subjects (units of dispatchable work) are now pluggable so teams can keep using Jira / Linear / GitHub Issues / Notion as their source of truth. Provider plugins (Claude, Codex, Gemini, OpenAI-compatible, OpenCode) move from in-tree workspace crates to standalone repositories. The plugin protocol crates move to a dedicated `launchapp-dev/animus-protocol` repository and ship as published Rust SDK crates that any plugin author depends on.
3. **The skill system rearchitects around the ecosystem-standard SKILL.md format.** Bundled YAML skills become an installable `animus.core-skills` pack. Skills installed for other agent hosts (Claude Code, Codex, Cursor, OpenCode) become discoverable subject sources for Animus workflows.

Expect breaking changes throughout. The migration is small in absolute terms because the v0.3.x install base is small; the rename is a deliberate inflection point that commits to Animus as an ecosystem rather than a single tool.

### Breaking changes

- **Full rename to `animus` (no aliases).** Every `ao.*` MCP tool name becomes `animus.*` (58 tools). Every `AO_*` environment variable becomes `ANIMUS_*` (51 vars). The project-local config directory `.ao/` becomes `.animus/`. Scoped runtime state at `~/.ao/<repo-scope>/` moves to `~/.animus/<repo-scope>/`. Pack ids `ao.task` / `ao.review` / `ao.requirement` become `animus.task` / `animus.review` / `animus.requirement`. The JSON output envelope schema `ao.cli.v1` becomes `animus.cli.v1`. The plugin protocol host name `"ao"` becomes `"animus"`. The in-tree provider crates rename from `ao-provider-*` to `animus-provider-*`; `ao-plugin-smoke` becomes `animus-plugin-smoke`. The migration guide has the complete list with before/after examples.
- **Bundled YAML skills moved to an installable pack.** `BUILTIN_SKILL_YAMLS` is removed. The 19 skills that previously shipped baked into the binary are now distributed as the `animus.core-skills` pack, auto-installed during `animus init` / `animus setup`. Existing references resolve via the `Installed` source.
- **`animus plugin install --url <url>` now requires `--sha256`.** Previously optional; URL-sourced installs without an integrity check are rejected at the CLI layer.
- **Plugin discovery no longer scans `$PATH` by default.** Discovery is limited to `.animus/plugins/` and `$ANIMUS_PLUGIN_PATH`. Restoring the prior behavior requires explicit `--include-system-path` on the discovery surface.
- **SKILL.md frontmatter: Animus-specific fields move under a vendor namespace.** `tool_policy`, `extra_args`, `env`, `mcp_servers`, `adapters`, `codex_config_overrides` move from top-level frontmatter to `animus:` in the SKILL.md frontmatter. Keeps SKILL.md portable across the ecosystem (Claude Code, Codex, Cursor, gstack) by isolating Animus-specific fields.
- **Subjects are now pluggable.** Native `animus task` is one backend among many. Workflows can declare `subject_type:` to operate over Linear/Jira/GitHub Issues subjects via plugin backends. Existing workflows that omit `subject_type:` continue using the native task backend with no change.
- **Plugin protocol crates move to standalone repository.** `orchestrator-plugin-protocol` is replaced by `animus-plugin-protocol` published from `launchapp-dev/animus-protocol`. The new `animus-subject-protocol`, `animus-provider-protocol`, and `animus-plugin-runtime` crates also publish from that repository. Plugin authors depend on these crates from crates.io rather than path-depending on the core workspace.
- **Provider plugins move to standalone repositories.** `animus-provider-{claude, codex, gemini, oai, opencode}` each ship from their own repository under `launchapp-dev/`. The daemon image installs them at build time from crates.io.

### Architectural commitments (documented for the first time)

- **Naming**: one name everywhere. See [naming contract](docs/architecture/naming-contract.md).
- **Plugin-first architecture**: subjects, providers, triggers, and skills are all plugins. The core is the orchestration runtime; plugins are independent release artifacts. See [subject backend plugins](docs/architecture/subject-backend-plugins.md).
- **Standalone plugin repositories**: every plugin lives in its own GitHub repository under `launchapp-dev/animus-{kind}-{name}`. The repository name, crate name, and binary name all match. Plugin authors generate new plugins from the `animus-plugin-template` scaffold via `animus plugin new --kind <kind> --name <name>`.

### Features

#### Plugin host
- **Stdio plugin host foundation**: New `orchestrator-plugin-host` (in core) plus `animus-plugin-protocol` (extracted to `launchapp-dev/animus-protocol`) implement newline-delimited JSON-RPC 2.0 plugin protocol with handshake, request/response, notifications, and streaming over stdio.
- **Plugin host wired through the runtime**: CLI, MCP server, subject dispatch, and daemon all route through the new plugin host. New `animus plugin install/list/get` commands and `animus.plugin.*` MCP tools.
- **Claude and Codex shipped as stdio plugins**: First-class `animus-provider-claude` and `animus-provider-codex` provider plugins running over the new protocol, distributed from their own repositories.
- **Three additional providers plus mock + runtime**: `animus-provider-gemini`, `animus-provider-opencode`, `animus-provider-oai`, plus the in-tree `animus-provider-mock` (testing) and the shared `animus-plugin-runtime` crate (used by every provider for the RPC loop, streaming, and logging).
- **Plugin smoke test**: `animus-plugin-smoke` crate exercises the end-to-end plugin contract for CI.
- **Subject backend plugins**: New `animus-subject-protocol` defines the `SubjectBackend` trait + normalized Subject schema. First reference implementation: `animus-subject-linear` ships from its own repository. The `animus plugin new` scaffold + `animus-plugin-template` make new subject backends a single-command bootstrap.
- **Template-driven `animus init`**: External template registry (default `launchapp-dev/animus-project-templates`) cloned and cached under `~/.animus/template-registries/<registry-id>/`. Three bundled templates: `task-queue`, `conductor`, `direct-workflow`. Flags: `--template <id>`, `--path <local-dir>`, `--non-interactive`, `--force`, `--update-registry`.
- **Template registry commit pinning**: `animus init` captures the registry HEAD on first clone into `.commit` metadata and refuses to silently fast-forward on subsequent runs. Use `--update-registry` to fetch latest and re-pin.
- **Bootstrap packs externalized**: Templates carry `[[packs]]` declarations that activate during init (e.g. `animus.task`, `animus.requirement`).
- **Markdown skill installs**: `animus skill install` supports SKILL.md format (frontmatter + body) alongside YAML. Body content lands in `prompt.system`. Animus-specific runtime fields live under the `animus:` frontmatter namespace.
- **Agent memory and communication config**: New per-agent memory state plus inter-agent messaging plumbed into phase prompt rendering. Memory MCP tools (`animus.memory.{get, list, append, clear}`) capability-gated on `agent.capabilities.memory == true`.
- **Plugin streaming + structured logging**: Provider RPC loop streams chunks back over notifications; structured logs flow to the host stderr sink.
- **`animus plugin install --url <https-url>`**: Download a plugin binary from an HTTPS URL with mandatory SHA256 integrity verification.

#### Subject backend protocol (v0.4.0 plugin extraction)
- **`animus-subject-protocol` crate**: New workspace crate (also published from `launchapp-dev/animus-protocol`) defining the `SubjectBackend` trait, normalized `Subject` schema (`SubjectId`, `SubjectStatus`, `SubjectFilter`, `SubjectList`, `SubjectPatch`, `SubjectSchema`, `ChangeKind`, `SubjectChangedEvent`), and JSON-RPC method constants (`subject/list`, `subject/get`, `subject/update`, `subject/watch`, `subject/schema`).
- **`animus-provider-protocol` crate**: New workspace crate defining the `ProviderBackend` trait, `AgentRunRequest`/`AgentRunResponse`/`AgentResumeRequest`/`AgentCancelRequest`/`ProviderManifest`/`ProviderCapabilities`/`TokenUsage` types, and `BackendError` with structured RPC error mapping.
- **`animus-plugin-runtime`**: Generalized from `ao-provider-runtime`. Provides `subject_backend_main()` and `provider_main()` entry points, full stdin/stdout JSON-RPC loop, lifecycle method handling (initialize/initialized/$/ping/shutdown/exit/health/check), and the `animus:` shape future plugin authors target.
- **Subject backend design doc**: `docs/architecture/subject-backend-plugins.md` defines the v0.4.0 plugin contract — `SubjectBackend` trait, normalized schema, JSON-RPC method set, workflow YAML binding, native `animus task` migration plan, and 8 open questions for red-team.

#### Skill system rearchitect
- **`animus.core-skills` pack**: The 19 bundled YAML skills now ship as an installable pack at `crates/orchestrator-config/config/bundled-packs/animus.core-skills/`. Auto-installs during `animus init` and `animus setup` and pins itself in `.animus/state/pack-selection.v1.json`. Users can `cd ~/.animus/packs/animus.core-skills/0.1.0/skills/` to read, fork, or override any of the 27 catalog skill names. Pack updates ship via `animus skill install` — no binary upgrade required.
- **`PackManifest::workflows` is now optional**: Skills-only packs are first-class. Validation requires at least one of `[workflows]` or `[skills]`. `PackSkills` is a sibling to `PackWorkflows`.
- **`SkillSourceOrigin::AgentHost { host, scope }`**: New variant scans skills installed for other agent hosts. Six hosts wired via `AGENT_HOST_SPECS`: Claude Code (`~/.claude/skills/`, `<project>/.claude/skills/`), Codex (`~/.codex/skills/`), OpenCode (`~/.config/opencode/skills/`), Cursor (`~/.cursor/skills/`), Kiro (`~/.kiro/skills/`), Slate (`~/.slate/skills/`). Project-scoped variants take precedence over global. Each scope is `Project | Global`.
- **Two-tier trust model**: `strip_structural_fields_for_agent_host` removes `tool_policy`, `extra_args`, `env`, `mcp_servers`, `adapters`, `codex_config_overrides`, and `capabilities` from any AgentHost-discovered skill at parse time. AgentHost skills can only contribute prompt text + directives. To use a host-discovered skill's full structural fields, the user must explicitly run `animus skill install --path` to promote it to the high-trust `Installed` source.
- **`animus:` vendor namespace in SKILL.md frontmatter**: Animus-specific runtime fields (`tool_policy`, `mcp_servers`, `model.preferred`, `adapters`, etc.) now live under `animus:` in the SKILL.md frontmatter, keeping SKILL.md portable across other agent hosts (Claude Code, Codex, Cursor, gstack) that don't recognize Animus-specific keys. Top-level placement of these fields is no longer parsed (hard cut, no deprecation warning). The trust strip still applies even when `animus:` namespace is present in an AgentHost source.
- **Source chain priority**: `AgentHost::Global < AgentHost::Project < Builtin < Installed (pack + registry) < User < Project`. `load_skill_sources` returns sources in lowest-to-highest order; resolution iterates in reverse for highest-priority match.
- **`docs/architecture/skill-system.md`**: New design doc covering the source chain, trust tiers, vendor namespace, and `animus.core-skills` pack layout.

#### `animus.skill.*` MCP tools
- **`animus.skill.list`**: Enumerate all skills across every source (Builtin, Installed, AgentHost::Project, AgentHost::Global, User, Project). Each entry carries `source` (variant tag) + `source_detail` with provenance: registry/source/version for Installed; host/scope plus `structural_fields_stripped: true` and `trust_tier: "prompt_text_only"` for AgentHost. Optional `source` filter (`builtin`, `installed`, `agent_host`, or a host id like `claude-code`).
- **`animus.skill.get`**: Fetch the full `SkillDefinition` for a skill by name. AgentHost-source responses include a `notice` field explaining that structural fields were stripped at parse time.
- **`animus.skill.search`**: Case-insensitive substring match over name, description, and tags. Optional source filter and result limit.

External agents (Claude Code session driving Animus, workflow-spawned subprocesses connected over MCP) can now discover the skill catalog at runtime — including the user's existing `~/.claude/skills/` library that v0.3.x couldn't reach — without shelling out to `animus skill list --json` or hardcoding skill names.

### Fixes
- **Honor configured rework routing** (#024f0161): `rework` verdicts now respect the configured routing target instead of defaulting to the immediately preceding phase.
- **Plugin handshake hangs blocked**: `PluginHost::handshake` now wraps the initialize round trip in a 30-second timeout so a non-responsive plugin can't deadlock daemon startup.
- **`expect()` panics removed from plugin code paths**: `animus.plugin.call` MCP tool and the provider runtime's `initialize` / `health/check` paths now return structured RPC errors instead of panicking when serialization or registry initialization fails.
- **`git clone` / `git pull` errors surfaced**: Template registry sync now captures stdout/stderr from git subprocess invocations and includes them in error messages (was previously swallowed).

### Docs
- **Naming contract**: `docs/architecture/naming-contract.md` commits to one name across every surface (Animus). Cross-linked from architecture index, VitePress nav, `CONTRIBUTING.md`, and `CLAUDE.md`.
- **Subject backend plugin protocol**: `docs/architecture/subject-backend-plugins.md` defines the v0.4.0 subject backend plugin design.
- **Skill system v0.4.0**: `docs/architecture/skill-system.md` — source chain, trust tiers, `animus:` namespace, `animus.core-skills` pack layout.
- **Migration guide**: `docs/migration/v0.3-to-v0.4.md` walks through every breaking change with before/after examples covering all 14 surfaces (rename, plugin discovery, template registry pinning, skill system, subject backend pluggability).
- **`feature-status.md`**: Template-driven project init flipped from In-Flight to Shipped.
- **`docs/architecture/index.md`**: Updated to reflect the all-`animus` naming, the ~20-crate workspace size, and the v0.4.0 subject backend plugin link.
- **`CLAUDE.md`** (project guide): Updated state paths from `.ao/` → `.animus/`, env var references from `AO_*` → `ANIMUS_*`, and the working-rules naming guidance to the all-`animus` model.

### Internal
- **Cargo aliases renamed**: `cargo ao-fmt`/`ao-lint`/`ao-bin-build`/`ao-bin-check` → `cargo animus-fmt`/`animus-lint`/`animus-bin-build`/`animus-bin-check`. CI release workflow updated.

### Known issues / fix-before-tag
- Plugin discovery executes `--manifest` on every candidate found in `.animus/plugins/` and `$ANIMUS_PLUGIN_PATH` with no signature, allowlist, or sandboxing. `$PATH` is opt-in (off by default). Treat the install / discovery surface as trusted-only until a richer trust model lands.
- Subject backend reference plugin (`animus-subject-linear`) and `animus plugin new` scaffold command are still on the v0.4.0 roadmap; the protocol crate stack is in place to support them. Native `animus task` migration to satisfy the `SubjectBackend` trait also remains pending.
- The optional `animus.skill.registry.list` MCP tool was deferred — `SkillRegistrySourceConfig` is `pub(super)` to `ops_skill` and exposing it cleanly needs a thin `pub fn list_skill_registries()` accessor first. Follow-up.

## [0.3.2] - 2026-04-10

### Fixes
- **Default decision contract evidence types** (TASK-222): Implementation phase decision contract now accepts the common evidence kinds agents actually return (`bug_confirmed`, `fix_identified`, etc.), not only `files_modified`. First workflow runs no longer fail immediately when an agent does an analysis-only pass.
- **`phase_decision.evidence` is now optional** when no required evidence types are configured.
- **Strip secrets from tracked state**: Removed `.ao/sync.json` from tracking; it can contain auth tokens.
- **Rustfmt + lint cleanups**: `runtime_contract.rs` method chain and test formatting.

### Features
- **`animus task status --force`**: Allow forced status transitions for skip/cancel paths.
- **Surface agent file-edit decisions**: When an agent runs an analysis phase and modifies no files, the phase output now explains why (permissions, working directory, prompt constraints, or "no change needed") instead of leaving the user guessing.

### Docs
- Restructured getting-started to lead with autonomous mode.
- README updated with v0.3.0 feature surface.

## [0.3.1] - 2026-04-07

### Features
- **Animus daemon Docker image**: Multi-stage `Dockerfile` produces a `linux/amd64` image with minimal dependencies, sized for Fly.io cloud deployment.
- **Claude Code, Codex, and OpenCode bundled in the daemon image**: The cloud daemon ships with all three coding CLIs pre-installed so spawned workflow agents work without per-tenant install steps.
- **Docker image publish in release workflow**: CI now pushes `registry.fly.io/ao-daemon` on tagged releases.

### Fixes
- **Auto-start daemon and agent-runner on sync workflow execution** (TASK-221): Fixes "first-run socket error" when invoking a sync workflow without a daemon already running.
- **Dockerfile Rust bump to 1.89**: `async-graphql` requires rustc 1.89+.

## [0.3.0] - 2026-04-06

### Features
- Cloud status integration: ao cloud status calls /api/cli/status for cloud projects and daemons — show user's cloud projects, daemon states, and active workflows from the cloud API
- Cloud API routing: ao-cli route deploy commands through cloud API instead of direct Fly.io — animus cloud deploy create/destroy/start/stop/status should call /api/cli/daemons/* endpoints instead of requiring Fly.io credentials locally
- Auto-detect project linking from git remote + GitHub App installation
- Config alignment: animus cloud push now sends .ao/ config files
- Device auth flow: implement animus cloud login device auth flow
- ACP evaluation: research ACP spec and design AO as ACP server for IDE integration
- Deploy subcommands: add start/stop/status/create/destroy deployment subcommands
- Feature branch workflow: enable feature branch workflow in standard task pipelines
- Cloud CLI: evolve ao sync into ao cloud CLI subcommands
- Memory MCP: wire ao-memory-mcp into default workflow phases
- Marketplace MVP: Skill marketplace MVP — GitHub registry fetch + pack download
- Event triggers Phase 2: Generic Webhook Support
- Event triggers Phase 1: file watcher support
- Remote MCP servers: HTTP/remote MCP server support

### Fixes
- Cloud linking: rustfmt fix for cloud.rs auto-detect linking
- Config test isolation: improve orchestrator-config test isolation and lock handling
- Model references: replace broken glm-5 model refs with claude-haiku, disable pr-sweep schedule
- Sparkcube removal: remove sparkcube tool_profile refs blocking all daemon workflows
- CLI E2E tests: resolve all 6 storage-mismatch and warning failures
- Lint warnings: resolve all clippy warnings workspace-wide
- Daemon task state: fix last failing unit test — daemon_run task-state-change assertion
- Config test assertion: align test assertion with standard-workflow ID rename
- Env lock handling: recover from poisoned env lock in tests
- Path references: replace hardcoded fixture paths and absolute paths with CARGO_MANIFEST_DIR

### Refactors
- Rebrand to animus: rename primary binary from 'ao' to 'animus', maintain 'ao' as alias
- Help text updates: update all help text and error messages from 'ao' to 'animus'
- Test updates: update test assertions for rebrand to 'animus'
- Unused dependencies: strip unused MCP servers — remove filesystem, sequential-thinking, memory, rust-docs

### Documentation
- Public repo preparation: clean README.md with Animus branding, verify ELv2 LICENSE, remove hardcoded local paths, add CONTRIBUTING.md, ensure .gitignore covers sensitive files
- README updates: fix typo and update GitHub release links

### Style
- Formatting fixes: rustfmt fixes for line length, import ordering, and cloud status command

## [0.2.35] - Previous Release

See git history for earlier releases.
