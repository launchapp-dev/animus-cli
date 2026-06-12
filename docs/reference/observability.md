# Observability

Reference for the Animus daemon's metrics and structured logging surface. Both
surfaces are read-only — operators consume them to diagnose what the daemon
is doing without scraping internal state files.

The metrics endpoint shipped in v0.4.12 alongside the existing
`daemon health`, `daemon events`, and `daemon logs` surfaces. Use this
page to wire the daemon into your observability stack.

## Metrics

### Accessing the snapshot

The daemon exposes a single in-tree control method, `daemon/metrics`, that
returns a plain-JSON snapshot of every counter, gauge, and histogram the
process has recorded since startup.

```bash
animus daemon metrics                 # one-shot JSON snapshot to stdout
animus daemon metrics --pretty        # human-readable table
animus daemon metrics --watch         # refresh every 5s (Ctrl+C to stop)
animus daemon metrics --watch --interval-secs 1
```

`daemon metrics` requires the daemon to be running — it talks over the
control socket. There is no on-disk fallback because counters reset on
daemon restart.

As of v0.6, `animus daemon metrics` also carries the opt-in anonymous usage
telemetry controls as subcommands — `status`, `enable`, `disable`, `flush`,
and `cleanup` (formerly the top-level `animus metrics` group, removed with
no aliases). Bare `animus daemon metrics` keeps the snapshot display above.
See [configuration.md](configuration.md) for the telemetry contract.

### Snapshot shape

```json
{
  "counters": {
    "workflow_runs_total{status=completed}": 14,
    "workflow_runs_total{status=failed}": 1,
    "phase_executions_total{status=completed}": 47,
    "phase_executions_total{status=failed}": 3,
    "subscription_events_total{kind=phase_started}": 50,
    "subscription_events_total{kind=phase_completed}": 47,
    "subscription_events_total{kind=workflow_completed}": 14,
    "subscription_events_total{kind=workflow_failed}": 1,
    "plugin_restarts_total{plugin=animus-trigger-slack}": 2,
    "plugin_disabled_total{plugin=animus-trigger-slack}": 0,
    "plugin_start_failures_total{plugin=animus-trigger-github}": 0,
    "trigger_events_total{plugin=animus-trigger-slack,routed=true}": 9
  },
  "gauges": {
    "daemon_uptime_seconds": 3742,
    "plugins_installed_total": 6,
    "plugins_installed{kind=provider}": 4,
    "plugins_installed{kind=subject_backend}": 1,
    "plugins_installed{kind=trigger_backend}": 1
  },
  "histograms": {},
  "captured_at": "2026-05-24T09:14:22.184310+00:00",
  "uptime_seconds": 3742
}
```

Counter and gauge keys use a deliberately Prometheus-like
`name{label=value,label=value}` shape so the JSON can be transformed into
Prometheus exposition format with a small converter. The metrics layer
itself does NOT emit Prometheus format — keep the daemon dependency-free
and let operators pick their preferred shape.

### Metric catalog

#### Counters

| Name | Labels | Description |
| --- | --- | --- |
| `workflow_runs_total` | `status` (`completed` / `failed`) | Total workflows that reached a terminal status. |
| `phase_executions_total` | `status` (`completed` / `failed`) | Total phase executions that reached a terminal status. |
| `subscription_events_total` | `kind` (the WorkflowEvent kind string) | Every workflow event that passed through the broadcaster. |
| `plugin_restarts_total` | `plugin` | Trigger-plugin supervised restarts. Excludes the initial spawn. |
| `plugin_disabled_total` | `plugin` | Trigger-plugin "crashed past restart budget" terminal events. |
| `plugin_start_failures_total` | `plugin` | Trigger-plugin failures during initial spawn / handshake. |
| `trigger_events_total` | `plugin`, `routed` (`true` / `false`) | Trigger events observed and whether they matched a configured workflow. |

#### Gauges

