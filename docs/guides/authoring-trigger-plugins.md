# Authoring Trigger Plugins

Audience: developers extending Animus with a custom event source — Slack
sockets, file watchers, cron timers, third-party webhook adapters, or
anything that pushes work into the daemon from outside.

This guide is the trigger-flavored companion to
[Plugin Author Guide](./plugin-author-guide.md). Read that first; this
document zooms in on the `trigger_backend` plugin kind specifically.

---

## 1. When to reach for a trigger plugin

Animus ships four trigger types in workflow YAML:

| `type:` | What it is | When to use it |
|---|---|---|
| `file_watcher` | Built-in glob watcher driven by the daemon's tick loop | Watching files inside the project root with a simple debounce |
| `webhook` | Built-in HTTP webhook listener | Receiving inbound HTTP POSTs (Stripe, generic SaaS) into the daemon's HTTP transport |
| `github_webhook` | Built-in GitHub webhook listener with event-shape validation | Reacting to GitHub pushes, PRs, issues |
| `plugin` | **External `trigger_backend` plugin** | Anything else: Slack sockets, JetBrains IDE hooks, custom polling adapters, queue subscriptions, cron, IMAP listeners — anything that needs first-party process state outside the daemon |

A `plugin` trigger delegates event production to a standalone executable
the daemon discovers, spawns, and supervises. The plugin emits
`trigger/event` notifications over stdio; the daemon routes each event
into the same `pending_events` queue the built-in trigger types use,
then drains them on the tick loop and dispatches the configured
workflow.

Pick `plugin` when:

- The source needs a long-lived subprocess (websocket, IMAP IDLE, an
  inotify watcher outside the project tree, a tail of a remote log).
- The event shape needs domain-specific decoding the daemon should not
  carry (Slack thread metadata, JIRA webhook signatures, a corporate
  intranet listener).
- You want to package and version the integration independently of the
  daemon release cycle.

Pick a built-in type when:

- Globs under the project root suffice (`file_watcher`).
- The daemon's HTTP transport can be a public endpoint (`webhook`,
  `github_webhook`).

---

## 2. The trigger protocol surface

