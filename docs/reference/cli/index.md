# CLI Command Surface

Complete reference of every `animus` command, subcommand, and key flag. This tree is the authoritative map of the CLI surface area. For global flags that apply to all commands, see [Global Flags](global-flags.md). For exit code semantics, see [Exit Codes](exit-codes.md).

## Global Flags

| Flag | Description |
|---|---|
| `--json` | Machine-readable JSON output (`animus.cli.v1` envelope) |
| `--project-root <PATH>` | Override project root resolution for the current command |
| `--as <PRINCIPAL>` | Impersonate a declared principal for daemon-backed operations. Honor-system on local sockets, warned loudly, and ignored when the active RBAC policy is `single-user` |
| `--no-cache` | (v0.5.9) Bypass hot-path read caches for this invocation. Mirrors `ANIMUS_DISABLE_CI_CACHE=1` and `ANIMUS_DISABLE_WORKFLOW_CACHE=1`. Caches remain best-effort and fall through to live source on any error |

### `--json` envelope coverage

The root `--json` flag is global: every non-streaming verb in every command
family — including `pack`, `skill`, `runner`, `trigger`, `logs`, `history`,
and `git` — answers it with the standard `animus.cli.v1` envelope
(`{"schema":"animus.cli.v1","ok":true,"data":...}` on stdout for success,
`{"schema":"animus.cli.v1","ok":false,"error":{...}}` on stderr for failure).
The contract is pinned by `crates/orchestrator-cli/tests/cli_json_contract.rs`
with at least one envelope-shape test per family.

Exemptions — live streams are not wrapped in an envelope; they emit raw
lines (JSONL passthrough where structured) until interrupted:

- `animus daemon stream` (structured JSONL event stream)
- `animus daemon events --follow` (event stream; the non-follow form honors `--json`)
- `animus events tail` (workflow event stream; with `--json` each event is emitted as one JSONL line)
- `animus chat send` (streams provider output as it arrives)

`animus logs tail` is *not* exempt: it is a bounded, pull-style reader that
returns one envelope and exits; its `--follow` flag is reserved and currently
a no-op (see the `animus logs tail` section below).

---

## Top-Level Command Tree