| Name | Labels | Description |
| --- | --- | --- |
| `daemon_uptime_seconds` | — | Seconds since the daemon process started. Updated lazily on every snapshot. |
| `plugins_installed_total` | — | Total installed plugins discovered at startup. |
| `plugins_installed` | `kind` (`provider` / `subject_backend` / `trigger_backend` / `transport`) | Installed plugin counts broken down by plugin kind. |

#### Histograms

The metrics layer reserves room for `phase_duration_seconds{phase_name}` and
`plugin_request_duration_seconds{plugin,method}` — observing these from the
workflow runner / session-host crates is a future enhancement. Histograms
emitted via `orchestrator_daemon_runtime::metrics::observe` will show up
here automatically once the call sites are wired.

Bucket layout: 1ms, 5ms, 10ms, 50ms, 100ms, 500ms, 1s, 2.5s, 5s, 10s, +Inf.

### Plugin invocation metrics

The session-host crate (which dispatches LLM provider calls) does not yet
emit `plugin_invocations_total` directly — adding it requires a cross-crate
dependency that the v0.4.12 timeline does not justify. As a stand-in,
operators can derive an "invocations" view from
`subscription_events_total{kind=phase_started}` (one per phase invocation)
combined with `subscription_events_total{kind=phase_completed}` /
`phase_failed`.

## Structured logging

The daemon writes all events to the project-scoped
`~/.animus/<scope>/logs/events.jsonl`. Each line is a redacted, structured
`LogEntry` (see `crates/orchestrator-logging/src/lib.rs::LogEntry`).

### Secret redaction

Before a `LogEntry` is persisted, two parallel mechanisms strip secrets
from `msg`, `content`, `error`, and the recursive `meta` JSON tree:

1. **Value-content regex redaction.** String contents are scanned for
   `key=value` / `key: value` shapes (e.g. `api_key=sk_live_abc`,
   `authorization: Bearer ...`). The default pattern covers `api_key`,
   `api-key`, `password`, `token`, `secret`, `authorization`. Matches
   collapse to `<key>=***REDACTED***`. Extend the pattern set with
   `ANIMUS_LOG_REDACT_PATTERNS` (comma-separated regex strings; invalid
   patterns are silently skipped).

2. **Key-name redaction (since v0.4.13).** When recursing into a JSON
   object inside `meta`, any `(key, value)` pair whose key matches the
   secret-key set has its value replaced with `***REDACTED***` regardless
   of the value's content. This catches the
   `meta({"api_key":"sk_live_..."})` case, where the bare value
   `"sk_live_..."` would not match any content regex on its own.

   Key normalization is the matching strategy:

   - lowercase
   - collapse `-` to `_`
   - insert `_` at camelCase / PascalCase boundaries so `secretKey`
     becomes `secret_key`, `apiKey` becomes `api_key`, `X-API-Key`
     becomes `x_api_key`

   Matching is then **word-boundary aware** (delimited by `_` or
   string boundaries), so `token` matches `access_token`,
   `accessToken`, and `x_api_token`, but does NOT match `input_tokens`,
   `max_tokens`, or `tokenizer` — token-count observability metadata
   stays intact.

   Defaults (case-insensitive; snake_case, kebab-case, camelCase,
   PascalCase, and `X-*-*` headers all hit):

   ```
   api_key, apikey, token, access_token, accesstoken, refresh_token,
   refreshtoken, id_token, idtoken, secret, client_secret, clientsecret,
   secretkey, password, passwd, pwd, authorization, bearer, bearertoken,
   private_key, privatekey, signing_key, signingkey, x-api-key, xapikey
   ```

   This naturally covers `X-API-Key`, `X-Access-Token`, `apiKey`,
   `secretKey`, `privateKey`, `bearerToken`, and similar header /
   config families. Override the list with `ANIMUS_LOG_REDACT_KEYS`
   (comma-separated names; override **replaces** the defaults rather
   than appending — set it to the full list you want).

Identifier-shaped typed fields on `LogEntry` (`workflow_id`, `model`,
`provider`, etc.) are not scrubbed — they are not secret-bearing and
skipping them keeps the hot path cheap.

