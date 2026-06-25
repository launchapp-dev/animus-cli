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
family — including `pack`, `skill`, `trigger`, `logs`, `history`,
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
returns one envelope and exits; its `--follow` flag is a hidden deprecated
no-op (see the `animus logs tail` section below).

### Destructive commands: dry-run by default, `--yes` to apply

The convention for destructive verbs is dry-run by default: invoked without
`--yes` they print a preview of what *would* be deleted and exit 0 without
touching anything; pass `--yes` to actually
apply. `subject delete`, `workflow prune`, and `workflow delete` follow it
today. Two families still differ: `queue drop --all` confirms interactively
and requires `--yes` in non-TTY contexts (scripts, CI, `--json` pipelines),
while `skill uninstall` / `pack uninstall` apply immediately and take
`--dry-run` to preview instead. New destructive verbs should follow the
dry-run-by-default + `--yes` convention.

### Status values

Status enums are kebab-case in canonical form (`ready`, `in-progress`,
`blocked`, `done`); the snake_case spellings (e.g. `in_progress`) also parse
as aliases. Examples in this reference use kebab-case throughout.

---

## CLI Conventions

The command surface converges on a small set of conventions:

- **Verbs**: `list` / `get` / `info` / `create` / `update` / `delete`.
  `info` is the detail-view verb across groups (`plugin info`, `pack info`,
  `skill info`, `flavor info`). The pre-convergence verbs (`pack inspect`,
  `skill show`, `flavor describe`, `output run`, `project load`) were
  retired in v0.5.14 — only the primary names parse.
- **Confirmation**: destructive verbs are dry-run by default and take `--yes`
  to apply (`workflow prune --yes`, `subject delete --yes`, `queue drop --all
  --yes`). `--force` is reserved for overriding a *safety check* (overwrite an
  existing install, remove a dirty worktree, bypass a reference guard), not
  for skipping confirmation. `workflow pause/cancel --confirm <id>` is a
  deliberate repeat-the-id safety pattern for remote mutation and stays as-is.
- **IDs**: domain-prefixed id flags (`--task-id`, `--run-id`,
  `--workflow-id`) are accepted wherever the domain is unambiguous; workflow
  commands take `--workflow-id` as an alias of `--id`. `subject --id` stays
  kind-generic by design.
- **Durations**: age/window flags accept unit suffixes `s`/`m`/`h`/`d`
  (`--since 2h`, `workflow prune --older-than 12h`). Bare numbers keep their
  historical default unit (`--older-than 30` still means 30 days).

Note on overlapping observability surfaces: `daemon logs` reads the daemon's
own process log and `daemon events` prints daemon event history, while
`logs tail` reads the structured log storage backend and `events tail`
streams workflow lifecycle events. They are distinct data sources, not
aliases of each other. **Start at `animus daemon observe`** — it is the single
entry point that prints a data-source matrix (verb | data source | live? |
filters | when to use) and routes to the right surface via `--source`,
`--follow`, `--since`, and `--workflow`. See the `animus daemon observe`
section below and `docs/reference/observability.md`.

## Top-Level Command Tree