```
animus
├── version                  Show installed animus version
├── daemon                   Manage daemon lifecycle and automation settings
│   ├── start                Start the daemon in detached/background mode
│   ├── run                  Run the daemon in the current foreground process
│   ├── stop                 Stop the running daemon
│   ├── restart              Stop the running daemon (graceful), then start it again with the supplied start flags
│   ├── status               Show daemon runtime status
│   ├── health               Show daemon health diagnostics
│   ├── pause                Pause daemon scheduling
│   ├── resume               Resume daemon scheduling
│   ├── events               Print recent daemon event history and exit; `--follow` keeps streaming new events until Ctrl-C
│   ├── logs                 Read daemon logs
│   ├── stream               Stream structured log events in real-time across daemon, workflows, and runs
│   ├── clear-logs           Clear daemon logs
│   ├── agents               List daemon-managed agents
│   ├── config               Update daemon automation configuration
│   ├── preflight            Report plugin preflight status (which required plugins are installed, which are missing, and the fix commands)
│   └── metrics              Print daemon observability metrics (counters, gauges, histograms)
│
├── agent                    Run and inspect agent executions
│   ├── list                 List configured agent profiles
│   ├── get                  Get a configured agent profile
│   ├── run                  Start an agent run
│   ├── control              Control an existing agent run
│   ├── status               Read status for a run id
│   ├── memory
│   │   ├── get              Read memory for a configured agent
│   │   ├── append           Append a memory entry for a configured agent
│   │   └── clear            Clear memory for a configured agent
│   ├── message
│   │   ├── send             Send a message on an agent channel
│   │   └── list             List agent messages
│   └── interactions
│       ├── list             List pending agent questions and approval requests
│       ├── show             Show a single interaction by id
│       └── answer           Answer a pending question or approval request
│
├── chat                     Hold multi-turn conversations with a provider tool (v0.5.10)
│   ├── new                  Start a new (empty) conversation and print its id
│   ├── send                 Send a user message and stream the reply (`--title` names the conversation)
│   ├── get                  Print a conversation's full transcript
│   ├── list                 List conversations, most-recently-updated first
│   ├── rename               Set or clear a conversation's title (`--title`; empty clears)
│   ├── delete               Permanently delete a conversation
│   ├── export               Export a transcript (`--format markdown|json`, `--output <path>`)
│   └── search               Grep conversation transcripts across the scope (`--limit`, `--case-sensitive`)
│
├── project                  Manage project registration and metadata
│   ├── list                 List registered projects
│   ├── active               Show the active project
│   ├── get                  Get a project by id
│   ├── create               Create a new project entry
│   ├── load                 Mark a project as active
│   ├── rename               Rename a project
│   ├── archive              Archive a project
│   └── remove               Remove a project
│
├── queue                    Inspect and mutate the daemon dispatch queue
│   ├── list                 List queued dispatches
│   ├── stats                Show queue statistics
│   ├── enqueue              Enqueue a subject dispatch for a task, requirement, or custom title
│   ├── hold                 Hold one or more queued subjects (ids or --all)
│   ├── release              Release one or more held queued subjects (ids or --all)
│   ├── drop                 Drop (remove) one or more queued subject dispatches regardless of status (ids or --all)
│   └── reorder              Reorder queued subjects by subject id
│
├── workflow                 Run and control workflow execution
│   ├── list                 List workflows
│   ├── get                  Get workflow details
│   ├── decisions            Show workflow decisions
│   ├── checkpoints
│   │   ├── list             List checkpoints for a workflow
│   │   ├── get              Get a specific checkpoint for a workflow
│   │   └── prune            Prune checkpoints using count and/or age retention
│   ├── run                  Run a workflow. Spawns a detached workflow_runner by default; use --sync to run in terminal (both require the workflow_runner plugin)
│   ├── resume               Resume a paused workflow and respawn its workflow_runner
│   ├── resume-status        Check whether a workflow can be resumed
│   ├── pause                Pause an active workflow (confirmation required)
│   ├── cancel               Cancel a workflow (confirmation required)
│   ├── prune                Prune terminal workflow runs from history and disk; dry-run by default, `--yes`/`--force` deletes
│   ├── delete               Delete a single terminal workflow run from history and disk; dry-run by default, `--yes`/`--force` deletes
│   ├── phase
│   │   ├── approve          Approve a pending phase gate
│   │   └── reject           Reject a pending phase gate
│   ├── phases
│   │   ├── list             List configured workflow phases
│   │   ├── get              Get a workflow phase by id
│   │   ├── upsert           Create or replace a phase definition in the generated overlay
│   │   └── remove           Remove a generated-overlay phase definition (confirmation required)
│   ├── definitions
│   │   ├── list             List configured workflow definitions
│   │   └── upsert           Create or replace a workflow definition
│   ├── config
│   │   ├── get              Read resolved workflow config
│   │   ├── validate         Validate workflow config shape and references (also reports declared-but-unenforced fields in `warnings`)
│   │   ├── compile          Validate and resolve YAML workflow files (also reports declared-but-unenforced fields in `warnings`)
│   │   └── reload           Re-run YAML compile pipeline (hot-reload fallback)
│   ├── state-machine
│   │   ├── get              Read workflow state-machine config
│   │   ├── validate         Validate workflow state-machine config
│   │   └── set              Replace workflow state-machine config JSON
│   ├── agent-runtime
│   │   ├── get              Read workflow agent-runtime config
│   │   ├── validate         Validate workflow agent-runtime config
│   │   └── set              Replace workflow agent-runtime config JSON
│   ├── prompt
│   │   └── render           Render workflow phase prompt text and prompt sections
│
├── history                  Inspect and search execution history
│   ├── task                 List history records for a task
│   ├── get                  Get a history record by id
│   ├── recent               List recent history records
│   ├── search               Search history records
│   └── cleanup              Remove old history records
│
├── git                      Manage Git repositories, worktrees, and confirmation requests
│   ├── repo
│   │   ├── list             List registered repositories
│   │   ├── get              Get details for one repository
│   │   ├── init             Initialize and register a local repository
│   │   └── clone            Clone and register a repository
│   ├── branches             List repository branches
│   ├── status               Show repository status
│   ├── commit               Commit staged/untracked changes
│   ├── push                 Push branch updates
│   ├── pull                 Pull branch updates
│   ├── worktree
│   │   ├── create           Create a repository worktree
│   │   ├── list             List repository worktrees
│   │   ├── get              Get one worktree by name
│   │   ├── remove           Remove a worktree (confirmation required)
│   │   ├── prune            Prune managed task worktrees for done/cancelled tasks
│   │   ├── pull             Pull updates in a worktree
│   │   ├── push             Push updates from a worktree
│   │   ├── sync             Pull then push a worktree
│   │   └── sync-status      Show synchronization status for a worktree
│   └── confirm
│       ├── request          Request a confirmation record for a destructive git operation
│       ├── respond          Approve or reject a confirmation request
│       └── outcome          Record operation outcome for a confirmation request
│
├── skill                    Search, install, update, uninstall, and publish versioned skills
│   ├── search               Search skills across built-in, user, project, and registry sources
│   ├── install              Install a skill with deterministic resolution
│   ├── list                 List all available skills (built-in, user, project, and installed)
│   ├── show                 Show details of a resolved skill definition
│   ├── update               Re-resolve one or all installed skills
│   ├── uninstall            Remove an installed skill's materialized files and registry/lock entries (supports --source and --dry-run)
│   ├── publish              Publish a new skill version into the registry catalog
│   ├── migrate-from-ao      Move legacy .ao/skills/ into .animus/skills/ (v0.3 → v0.4)
│   └── registry
│       ├── add              Register a new registry source or update an existing one
│       ├── remove           Remove a registered registry source
│       └── list             List all registered registry sources
│
├── model                    Inspect model availability, validation, and evaluations
│   ├── availability         Check model availability for one or more model ids
│   ├── status               Show configured model and API-key status
│   ├── validate             Validate model selection for a task or explicit list
│   ├── roster
│   │   ├── refresh          Refresh model roster from providers
│   │   └── get              Get current model roster snapshot
│   └── eval
│       ├── run              Run model evaluation
│       └── report           Show latest model evaluation report
│
├── pack                     Install, inspect, pin, and uninstall workflow packs
│   ├── install              Install a pack from a local path or marketplace registry
│   ├── list                 List discovered packs and indicate which ones are active for this project
│   ├── inspect              Inspect a discovered pack or a local pack manifest
│   ├── pin                  Pin a pack version/source or toggle enablement for this project
│   ├── uninstall            Remove an installed pack (all versions or --version) plus its project selection entry; refuses while project workflow YAML references the pack unless --force (supports --dry-run)
│   ├── search               Search packs across marketplace registries
│   └── registry
│       ├── add              Add a marketplace registry (git URL)
│       ├── remove           Remove a marketplace registry
│       ├── list             List all registered marketplace registries
│       └── sync             Sync (re-clone) a registry to get latest pack catalog
│
├── plugin                   Discover, inspect, install, and call Animus STDIO plugins
│   ├── list                 Discover plugins via plugins.yaml, .animus/plugins/, $ANIMUS_PLUGIN_DIR, and $ANIMUS_PLUGIN_PATH
│   ├── info                 Print a plugin's manifest plus initialize-time capabilities
│   ├── call                 Send a JSON-RPC request to a plugin and print its response
│   ├── ping                 Health-check a plugin by spawning it, completing the handshake, and pinging
│   ├── install              Install a plugin binary from a public GitHub release (OWNER/REPO[@TAG]), a local path, or a direct URL into ~/.animus/plugins/ (override with --plugin-dir or $ANIMUS_PLUGIN_DIR)
│   ├── uninstall            Remove a previously installed plugin from ~/.animus/plugins/ (override with --plugin-dir or $ANIMUS_PLUGIN_DIR) and ~/.animus/plugins.yaml
│   ├── new                  Scaffold a new plugin project from the launchapp-dev/animus-plugin-template scaffold
│   ├── search               Search the public Animus plugin registry by substring + filters
│   ├── browse               Browse the public Animus plugin registry, grouped by kind
│   ├── update               Update one or all installed release-source plugins to the latest tag
│   ├── install-defaults     Install the standard set of provider plugins from public GitHub releases (claude, codex, gemini, opencode, oai). Skips plugins that are already installed. Optional flags pull in additional default plugins
│   ├── lock                 Inspect and verify the plugin lockfile (`.animus/plugins.lock`). The lockfile records sha256 + version for every installed plugin so an `install --force` or tampered-binary scenario is visible to operators
│   │   ├── list             List every entry currently recorded in the plugin lockfile
│   │   └── verify           Re-hash every installed plugin binary and report mismatches against the lockfile
│   ├── doctor               Per-role view of installed plugins. Shows every preflight role with its installed plugins (by installed_kind + native_kind) and flags duplicates so collisions are visible without spelunking through the lockfile (v0.5.7)
│   ├── status               Per-plugin runtime status (pid, state, last RPC, restart count, last error). Answers "why does this plugin feel stuck?" by surfacing the supervisor's restart counter for every discovered plugin (v0.5.8)
│   ├── cache                Inspect or wipe the on-disk plugin manifest cache (`~/.animus/cache/manifests/`) that backs the v0.5.9 discovery speed-up (v0.5.9)
│   │   ├── clear            Remove every cached manifest entry; discovery repopulates on next call
│   │   └── list             List cached entries with sha256, size, and mtime
│   └── scope                Per-project plugin scope (`.animus/plugin-scope.yaml`). Lets a project opt into a subset of the globally installed plugin set so discovery, preflight, and the plugin-status registry iterate just the project's relevant plugins (v0.5.9)
│       ├── show             Print the effective scope (mode + resolved admit-set) for the current project
│       ├── set              Write `.animus/plugin-scope.yaml` with the supplied mode + allow/extras/require sets
│       └── reset            Delete `.animus/plugin-scope.yaml` and fall back to the default scope
│
├── runner                   Inspect provider-plugin health and orphaned CLI processes
│   ├── health               Show provider plugin health (one row per discovered provider) and daemon health
│   └── orphans
│       ├── detect           Detect orphaned CLI processes tracked under the cli-tracker
│       └── cleanup          Clean orphaned CLI processes
│
├── status                   Show a unified project status dashboard
├── output                   Inspect run output and artifacts
│   ├── run                  Read run event payloads
│   ├── phase-outputs        Read persisted workflow phase outputs
│   ├── artifacts            List artifacts for an execution id
│   ├── download             Download an artifact payload
│   ├── jsonl                Read aggregated JSONL output streams for a run
│   ├── monitor              Inspect run output with optional task/phase filtering
│   └── cli                  Infer CLI provider details from run output
│
├── mcp                      Run the Animus MCP service endpoint
│   ├── serve                Start the MCP server in the current process. --management also exposes the animus.interactions.* inbox tools (off by default so agent-injected servers cannot answer their own approvals); --agent-id <ID> pins the identity used by the blocking ask/request_approval tools; --workflow-id <ID> pins the workflow context and flips their default wait mode to suspend (env fallback: ANIMUS_MCP_WORKFLOW_ID)
│   ├── memory               Start the memory context MCP server for workflow phases
│   ├── auth <server>        Authenticate an OAuth-protected MCP server (discovery + DCR + auth_code/PKCE + browser login); tokens stored in the OS keychain. Least-privilege: with no --scopes/config scopes, requests NONE (server default). Previews scopes + asks y/N before opening the browser. --url for servers not in config; --scopes to override; --yes to skip the prompt; --dry-run to resolve scopes without authorizing
│   ├── auth-status          Show which OAuth-protected MCP servers are authenticated, with token expiry per principal. --server + --url to inspect a URL-bound token not in config
│   └── auth-logout <server> Delete stored OAuth tokens for an MCP server. --url to address a token authenticated against a not-in-config URL
│
├── web                      Serve and open the Animus web UI
│   ├── serve                Spawn installed transport_backend + web_ui plugins and report bound URLs. Requires plugins from `animus plugin install-defaults --include-transports`
│   └── open                 Open the Animus web UI URL in a browser. Resolves the URL from an installed web_ui or transport_backend plugin unless --url is supplied
│
├── init                     Initialize an Animus project from a template
│   (no subcommands)         Supports registry-backed or local copy templates, plan mode, and daemon defaults
│
├── doctor                   Run environment and configuration diagnostics. `--fix` applies safe remediations (stale daemon pid cleanup, zombie phase-session normalization, lock-file removal, chmod plugin binaries); `--fix --yes` additionally removes orphan worktrees via `git worktree remove --force`. `--check <id|category>` narrows to a single check; `--filter <substr>` keeps the legacy substring match.
│
├── trigger                  Inspect and manage event triggers
│   ├── list                 List all configured event triggers for this project
│   └── fire                 Manually fire a webhook trigger (for testing and development)
│
├── logs                     Tail and inspect daemon log output (in-tree or via log_storage_backend plugin)
│   └── tail                 Tail recent log entries from the active log storage backend
│
├── subject                  List, get, create, update, status, and delete subjects via installed subject_backend plugins
│   ├── list                 List subjects for a given kind via the active subject_backend plugin
│   ├── get                  Fetch a single subject by id from the active subject_backend plugin
│   ├── create               Create a subject through the active subject_backend plugin
│   ├── update               Update a subject through the active subject_backend plugin
│   ├── next                 Return the highest-priority Ready subject for the given kind
│   ├── status               Set the status of a subject by id through the active subject_backend
│   └── delete               Delete a subject by id; requires --yes to confirm (otherwise prints preview)
│
├── flavor                   Inspect or install Animus flavor manifests (`flavors/<name>.toml`) — v0.5
│   ├── list                 List available flavor manifests on disk
│   ├── current              Show the active flavor and drift against the manifest
│   ├── describe             Print a parsed flavor manifest (TOML by default, JSON via `--json`)
│   └── install              Install the named flavor (`default` only in v0.5); equivalent to `animus plugin install-defaults --include-subjects --include-transports` plus the default `workflow_runner` and `queue` plugins
│
├── self                     Manage the `animus` binary itself — check for and install updates
│   └── update               Check for, download, and atomically install a newer `animus` release
│
├── update                   Top-level alias for `animus self update` (`--check / --yes / --channel`)
│
├── metrics                  Manage opt-in anonymous usage telemetry (v0.5.4)
│   ├── status               Show enabled flag, install_id, pending event count, last-send timestamp
│   ├── enable               Opt in to anonymous metrics (skips the first-run prompt re-show)
│   ├── disable              Opt out and drop any buffered events
│   ├── flush                Force-send buffered events to the configured endpoint (debug)
│   └── cleanup              Sweep all scopes for orphaned/oversized flushing snapshots + oversized buffers
│
├── cost                     Inspect token + USD spend across workflow runs (v0.5.5)
│   ├── summary              Aggregate spend over `--since <DURATION>` (default 24h) + top spenders
│   ├── workflow             Per-phase breakdown for one `<WORKFLOW_RUN_ID>`
│   ├── top                  Rank workflows by `--by tokens|cost` (default cost), `--limit N`
│   ├── trends               Bucket spend by `--window day|week|month`, last `--n N` buckets
│   └── conversation         Show token + USD spend for one `<CONVERSATION_ID>` (v0.5.10)
│
├── auth                     Inspect identity + permissions (v0.5.8 small-core RBAC)
│   └── whoami               Print the currently resolved principal (id + kind + peer OS user)
│
├── events                   Stream workflow lifecycle events from the daemon (v0.5.8)
│   └── tail                 Subscribe to `workflow/events` and render phase_started / phase_completed / workflow_completed / workflow_failed; supports `--workflow-id`, client-side `--since`, `--json`. The stream naturally terminates when a `--workflow-id` filter sees workflow_completed/workflow_failed; otherwise it follows until Ctrl-C.
│
├── state                    Export and import scoped runtime state for backup or migration (v0.5.8)
│   ├── export               Write `animus-state-<scope>-<ts>.tar.zst` with `config/` + `daemon/` + `principals.yaml`
│   └── import               Restore an export archive; `--yes` overwrites, `--into-project` re-scopes
│
├── secret                   Manage project-scoped secrets stored in the OS keychain (v0.5.8)
│   ├── set                  Store a secret (value from --value or stdin)
│   ├── get                  Print a stored value
│   ├── list                 List stored KEY names (never values)
│   ├── rm                   Remove a stored secret
│   ├── import-env           Bulk-import KEY=VALUE pairs from a .env file
│   └── export-env           Export keychain entries to a .env file (loud warn)
│
└── help                     Print this message or the help of the given subcommand(s)
```