For the `tracing` event stream (everything that goes through
`tracing::info!`, `tracing::warn!`, etc.), the daemon defaults to
human-readable formatting on stderr. Enable JSON-line output by setting
either of these environment variables before launching the daemon:

```bash
ANIMUS_LOG_FORMAT=json animus daemon start
# or, the legacy alias (still honored):
ANIMUS_LOG_JSON=1 animus daemon start
```

With JSON formatting enabled, every tracing event is serialized as a
single-line JSON object suitable for ingestion by log aggregators. Pair
with `RUST_LOG` to control verbosity:

```bash
RUST_LOG=info,orchestrator_daemon_runtime=debug \
ANIMUS_LOG_FORMAT=json \
animus daemon start
```

### Shipping logs to an aggregator

For a typical Linux deployment:

```bash
# 1. Run the daemon as a systemd unit, JSON tracing on stderr.
#    journald captures stderr automatically.

# 2. Tail journald with vector → forward to Loki/Splunk/Datadog.
journalctl -u animus.service -o cat -f | vector --config vector.yaml

# 3. Vector pipeline (sketch):
#    sources.journald → transforms.parse_json → sinks.loki
```

For macOS / non-systemd dev environments:

```bash
ANIMUS_LOG_FORMAT=json animus daemon start 2> >(tee -a daemon.json.log >&2)
```

Then feed `daemon.json.log` to whatever ingester you prefer.

### Log entry vs. tracing event

There are two distinct streams:

- **`events.jsonl`** — emitted by `orchestrator_logging::Logger` from
  lifecycle events the daemon explicitly records (workflow transitions,
  agent runs, schedule fires). Always JSON; no opt-in required. Read via
  `animus daemon logs` or `animus daemon stream`.
- **tracing** — emitted by `tracing::info!` / `tracing::warn!` etc. across
  the codebase. Goes to stderr. JSON or human-readable per
  `ANIMUS_LOG_FORMAT`. Read via stderr capture or journald.

Operators integrating with an aggregator generally want both. The
`events.jsonl` stream is structured first-class data; the tracing stream
captures the diagnostic fan-out from running code.

## Failure notifications

Every daemon event the run loop emits is also dispatched to installed
`notifier` plugins (no-op when none are installed). Two event types exist
specifically so operators of an autonomous daemon learn when work is stuck:

- **`workflow-failed`** — emitted once per workflow-runner process that
  exits with a `failed` / `escalated` workflow status (or crashes without
  reporting one). Payload: `workflow_id`, `workflow_ref`, `subject_id`,
  `task_id`, `failure_reason`. Completed processes are reconciled exactly
  once, so a given run produces at most one event.
- **`task-blocked`** — emitted once when a task transitions into the
  `blocked` status (for example after a failed workflow blocks its task).
  Payload: `task_id`, `from_status`, `blocked_reason`, `changed_at`.
  Transitions are diffed against the pre-tick snapshot, so re-evaluating an
  already-blocked task on later ticks does not re-notify.

The generic `task-state-change` event also carries a `blocked_reason` field
when the target status is `blocked`.

Subscribe a notifier connector to these via the persisted
`notification_config` block's `subscriptions[].event_types` (for example
`["workflow-failed", "task-blocked"]`, or `["*"]` for everything). See
`docs/reference/configuration.md` for how `notification_config` is stored
and which env vars are forwarded to notifier plugins.

## Quick recipes

### "Is the daemon healthy right now?"

```bash
animus daemon health      # plugins + status, per-plugin health probe
animus daemon metrics --pretty | grep workflow_runs_total
```

### "Why did this plugin keep restarting?"

```bash
animus daemon metrics | jq '.counters | to_entries | map(select(.key | startswith("plugin_restarts_total"))) '
animus daemon logs --search plugin_restart
```

### "Watch the dispatch loop live"

```bash
animus daemon metrics --watch --pretty --interval-secs 2
```