```
animus
├── version                  Show installed animus version
├── daemon                   Manage daemon lifecycle and automation settings
│   ├── start                Start the daemon as a detached background process (always detaches as of v0.5.14)
│   ├── run                  Run the daemon in the current foreground process (dev/debug)
│   ├── stop                 Stop the running daemon
│   ├── restart              Stop the running daemon (graceful), then start it again detached with the supplied start flags
│   ├── status               Show daemon runtime status
│   ├── health               Show daemon health diagnostics
│   ├── pause                Pause daemon scheduling
│   ├── resume               Resume daemon scheduling
│   ├── observe              One observability front-door: routes to events/logs/stream; bare prints a data-source matrix + recent merged tail
│   ├── events               Print recent daemon event history scoped to the current project and exit; `--follow` keeps streaming new events until Ctrl-C; `--all-projects` shows events for every project root on this host
│   ├── logs                 Read daemon logs
│   ├── stream               Stream structured log events in real-time across daemon, workflows, and runs
│   ├── clear-logs           Clear daemon logs
│   ├── agents               List daemon-managed agents
│   ├── config               Update daemon automation configuration
│   ├── preflight            Report plugin preflight status (which required plugins are installed, which are missing, and the fix commands)
│   └── metrics              Print daemon observability metrics (counters, gauges, histograms); subcommands manage opt-in anonymous usage telemetry (v0.5.14). Offline (no daemon): bare invocation prints the telemetry status and exits 0; `--watch` still requires a live daemon
│       ├── status           Show telemetry enabled flag, install_id, pending event count, last-send timestamp
│       ├── enable           Opt in to anonymous metrics (skips the first-run prompt re-show)
│       ├── disable          Opt out and drop any buffered events
│       ├── flush            Force-send buffered events to the configured endpoint (debug)
│       └── cleanup          Sweep all scopes for orphaned/oversized flushing snapshots + oversized buffers
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
│   ├── decisions            Show phase-advance decision history recorded on workflow state (advance/skip/rework verdicts). For the per-run LLM decision log, use `animus output decisions`
│   ├── checkpoints
│   │   ├── list             List checkpoints for a workflow
│   │   ├── get              Get a specific checkpoint for a workflow
│   │   └── prune            Prune checkpoints using count and/or age retention
│   ├── run                  Run a workflow. Spawns a detached workflow_runner by default; use --sync to run in terminal (both require the workflow_runner plugin)
│   ├── resume               Resume a paused workflow and respawn its workflow_runner
│   ├── resume-status        Check whether a workflow can be resumed
│   ├── pause                Pause an active workflow (confirmation required)
│   ├── cancel               Cancel a workflow (confirmation required)
│   ├── prune                Prune terminal workflow runs from history and disk; dry-run by default, `--yes` deletes
│   ├── delete               Delete a single terminal workflow run from history and disk; dry-run by default, `--yes` deletes
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
│   │   ├── reload           Re-run YAML compile pipeline (hot-reload fallback)
│   │   ├── set              Replace the full config via the writable config_source plugin (validates first; rejected on read-only sources)
│   │   ├── agent-set        Create or replace one agent definition (read-modify-write the full config)
│   │   ├── agent-remove     Remove one agent definition (read-modify-write the full config)
│   │   ├── workflow-set     Create or replace one workflow definition (read-modify-write)
│   │   └── workflow-remove  Remove one workflow definition (read-modify-write)
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
├── history                  Inspect and search execution history (records carry `run_id` when resolvable, pivoting into `animus output read --run-id`)
│   ├── task                 List history records for a task
│   ├── get                  Get a history record by id
│   ├── recent               List recent history records
│   ├── search               Search history records (`--since 7d|12h|30m` relative window, or RFC3339 `--started-after`/`--started-before`; `--since` conflicts with `--started-after`)
│   └── cleanup              Remove old history records
│
├── git                      Inspect Git repositories and worktrees
│   ├── repo
│   │   └── list             List registered repositories
│   └── worktree
│       ├── list             List repository worktrees
│       └── prune            Prune managed task worktrees for done/cancelled tasks
│
├── approval                 Manage approval records gating destructive operations (formerly `git confirm`)
│   ├── request              Request an approval record for a destructive operation
│   ├── respond              Approve or reject an approval request
│   └── outcome              Record operation outcome for an approval request
│
├── skill                    Author, search, install, update, uninstall, and publish versioned skills
│   ├── create               Author a new skill definition at project (default) or user scope
│   ├── search               Search skills across built-in, user, project, and registry sources
│   ├── install              Install a skill with deterministic resolution
│   ├── list                 List all available skills; definition rows carry a non-fatal `warnings` array when inert tool-id declarations are detected
│   ├── info                 Show details of a resolved skill definition; includes the same non-fatal `warnings` array
│   ├── update               Re-resolve one or all installed skills
│   ├── uninstall            Remove an installed skill's materialized files and registry/lock entries (supports --source and --dry-run)
│   ├── publish              Publish a new skill version into the registry catalog
│   ├── migrate-from-ao      Move legacy .ao/skills/ into .animus/skills/ (v0.3 → v0.4)
│   └── registry
│       ├── add              Register a new registry source or update an existing one
│       ├── remove           Remove a registered registry source
│       └── list             List all registered registry sources
│
├── pack                     Install, inspect, pin, and uninstall workflow packs
│   ├── install              Install a pack from a local path or marketplace registry; installs the pack's declared non-optional pack dependencies (skip with --no-deps), checks [[requires_plugins]] against the installed-plugin registry (--install-plugins installs missing ones non-interactively), and --dry-run prints the dependency closure + plugin requirements without installing
│   ├── list                 List discovered packs and indicate which ones are active for this project
│   ├── info                 Show details of a discovered pack or a local pack manifest, including pack dependencies and required plugins with installed/missing status
│   ├── pin                  Pin a pack version/source or toggle enablement for this project
│   ├── uninstall            Remove an installed pack (all versions or --version) plus its project selection entry; refuses while project workflow YAML references the pack unless --force (supports --dry-run)
│   ├── search               Search packs across marketplace registries
│   ├── publish              Register a pack in a locally cached marketplace registry clone and print git commit/push instructions (does not push automatically)
│   └── registry
│       ├── add              Add a marketplace registry (git URL)
│       ├── remove           Remove a marketplace registry
│       ├── list             List all registered marketplace registries
│       └── sync             Sync (re-clone) a registry to get latest pack catalog
│
├── plugin                   Discover, inspect, install, and call Animus STDIO plugins
│   ├── list                 Discover plugins via plugins.yaml (a derived cache regenerated from plugins.lock), .animus/plugins/, $ANIMUS_PLUGIN_DIR, and $ANIMUS_PLUGIN_PATH
│   ├── info                 Print a plugin's manifest plus initialize-time capabilities
│   ├── call                 Send a JSON-RPC request to a plugin and print its response
│   ├── ping                 Health-check a plugin by spawning it, completing the handshake, and pinging
│   ├── install              Install a plugin binary from a public GitHub release (OWNER/REPO[@TAG]), a local path, or a direct URL into ~/.animus/plugins/ (override with --plugin-dir or $ANIMUS_PLUGIN_DIR)
│   ├── uninstall            Remove a previously installed plugin from ~/.animus/plugins/ (override with --plugin-dir or $ANIMUS_PLUGIN_DIR) and ~/.animus/plugins.yaml
│   ├── prune                Remove stale `plugins.yaml` entries whose binary is gone. Dry-run by default; `--yes` removes the entries. Stale-entry warnings in `plugin list`/`outdated`/`browse --installed` point here for cleanup
│   ├── new                  Scaffold a new plugin project from the launchapp-dev/animus-plugin-template scaffold
│   ├── scaffold             Emit a minimal offline starter Cargo project for a new plugin kind
│   ├── search               Search the public Animus plugin registry by substring + filters
│   ├── browse               Browse the public Animus plugin registry, grouped by kind
│   ├── update               Update installed release-source plugins to the recommended pins from `default-install.json` (`--all`, `--kind`, or `--name`)
│   ├── outdated             Report version drift: installed tag vs recommended pin vs latest published tag
│   ├── install-defaults     Install every plugin the flavor manifest (`--flavor <name>`, default `default`) marks `required` from public GitHub releases. `--include-recommended` adds the recommended set. Skips plugins that are already installed
│   ├── lock                 Inspect and verify the plugin lockfile (`.animus/plugins.lock`) — the SOURCE OF TRUTH for the installed plugin set. It records sha256 + version + source for every installed plugin; `plugins.yaml` (the registry discovery reads) is a DERIVED cache regenerated from the lock on every install/update/uninstall, so the two can never drift
│   │   ├── list             List every entry currently recorded in the plugin lockfile
│   │   └── verify           Re-hash every installed plugin binary and report drift against the lockfile: mismatch (sha changed), missing_binary, and extra (installed but not in the lockfile). Exits non-zero on any drift (CI gate)
│   ├── doctor               Per-role view of installed plugins. Shows every preflight role with its installed plugins (by installed_kind + native_kind) and flags duplicates so collisions are visible without spelunking through the lockfile (v0.5.7)
│   ├── rename               Rename an installed plugin's dispatch kind without touching the on-disk binary
│   ├── status               Per-plugin runtime status (pid, state, last RPC, restart count, last error). Answers "why does this plugin feel stuck?" by surfacing the supervisor's restart counter for every discovered plugin (v0.5.8)
│   ├── cache                Inspect or wipe the on-disk plugin manifest cache (`~/.animus/cache/manifests/`) that backs the v0.5.9 discovery speed-up (v0.5.9)
│   │   ├── clear            Remove every cached manifest entry; discovery repopulates on next call
│   │   └── list             List cached entries with sha256, size, and mtime
│   ├── scope                Per-project plugin scope (`.animus/plugin-scope.yaml`). Lets a project opt into a subset of the globally installed plugin set so discovery, preflight, and the plugin-status registry iterate just the project's relevant plugins (v0.5.9)
│   │   ├── show             Print the effective scope (mode + resolved admit-set) for the current project
│   │   ├── set              Write `.animus/plugin-scope.yaml` with the supplied mode + allow/extras/require sets
│   │   └── reset            Delete `.animus/plugin-scope.yaml` and fall back to the default scope
│   ├── trust                Inspect the TOFU org allowlist (`~/.animus/trusted-orgs.yaml`)
│   │   └── list             List trusted orgs (current + revoked tombstones) with `trusted_at`/`revoked_at` and how each grant was decided
│   └── revoke-trust         Revoke trust for a GitHub org (tombstones `revoked_at` so re-installs re-prompt); the built-in `launchapp-dev` org cannot be revoked
│
├── status                   Show a unified project status dashboard. `--failures N` sets how many recent workflow failures to list (default 3; applies to both human and JSON output). Human view includes three additional sections: **Task Summary** (live from SubjectRouter; renders "unavailable" with an error when the router is unreachable), **Blocked / Paused** (id + reason + blocked_by + age), and **Needs You** (pending agent interactions with answer-command hints). The Daemon section reports `provider_plugins_healthy` (the replacement for the removed `runner_connected`/`runner_pid` fields, which were dropped from both the human view and the `--json` envelope)
├── output                   Inspect run output and artifacts
│   ├── read                 Read run event payloads (`--run-id`, or `--workflow-id` to resolve the latest run recorded for that workflow; clear error when none or ambiguous)
│   ├── phase-outputs        Read persisted workflow phase outputs. Human view renders one block per phase (verdict, reason, commit) plus a Skills section — requested vs applied (with source scope and contribution kinds) vs missing — so an attached skill is verifiably applied, never a silent no-op; `--json` carries the raw outputs incl. the persisted skill records
│   ├── artifacts            List artifacts for an execution id
│   ├── download             Download an artifact payload
│   ├── jsonl                Read aggregated JSONL output streams for a run
│   ├── monitor              Inspect run output with optional task/phase filtering
│   ├── cli                  Infer CLI provider details from run output
│   └── decisions            Read the per-run LLM decision log (`runs/<run_id>/decisions.jsonl`: session header, prompts, tool calls/results, errors) by `--run-id` or `--workflow-id`. Distinct from `workflow decisions`, which shows phase-advance decision history stored on workflow state
│
├── mcp                      Run the Animus MCP service endpoint
│   ├── serve                Start the MCP server in the current process. --management also exposes the animus.interactions.* inbox tools (off by default so agent-injected servers cannot answer their own approvals); --agent-id <ID> pins the identity used by the blocking ask/request_approval tools; --workflow-id <ID> pins the workflow context and flips their default wait mode to suspend (env fallback: ANIMUS_MCP_WORKFLOW_ID)
│   ├── memory               Start the memory context MCP server for workflow phases
│   ├── auth <server>        Authenticate an OAuth-protected MCP server (discovery + DCR + auth_code/PKCE + browser login); tokens stored in the OS keychain. Scopes: with no --scopes/config scopes, auto-detects the server's advertised scopes_supported (marked auto-detected); if none advertised, requests NONE (server default). Previews scopes + asks y/N before opening the browser. --url for servers not in config; --scopes to override/narrow (or `--scopes none` to force no scopes); --yes to skip the prompt; --dry-run to resolve scopes without authorizing. Delegated (headless/web) two-step flow for a remote host (e.g. the portal) that drives the redirect itself: `--print-url --redirect-uri <callback>` resolves + registers/configures the client and PRINTS the authorization URL + `state` (persisting the PKCE state) instead of opening a browser or binding a localhost listener; `--complete --code <code> --state <state>` runs in a fresh process to exchange the callback code for a token. The interactive loopback flow (no flags) is unchanged
│   ├── auth-status          Show which OAuth-protected MCP servers are authenticated, with token expiry per principal. --server + --url to inspect a URL-bound token not in config
│   └── auth-logout <server> Delete stored OAuth tokens for an MCP server. --url to address a token authenticated against a not-in-config URL
│
├── web                      Serve and open the Animus web UI
│   ├── serve                Spawn installed transport_backend + web_ui plugins and report bound URLs. Requires plugins from `animus plugin install-defaults --include-transports`
│   └── open                 Open the Animus web UI URL in a browser. Resolves the URL from an installed web_ui or transport_backend plugin unless --url is supplied
│
├── init                     Initialize an Animus project from a template
│   (no subcommands)         Supports registry-backed or local copy templates, plan mode, and daemon defaults. Also scaffolds `animus.toml`, `.env.example`, and a merge-safe project `.gitignore`
│
├── install                  Resolve `animus.toml` into the lockfile and install the declared plugins + packs. `--locked` reproduces exactly the committed `.animus/plugins.lock` (npm-ci style; fails on manifest↔lock drift)
│   (no subcommands)         `--force` reinstalls already-present dependencies
│
├── add                      Add a plugin (or pack with `--pack`) to `animus.toml` and install it. SPEC is `name[@version]`, `OWNER/REPO[@tag]`, or a bare `name`; `--path PATH` adds a local source dependency
│   (no subcommands)
│
├── remove                   Remove a plugin (or pack with `--pack`) from `animus.toml` and uninstall it
│   (no subcommands)
│
├── doctor                   Run environment and configuration diagnostics. Exits non-zero (code 5, Unavailable) when any `[fail]` check remains after the run; `[warn]` findings do not affect the exit code. `--fix` applies safe remediations (stale daemon pid cleanup, zombie phase-session normalization, lock-file removal, chmod plugin binaries); `--fix` also prunes stale cli-tracker entries for exited CLI processes (absorbed from the removed `animus runner orphans` verbs; live tracked PIDs get a manual `kill` suggestion instead because the tracker is global across projects); `--fix --yes` additionally removes orphan worktrees via `git worktree remove --force`. `--check <id|category>` narrows to a single check; `--filter <substr>` keeps the legacy substring match. API-key checks are satisfied by a provider-CLI login session or an OS keychain entry, not just an environment variable.
│
├── trigger                  Inspect and manage event triggers
│   ├── list                 List all configured event triggers for this project
│   └── fire                 Manually fire a webhook trigger (for testing and development)
│
├── logs                     Tail and inspect daemon log output (in-tree or via log_storage_backend plugin)
│   └── tail                 Tail recent log entries from the active log storage backend. `--follow` is a hidden deprecated no-op; use `animus daemon stream` for live follow behavior
│
├── subject                  List, get, create, update, status, and delete subjects via installed subject_backend plugins
│   ├── list                 List subjects for a given kind via the active subject_backend plugin
│   ├── get                  Fetch a single subject by id from the active subject_backend plugin
│   ├── create               Create a subject through the active subject_backend plugin
│   ├── batch-create         Create up to 100 subjects from a JSON `--file` items array; `--on-error stop|continue`; exits non-zero when any item failed (payload under `/error/details`); mirrors the `animus.subject.batch-create` MCP tool
│   ├── update               Update a subject through the active subject_backend plugin
│   ├── batch-update         Patch up to 100 subjects from a JSON `--file` items array; `--on-error stop|continue`; exits non-zero when any item failed (payload under `/error/details`); mirrors the `animus.subject.batch-update` MCP tool
│   ├── next                 Return the highest-priority Ready subject for the given kind
│   ├── status               Set the status of a subject by id through the active subject_backend
│   └── delete               Delete a subject by id; requires --yes to confirm (otherwise prints preview)
│
├── flavor                   Inspect or install Animus flavor manifests (`flavors/<name>.toml`) — v0.5
│   ├── list                 List available flavor manifests on disk
│   ├── current              Show the active flavor and drift against the manifest
│   ├── info                 Print a parsed flavor manifest (TOML by default, JSON via `--json`)
│   └── install              Install every plugin the named flavor's manifest marks `required` (delegates to `animus plugin install-defaults --flavor <name>`); `--include-recommended` adds the recommended set
│
├── update                   Manage the `animus` binary itself — check for, download, and atomically install a newer release (`--check / --yes / --channel / --force / --prerelease`). When the binary is managed by [avm](https://github.com/launchapp-dev/avm) (running from `~/.avm/versions/`), `update` defers to avm and prints the `avm install` / `avm use` commands instead of self-replacing the managed binary
│
├── cost                     Inspect token + USD spend across workflow runs (v0.5.5)
│   ├── summary              Aggregate spend over `--since <DURATION>` (default 24h) + top spenders. Default reports spend incurred inside the window only; `--lifetime` reports each touched run's full lifetime spend instead (restores pre-v0.5.x semantics). Output splits reported vs estimated cost with `(est.)` markers. `--by provider|model|task` groups spend by tool, model, or subject/task with token/USD totals and percentages
│   ├── workflow             Per-phase breakdown for one `<WORKFLOW_RUN_ID>`. `--by provider|model|phase` regroups the same run's spend (rejected for archived runs, which lack per-phase detail)
│   ├── top                  Rank workflows by `--by tokens|cost` (default cost), `--limit N`. `--by model` / `--by provider` switch to a cross-run model/provider leaderboard (token/USD totals + percentages). Grouped views print a one-line note when the `unknown` attribution bucket exceeds 20% of grouped cost
│   ├── trends               Bucket spend by `--window day|week|month`, last `--n N` buckets (workflow-level totals; not split by provider/model)
│   ├── conversation         Show token + USD spend for one `<CONVERSATION_ID>` (v0.5.10)
│   └── decisions            List recorded budget-cap breaches from the scoped breach log; `--since <DURATION>` filters the window
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
│   ├── export-env           Export keychain entries to a .env file (loud warn)
│   └── migrate              Move every stored secret between backends (`--to device|keyring`); non-destructive unless `--remove-source`
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

## Project manifest: `animus.toml` (`install` / `add` / `remove`)

A committed `animus.toml` declares a project's intended kernel, plugins, and
packs — the npm/cargo model. `animus install` resolves it into
`.animus/plugins.lock` (the source of truth) and installs the set; the lock
pins the exact per-platform artifacts. `animus init` scaffolds `animus.toml`,
`.env.example`, and a merge-safe project `.gitignore`, so onboarding is:

```bash
git clone <repo> && cd <repo>
cp .env.example .env && $EDITOR .env   # fill in the project's declared keys
animus install            # resolve animus.toml -> lock -> install plugins + packs,
                          # then load .env into the secret store (warns on unset keys)