> **v0.4.4 surfaces removed.** Use `animus subject --kind task` for the
> former `animus task ...` tree and `animus subject --kind requirement`
> for the former `animus requirements ...` tree. `animus setup` was
> folded into `animus init`, `animus now` into `animus status`, and
> `animus errors` into `animus history`. `animus cloud` was retired.
> See the v0.4.4 entry in [CHANGELOG.md](../../../CHANGELOG.md) for the
> full surface map.

## Selected Command Flags

The full flag set lives in `crates/orchestrator-cli/src/cli_types/`. This section
documents flags that were added or hardened in v0.4.0 and that callers most often
need to script against.

### `animus daemon start` / `animus daemon run` (plugin preflight)

The daemon runs a plugin preflight on every startup. Default posture is
**default-deny**: if a required role is unsatisfied — no provider plugin, no
subject backend claiming `task` / `requirement`, no `workflow_runner` plugin
(v0.5+: `launchapp-dev/animus-workflow-runner-default`), or no `queue` plugin
(v0.5+: `launchapp-dev/animus-queue-default`) — the daemon refuses to start and
prints the exact `animus plugin install ...` command to remediate. No in-tree
fallback runs in production; `--skip-preflight` bypasses the check but the
daemon will fail at the first plugin RPC if the plugin really is missing.

| Flag | Description |
|---|---|
| `--auto-install` | When preflight finds a missing role, install the daemon's recommended default plugin (pinned `owner/repo@tag`) before continuing. Avoids surprise network fetches when omitted. |
| `--skip-preflight` | Bypass preflight entirely. Escape hatch for dev iteration or intentionally degraded runs when required provider or subject plugins are not installed. |

### `animus daemon start` / `animus daemon run` (`--interval-secs` = fallback heartbeat)

