# animus-trigger-fswatch

Reference Animus trigger backend that fires when files matching a glob
are modified.

This is the canonical worked example for
[`docs/guides/authoring-trigger-plugins.md`](../../../docs/guides/authoring-trigger-plugins.md).
It demonstrates the end-to-end shape of a custom `trigger_backend` plugin:
manifest, `initialize` handshake, `trigger/watch` config parsing,
notification-loop emission of `trigger/event`, `trigger/ack`, and
health/check reporting.

## Build

```bash
cd examples/triggers/fswatch
cargo build --release
```

The compiled binary is `target/release/animus-trigger-fswatch`.

## Install

```bash
animus plugin install --path target/release/animus-trigger-fswatch
animus plugin info --name animus-trigger-fswatch
animus plugin ping --name animus-trigger-fswatch
```

`animus plugin install` copies the binary into `~/.animus/plugins/`,
registers it in `~/.animus/plugins.yaml`, and records its SHA256 in
`~/.animus/plugins.lock`. Source the binary from `examples/triggers/fswatch`
during development; release builds for end-users live on the public
plugin registry.

## Wire into a workflow

Add a `triggers:` entry of `type: plugin` to your `.animus/workflows.yaml`.
The plugin reads its `globs`, `debounce_ms`, and `trigger_id` out of the
opaque `config` block forwarded with the `trigger/watch` request:

```yaml
workflows:
  - id: respond-to-source-change
    phases:
      - name: review
        tool: claude

triggers:
  - id: fswatch-default
    type: plugin
    workflow_ref: respond-to-source-change
    config:
      trigger_id: fswatch-default
      globs:
        - src/**/*.rs
        - docs/**/*.md
      debounce_ms: 250
```

Restart the daemon to pick up the workflow file change:

```bash
animus daemon stop
animus daemon start
animus daemon preflight
```

Touch a watched file and confirm a workflow run was enqueued:

```bash
touch src/lib.rs
animus queue list
```

## Configuration reference

| Key | Type | Default | Purpose |
|---|---|---|---|
| `trigger_id` | string | none | Set on every emitted event so the daemon can route to the matching `triggers[].id` |
| `globs` | array<string> | none | One or more glob patterns relative to the project root. Required. |
| `debounce_ms` | integer | `250` | Bursty modify events for the same path are coalesced into one delivery within this window. |

## Event shape

Each `trigger/event` notification carries:

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

The daemon resolves `trigger_id` against your workflow YAML, runs the
configured `workflow_ref`, and acknowledges receipt by sending
`trigger/ack` with the `event_id`. fswatch removes acked ids from its
in-memory delivered set; backends with durable state would persist the
cursor here.

## Debugging

- Plugin stderr is forwarded into the daemon log at
  `~/.animus/<repo-scope>/daemon.log`.
- Set `RUST_LOG=animus_trigger_fswatch=debug` in the daemon's environment
  to surface watcher attach + event paths.
- Skip the supervisor entirely with `ANIMUS_DAEMON_DISABLE_TRIGGERS=1`
  when you want to confirm the plugin is the source of observed events.
- Drive the plugin manually (without the daemon) by piping JSON-RPC on
  stdin:

  ```bash
  printf '%s\n%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":"0.1","host_info":{"name":"manual","version":"0.0"},"capabilities":{}}}' \
    '{"jsonrpc":"2.0","id":2,"method":"trigger/watch","params":{"config":{"trigger_id":"fswatch-default","globs":["./*"]}}}' \
    | ./target/release/animus-trigger-fswatch
  ```

  Touch a file in the watched directory and observe a
  `trigger/event` notification land on stdout.

## What this example is and is not

- This is a working reference, not a production-grade integration. It
  uses an in-memory delivered set; a real backend that survives restarts
  should persist its cursor.
- The `recursive` watcher attached by `notify` may emit duplicate events
  on some filesystems. The 250 ms debounce smooths most of this; tune
  `debounce_ms` for your workload.
- Pattern matching uses `glob::Pattern` against `path.to_string_lossy()`,
  which is platform-relative. Paths the daemon spawns the plugin against
  should be relative to the project root for portable matching.

## See also

- [`docs/guides/authoring-trigger-plugins.md`](../../../docs/guides/authoring-trigger-plugins.md)
  — full walkthrough of the trigger plugin authoring story.
- [`crates/animus-plugin-protocol/src/lib.rs`](../../../crates/animus-plugin-protocol/src/lib.rs)
  — wire-shape source of truth.
- [`crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs`](../../../crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs)
  — daemon-side supervisor that discovers, spawns, and restarts trigger
  plugins.