# in CI / a container:
animus install --locked   # reproduce EXACTLY the committed lockfile (npm ci)
```

### `animus.toml` format

```toml
[project]
kernel = ">=0.6.8"

[plugins]
animus-provider-claude = ">=0.2.7"                                                        # curated: version req
animus-queue-default   = { git = "launchapp-dev/animus-queue-default", tag = "v0.3.3" }   # explicit git pin
animus-config-postgres = { path = "deploy/plugin-src/animus-config-postgres" }            # local/vendored

[packs]
"animus.core-skills" = ">=0.1.0"
```

A dependency value is one of: a bare version string (resolved against the
curated plugin/pack tables at install time), `{ git = "OWNER/REPO", tag = "..." }`,
or `{ path = "..." }`. Version requirements are advisory in v1 — the curated
tag (or the explicit git tag) selects the release, and the lock records the
installed sha. `animus init` emits explicit `{ git, tag }` pins for the default
set so the scaffold is fully reproducible.

- `animus install [--locked] [--force]` — resolve + install plugins and packs,
  then **provision secrets from `.env`** into the device-encrypted store
  (idempotent; keys already set are skipped) and warn about any keys declared
  (uncommented) in `.env.example` that are still unset. `--locked` refuses to
  proceed if the manifest declares a plugin the lockfile does not pin (run
  `animus install` without `--locked` to refresh). A missing `.env` is a no-op,
  so the secret step never blocks the install.
- `animus add <spec> [--pack] [--path PATH] [--force]` — `spec` is
  `name[@version]`, `OWNER/REPO@tag`, or a bare `name`. Updates `animus.toml`
  and installs the one dependency. `--pack` targets `[packs]`.
- `animus remove <name> [--pack]` — drop from `animus.toml` and uninstall
  (plugins) or deactivate the project's pack selection (`--pack`).

### What `animus init` commits vs ignores

The scaffolded `.gitignore` keeps the reproducibility inputs committed and the
derived/secret/scratch outputs ignored:

| Committed | Ignored |
| --- | --- |
| `animus.toml`, `animus.lock` / `.animus/plugins.lock` | `.env` |
| `.env.example` | `.animus/plugins/` (binaries), `.animus/plugins.yaml` |
| `.animus-version` | `.animus/*.lock` sidecars (lock itself re-included) |
| `.animus/workflows.yaml`, `.animus/workflows/*.yaml` | `.animus/runs/`, `.animus/artifacts/` |

The merge is additive: an existing `.gitignore` is never rewritten — only the
missing managed lines are appended.

## Selected Command Flags

The full flag set lives in `crates/orchestrator-cli/src/cli_types/`. This section
documents flags that were added or hardened in v0.4.0 and that callers most often
need to script against.

### `animus daemon start` vs `animus daemon run`

As of v0.5.14 the two verbs have a fixed, non-overlapping split:

- `animus daemon start` **always** spawns the daemon as a detached background
  process and returns immediately. The success output reports the daemon
  `pid` and the background `log_path` (`~/.animus/<repo-scope>/daemon/daemon.log`).
  Starting while a daemon is already running is idempotent: it reports the
  running pid instead of failing.
- `animus daemon run` **always** runs the daemon in the current foreground
  process. This is the dev/debug verb; use Ctrl-C to stop. `--once` runs a
  single scheduler tick and exits.

The legacy `--autonomous` flag was **removed**: detached mode is the default
and only behavior of `daemon start`, so the flag carried no meaning.
`animus daemon start --autonomous` now errors with an unknown-argument
message; drop the flag. (Use `animus daemon run` for the foreground verb.)

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
stop`), then starts it again detached — same default as `animus daemon
start`. If the daemon is not running, it just starts. Accepts every flag
`animus daemon start` accepts (`--auto-install`, `--skip-preflight`,
scheduler overrides, ...) — the start flags are taken from the restart
invocation, not recovered from the previous run.

| Flag | Description |
|---|---|
| `--shutdown-timeout-secs <SECONDS>` | Maximum seconds to wait for in-flight agents to finish before force-stopping the old daemon (default 60). |
| (all `animus daemon start` flags) | Forwarded to the start step. |

### `animus daemon preflight`

Standalone preflight report. Runs the same checks as daemon startup but never
starts the daemon. Useful for CI and onboarding to confirm a project's plugin
prerequisites are in place. Human output renders as a checklist (one
`[pass]`/`[fail]` line per required role), not a JSON blob; `--json` emits the
`animus.daemon.preflight.v1` envelope.

| Flag | Description |
|---|---|
| `--auto-install` | Install missing required plugins from the daemon's recommended defaults instead of just reporting them. |

JSON envelope: `animus.daemon.preflight.v1` with fields `satisfied`, `missing`,
`auto_installed`, `flavor_manifest_error`, `ok`, `fix_message`.

Exit code matrix:

| Code | Meaning |
|---|---|
| 0 | All required roles satisfied. |
| 2 | At least one required role is missing. The error envelope's `message` carries the `animus plugin install ...` fix per role; when more than one role is missing it also prints the one composed fix (`animus plugin install-defaults --flavor default --yes`). CI scripts and `&&` chains can rely on this. |
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
  The list view also reports aggregate provider health (absorbed from the
  removed `animus runner health` verb): a `providers` array with one entry
  per discovered provider binary (`installed` is true only when the binary
  is executable) plus a rolled-up `provider_plugins_healthy` boolean.

All new fields are additive with serde defaults — pre-v0.5.10 payloads
still parse, and old consumers ignore the new keys.

v0.5.x adds a one-line health verdict. Human `animus daemon health` output
now LEADS with a `status:` word (e.g. `stopped`, `running`, `degraded`) and a
`providers:` summary line, followed by `healthy: true|false`. `animus daemon health --json` (and
the `animus.daemon.health` MCP tool) carries the same boolean as an additive
`healthy` key. The rule:

- `healthy: false` when the daemon is not running, crashed (snapshot claims
  Running/Paused but the pid is dead), reports a critically failing
  subsystem (`Unhealthy`/`Down`), or when any plugin row reports
  `Unhealthy`/`Down` — which includes plugins disabled by the restart
  supervisor.
- `healthy: true` otherwise. Transitional `Degraded` states (daemon
  starting/stopping, a plugin restarting) stay `true`.
- A paused runtime is a deliberate operator action, not a failure: it
  renders as `healthy: true (paused)` (JSON: `healthy: true` plus
  `runtime_paused: true`).

### `animus daemon observe` (observability front-door)

`animus daemon observe` is the single entry point for the daemon's
observability surfaces. It is a **router, not a new data path** — every branch
delegates to the existing `daemon events` / `daemon logs` / `daemon stream`
handlers; it adds no reader of its own.

| Flag | Behavior |
|---|---|
| `--follow` | Live follow: delegates to `daemon stream` (pretty for humans, raw JSONL under `--json`). Carries `--workflow` and `--limit` as the stream tail. Cannot be combined with `--since` (a live tail has no past window). |
| `--since <DURATION>` | Recent window (`15m`, `2h`, `1d`): merges daemon **events** + **logs** chronologically and labels each line's source (`events` / `logs`). Also applies to `--source` routes (a `--source stream`/`workflow` request with `--since` falls back to the windowed log collector). |
| `--source <logs\|events\|stream\|workflow>` | Route to one specific existing surface. `events` uses the project-scoped, source-labeled reader. `workflow` and `stream` route to the pretty `daemon stream` tail in human mode; under `--json` or `--since` they fall back to the scoped log reader so the JSON contract and the window still hold. Combining a non-stream `--source` (e.g. `events`) with `--follow` is rejected — `--follow` only tails the live stream. |
| `--workflow <ID>` | Scope to a workflow id/ref where the underlying surface supports it (the stream surface and the merged-window event filter). |
| `--limit <COUNT>` | Number of recent merged lines for the bare/window views, and the stream tail for delegated surfaces. Default `20`. |

Bare `animus daemon observe` prints a **data-source matrix** (verb | data
source | live? | filters | when to use) followed by the last `--limit` merged
lines, so an operator who doesn't know which surface to reach for can start
here. `--json` returns `{matrix, recent}` for the bare form and `{lines}` for
the window/source forms, each line carrying its `source` label. Each `matrix`
entry is a keyed object `{verb, data_source, live, filters, when}` (not a
positional array).

Because it reuses the underlying readers, `daemon observe` inherits their
scope: events are project-scoped, logs are per-project structured logs, and
the live stream spans daemon + workflows + runs. See
`docs/reference/observability.md` for the full surface map.

### `animus daemon metrics` (offline-tolerant display)

Bare `animus daemon metrics` prints the live counter/gauge/histogram snapshot
when the daemon is reachable over the control socket. When the daemon is
**not running**, the live counters are unavailable but the offline-capable
telemetry status (opt-in state, install id, pending buffer) still prints — so
the command degrades instead of erroring:

- human: prints `daemon not running; live metrics unavailable` plus a
  one-line telemetry summary, and exits `0`.
- `--json`: returns `{"daemon_running": false, "telemetry": {...}}` and exits
  `0`.

The hard "daemon required" behavior is retained only for `--watch`: a live
dashboard is meaningless without a running daemon, so `daemon metrics --watch`
still errors when the daemon is down. The telemetry subcommands
(`status` / `enable` / `disable` / `flush` / `cleanup`) are unchanged and have
always worked offline.

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

Beyond MCP servers and tool policy, the selected `--skill`'s FULL application
now applies on both ad-hoc paths (matching workflow phases):

- **Prompt fragments** — `prompt.prefix`, `prompt.directives`, and
  `prompt.suffix` wrap the outgoing prompt (prefixes, a `Skill directives:`
  section, the body, suffixes — the same ordering as workflow phases).
  `prompt.system` rides the provider session's `system_prompt`; an explicit
  `--context-json '{"system_prompt": ...}'` value comes FIRST, then the
  skill's fragments.
- **`extra_args` / `codex_config_overrides`** — grafted onto the runtime
  contract's `cli.launch` block (the same mechanism workflow phases use);
  codex config overrides are codex-only. An explicit `--permission-mode` and
  codex `--reasoning-effort` are re-applied onto the grafted launch so CLI
  flags keep winning over the skill.
- **`env`** — rides the session request's env (and the grafted launch env).
  The plugin host still gates forwarded env against the provider plugin's
  manifest `env_required`, same as the workflow launch-env channel.
- **`model` preference / `timeout_secs`** — used when no explicit
  `--model` / `--timeout-secs` (or context-json value) is given.
- Precedence everywhere: explicit CLI flag / context-json value > skill >
  defaults. A caller-supplied `--runtime-contract-json` (or a
  `runtime_contract` key in `--context-json`) disables skill application
  entirely — a hand-built contract is the full-override channel.
- On `animus chat send`, the skill binds once per send invocation and applies
  to every turn attempt. A skill with launch-affecting fields (`extra_args` /
  `codex_config_overrides` / `env`) forces the full-history replay path
  instead of native session resume, so the launch flags apply to every
  turn's provider process consistently. `animus agent run` deliberately has
  no analogous replay-forcing: it is single-shot, so it rebuilds the launch
  graft from the real prompt on every invocation — there is no native-resume
  seam to poison. Even `agent run`'s one continuation channel
  (`--context-json '{"session_id": ...}'`) forwards the freshly-grafted
  `runtime_contract` alongside the `session_id`, so the launch flags still
  apply. The asymmetry between the two paths is therefore correct, not a gap.

| Flag | Description |
|---|---|
| `--agent <AGENT_ID>` | Select an agent profile; the run receives that profile's declared `mcp_servers`. On `animus agent run` this is applied only when no `--runtime-contract-json` (or `runtime_contract` in `--context-json`) was supplied — a caller-supplied contract is never clobbered. |
| `--skill <SKILL>` | Select a skill; its declared `mcp_servers` are unioned into the resolved set, and its full application (prompt fragments, `extra_args`, `env`, `codex_config_overrides`) applies to the run as described above. An unknown skill name is an error. |
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

When the claude transport wires `animus.agent.request_approval` as the CLI's
`--permission-prompt-tool`, native tool approvals and `AskUserQuestion`
clarifying questions land in this same inbox. Native `AskUserQuestion`
records carry structured `questions[]`; `show` renders them readably
(numbered questions with options) and `answer` resolves them with `--select`
and/or `--text`.

```bash
animus agent interactions list                # pending only
animus agent interactions list --all          # include answered + expired
animus agent interactions show <ID>
animus agent interactions answer <ID> --text "use the copy table"   # question
animus agent interactions answer <ID> --allow                       # approval
animus agent interactions answer <ID> --deny --message "too risky"  # approval
animus agent interactions answer <ID> --allow --remember            # echo localSettings suggestions
animus agent interactions answer <ID> --allow --updated-input '{"command":"rm -rf build/sandbox"}'
animus agent interactions answer <ID> \
  --select "Format=Summary" --select "2=Introduction,Conclusion" \
  --text "keep it short"                      # structured (AskUserQuestion)
```

| Flag | Description |
|---|---|
| `--all` | (`list`) Include answered and expired interactions; default lists pending only |
| `--agent <AGENT_ID>` | (`list`) Filter by the requesting agent profile id |
| `--text <TEXT>` | (`answer`) Answer text for a question interaction. On structured records: maps to the single question's answer (one-question records) or to the freeform `response` (multi-question records, or when `--select` is also given) |
| `--select <QUESTION=LABEL>` | (`answer`) Answer one structured question: left side is the question text, its header, or its 1-based index; comma-separate labels (or repeat `--select` for the same question) for multi-select. Repeatable |
| `--allow` / `--deny` | (`answer`) Decision for an approval interaction; exactly one is required |
| `--message <TEXT>` | (`answer`) Optional message returned to the agent alongside the decision |
| `--remember` | (`answer`, with `--allow`) Echo the request's localSettings-destination permission suggestions back as `updatedPermissions` |
| `--updated-input <JSON>` | (`answer`, with `--allow`) Operator-modified tool input echoed as `updatedInput` instead of the original |
| `--by <NAME>` | (`answer`) Who answered; defaults to `human` |

Answering emits an `interaction_answered` record to the daemon event log
(creation and expiry emit `interaction_created` / `interaction_expired`), so
`animus daemon events` surfaces the round-trip without polling the store.

### `animus approval` (formerly `git confirm`)

Manages the approval records that gate destructive git operations.
`animus git worktree prune` refuses to run until an approved
`prune_worktrees` record exists and its id is passed back via
`--confirmation-id`. The record schema still accepts the legacy
operation types (`force_push`, `remove_worktree`, `remove_repo`,
`hard_reset`, `clean_untracked`) for plugins that perform their own git
mutations.

```bash
animus approval request --operation-type prune_worktrees --repo-name demo
animus approval respond --request-id <ID> --approve [--comment <TEXT>]
animus approval respond --request-id <ID> --reject [--comment <TEXT>]
animus approval outcome --request-id <ID> --success --message "pruned"
```

| Flag | Description |
|---|---|
| `--operation-type <TYPE>` | (`request`) Operation type, for example `prune_worktrees` |
| `--repo-name <REPO>` | (`request`) Repository the approval applies to |
| `--context-json <JSON>` | (`request`) Optional JSON context payload stored on the record |
| `--request-id <ID>` | (`respond` / `outcome`) Approval request identifier |
| `--approve` | (`respond`) Approve the request. Mutually exclusive with `--reject`; exactly one is required |
| `--reject` | (`respond`) Reject the request. Mutually exclusive with `--approve`; exactly one is required |
| `--comment <TEXT>` / `--user-id <USER>` | (`respond`) Optional reviewer comment and id |
| `--success` | (`outcome`) Mark the recorded operation as successful; omit for failure |
| `--message <TEXT>` / `--metadata-json <JSON>` | (`outcome`) Outcome message and optional metadata |

All subcommands support `--json` with the standard `animus.cli.v1` envelope.
Records live in the project-local git-confirmations store — this is a
different store from the agent human-in-the-loop inbox: approvals that agents
raise mid-run via `animus.agent.request_approval` are answered with
`animus agent interactions answer <ID> --allow|--deny`, not here.

The former `animus git confirm {request, respond, outcome}` alias was
removed in v0.5.14 — `animus approval` is the only surface.

### `animus queue enqueue` (immediate + deferred dispatch)

Enqueue a subject dispatch for a task, requirement, or custom title. By
default the entry is dispatched as soon as the daemon has capacity.

```bash
animus queue enqueue --task-id TASK-001
animus queue enqueue --requirement-id REQ-042 --workflow-ref ops
animus queue enqueue --title "Investigate flaky test" --description "Fails on CI"
```

**Deferred dispatch.** `--at` schedules the entry for a future time; it stays
queued (counted under `pending`, and broken out as `deferred` in
`queue stats`) but is not leased until the time passes, then dispatches on
the next daemon pickup.

```bash
animus queue enqueue --task-id TASK-001 --at 2026-06-13T15:00:00Z   # absolute (RFC 3339)
animus queue enqueue --task-id TASK-001 --at 2h                     # relative: 90s / 30m / 2h / 3d
animus queue enqueue --task-id TASK-001 --at 2h --expire-after 10m  # drop if not dispatched within grace
```

| Flag | Description |
|---|---|
| `--at <WHEN>` | Defer until an RFC 3339 timestamp or a relative offset (`90s`, `30m`, `2h`, `3d`; bare number = seconds). Omit for immediate dispatch. |
| `--expire-after <DURATION>` | Grace window after `--at`. If still pending past `--at + this`, the entry is dropped instead of dispatched late. Requires `--at`; omit to always fire late. |

Enqueue is **not deduplicated** (immediate or deferred): every enqueue
creates a new entry. When another entry already targets the same subject,
the enqueue still succeeds and returns a `warning` (in the human output and
the `animus.cli.v1` envelope's `warning` field) — the caller decides whether
to `drop` the duplicate. Lease-side exclusivity still prevents two entries
for the same subject from running concurrently.

> **Dispatch model.** The daemon is **queue-only**: it executes only what is
> explicitly enqueued (leasing as agent slots free) plus cron `schedules:`.
> It does **not** scan the subject backend for `Ready` tasks — putting work
> in the queue is the end user's job (an agent, a script, or a configured
> trigger calling `animus queue enqueue`). The `daemon --auto-run-ready` flag
> and `daemon.auto_run_ready` config were **removed** — declaring
> `daemon.auto_run_ready` in workflow YAML now emits a removed-key warning.

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

### `animus workflow pause` / `resume` / `cancel` (task sync)

Workflow lifecycle controls keep the bound task subject explainable instead
of leaving ghost state:

- `workflow pause` annotates the task's `blocked_reason` with
  `paused by workflow <id>` (and sets `blocked_by` to the workflow id) so
  `animus subject get` explains the stall. The annotation is informational
  only — the task's status and `paused` flag are untouched, so the daemon's
  scheduling view never changes.
- `workflow resume` clears that exact annotation. A genuine failure-projected
  `blocked_reason` is never clobbered, and any non-blocked status transition
  (e.g. `animus subject status --status ready`) also clears it.
- `workflow cancel` syncs the task to `cancelled` (unless it is already
  `done`/`cancelled`), matching the daemon-side projection used by the
  execution-fact path and the orphan reconciler — the end state is the same
  no matter which surface handled the cancel. Use the task reopen flow to
  pick the work back up.
- `animus subject status ... --status ready` prints an
  `unstuck: cleared paused, blocked_reason (...)` line (stderr, human output
  only) whenever the transition cleared stuck-state flags, so the unsticking
  is visible.

### `animus workflow prune` / `animus workflow delete`

Scope: `prune` is the batch verb — it sweeps all terminal runs matching its
filters (`--older-than`, `--keep-last`, `--status`); `delete` removes exactly
one run identified by `--run-id`.

Reclaim disk from finished workflow runs. Both commands remove the run's row
(and checkpoints) from `workflow.db` plus its `runs/<run-id>/` and
`artifacts/<run-id>/` directories under the scoped runtime root
(`~/.animus/<repo-scope>/`). Legacy repo-local run paths are never touched.
Only terminal runs (`completed`, `failed`, `escalated`, `cancelled`) are ever
eligible — in-progress, queued, and paused runs are always skipped, and
`animus workflow delete` refuses a non-terminal run.

Both commands are dry-run by default: they print the runs that would be
deleted and the bytes that would be reclaimed. Pass `--yes`
to actually delete. With `--json`, output is an `animus.cli.v1` envelope whose
`data` carries `dry_run`, the `deleted` list (`workflow_id`, `status`,
`bytes_reclaimed`), and `total_bytes_reclaimed`.

| Flag | Description |
| --- | --- |
| `--older-than <AGE>` | (prune) Only prune runs that completed (or started) more than AGE ago. Bare number = days; unit suffixes `s`/`m`/`h`/`d` accepted (`30`, `30d`, `12h`) |
| `--keep-last <COUNT>` | (prune) Keep the COUNT most recent matching runs overall — not per workflow definition — and prune the rest |
| `--status <STATUS>` | (prune) Only prune runs with this terminal status; default is all terminal statuses |
| `--run-id <RUN_ID>` | (delete) Workflow run identifier to delete |
| `--yes` | Actually delete; without it the command is a dry-run preview |

```bash
animus workflow prune --older-than 30              # preview (30 days)
animus workflow prune --older-than 30 --yes        # delete
animus workflow prune --older-than 12h --yes       # unit suffix: hours
animus workflow prune --keep-last 50 --status failed --yes
animus workflow delete --run-id <RUN_ID> --yes
```

### `animus skill create`

Author a new YAML skill definition. The written file is round-tripped through
the skill parser before landing on disk, so a malformed skill is never left
behind. Project-scoped skills shadow user-scoped skills with the same name
during resolution.

| Flag | Description |
|---|---|
| `--name <SLUG>` | Skill slug: lowercase ASCII letters/digits plus `-`/`_`, no path separators. Becomes the file name `<scope dir>/<name>.yaml` |
| `--description <TEXT>` | Human-readable description shown in `skill list` / `skill search` |
| `--prompt <TEXT>` | Skill instruction body (stored as `prompt.system`). Mutually exclusive with `--prompt-file` |
| `--prompt-file <PATH>` | Read the instruction body from a file |
| `--category <CATEGORY>` | Optional: `implementation`, `testing`, `review`, `research`, `documentation`, `operations`, `planning` |
| `--tags <TAGS>` | Optional discovery tags (comma-separated or repeated) |
| `--project` | Write to the project scope at `.animus/config/skill_definitions/` (default). Mutually exclusive with `--user` |
| `--user` | Write to the user scope at `~/.animus/config/skill_definitions/` (shared across projects) |
| `--force` | Overwrite an existing skill definition at the same scope; without it the command refuses |

```bash
animus skill create --name pr-reviewer --description "Reviews PRs" --prompt "You review pull requests."
animus skill create --name rust-tips --description "Rust guidance" --prompt-file tips.md --user
animus skill create --name pr-reviewer --description "v2" --prompt "..." --force
```

### `animus skill install`

Install a skill from a local path, a registry, or a GitHub-hosted source. The
GitHub form **imports and normalizes** any Anthropic "Agent Skills"
`SKILL.md` — the same standard used by Claude Code, Hermes, OpenClaw, and
others — into an Animus skill before installing it.

| Flag | Description |
|---|---|
| `<SOURCE>` (positional) or `--github <SOURCE>` | GitHub-hosted skill to import. Mutually exclusive with `--path`/`--name` |
| `--path <PATH>` | Install a local Markdown skill file, skill folder, or directory of skill folders |
| `--name <NAME>` | Resolve and install a named skill from the registry catalog |
| `--version <REQ>` | Optional semver constraint (registry installs) |
| `--source <SOURCE>` / `--registry <ID>` | Optional registry constraints |
| `--allow-prerelease` | Allow pre-release versions during registry resolution |

Accepted GitHub source shapes:

- `OWNER/REPO` and `OWNER/REPO@REF` (branch, tag, or commit sha)
- `https://github.com/OWNER/REPO[.git]`
- `https://github.com/OWNER/REPO/tree/<ref>/<subpath>`
- `https://github.com/OWNER/REPO/blob/<ref>/<path>/SKILL.md`
- raw `https://raw.githubusercontent.com/OWNER/REPO/<ref>/<path>/SKILL.md`

The `<ref>` may be a branch, tag, or commit sha; it is resolved to a commit sha
before fetching so refs work even on private-default-branch repos. A branch name
that itself contains `/` (e.g. `feature/foo`) is ambiguous inside a
`/tree/<ref>/<path>` URL — use the unambiguous `OWNER/REPO@feature/foo` slug
form for those.

Discovery: a subpath pointing at a single `SKILL.md` (or its folder) installs
that one skill; a directory containing multiple `skills/<name>/SKILL.md`
folders installs every skill found. Bundled `scripts/`, `references/`, and
other assets next to a `SKILL.md` are downloaded alongside it so the
instructions' relative references resolve.

Supported formats and the import mapping:

- **Anthropic "Agent Skills" `SKILL.md`** (the primary target, shared by Claude
  Code / Hermes / OpenClaw): frontmatter `name` → skill name, `description` →
  description, `allowed-tools` → the Animus `tool_policy.allow` permission
  list, and the **markdown body becomes the Animus `system` prompt** (this is
  where the instructions live). Model/agent routing stays at Animus defaults.
- **Animus-native `SKILL.md`** (frontmatter carries an `animus:` runtime
  namespace): installed as-is — its explicit `tool_policy`, `model`, adapters,
  etc. flow through unchanged and are never overwritten by a stray
  `allowed-tools` key.

Detection picks Animus-native when an `animus:` namespace is present; otherwise
the file is imported under Anthropic semantics (non-empty body as instructions).
Content with neither a `name` nor an instruction body is rejected with an
"unsupported skill format" error.

Each installed GitHub skill records provenance — its `origin` (`OWNER/REPO@ref`)
and detected `format` (`anthropic-agent-skill` or `animus-native`) — surfaced by
`animus skill list` and `animus skill info`. Public repos only; set
`GITHUB_TOKEN` to lift API rate limits.

```bash
# Import a whole repo of skills (e.g. Anthropic's skills repo)
animus skill install anthropics/skills

# One skill folder at a tag (positional source or --github)
animus skill install anthropics/skills@v1.0.0
animus skill install --github https://github.com/anthropics/skills/tree/main/document-skills/pdf

# A single raw SKILL.md
animus skill install https://raw.githubusercontent.com/owner/repo/main/skills/foo/SKILL.md

# Local install (unchanged)
animus skill install --path ./my-skill
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
| `--follow` | **Deprecated no-op (hidden).** The flag is accepted for back-compat but is ignored; the command always returns the bounded batch and exits. Use `animus daemon stream` for live follow behavior |

When a `log_storage_backend` plugin is installed, `animus logs tail` reads
through that backend. Set
`ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN=1` to force the in-tree
`~/.animus/<repo-scope>/logs/events.jsonl` fallback.

### `animus doctor` (exit-code contract)

`animus doctor` exits non-zero (code **5**, `Unavailable`) when any check
finishes with a `[fail]` verdict after the run — including after `--fix`
remediations. `[warn]` findings do not affect the exit code; the command exits
0 when only warnings remain. CI gates can rely on `$?` to distinguish clean
(`0`), warnings-only (`0`), and failed (`5`) runs.

API-key checks are satisfied by any of: the expected environment variable
present, a provider-CLI login session on disk, or an OS keychain entry for the
current `repo-scope`. The old env-var-only test was removed.

| Flag | Description |
|---|---|
| `--fix` | Apply safe local remediations (stale pid cleanup, zombie phase normalization, lock-file removal, chmod plugin binaries, prune stale cli-tracker entries for exited processes) |
| `--fix --yes` | Also remove orphan worktrees via `git worktree remove --force` |
| `--check <NAME>` | Run only checks whose id or category matches exactly (repeatable) |
| `--filter <SUBSTR>` | Run only checks whose id contains the substring (repeatable, case-insensitive) |
| `--skip-subprocess` | Skip checks that spawn external subprocesses (cosign verify, plugin `--manifest` probes) |

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
| `--walkthrough` | Run the onboarding walkthrough: detect CLIs, install default plugins, and install the recommended workflow packs. As of v0.6 the walkthrough no longer scaffolds a built-in workflow; a fresh project becomes functional via the recommended pack install (which provide workflows/phases/agents) |
| `--no-install` | Walkthrough only: skip `animus plugin install-defaults` |
| `--install-packs` | Install and activate the recommended workflow packs (`animus.core-skills`, `animus.task`, `animus.requirement`, `animus.review`) from their pinned GitHub release tags in `default-install.json`. Works with both the walkthrough and the `--template`/`--path` flows. In the interactive walkthrough this is offered automatically (default yes); non-interactive runs must pass the flag explicitly |
| `--no-template` | Deprecated no-op (kept for back-compat). The walkthrough no longer scaffolds a built-in workflow |
| `--auto-start` | Walkthrough only: start the autonomous daemon after init completes |
| `--walkthrough-template <NAME>` | Deprecated no-op (kept for back-compat, hidden). The walkthrough no longer copies a bundled workflow template |

The template registry URL can be overridden globally via `ANIMUS_TEMPLATE_REGISTRY_URL`.
In `--json` mode, `animus init` also returns `recommended_install`, sourced from
`crates/orchestrator-cli/config/default-install.json`, so automations can read the
recommended pack and plugin set without scraping prose.

Recommended pack installs are per-pack and never abort init: each pack reports
`installed`, `already_installed`, or `failed` (with the manual
`git clone ... && animus pack install --path ... --activate` recovery command).
After a successful install, init prints the first runnable workflow command
(`animus workflow run animus.task/standard --task-id ... --sync`). The
`ANIMUS_INIT_PACK_SOURCE_DIR` env var overrides the GitHub clone with a local
`<dir>/<pack-id>` source directory for offline installs and tests.

The walkthrough also detects API keys held in environment variables
(`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, ...). When found, it suggests migrating
them to the OS keychain (`animus secret set <KEY>`, or `animus secret
import-env` for `.env` files — see `docs/reference/secrets.md`), and in
interactive mode offers to store them now (default **no**; the keychain is
never touched silently or in non-interactive runs).

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

# 4. Reproducible install: reinstall exactly what `.animus/plugins.lock` pins
#    and verify each artifact sha256 against the lock (fresh-machine / CI path)
animus plugin install --locked
```

| Argument / Flag | Description |
|---|---|
| `<OWNER/REPO[@TAG]>` | Public GitHub repo slug (positional). Resolves the latest release (or supplied tag), downloads the matching architecture asset, verifies the published checksum, installs the binary, records it in the authoritative lockfile (`plugins.lock`), and regenerates the derived registry cache (`~/.animus/plugins.yaml`) from the lock. Mutually exclusive with `--path` and `--url` |
| `--path <PATH>` | Local path to the plugin binary. SHA256 verification is optional for local installs |
| `--url <URL>` | HTTPS URL to download the plugin binary from. `--sha256` is **required** when installing from a URL (v0.4.0 supply-chain hardening) |
| `--tag <TAG>` | Release tag to install when using the `owner/repo` positional. Defaults to the latest release. Conflicts with the `@tag` syntax on the positional |
| `--latest` | Explicit opt-in to resolving the latest release when no tag is given (this is the default; the flag exists for self-documenting commands). Conflicts with `--tag` and `owner/repo@tag` syntax |
| `--name <NAME>` | Optional logical plugin name. Defaults to the binary file name |
| `--sha256 <HEX>` | Expected SHA256 hex digest. Required with `--url`; optional with `--path` or a public-repo install. The install fails if the downloaded/copied binary's checksum does not match |
| `--force` | Overwrite an existing installed plugin with the same name |
| `--skip-manifest-check` | Skip running `--manifest` against the installed binary to verify it (use sparingly) |
| `--plugin-dir <PATH>` | Override the plugin install directory. Takes precedence over `$ANIMUS_PLUGIN_DIR`. Defaults to `~/.animus/plugins/` |
| `--project` | Install into the **project-local** plugin root instead of the global one: binary → `<project>/.animus/plugins/`, registry → `<project>/.animus/plugins.yaml`, lockfile → `<project>/.animus/plugins.lock`. Project-local installs shadow a global install of the same name during discovery. Signature/TOFU verification is identical to global installs. Mutually exclusive with `--plugin-dir`. See [Configuration › Project vs global plugin installs](../configuration.md#project-vs-global-plugin-installs) |
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
| `--locked` | Reproducible, **cross-platform** install: ignore any positional source and reinstall **exactly** the set pinned in `.animus/plugins.lock`. For each entry it resolves the recorded `source_repo` + tag (release slug), `--url`, or `path:` source, downloads the **current platform's** tarball, and verifies it against `targets[<current-triple>].archive_sha256` **before** extracting/executing (this is what lets a lock generated on macOS reproduce verified on a Linux container). Fails the whole run if the lockfile is missing/empty, an entry has no recorded source, or — for the current platform — has no recorded archive sha (a 1.0-migrated entry, or a lock generated without this platform: regenerate the lock here, or `plugin install` once on this platform), or any downloaded tarball drifts from the pin. The fresh-machine / CI path. Mutually exclusive with a positional source, `--path`, `--url`, `--tag`, and `--latest` |

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
animus flavor info --name default
animus flavor info --name default --json

# Show the active flavor + drift: which required plugins are installed vs missing.
# With no --name, `current` probes the project's PERSISTED active flavor
# (see "Active flavor persistence" below), falling back to `default`.
animus flavor current
animus flavor current --json
animus flavor current --name enterprise   # probe a specific flavor regardless

# Install the flavor: every plugin the manifest marks `required`.
animus flavor install            # uses `default`
animus flavor install default
animus flavor install --include-recommended   # required + recommended
```

`animus flavor install <name>` delegates to
`animus plugin install-defaults --flavor <name>`: the flavor manifest is the
source of truth for the install plan. Everything the manifest marks
`required` installs (providers, subject backends, transports,
`workflow_runner`, `queue`); `--include-recommended` adds the `recommended`
set (extra providers, web UI, triggers, ...). The required set covers every
daemon-preflight role, so `animus flavor install` followed by
`animus daemon start` works without a second install command. Tags come from
the curated pins in
[`crates/orchestrator-core/src/plugin_registry.rs`](../../../crates/orchestrator-core/src/plugin_registry.rs).
Slugs the manifest declares but the constants table hasn't pinned yet (e.g.
`animus-provider-ollama`, `animus-trigger-cron`) emit a warning and are
skipped — the manifest is forward-looking; the constants table is the
authoritative tag pin. `animus flavor current` reports drift against the
exact same required set.

The loader probes for `flavors/<name>.toml` in this order:

1. `$ANIMUS_FLAVORS_DIR` if set
2. `<cwd>/flavors/`
3. parent directories walking up to `/`

JSON output uses the `animus.flavor.cli.v1` envelope. `animus flavor current`
adds a `source` field reporting where the probed name came from: `flag` (you
passed `--name`), `persisted` (read from the project's
`.animus/plugin-scope.yaml` `active_flavor:` key), or `default` (no persisted
selection).

The drift report counts a required plugin as installed when it is present in
EITHER the global plugin install dir OR the project-scoped install set
(`<project>/.animus/plugins/` + `<project>/.animus/plugins.yaml`). A plugin
installed `animus plugin install --project` therefore reports satisfied, in
line with `animus plugin list` / `animus plugin outdated`.

#### Active flavor persistence

A successful `animus plugin install-defaults --flavor <name>` (or
`animus flavor install <name>`) records the selected flavor project-locally
in `.animus/plugin-scope.yaml` under an `active_flavor:` key. The daemon's
flavor-only plugin scope resolver and the CLI's `animus plugin list` /
`animus plugin scope show` read that selection back, so a non-default flavor's
plugins are admitted by scoped discovery instead of being filtered out against
`flavors/default.toml`. Selecting `default` again clears the persisted key
(and downgrades a leftover `flavor-only` mode to `all` when no on-disk
`flavors/default.toml` exists, so the project keeps a working scope). When the
persisted name has no `flavors/<name>.toml` on disk (a stale selection), the
resolver logs a warning and falls back to the `default` flavor's plugin set
(on-disk or binary-bundled) for the admit set — it never fail-closes
discovery to an empty set. The scope file's `mode` is preserved; `animus
plugin scope show` still reports the raw persisted name plus
`active_flavor_source`. The selection is merged into any existing scope file,
preserving the operator's `mode` / `allow` / `extras`.

### `animus plugin install-defaults`

Manifest-driven bulk install. The flavor manifest named by `--flavor`
(default: `default`, read from `flavors/<name>.toml` with a binary-bundled
fallback for `default`) is the source of truth: every plugin the manifest
marks `required` installs — providers, subject backends, transports,
`workflow_runner`, and `queue` — which covers every daemon-preflight role in
one command. `--include-recommended` adds the manifest's `recommended` set.
Each repo runs through the same install pipeline as `animus plugin install`,
so signature checks, manifest probes, and the `launchapp-dev` org allowlist
are preserved.

Unknown flavor names (no `flavors/<name>.toml` on disk) error out. Only when
`flavors/default.toml` exists but fails to load does the command fall back to
the hardcoded `DEFAULT_PROVIDER_PLUGINS / DEFAULT_WORKFLOW_RUNNER_PLUGINS /
DEFAULT_QUEUE_PLUGINS / ...` tables (with an error log naming the broken
manifest).

```bash
# Install the default flavor's required set: provider-claude,
# subject-default, subject-requirements, transport-http,
# workflow-runner-default, queue-default
animus plugin install-defaults

# Required + recommended (extra providers, subjects, graphql, web UI, ...)
animus plugin install-defaults --include-recommended

# Install another flavor manifest from flavors/<name>.toml
animus plugin install-defaults --flavor <name>

# Add the OAI-agent plugin
animus plugin install-defaults --include-oai-agent

# Back-compat: add just the recommended subject_backend plugins
animus plugin install-defaults --include-subjects
```

| Flag | Description |
|---|---|
| `--flavor <NAME>` | Flavor manifest that drives the install plan (default: `default`). Reads `flavors/<NAME>.toml` (`$ANIMUS_FLAVORS_DIR` override); the `default` flavor falls back to the manifest bundled in the binary |
| `--include-recommended` | Also install every plugin the manifest marks `recommended` |
| `--plugin-dir <PATH>` | Override the plugin install directory. Same semantics as `animus plugin install --plugin-dir` |
| `--force` | Reinstall plugins that are already present (default: skip with a warning) |
| `--yes` | Auto-confirm the trust-on-first-use prompt for the `launchapp-dev` org |
| `--include-oai-agent` | Also install `animus-provider-oai-agent` (curated tag in `orchestrator-core::plugin_registry::DEFAULT_OAI_AGENT_PLUGINS`) |
| `--include-subjects` | Back-compat: also install the flavor's *recommended* subject_backend plugins (`subject-linear`, `subject-sqlite`, `subject-markdown`, ...). The required subject backends always install |
| `--include-transports` | Back-compat: also install the flavor's *recommended* transport + web_ui plugins (`transport-graphql`, `web-ui`) that back `animus web`. The required `transport-http` always installs |
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

Stale `plugins.yaml` entries (a configured plugin whose binary path has
vanished) collapse to a single summary warning line above the table — pointing
at `animus plugin prune` as the cleanup command instead of repeating one
warning per entry. `plugin list`, `plugin outdated`, and `plugin browse
--installed` all read one reconciled discovery view; the stale-entry warning
is consistent across all three. VERSION is sourced from the lockfile tag with
a binary/source mismatch indicator when the on-disk binary's manifest-reported
version disagrees with the lockfile. Warnings from other discovery tiers (e.g.
a genuinely broken installed plugin) still print one line each. Pass
`--verbose` to restore the full per-entry detail for every tier; `--json`
always carries the complete `warnings` array regardless.

`animus plugin info`, `animus plugin ping`, and `animus plugin call` spawn the
target binary with manifest-derived env checks enabled. If the plugin declares
required vars in `env_required` and they are unset, these commands now fail
before handshake instead of proceeding with a partially initialized process.

| Command | Flags |
|---|---|
| `animus plugin list` | `--include-system-path`, `--verbose` |
| `animus plugin info <NAME>` | `--include-system-path` |
| `animus plugin call` | `--name <NAME>`, `--method <METHOD>`, `--params <JSON>`, `--include-system-path` |
| `animus plugin ping` | `--name <NAME>`, `--include-system-path` |
| `animus plugin uninstall` | `--name <NAME>`, `--plugin-dir <PATH>`, `--project` (project-local root; mutually exclusive with `--plugin-dir`) |

Default discovery order (no `--include-system-path`):
project-local tier (`.animus/plugins/` dir scan, then the
`.animus/plugins.yaml` project registry) → `~/.animus/plugins.yaml` (or the
legacy `~/.config/animus/plugins.yaml` only when the new registry is absent)
→ global install dir (`$ANIMUS_PLUGIN_DIR` when set, otherwise
`~/.animus/plugins/`) → `$ANIMUS_PLUGIN_PATH`. With `--include-system-path`,
`$PATH` is appended. The project-local tier wins name collisions, so a
project-scoped install shadows both registry-recorded and global installs of
the same name.

`animus plugin list` reports each row's install scope in the `SCOPE` column
(`project` or `global`; the same `scope` field appears in the local JSON
output). When a project-local install hides a same-named global binary, the
hidden global row is surfaced in the `shadowed` array (text mode prints
`note: global install of '<name>' at <global path> is shadowed by the
project install at <project path>`). Caveat: when the daemon is running,
`animus plugin list --json` round-trips through the daemon's control-wire
`PluginListResponse`, which does not yet carry `scope`/`shadowed` — the
discovered set is identical (project installs still win collisions), only
the envelope shape differs. Text mode and the daemon-down JSON path always
use the richer local shape.

Project-scoped installs are discovered through two project-local channels:
the `<project>/.animus/plugins/` dir scan (which matches `animus-plugin-*` /
`animus-provider-*` file names) and the project registry
(`<project>/.animus/plugins.yaml`), which resolves every other name —
official plugins like `animus-subject-default` or custom `--name` values.

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
`animus plugin info <plugin-name>` and set any missing `env_required`
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
| `animus plugin update` | `--all` \| `--kind <KIND>` \| `--name <NAME>` (exactly one required), `--check`, `--yes`, `--tag <TAG>` (only with `--name`), `--force`, `--restart-daemon`, `--project` (operate on the project-local registry + install root), `--json` |
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
is outdated, for CI gates. Project-local installs recorded in
`<project>/.animus/plugins.yaml` are included automatically; every row carries
a `scope` field (`global` or `project`, also shown as the `Scope` column in
text mode).

`animus plugin update --project` reads the installed set from the
project-local registry (`<project>/.animus/plugins.yaml`) and reinstalls
matching plugins in place under the project scope; without the flag only the
global registry is considered.

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

### `animus plugin prune`

Remove stale `plugins.yaml` registry entries — keys pointing at a binary that
has been deleted out of band (e.g. manually removed from `~/.animus/plugins/`
without going through `animus plugin uninstall`). Scans both the global
registry (`~/.animus/plugins.yaml`) and the project-local one
(`.animus/plugins.yaml`).

Dry-run by default: without `--yes` the command previews the stale set and
exits 0 without writing anything. Pass `--yes` to remove the entries.

```bash
animus plugin prune          # preview stale entries
animus plugin prune --yes    # remove them
animus plugin prune --json   # machine-readable preview/result
```

| Flag | Description |
|---|---|
| `--yes` | Remove the stale entries; without this flag the command previews only |
| `--json` | Emit the result envelope as JSON instead of human-readable text |

Stale-entry warnings in `animus plugin list`, `animus plugin outdated`, and
`animus plugin browse --installed` all point at this command for cleanup.

### `animus plugin lock`

`.animus/plugins.lock` **is** the Animus plugin lockfile. It is a TOML file
(`schema_version = "2.0"`, tool-managed — do **not** hand-edit) recording one
entry per installed plugin: `name`, `version` (the resolved release tag),
`installed_at`, `installed_kind` / `native_kind` (v0.5.7 kind translator), the
source-provenance fields `source_repo` (the `owner/repo` slug, `--url`, or
`path:<...>` the plugin was installed from) and `resolved_commit` (the exact
40-hex commit sha when a release resolved to one — releases tagged at a branch
leave it unset), and a **per-target** `targets` map.

**Platform-aware integrity (v2.0).** Each entry's integrity claim is keyed by
target triple under `[plugins.targets.<triple>]`, with `archive_sha256` (the
sha256 of the release **tarball** for that platform, exactly what a GitHub
release's `SHA256SUMS.txt` lists), an optional `signature_bundle_sha256`, and
an optional host-only `installed_binary_sha256` (the extracted-binary sha,
recorded only for the platform that performed the install — used for fast
on-disk tamper detection). A release install fetches `SHA256SUMS.txt` and
records the tarball sha for **every published platform**, so a lock generated
on macOS carries the Linux (and every other platform's) tarball sha too. That
is what makes the committed lock portable: `animus plugin install --locked` can
download the current platform's tarball and verify it against
`targets[<current-triple>].archive_sha256` **before** extracting/executing, on
a platform different from the one that generated the lock. `--path` / `--url`
sources have no `SHA256SUMS.txt`, so they record only the install platform's
target (non-portable by nature).

**1.0 → 2.0 migration.** A 1.0 lockfile's flat `artifact_sha256` was the
install machine's binary hash, not a tarball sha, so it cannot be reused as a
2.0 `archive_sha256`. Reading a 1.0 file does **not** fail: each entry migrates
to a 2.0 entry with an **empty** `targets` map and must be re-`install`ed once
on the current platform to gain a usable claim (`lock verify` reports it as
`missing_target` and `--locked` fails it with a reinstall hint). The next write
emits 2.0.

The lockfile is the integrity + reproducibility anchor: the install pipeline
writes it on success and uninstall removes the entry. `.gitignore` keeps the
installed **binaries** out of version control, but the lockfile itself is meant
to be committed so the pinned set travels with the repo and `animus plugin
install --locked` can reproduce it on a fresh machine — or a different
platform. Project-local installs use `.animus/plugins.lock`; otherwise commands
fall back to `~/.animus/plugins.lock`.

| Command | Flags |
|---|---|
| `animus plugin lock list` | `--lockfile <PATH>`, `--json` |
| `animus plugin lock verify` | `--lockfile <PATH>`, `--plugin-dir <PATH>`, `--json` |

`animus plugin lock verify` sweeps **both** lockfile roots by default: the
global `~/.animus/plugins.lock` (entries hashed against the global install
dir) and the project `<project>/.animus/plugins.lock` (entries hashed against
`<project>/.animus/plugins/` first, falling back to the global dir for
entries recorded by pre-`--project` installs). Each result entry carries a
`scope` (`global` / `project` / `explicit`, plus `discovered` for extras) and
the lockfile path it came from; the envelope's `lockfiles` array lists every
root swept and its top-level `target` names the current build triple every
entry was checked against. The verify is **target-aware**: each entry is
compared against its host-only `installed_binary_sha256` for the current
target. It reports drift in several directions: `mismatch` (sha256 disagrees
with the pin), `missing_binary` (a locked entry's binary is gone),
`missing_target` (the entry has no integrity claim for this platform — a
1.0-migrated entry, or a lock generated on another platform; reinstall here to
record it), and `extra` (a discovered installed plugin absent from every
lockfile). Passing
`--lockfile <PATH>` restricts the sweep to that single file (legacy
behavior). Any mismatch, missing binary, **or** extra in either root exits
non-zero, so the command works as a CI drift gate. Plugin lockfile drift is
**also** surfaced as a non-fatal warning by `animus daemon preflight` and at
daemon start — it appears in the preflight `warnings` array and never blocks
startup.

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

### `animus update`

The canonical self-update command. Polls
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
| `--force` | Re-install the resolved release even when it matches the installed version (repairs a broken install). |
| `--prerelease` | Consider prereleases regardless of the selected `--channel`. |
| `--json` (global) | Emit the `animus.update.cli.v1` envelope: `{ schema, action: "up_to_date" \| "available" \| "installed", current, latest?, installed?, channel }`. |

The former `animus self update` group was retired in v0.5.x — `animus
update` is the only self-update surface, with `--check` replacing the old
`--check-only` and `--force` / `--prerelease` folded in. No aliases.

### Human-mode table output (default for listing commands)

The following commands default to a human-readable table in non-`--json`
mode. Pass `--json` to get the `animus.cli.v1` envelope instead.

| Command | Notes |
|---|---|
| `animus subject list` | Table with id, title, status, priority. Returns a bounded page by default (`--limit` defaults to 50); a footer shows `next_cursor` (and `total` when the backend reports it) when more remain. Pass `--cursor <c>` to page, or `--limit 0` to remove the cap. |
| `animus subject get` | Detail card |
| `animus subject next` | Single-row card; prints nothing when no ready subject exists |
| `animus queue list` | Table with id, title, status, priority |
| `animus workflow list` | Table; empty set prints a "Start one with:" hint |
| `animus agent list` | Table of configured agent profiles |
| `animus agent interactions list` | Table of pending interactions with answer-command hints |
| `animus chat list` | Table, most-recently-updated first; `--as-user <id>` limits to that user's own + shared conversations |
| `animus pack list` | Table; active packs flagged |
| `animus skill list` | Table; `--verbose` surfaces per-file unparseable-skill warnings (otherwise suppressed/aggregated) |
| `animus daemon preflight` | Checklist (pass/fail per role), not a JSON blob |

Priority is displayed in `p0`–`p3` bucket notation in all human-readable views.

### ID normalization

`subject get/update/status --id`, `queue hold/release/drop`, and
`workflow run --task-id` all accept **either** the bare native id (e.g.
`TASK-001`) or the kind-qualified form (e.g. `task:TASK-001`). The bare form
is normalized to the qualified form at the CLI boundary, so both resolve the
same subject. You do not need to run any command in `--json` mode to discover
the id format.

### `animus pack search` (positional query)

`pack search` now takes the search query as a positional argument (with
`--query` as an alias), matching the `animus plugin search` convention:

```bash
animus pack search "database"          # positional form
animus pack search --query "database"  # flag alias
animus pack search --category devops   # filter-only, no query
```

### `animus workflow config validate` (human summary + rich errors)

`animus workflow config validate` now prints a one-line human summary by
default (e.g. `ok — 3 workflows, 0 errors, 2 warnings`) and renders
validation errors with rich caret-style line/column indicators. Under
`--json` the result envelope carries a structured `errors[]` array with
`line`, `col`, `message`, and `context` fields, matching `workflow config
compile`. The `warnings[]` array reports declared-but-unenforced fields in
both modes.

### `animus workflow config set` / entity write-back

The `set` family persists config back through the installed **writable**
`config_source` plugin (the kernel ships the entire validated canonical model;
the plugin handles storage). The kernel validates the post-pack-merge result
before writing but persists only the RAW source model, and a read-only source
(e.g. the default `animus-config-yaml`, which does not advertise the
`config_write` capability) is rejected up front with an actionable error naming
the plugin — nothing is partially written.

- `animus workflow config set --file <path>` (or stdin) — replace the entire
  RAW source `WorkflowConfig`. **Do NOT round-trip `animus workflow config get`
  into this.** `config get` returns the EFFECTIVE config (after pack overlays
  are merged); feeding it back would bake pack-provided agents/workflows/phases
  into your source and shadow later pack updates. Use `set` only with an
  externally-authored raw model; for edits, use the entity verbs below (they
  load and rewrite the raw source for you).
- `animus workflow config agent-set --id <id> --input-json <json>` — upsert one
  agent definition. Read-modify-write: the kernel loads the RAW source config,
  upserts the agent, validates the post-merge result, and writes the full raw
  model back. This is the **definition**-management verb; it does not collide
  with the runtime `animus agent {list,get,run,...}` surface.
- `animus workflow config agent-remove --id <id>` — remove one agent.
- `animus workflow config workflow-set --input-json <json>` — upsert one
  workflow definition (the JSON must include an `id`).
- `animus workflow config workflow-remove --id <id>` — remove one workflow.

After each write the kernel re-runs the config_source load+compile pipeline to
confirm the source round-trips the new model; the result envelope carries the
written config's `hash`, a `summary` of entity counts, and a `refreshed` block
with the freshly-loaded compiled hash/source. A separate running daemon
refreshes its own in-memory snapshot through the plugin's `config/changed`
watch (gated on the `config_watch` capability).

## Summary

| Metric | Count |
|---|---|
| Top-level commands | 28 |
| Nested command entries (all levels) | 214 |

Counting basis: counts are derived from the command tree above. "Top-level
commands" counts the column-0 tree roots (`animus <command>`); "Nested
command entries (all levels)" counts every indented subcommand/leaf entry
beneath them at any depth. Both exclude the autogenerated `help` entry. To
re-derive after editing the tree:

```bash
# top-level commands
grep -E '^(├──|└──) ' docs/reference/cli/index.md | grep -vE '^(├──|└──) help' | wc -l
# nested entries (all levels)
grep -E '^[│ ].*(├──|└──) ' docs/reference/cli/index.md | grep -vE '(├──|└──) help ' | wc -l
```