As of the event-driven scheduler, `--interval-secs` is **not** the dispatch
latency. The daemon main loop wakes on events — a `daemon/nudge` control
message sent fire-and-forget by `animus subject create/update/status` and
`animus queue enqueue/release` (and their MCP equivalents), workflow/phase
completion events, and workflow-config hot-reloads — and on precise cron
deadlines computed from compiled `schedules:`. `--interval-secs` is the
fallback heartbeat: the maximum time the loop sleeps when no event arrives.
It bounds how long out-of-band state mutations (processes editing subject
state without the CLI) wait for pickup, and it paces the heavier
housekeeping legs (zombie-workflow reconciliation, manual-timeout
reconciliation, stale in-progress sweeps), which run at most once per
heartbeat period even under a burst of event wakes. See
[configuration.md#scheduler-wake-model](../configuration.md#scheduler-wake-model).

### `animus daemon restart`

Stops the running daemon (graceful shutdown, same path as `animus daemon
stop`), then starts it again. If the daemon is not running, it just starts.
Accepts every flag `animus daemon start` accepts (`--autonomous`,
`--auto-install`, `--skip-preflight`, scheduler overrides, ...) — the start
flags are taken from the restart invocation, not recovered from the previous
run, so pass `--autonomous` to restart into detached/background mode.

| Flag | Description |
|---|---|
| `--shutdown-timeout-secs <SECONDS>` | Maximum seconds to wait for in-flight agents to finish before force-stopping the old daemon (default 60). |
| (all `animus daemon start` flags) | Forwarded to the start step. |

### `animus daemon preflight`

Standalone preflight report. Runs the same checks as daemon startup but never
starts the daemon. Useful for CI and onboarding to confirm a project's plugin
prerequisites are in place.

| Flag | Description |
|---|---|
| `--auto-install` | Install missing required plugins from the daemon's recommended defaults instead of just reporting them. |

JSON envelope: `animus.daemon.preflight.v1` with fields `satisfied`, `missing`,
`auto_installed`, `flavor_manifest_error`, `ok`, `fix_message`.

Exit code matrix:

| Code | Meaning |
|---|---|
| 0 | All required roles satisfied. |
| 2 | At least one required role is missing. The error envelope's `message` carries the `animus plugin install ...` fix. CI scripts and `&&` chains can rely on this. |
| 1 | Transient plugin discovery failure (broken install index, IO error, etc.). Distinct from "ran successfully and found gaps". |

Broken flavor manifest: when `flavors/default.toml` exists on disk but fails
to load (parse error, unknown schema) and the project's plugin scope is in
`flavor-only` mode, preflight fails closed — the scope admits no plugins, so
every required role reports missing, `flavor_manifest_error` names the broken
manifest, and `fix_message` leads with "fix (or delete) the manifest" instead
of install advice. Daemon startup (`animus daemon start` / `animus daemon
run`) refuses with the same message. This matches the fail-closed posture of
discovery and `animus plugin list`; see
[plugin-scope.md](../plugin-scope.md#preflight-interaction).

### `animus daemon status` / `animus daemon health` (pause + plugin supervisor visibility)

v0.5.10: both commands surface whether the scheduling runtime is paused
(`animus daemon pause`) and what the plugin supervisor knows about each
plugin, so operators no longer have to read state files or logs to tell
"paused" apart from "stuck":

- `runtime_paused` (bool) and `paused_at` (RFC3339, present only while
  paused) appear in `animus daemon health --json` (both the live
  control-wire response and the offline fallback snapshot), in
  `animus daemon status --json` while the daemon is reachable over the
  control wire, in the `daemon` slice of `animus status`, and in the
  matching MCP tools (`animus.daemon.status`, `animus.daemon.health`).
  Exception: when the daemon is offline, `animus daemon status --json`
  (and the MCP status fallback) still returns the bare `DaemonStatus`
  string — which itself reads `"paused"` when the runtime was paused —
  without the new keys. Human-readable `animus daemon health` prints a
  `runtime: paused (since <ts>)` / `runtime: active` line.
- While the daemon is running, `animus daemon health` lists one row per
  plugin from the daemon's live status registry. A plugin disabled by the
  restart supervisor (default budget: 3 restarts in 60s, then a 5-minute
  cooldown) reports `Unhealthy` with a `disabled by supervisor after N
  restart(s); cooldown until <ts>` detail, and the top-level verdict
  degrades to `Degraded` with `last_error` naming the disabled plugins.
- `animus plugin status [--json]` carries the same supervisor state per
  plugin via the additive `disabled_by_supervisor` and `cooldown_until`
  fields (older daemons omit them; they default to `false` / absent).

All new fields are additive with serde defaults — pre-v0.5.10 payloads
still parse, and old consumers ignore the new keys.

### `animus agent run` / `animus chat send` (reasoning effort)

Both surfaces accept a `--reasoning-effort` flag that controls how much
reasoning/thinking budget the provider CLI spends on the turn.

| Flag | Description |
|---|---|
| `--reasoning-effort <LEVEL>` | One of `low`, `medium`, `high`. Threaded into the provider session request as `extras.reasoning_effort`; each provider transport maps it to its own flag (codex `-c model_reasoning_effort="<level>"`, claude `--effort <level>`). Omit to leave the provider on its own default effort. The flag overrides any `reasoning_effort` configured on the agent profile or phase runtime block. |

A caller-supplied override always wins: if a runtime contract already carries
codex's `model_reasoning_effort` config key or claude's `--effort` flag, the
transport leaves it untouched.

### `animus agent run` / `animus chat send` (permission mode)

Both surfaces accept a `--permission-mode` flag that controls the spawned
provider CLI's permission/approval posture for the run or turn.

| Flag | Description |
|---|---|
| `--permission-mode <MODE>` | Provider permission/approval mode, set on the typed `SessionRequest.permission_mode` field and forwarded verbatim; each provider transport maps it to its own flag (claude `--permission-mode <mode>`, codex `-c approval_policy="<mode>"`, gemini its approval-mode mapping). Omit to leave the provider on its own default. |

Values are provider-specific:

| Provider | Accepted modes |
|---|---|
| claude | `default`, `acceptEdits`, `bypassPermissions`, `plan` |
| codex | `untrusted`, `on-failure`, `on-request`, `never` |
| gemini | `default`, `auto_edit`, `yolo` |

Resolution precedence: the explicit `--permission-mode` flag wins over a
`permission_mode` key in `--context-json` (`animus agent run` only), which
wins over the selected `--agent` profile's `permission_mode` field; when none
is set the field stays unset and the provider uses its own default. A value
outside the union of known provider modes prints a warning on stderr but is
still passed through verbatim — Animus never blocks on it.

### `animus agent run` / `animus chat send` (kernel-mediated approvals)

Both surfaces accept an `--approvals` flag that enables kernel-mediated
approvals for the run or turn.

| Flag | Description |
|---|---|
| `--approvals` | Sets `extras.approvals = true` on the provider session request so transports route permission decisions through the `animus.agent.request_approval` MCP tool (claude wires `--permission-prompt-tool`; other providers receive a system-prompt instruction block). Implied when the selected `--agent` profile declares an `approval_policy`; absent otherwise. |

### `animus agent run` / `animus chat send` (per-agent MCP servers)

Ad-hoc agents now receive the MCP servers their selected profile / skill
declares — a trading agent gets the trading servers, a marketing agent gets
the marketing ones — instead of no MCP servers at all. The resolved set is
the profile's `mcp_servers` ∪ the skill's `mcp_servers` ∪ `--mcp-server`
additions, minus the built-in `animus` server when `--no-animus-mcp` is
passed. Each name is resolved against the project's `mcp_servers` map
(workflow YAML `mcp_servers` first, then `.animus/config.json`); the name
`animus` resolves to the built-in `animus mcp serve` stdio surface. OAuth
servers are routed through `animus-mcp-proxy`, the same as workflow runs.

| Flag | Description |
|---|---|
| `--agent <AGENT_ID>` | Select an agent profile; the run receives that profile's declared `mcp_servers`. On `animus agent run` this is applied only when no `--runtime-contract-json` (or `runtime_contract` in `--context-json`) was supplied — a caller-supplied contract is never clobbered. |
| `--skill <SKILL>` | Select a skill; its declared `mcp_servers` are unioned into the resolved set. An unknown skill name is an error. |
| `--mcp-server <NAME>` | Add an MCP server by name (repeatable). The name must exist in the project's `mcp_servers` map, or `animus` for the built-in surface; an unknown name is an error. |
| `--no-animus-mcp` | Drop the built-in `animus` server from the resolved set. |

When no profile/skill names any server (plain `animus chat send` or a bare
`animus agent run`), the baseline set is just the built-in `animus` server so
the agent still has the Animus tools. A tool whose CLI cannot speak MCP
(`cli/capabilities/supports_mcp` is false) receives no MCP wiring.

### `animus agent interactions`

The inbox for human-in-the-loop round-trips. Agents running with the injected
`animus` MCP server can call the `animus.agent.ask` /
`animus.agent.request_approval` tools; in block mode (the default for ad-hoc
runs) each call parks the agent on a pending interaction stored under
`~/.animus/<repo-scope>/interactions/` until a human answers here (or the
call times out — questions return a structured best-judgment error,
approvals deny fail-closed). When the serving MCP process is pinned to a
workflow (`animus mcp serve --workflow-id <ID>` or `ANIMUS_MCP_WORKFLOW_ID`)
the default wait mode is suspend instead: the tool returns immediately, the
workflow is paused, and answering here resumes it with the decision as
feedback via the detached-runner resume path (only suspend-created records
ever trigger a resume — a block-mode payload `workflow_id` is observability
metadata only). If the resume spawn fails the
answer still succeeds and the output carries a `workflow_resume.guidance`
field with the exact `animus workflow resume <id>` command to run. All
subcommands support `--json` with the standard `animus.cli.v1` envelope.

```bash
animus agent interactions list                # pending only
animus agent interactions list --all          # include answered + expired
animus agent interactions show <ID>
animus agent interactions answer <ID> --text "use the copy table"   # question
animus agent interactions answer <ID> --allow                       # approval
animus agent interactions answer <ID> --deny --message "too risky"  # approval
```

| Flag | Description |
|---|---|
| `--all` | (`list`) Include answered and expired interactions; default lists pending only |
| `--agent <AGENT_ID>` | (`list`) Filter by the requesting agent profile id |
| `--text <TEXT>` | (`answer`) Answer text for a question interaction |
| `--allow` / `--deny` | (`answer`) Decision for an approval interaction; exactly one is required |
| `--message <TEXT>` | (`answer`) Optional message returned to the agent alongside the decision |
| `--by <NAME>` | (`answer`) Who answered; defaults to `human` |

Answering emits an `interaction_answered` record to the daemon event log
(creation and expiry emit `interaction_created` / `interaction_expired`), so
`animus daemon events` surfaces the round-trip without polling the store.

### `animus queue hold` / `release` / `drop` (bulk subject operations)

`hold`, `release`, and `drop` accept one or more subject ids as positional
arguments. Each id is processed independently: per-item failures do not stop
the batch, results are summarized at the end, and the exit code is non-zero
if any item failed. The legacy `--subject-id <ID>` flag form still works and
may be combined with positional ids.

```bash
animus queue hold TASK-001 TASK-002 TASK-003
animus queue drop --all --yes
animus queue release --subject-id TASK-001   # legacy flag form
```

| Flag | Description |
|---|---|
| `--all` | Target every queue entry eligible for the verb (`hold`: pending, `release`: held, `drop`: pending/held/assigned). Mutually exclusive with explicit subject ids. |
| `--yes` | Skip the confirmation prompt required by `--all`. Required in non-interactive contexts (scripts, CI, `--json` pipelines). Only valid together with `--all`. |

With `--json`, the `animus.cli.v1` envelope carries per-item results:
`{"op", "all", "requested", "succeeded", "failed", "items": [{"subject_id",
"ok", "dropped_entries"?, "error"?}], "via": "plugin_host"}`. When some items
fail, the command emits an error envelope whose `error.details` field carries
the same per-item payload.

The MCP tools `animus.queue.hold` / `release` / `drop` accept either a single
`subject_id` or a `subject_ids[]` array and route through the same CLI bulk
path.

### `animus workflow prune` / `animus workflow delete`

Reclaim disk from finished workflow runs. Both commands remove the run's row
(and checkpoints) from `workflow.db` plus its `runs/<run-id>/`,
`artifacts/<run-id>/`, and `state/workflows/<run-id>/` (persisted phase
outputs) directories under the scoped runtime root
(`~/.animus/<repo-scope>/`). Legacy repo-local run paths are never touched.
Only terminal runs (`completed`, `failed`, `escalated`, `cancelled`) are ever
eligible — in-progress, queued, and paused runs are always skipped, and
`animus workflow delete` refuses a non-terminal run.

Both commands are dry-run by default: they print the runs that would be
deleted and the bytes that would be reclaimed. Pass `--yes` (alias `--force`)
to actually delete. With `--json`, output is an `animus.cli.v1` envelope whose
`data` carries `dry_run`, the `deleted` list (`workflow_id`, `status`,
`bytes_reclaimed`), and `total_bytes_reclaimed`.

| Flag | Description |
| --- | --- |
| `--older-than <DAYS>` | (prune) Only prune runs that completed (or started) more than DAYS days ago |
| `--keep-last <COUNT>` | (prune) Keep the COUNT most recent matching runs overall — not per workflow definition — and prune the rest |
| `--status <STATUS>` | (prune) Only prune runs with this terminal status; default is all terminal statuses |
| `--run-id <RUN_ID>` | (delete) Workflow run identifier to delete |
| `--yes` / `--force` | Actually delete; without it the command is a dry-run preview |

```bash
animus workflow prune --older-than 30              # preview
animus workflow prune --older-than 30 --yes        # delete
animus workflow prune --keep-last 50 --status failed --yes
animus workflow delete --run-id <RUN_ID> --yes
```

### `animus logs tail`

Tail recent persisted log entries from the active log storage backend. This is
the bounded, pull-style log reader; for live structured events use
`animus daemon stream`.

| Flag | Description |
|---|---|
| `--plugin <NAME>` | Filter entries to a named source plugin. With the in-tree fallback this matches the structured entry's `provider` field |
| `--level <LEVEL>` | Minimum severity to include. One of `debug`, `info`, `warn`, `error`. Default `info` |
| `--since <DURATION>` | Only return entries newer than the supplied duration (for example `1h`, `30m`, `15s`). Default `1h` |
| `--limit <COUNT>` | Maximum number of entries to return. Default `100` |
| `--follow` | Reserved for future streaming support. Today the in-tree fallback still returns the requested batch and exits, so use `animus daemon stream` for live follow behavior |

When a `log_storage_backend` plugin is installed, `animus logs tail` reads
through that backend. Set
`ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN=1` to force the in-tree
`~/.animus/<repo-scope>/logs/events.jsonl` fallback.

### `animus init`

Initialize an Animus project from a template registry or a local template directory.

| Flag | Description |
|---|---|
| `--template <TEMPLATE_ID>` | Project template id to fetch from the default template registry. Conflicts with `--path` |
| `--path <PATH>` | Local template directory containing `template.toml`. Conflicts with `--template` |
| `--non-interactive` | Run without prompts. Requires `--template`, `--path`, or `--walkthrough` |
| `--plan` | Preview init changes without writing project files |
| `--force` | Overwrite existing project files targeted by the template |
| `--update-registry` | Fetch the latest commit from the template registry and re-pin the local cache before loading the template (v0.4.0 supply-chain hardening — by default the registry uses the pinned cache) |
| `--walkthrough` | Run the onboarding walkthrough: detect CLIs, install default plugins, and copy the bundled hello-world workflow |
| `--no-install` | Walkthrough only: skip `animus plugin install-defaults` |
| `--no-template` | Walkthrough only: skip copying the hello-world workflow template into `.animus/workflows/` |
| `--auto-start` | Walkthrough only: start the autonomous daemon after init completes |
| `--walkthrough-template <NAME>` | Walkthrough only: choose the bundled workflow template. Current default is `hello-world` |

The template registry URL can be overridden globally via `ANIMUS_TEMPLATE_REGISTRY_URL`.
In `--json` mode, `animus init` also returns `recommended_install`, sourced from
`crates/orchestrator-cli/config/default-install.json`, so automations can read the
recommended pack and plugin set without scraping prose.

### `animus plugin install`

Install a plugin binary into `~/.animus/plugins/` after verifying its integrity.

Three install sources, mutually exclusive:

```bash
# 1. Public GitHub repo (latest release, or pinned with @tag / --tag)
animus plugin install launchapp-dev/animus-provider-claude
animus plugin install launchapp-dev/animus-provider-claude@v0.2.2
animus plugin install launchapp-dev/animus-provider-claude --tag v0.2.2

# 2. Local binary
animus plugin install --path ./target/release/animus-provider-claude

# 3. HTTPS URL with mandatory checksum
animus plugin install --url https://example.com/plugin --sha256 a1b2c3d4...
```

| Argument / Flag | Description |
|---|---|
| `<OWNER/REPO[@TAG]>` | Public GitHub repo slug (positional). Resolves the latest release (or supplied tag), downloads the matching architecture asset, verifies the published checksum, installs the binary, and registers it in `~/.animus/plugins.yaml`. Mutually exclusive with `--path` and `--url` |
| `--path <PATH>` | Local path to the plugin binary. SHA256 verification is optional for local installs |
| `--url <URL>` | HTTPS URL to download the plugin binary from. `--sha256` is **required** when installing from a URL (v0.4.0 supply-chain hardening) |
| `--tag <TAG>` | Release tag to install when using the `owner/repo` positional. Defaults to the latest release. Conflicts with the `@tag` syntax on the positional |
| `--latest` | Explicit opt-in to resolving the latest release when no tag is given (this is the default; the flag exists for self-documenting commands). Conflicts with `--tag` and `owner/repo@tag` syntax |
| `--name <NAME>` | Optional logical plugin name. Defaults to the binary file name |
| `--sha256 <HEX>` | Expected SHA256 hex digest. Required with `--url`; optional with `--path` or a public-repo install. The install fails if the downloaded/copied binary's checksum does not match |
| `--force` | Overwrite an existing installed plugin with the same name |
| `--skip-manifest-check` | Skip running `--manifest` against the installed binary to verify it (use sparingly) |
| `--plugin-dir <PATH>` | Override the plugin install directory. Takes precedence over `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/` |
| `--signature-policy <strict\|warn\|disabled>` | Signature enforcement mode. `strict` fails closed, `warn` logs and proceeds, and `disabled` skips verification |
| `--allow-unsigned` | Convenience alias for `--signature-policy warn`; mutually exclusive with `--signature-policy` and `--require-signature` |
| `--require-signature` | Legacy alias for `--signature-policy strict` |
| `--skip-signature` | Legacy alias for `--signature-policy disabled` |
| `--trusted-signers <PATH>` | Path to a trusted-signers YAML allowlist. Defaults to `~/.animus/trusted-signers.yaml`. When the file is absent, the CLI verifies signatures against the cert's stated repo identity but does not enforce a publisher allowlist |
| `--allow-shadow-builtin` | Permit installing a provider plugin whose `provider_tool` collides with an in-tree backend (`claude` / `codex` / `gemini` / `opencode` / `oai-runner`). Without this flag the install pipeline refuses such plugins because they silently hijack all dispatch for the matching tool |
| `--allow-org <OWNER>` | Mark an additional GitHub owner as trusted (repeatable). Skips the trust-on-first-use prompt for that owner and writes the entry to `~/.animus/trusted-orgs.yaml` after the install succeeds |
| `--yes` | Auto-confirm the trust-on-first-use prompt for unknown orgs |
| `--force-rewrite-lockfile` | Discard an unparseable / schema-incompatible `.animus/plugins.lock` (or `~/.animus/plugins.lock`) and rebuild a fresh lockfile starting from this install. Without this flag, an unreadable lockfile fails the install **closed** with an actionable error pointing at the corrupt path. **Security warning**: rewriting drops the recorded sha256 integrity history, so subsequent `--force` installs cannot detect pre-existing tamper. See [Security › Lockfile fail-closed policy](../security.md#lockfile-fail-closed-policy) |
| `--as-kind <KIND>` | (v0.5.7) Override the user-facing `installed_kind` recorded in `plugins.lock` for a `subject_backend` plugin. The supplied `KIND` becomes the prefix the SubjectRouter dispatches against (e.g. `archive` for a second `subject_kind:task` backend). When omitted and the manifest-declared native kind collides with an existing install, the install pipeline auto-increments (`task` → `task-2` → `task-3`) and prints the assignment via the `animus.plugin.install.v1` envelope. When `--as-kind` is supplied and the explicit value also collides, the install fails with an actionable error. Only `subject_backend` plugins are eligible in v0.5.7; passing `--as-kind` on a provider, transport, workflow_runner, queue, or trigger plugin is rejected. See [Plugin kind translator (v0.5.7)](../../architecture/plugin-kind-translator-v0.5.7.md) |

#### Signature verification (v0.4.x+)

When installing from a public repo, the CLI looks for a cosign keyless bundle next to the release asset and verifies it via `cosign verify-blob`. The outcome (one of `verified`, `unsigned`, `invalid`, `untrusted_signer`, `skipped`) is persisted in `~/.animus/plugins.yaml` and surfaced in the `SIG` column of `animus plugin list`. See [Security](../security.md) and [Plugin Signing](../../architecture/plugin-signing.md).

- **`--signature-policy strict`**: refuse install when the bundle is missing, invalid, or signed by an untrusted identity. Requires `cosign` on `$PATH`.
- **`--signature-policy warn`**: log signature failures and install with `signature_status=unsigned` or `untrusted_signer`. This is the v0.4.12 transition default.
- **`--signature-policy disabled`**: bypass verification entirely; install records `signature_status=skipped`.
- **Legacy flags**: `--require-signature` maps to `strict`, `--skip-signature` maps to `disabled`, and `--allow-unsigned` maps to `warn`.

The trusted-signers file format:

```yaml
trusted_signers:
  - identity: "launchapp-dev/animus-*"
    issuer: "https://token.actions.githubusercontent.com"
```

`identity` is a glob (`*` / `?`) matched against `<owner>/<repo>`. When the file is absent, the default is "any signer is acceptable, but the cosign cert must claim an identity rooted at the repo we downloaded from."

### `animus flavor`

v0.5 introduces a single curated flavor manifest at
`flavors/default.toml`. The `animus flavor` subcommand inspects and installs
it.

```bash
# Show every flavor manifest the loader discovered.
animus flavor list

# Print the parsed manifest (TOML by default, JSON via --json).
animus flavor describe --name default
animus flavor describe --name default --json

# Show drift: which required plugins are installed vs missing.
animus flavor current
animus flavor current --json

# Install the flavor. v0.5 only supports `default`; refuses other names
# per the "One flavor at launch" discipline rule.
animus flavor install            # uses `default`
animus flavor install default
```

`animus flavor install` is equivalent to
`animus plugin install-defaults --include-subjects --include-transports`.
That install path always includes the base provider set plus the default
`workflow_runner` and `queue` plugins, then layers on the subject and
transport plugins behind the explicit `--include-*` flags. It installs every
required plugin slug the manifest declares whose curated tag is pinned in
[`crates/orchestrator-core/src/plugin_registry.rs`](../../../crates/orchestrator-core/src/plugin_registry.rs).
Slugs the manifest declares but the constants table hasn't pinned yet (e.g.
`animus-provider-ollama`, `animus-trigger-cron`) emit a warning and are
skipped — the manifest is forward-looking; the constants table is the
authoritative tag pin.

The loader probes for `flavors/<name>.toml` in this order:

1. `$ANIMUS_FLAVORS_DIR` if set
2. `<cwd>/flavors/`
3. parent directories walking up to `/`

JSON output uses the `animus.flavor.cli.v1` envelope.

### `animus plugin install-defaults`

Bulk-install the standard provider plugin set in one shot. The base plan now
also includes the default `workflow_runner` and `queue` plugins required by
daemon preflight. Each repo runs through the same install pipeline as
`animus plugin install`, so signature checks, manifest probes, and the
`launchapp-dev` org allowlist are preserved.

When `flavors/default.toml` is present, the install plan is sourced from the
manifest's `[providers]`, `[subjects]`, `[transports]`, `[workflow_runner]`,
and `[queue]` sections (plus optional `[ui]` and recommended add-ons gated by
`--include-*` flags). When the manifest is absent, the hardcoded
`DEFAULT_PROVIDER_PLUGINS / DEFAULT_WORKFLOW_RUNNER_PLUGINS /
DEFAULT_QUEUE_PLUGINS / DEFAULT_SUBJECT_PLUGINS /
DEFAULT_TRANSPORT_PLUGINS` tables remain the fallback.

```bash
# Install the base defaults: 5 providers + workflow_runner + queue
animus plugin install-defaults

# Add the OAI-agent plugin
animus plugin install-defaults --include-oai-agent

# Add the default subject_backend plugins (default, requirements, linear, sqlite, markdown)
animus plugin install-defaults --include-subjects
```

| Flag | Description |
|---|---|
| `--plugin-dir <PATH>` | Override the plugin install directory. Same semantics as `animus plugin install --plugin-dir` |
| `--force` | Reinstall plugins that are already present (default: skip with a warning) |
| `--yes` | Auto-confirm the trust-on-first-use prompt for the `launchapp-dev` org |
| `--include-oai-agent` | Also install `animus-provider-oai-agent` (curated tag in `orchestrator-core::plugin_registry::DEFAULT_OAI_AGENT_PLUGINS`) |
| `--include-subjects` | Also install the default subject_backend plugins (`subject-default`, `subject-requirements`, `subject-linear`, `subject-sqlite`, `subject-markdown`) |
| `--include-transports` | Also install the default transport_backend + web_ui plugins (`transport-http`, `transport-graphql`, `web-ui`) that back `animus web` |
| `--json` | Emit per-plugin results + summary as JSON |
| `--force-rewrite-lockfile` | Discard an unparseable / schema-incompatible `plugins.lock` and rebuild a fresh lockfile for the batch. Without this flag the batch fails closed up front, *before* the per-target skip loop runs, so an all-skipped run cannot mask a corrupt lockfile. Same security caveat as `animus plugin install --force-rewrite-lockfile` |

The command pins each install to the curated release tags declared in
[`crates/orchestrator-core/src/plugin_registry.rs`](../../../crates/orchestrator-core/src/plugin_registry.rs)
and are shared with the daemon preflight, so bumping the registry rolls both
surfaces at once. Plugins that fail to install are recorded in the summary's
`failed` count, the per-repo failure is emitted in the JSON envelope, and the
process exits non-zero so installer scripts can detect partial failure
(codex round-6 P2). Exit semantics: 0 when every plugin installed or was
skipped as already present; non-zero on any failure, with the per-plugin
JSON summary (`results[].status` = `installed`/`skipped`/`failed` +
`message`) still printed first so machine callers can attribute the failure.
The root-level `animus --json` flag is honored in addition to the
subcommand's `--json`.

### `animus plugin list` / `info` / `call` / `ping`

The discovery scan deliberately omits `$PATH` by default in v0.4.0 to prevent stray
binaries from being picked up. Pass `--include-system-path` to opt in to scanning
`$PATH` for `animus-provider-*` and `animus-plugin-*` binaries.

`animus plugin info`, `animus plugin ping`, and `animus plugin call` spawn the
target binary with manifest-derived env checks enabled. If the plugin declares
required vars in `env_required` and they are unset, these commands now fail
before handshake instead of proceeding with a partially initialized process.

| Command | Flags |
|---|---|
| `animus plugin list` | `--include-system-path` |
| `animus plugin info` | `--name <NAME>`, `--include-system-path` |
| `animus plugin call` | `--name <NAME>`, `--method <METHOD>`, `--params <JSON>`, `--include-system-path` |
| `animus plugin ping` | `--name <NAME>`, `--include-system-path` |
| `animus plugin uninstall` | `--name <NAME>`, `--plugin-dir <PATH>` |

Default discovery order (no `--include-system-path`):
`~/.animus/plugins.yaml` (or the legacy `~/.config/animus/plugins.yaml` only
when the new registry is absent) → `.animus/plugins/` → global install dir
(`$ANIMUS_PLUGIN_DIR` when set, otherwise `~/.animus/plugins/`) →
`$ANIMUS_PLUGIN_PATH`. With `--include-system-path`, `$PATH` is appended.

`animus plugin list --json` returns a top-level `warnings` array when a configured
plugin failed its `--manifest` probe (binary missing, exited non-zero, returned
non-JSON, etc.). Human output emits each warning to stderr. The
`animus.plugin.list` MCP tool carries the same `warnings` field.

### `animus web serve` / `open`

`animus web` uses the same manifest-derived env checks as the one-shot plugin
commands above. Required vars declared by the selected `transport_backend` or
`web_ui` plugin must be present before the CLI will spawn them.

| Command | Flags |
|---|---|
| `animus web serve` | `--open`, `--json` |
| `animus web open` | `--url <URL>`, `--path <PATH>`, `--json` |

`animus web serve --open` starts the transport plugins and launches the
resolved browser URL in one step. `animus web open --url <URL>` skips plugin
discovery entirely and opens the supplied URL directly; `--path <PATH>`
appends a sub-path such as `/runs` when the URL is resolved from installed
plugins.

If `animus web serve` or `animus web open` fails even though the transport
plugins are installed, inspect the target plugin with
`animus plugin info --name <plugin-name>` and set any missing `env_required`
entries first.

### `animus plugin search` / `browse` / `update` / `outdated`

Marketplace commands read the public plugin registry. Update is registry-free
as of v0.5.8 — its source of truth is the bundled
`crates/orchestrator-cli/config/default-install.json` (the same file
`animus plugin install-defaults` resolves against).

| Command | Flags |
|---|---|
| `animus plugin search [QUERY]` | `--kind <KIND>`, `--tag <TAG>` (repeatable), `--org <ORG>`, `--stability <STABILITY>`, `--registry-url <URL>`, `--no-cache`, `--offline`, `--json` |
| `animus plugin browse` | `--kind <KIND>`, `--installed`, `--available`, `--registry-url <URL>`, `--no-cache`, `--offline`, `--json` |
| `animus plugin update` | `--all` \| `--kind <KIND>` \| `--name <NAME>` (exactly one required), `--check`, `--yes`, `--tag <TAG>` (only with `--name`), `--force`, `--restart-daemon`, `--json` |
| `animus plugin outdated` | `--exit-code`, `--registry-url <URL>`, `--no-cache`, `--offline`, `--json` |

Registry fetch resilience: a registry GET retries twice with short backoff on
transient failures (connection errors, 5xx, and HTTP 429 — the 429 error
message names the rate limit explicitly and points at `--offline`). When the
cached index (`~/.cache/animus/plugin-registry.json`, 6h TTL) has expired and
the network fetch still fails, the command falls back to the **stale** cache
with a loud age warning on stderr instead of hard-failing. `--offline` skips
the network entirely and serves the cache regardless of age (erroring only
when no cache exists yet); `--no-cache` forces a fresh fetch and never falls
back. The two flags are mutually exclusive.

`animus plugin outdated` reports version drift for every installed plugin:
installed tag vs the recommended pin in `default-install.json` vs the latest
tag published in the registry. Per-row `status` is `current`, `outdated`,
`ahead`, `unknown` (no pin and no registry entry), or `local` (plugins
installed from `--path`/`--url` sources that drift tracking cannot apply to).
The registry fetch is best-effort: with `--offline`, latest tags come from
the cached registry index when one exists (regardless of age); when the
registry cannot be resolved at all (no cache in offline mode, or the network
fetch and stale-cache fallback both fail), the command still compares against
the pins alone and reports `latest` as unknown (`registry_reachable: false`
plus `registry_error` in the JSON envelope). The command is informational and
always exits 0 — pass `--exit-code` to exit non-zero when at least one plugin
is outdated, for CI gates.

`--check` (or the legacy `--dry-run` alias) prints the diff and exits without
writing anything. `--yes` skips the confirmation prompt. `--force` reinstalls
even when the installed tag already matches the recommended pin, and downgrades
when the installed tag is *ahead* of the pin. Plugins installed from
`--path`/`--url` sources (i.e. `source_kind != "release"`) and plugins whose
slug has no recommended pin are reported with a clear `skip` note and never
mutated. `--restart-daemon` restarts the running daemon after a fully
successful update (graceful stop, then a detached/background start with
default flags) so the new plugin binaries are picked up; when the daemon is
not running it is a no-op with a note, and when any plugin update failed the
restart is skipped. In `--json` mode the outcome is reported under a
`daemon_restart` key in the result envelope.

### `animus plugin lock`

The plugin lockfile records installed plugin version and SHA256 metadata.
Project-local installs use `.animus/plugins.lock`; otherwise commands fall back
to `~/.animus/plugins.lock`.

| Command | Flags |
|---|---|
| `animus plugin lock list` | `--lockfile <PATH>`, `--json` |
| `animus plugin lock verify` | `--lockfile <PATH>`, `--plugin-dir <PATH>`, `--json` |

### `animus plugin doctor` (v0.5.7)

Per-role view of installed plugins, with explicit collision flags.
Iterates every required role from the daemon preflight spec
(`at_least_one_provider`, `subject_kind:task`, `subject_kind:requirement`,
`workflow_runner`, `queue`) and lists every installed plugin claiming
that role, showing both the user-facing `installed_kind` and the
plugin's manifest-declared `native_kind`.

```bash
animus plugin doctor
animus plugin doctor --json
```

Output highlights:

- `[ok] <role>` — exactly one plugin claims the role, or each claimant
  has a distinct `installed_kind`.
- `[COLLISION] <role>` — two or more plugins share the same
  `installed_kind` for the role. Each colliding kind is listed under a
  `! duplicate installed_kind '<kind>' claimed by multiple plugins`
  marker.
- `[UNSATISFIED] <role>` — no installed plugin claims this role; the
  daemon preflight will refuse startup until an `animus plugin install`
  remediates it.

The `--json` shape (`PluginDoctorOutput`) is wrapped in the standard
`animus.cli.v1` envelope when paired with the root `--json` flag.

See [Plugin kind translator (v0.5.7)](../../architecture/plugin-kind-translator-v0.5.7.md)
for the underlying renaming mechanism.

### `animus plugin cache` (v0.5.9)

Inspect or wipe the on-disk plugin manifest cache that backs the v0.5.9
discovery speed-up. Cached manifests live under
`~/.animus/cache/manifests/<sha256>.json` (override the parent directory
with `$ANIMUS_CACHE_DIR`); each cache hit replaces a `--manifest`
subprocess probe with a stat + JSON read.

```bash
animus plugin cache list           # show every cached entry
animus plugin cache list --json
animus plugin cache clear          # wipe the cache; discovery repopulates next call
animus plugin cache clear --json
```

Set `ANIMUS_DISABLE_MANIFEST_CACHE=1` to bypass the cache entirely (the
clear/list commands still work — they just report the cache as
disabled).

### `animus plugin rename <PLUGIN_NAME> --to <NEW_KIND>` (v0.5.8)

Post-install rename of a plugin's `installed_kind`. Reuses the v0.5.7
install pipeline's collision check, auto-increment behavior, and
invalid-character validation — only the lockfile's `installed_kind`
slot changes; the on-disk binary, manifest, and `native_kind` are
untouched.

```bash
animus plugin rename animus-subject-default --to archive
animus plugin rename animus-subject-default --to task --force   # auto-increments past collisions
```

| Flag | Description |
|---|---|
| `<PLUGIN_NAME>` (positional) | Lockfile entry name. Matches the `name` recorded by `animus plugin install` (the `--name <NAME>` override when supplied, otherwise the binary basename). |
| `--to <NEW_KIND>` | New `installed_kind`. Rejected if it contains `/`, `*`, `:`, or whitespace, mirroring `--as-kind` on install. |
| `--force` | When `--to` collides with another installed plugin, auto-increment (`task` -> `task-2` -> ...) instead of failing. Without `--force` a collision is a hard error so the operator picks the suffix explicitly. |
| `--json` | Emit the `animus.plugin.rename.v1` envelope as JSON. |

The handler errors out cleanly when `PLUGIN_NAME` has no lockfile
entry — install the plugin first or check `animus plugin lock list`.

### `animus plugin new`

Scaffold a new plugin project from the
[`launchapp-dev/animus-plugin-template`](https://github.com/launchapp-dev/animus-plugin-template)
repository. Clones the template at the requested ref, copies the
`<kind>/` subdirectory into the output directory, substitutes
`{{var}}` markers, and strips the `.tmpl` suffix from rendered files.

| Flag | Description |
|---|---|
| `--kind <KIND>` | Plugin kind: `subject`, `provider`, or `trigger` |
| `--name <NAME>` | Plugin short name in kebab-case (e.g. `jira`, `linear`, `openai-compat`) |
| `--org <ORG>` | GitHub org used in the generated project's repository field. Default `launchapp-dev` |
| `--description <TEXT>` | Short description. Defaults to `An Animus <kind> backend plugin` |
| `--out-dir <PATH>` | Output directory. Defaults to `./animus-<kind>-<name>` |
| `--template-version <REF>` | Git branch or tag to clone. Default `main` |
| `--template-repo <URL>` | Template git URL. Defaults to `launchapp-dev/animus-plugin-template` |
| `--template-path <PATH>` | Use a local checkout of the template repo (skips `git clone`) |
| `--force` | Overwrite an existing output directory |

Substitution variables (hardcoded today; see `template-manifest.toml`
in the template repo for the source of truth): `name`, `NAME_UPPER`,
`NAME_PASCAL`, `name_snake`, `kind`, `full_name`, `description`,
`org`, `year`, `author` (from `git config user.name`),
`author_email` (from `git config user.email`).

### `animus plugin scaffold trigger <NAME>`

Emit a minimal, self-contained starter Cargo project for a custom
trigger backend plugin. Unlike `animus plugin new`, this subcommand
writes everything from built-in templates so it works offline and
pins a known-good `launchapp-dev/animus-protocol` tag. The generated
project compiles against the in-tree wire shape the daemon's
[`TriggerSupervisor`](../../crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs)
expects.

See [Authoring Trigger Plugins](../../guides/authoring-trigger-plugins.md)
for a full walkthrough.

| Argument / Flag | Description |
|---|---|
| `<NAME>` | Plugin short name in kebab-case (e.g. `fswatch`, `cron`, `slack-thread`) |
| `--owner <OWNER>` | GitHub user/org for the generated `repository` field. Default `$USER`, then `launchapp-dev` |
| `--out-dir <PATH>` | Output directory. Default `./animus-trigger-<name>` |
| `--license <ID>` | SPDX license identifier for `Cargo.toml`. Default `MIT` |
| `--description <TEXT>` | Short description. Defaults to `Custom Animus trigger backend plugin (<name>)` |
| `--protocol-tag <TAG>` | Tag of `launchapp-dev/animus-protocol` to pin the generated project's protocol + runtime deps to. Default `v0.5.5` |
| `--force` | Overwrite an existing output directory |
| `--json` | Emit the result envelope as JSON |

Output layout:

```
animus-trigger-<name>/
  - Cargo.toml          # depends on animus-plugin-protocol + animus-plugin-runtime @ <protocol-tag>
  - plugin.toml         # static manifest (kind = trigger_backend, env_required = [])
  - src/main.rs         # initialize + trigger/watch + trigger/ack + health/check
  - README.md           # build, install, wire, debug
  - .gitignore
```

### `animus update` (v0.5.8)

Top-level shorthand for the existing `animus self update` flow. Polls
`launchapp-dev/animus-cli` GitHub releases, verifies the downloaded
tarball against the `digest` field on the asset (or the sha256 sidecar
inline in the release body, when present), and atomically swaps the
running binary in place via a same-directory rename. See
[self-update.md](../self-update.md) for the full lifecycle, asset
naming conventions, host allowlist, and rollback procedure.

| Flag | Description |
|---|---|
| `--check` | Print latest available + installed, exit without touching the binary. Exit 0 when an update is available, 1 when already current. |
| `--yes` | Skip the interactive `[y/N]` confirmation (required under CI / when stdin is not a tty). |
| `--channel <stable\|nightly>` | Release channel to poll. `stable` follows the latest non-prerelease release (default); `nightly` follows the most recent prerelease (mapped to `AutoUpdateChannel::Prerelease`). |
| `--json` (global) | Emit the `animus.update.cli.v1` envelope: `{ schema, action: "up_to_date" \| "available" \| "installed", current, latest?, installed?, channel }`. |

`animus self update` keeps its existing surface (`--check-only / --force
/ --prerelease / --yes`) and is unchanged. The top-level command is a
discoverability alias — both call the same `run_manual_update` flow.

## Summary

| Metric | Count |
|---|---|
| Top-level commands | 23 |
| Nested command entries (all levels) | 175 |

Counts exclude autogenerated `help` entries.