Triggers are the third plugin kind alongside `subject_backend` and
`provider`. Wire shapes live in
[`crates/animus-plugin-protocol/src/lib.rs`](../../crates/animus-plugin-protocol/src/lib.rs)
and are mirrored upstream in
[`launchapp-dev/animus-protocol`](https://github.com/launchapp-dev/animus-protocol)
at tag `v0.5.5`. Generated projects depend on the upstream crate; the
in-tree copy in this repo is the source of truth the daemon supervises
against.

### Method-name constants

| Constant | Wire method | Direction | Purpose |
|---|---|---|---|
| `TRIGGER_METHOD_WATCH` | `trigger/watch` | host → plugin | Open the event stream. Plugin acks then keeps pushing notifications. |
| `TRIGGER_METHOD_EVENT` | `trigger/event` | plugin → host | Notification carrying a `TriggerEvent`. |
| `TRIGGER_METHOD_ACK` | `trigger/ack` | host → plugin | Confirms a delivered event id. Plugins use this to trim a server-side queue or advance a cursor. |

Lifecycle methods (`initialize`, `initialized`, `$/ping`,
`health/check`, `shutdown`, `exit`) are inherited from the generic plugin
contract documented in the [Plugin Author Guide](./plugin-author-guide.md#3-lifecycle).

### Event payload

A `TriggerEvent` carries:

```rust
pub struct TriggerEvent {
    pub event_id: String,
    pub trigger_id: Option<String>,
    pub subject_id: Option<String>,
    pub subject_kind: Option<String>,
    pub action_hint: Option<TriggerActionHint>,
    pub payload: serde_json::Value,
}
```

- **`event_id`** — plugin-assigned, stable across restarts when possible.
  The daemon uses it to send `trigger/ack`.
- **`trigger_id`** — matches a `triggers[].id` in your workflow YAML.
  Required for the daemon to route the event to the right
  `workflow_ref`. Best practice: read the expected `trigger_id` out of
  the opaque `config` block in `trigger/watch` so workflow authors can
  rename it without touching plugin code.
- **`subject_id` / `subject_kind`** — optional. If set, the host
  resolves the subject through the configured subject backend and may
  kick that subject's assigned workflow instead of the trigger's
  `workflow_ref`.
- **`action_hint`** — advisory. `RunWorkflow` and `CreateTask` are the
  defined variants; the host falls back to the trigger's `workflow_ref`
  when omitted.
- **`payload`** — opaque JSON forwarded to the spawned workflow as
  input. Workflow YAML templates can reference it as
  `{{trigger.payload.<key>}}`.

### Acknowledgements

`TriggerAckParams` carries the `event_id` and an optional
`TriggerAckStatus` (`Dispatched`, `Queued`, `Unmatched`, `Skipped`,
`Failed`, `Shutdown`). Backends that don't track delivery state can
return a no-op response; backends with durable state should advance a
cursor when `status == Dispatched`.

---

## 3. The daemon lifecycle

The `TriggerSupervisor` (in
[`crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs`](../../crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs))
owns the runtime relationship with every installed trigger plugin:

1. **Discover.** On daemon start the supervisor calls
   `orchestrator_plugin_host::discover_plugins(project_root)` and keeps
   the subset whose manifest declares
   `plugin_kind = "trigger_backend"`.
2. **Spawn.** Each plugin is launched as a stdio child via
   `PluginHost::spawn_with_options`, with stderr forwarded into the
   daemon log.
3. **Handshake.** The supervisor drives the `initialize` / `initialized`
   exchange. Plugins that fail the handshake within the lifecycle timeout
   are abandoned with a `StartFailed` lifecycle event.
4. **Watch.** The supervisor sends `trigger/watch` with the merged
   config from any matching workflow YAML triggers and starts draining
   the notification stream.
5. **Route.** Every `trigger/event` notification is appended to
   `TriggerRunState::pending_events`. The tick loop's
   `TriggerDispatch::process_due_triggers` drains those events each
   tick and enqueues the configured workflow.
6. **Ack.** After the host accepts an event for dispatch it sends
   `trigger/ack` with the event id and a `TriggerAckStatus`.
7. **Supervise.** If the plugin's stdio closes or an error bubbles up,
   the supervisor restarts it with exponential backoff
   (1s, 2s, 4s, 8s, 16s, capped at 60s). After `MAX_RESTART_ATTEMPTS`
   consecutive failures it emits a final `Crashed` event and gives up;
   plugins that run cleanly for at least `HEALTHY_WINDOW` reset their
   attempt counter.
8. **Shutdown.** On daemon stop the supervisor sends `shutdown` to each
   plugin, waits up to 2 seconds for the child to exit, then closes
   stdio.

The `ANIMUS_DAEMON_DISABLE_TRIGGERS=1` env var skips trigger
supervision entirely. Useful when debugging whether observed behavior
is plugin-sourced or comes from the built-in webhook/file-watcher path.

---

## 4. Manifest format

A trigger plugin needs:

1. A `--manifest` JSON output on the binary itself (the daemon's
   primary discovery path).
2. A `plugin.toml` mirror of the manifest for static tooling
   (`animus plugin info` against an uninstalled tree, CI lint, the
   public registry).

Minimal `plugin.toml` for a trigger:

```toml
name        = "animus-trigger-fswatch"
version     = "0.1.0"
plugin_kind = "trigger_backend"
description = "Reference filesystem-watch trigger backend"
binary      = "animus-trigger-fswatch"
protocol    = "stdio"

env_required = []
```

The runtime manifest emitted by `--manifest` is built by the
`animus-plugin-runtime` `Plugin::new(...).description(...).methods(...)`
chain — keep `plugin.toml` in sync with that chain when you change
declared methods or env requirements.

The host clears the daemon's environment before spawn and forwards
only a minimal shell allowlist (`PATH`, `HOME`, `TMPDIR`, `LANG`,
`LC_ALL`, `RUST_LOG`, `RUST_BACKTRACE`, `TZ`) plus anything listed in
`env_required`. Secrets the plugin needs at spawn time (Slack token,
GitHub PAT, ...) must be declared here.

---

## 5. Scaffold a new trigger plugin

`animus plugin scaffold trigger <name>` writes a minimal, self-contained
Cargo project from built-in templates. Unlike `animus plugin new`
(which clones `launchapp-dev/animus-plugin-template`), the scaffold
subcommand works offline and pins a known-good protocol tag:

```bash
animus plugin scaffold trigger fswatch \
    --owner acme-co \
    --license MIT
```

Output (default):

```
animus-trigger-fswatch/
├── Cargo.toml          # depends on animus-plugin-protocol + animus-plugin-runtime @ v0.5.5
├── plugin.toml         # static manifest mirror
├── src/main.rs         # initialize + trigger/watch + trigger/ack + health/check
├── README.md           # build, install, wire, debug
└── .gitignore
```

Flags:

| Flag | Default | Purpose |
|---|---|---|
| `<NAME>` | required | Plugin short name in kebab-case |
| `--owner <OWNER>` | `$USER` then `launchapp-dev` | GitHub user/org for the generated `repository` field |
| `--out-dir <PATH>` | `./animus-trigger-<name>` | Output directory |
| `--license <ID>` | `MIT` | SPDX license identifier for `Cargo.toml` |
| `--description <TEXT>` | auto | Short description for Cargo.toml + README |
| `--protocol-tag <TAG>` | `v0.5.5` | `launchapp-dev/animus-protocol` git tag to pin |
| `--force` | off | Overwrite existing output directory |
| `--json` | off | Emit result envelope as JSON |

The generated `src/main.rs` is a working skeleton: it implements every
method the supervisor expects and emits a periodic heartbeat event so
you can wire the plugin end-to-end before plumbing real upstream
state. Replace the loop in `spawn_event_loop` with your real upstream
listener.

---

## 6. Walkthrough: `examples/triggers/fswatch`

`examples/triggers/fswatch/` is a complete working trigger plugin you
can use as the next reference past the scaffold output. It watches a
glob list and emits a `trigger/event` notification when a matching
file is modified.

### Build + install

```bash
cd examples/triggers/fswatch
cargo build --release
animus plugin install --path target/release/animus-trigger-fswatch
animus plugin info --name animus-trigger-fswatch
```

### Workflow YAML wiring

```yaml
workflows:
  - id: review-source-change
    phases:
      - name: review
        tool: claude

triggers:
  - id: fswatch-default
    type: plugin
    workflow_ref: review-source-change
    config:
      trigger_id: fswatch-default
      globs:
        - src/**/*.rs
        - docs/**/*.md
      debounce_ms: 250
```

The daemon forwards every enabled `type: plugin` entry from workflow
YAML in `TriggerWatchParams.config`:

```json
{
  "triggers": [
    {
      "id": "fswatch-default",
      "workflow_ref": "review-source-change",
      "config": {
        "trigger_id": "fswatch-default",
        "globs": ["src/**/*.rs", "docs/**/*.md"],
        "debounce_ms": 250
      }
    }
  ]
}
```

Plugins read this list, pick the entries they understand, and emit
events whose `trigger_id` matches the corresponding `id`. The daemon
is intentionally agnostic about per-plugin filtering — it ships every
plugin-typed trigger to every trigger plugin, and each plugin is
expected to ignore unknown entries silently. This keeps the workflow
YAML schema minimal (no `plugin_name` on each trigger) at the cost of
making plugins responsible for filtering.

fswatch deserializes its entry's `config` block into:

```rust
struct FswatchConfig {
    trigger_id: Option<String>,
    globs: Vec<String>,
    debounce_ms: Option<u64>,
}
```

The plugin attaches a `notify::RecommendedWatcher` to each glob's
parent directory, debounces bursty modify events into one delivery per
debounce window, and emits:

```json
{
  "event_id": "fswatch:src/lib.rs:1717003812000",
  "trigger_id": "fswatch-default",
  "action_hint": "run_workflow",
  "payload": {
    "path": "src/lib.rs",
    "kind": "modified",
    "occurred_at": "2026-05-30T18:30:12Z"
  }
}
```

The daemon resolves `trigger_id` against the workflow YAML, runs
`review-source-change`, and acknowledges receipt via `trigger/ack`. The
fswatch plugin removes the acked id from its in-memory delivered set;
backends with durable state would persist the cursor at this point.

### Architectural notes worth lifting

- **Cancellation.** The watch handler calls
  `ctx.keep_cancellation()` so the cancellation token survives the
  ack-and-return of the handler. The background loop selects on
  `cancellation.cancelled()` and exits cleanly when the daemon issues
  `shutdown`.
- **Health.** The plugin reports `Degraded` until the watcher is
  attached, `Healthy` while it is running, and `Unhealthy` if the
  background loop crashed. Use this pattern so the daemon's
  `animus daemon health` surface reflects reality.
- **Debounce.** A flapping editor that touches a file four times in
  one save is one event. Match the debounce window to your workload.

---

## 7. Debugging

| Symptom | First check |
|---|---|
| Plugin doesn't show up in `animus plugin list` | `--manifest` JSON shape — run the binary with `--manifest` and confirm `plugin_kind == "trigger_backend"` |
| Plugin shows up but never fires events | Daemon log at `~/.animus/<repo-scope>/daemon.log` — supervisor emits `StartFailed`, `Started`, `Restart`, `Crashed`, and `Event` lifecycle JSON lines |
| Events fire but no workflow runs | Workflow YAML `triggers[].id` does not match the plugin's `trigger_id`; check `animus workflow validate` |
| Plugin keeps restarting | Check the daemon log for the `Crashed` event and the stderr forwarded from the plugin |
| Want to isolate the plugin | `ANIMUS_DAEMON_DISABLE_TRIGGERS=1 animus daemon start` — supervisor is skipped, so any remaining trigger fires came from the built-in `file_watcher` / `webhook` path |
| Want to drive the plugin manually | Pipe JSON-RPC on stdin (see the fswatch README for a worked example) |

Plugin stderr is forwarded into the daemon log unchanged. Set
`RUST_LOG=<plugin_crate>=debug` in the daemon's environment to surface
the plugin's tracing output.

---

## 8. Publishing

Once your plugin is stable:

1. Tag a release on a public GitHub repo (`launchapp-dev/animus-trigger-foo`
   or your own org).
2. Use [`cosign sign-blob --keyless`](./plugin-author-guide.md) to sign
   the release asset.
3. Submit a PR to
   [`launchapp-dev/animus-plugin-registry`](https://github.com/launchapp-dev/animus-plugin-registry)
   adding your plugin to `plugins.json`. Once merged, users can install
   with `animus plugin install <owner>/<repo>` and discover via
   `animus plugin search --kind trigger`.

See [Plugin Author Guide §6](./plugin-author-guide.md) for the
publishing details that apply to every plugin kind.

---

## 9. See also

- [`docs/guides/plugin-author-guide.md`](./plugin-author-guide.md) — the
  kind-agnostic plugin authoring story.
- [`docs/reference/workflow-yaml.md`](../reference/workflow-yaml.md) —
  the workflow YAML reference, including the `triggers:` section.
- [`docs/reference/cli/index.md`](../reference/cli/index.md) —
  `animus plugin scaffold` and `animus plugin install` reference.
- [`examples/triggers/fswatch/`](../../examples/triggers/fswatch/) —
  worked reference implementation.
- [`crates/animus-plugin-protocol/src/lib.rs`](../../crates/animus-plugin-protocol/src/lib.rs)
  — the wire-shape source of truth.
- [`crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs`](../../crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs)
  — daemon-side supervisor.
