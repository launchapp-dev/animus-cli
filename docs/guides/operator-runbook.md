# Operator Runbook

Audience: operators running Animus in production.

This runbook covers healthchecks, common failure scenarios with
remediations, debug procedures, backup/restore, upgrades, and disaster
recovery. Every command in this guide is copy-pasteable against a
current v0.4.x install.

For the upgrade procedure itself, see
[`docs/migration/v0.4.11-to-v0.4.12.md`](../migration/v0.4.11-to-v0.4.12.md)
and the cross-version
[`docs/guides/upgrading.md`](upgrading.md).

---

## 1. Healthcheck procedures

### `animus daemon health`

Fast point-in-time check. Spawns every discovered plugin one-shot,
runs the handshake, calls `health/check`, and exits with non-zero on
any `Unhealthy` row. Wall time is roughly one probe regardless of
plugin count (probes fan out concurrently).

```bash
animus daemon health
animus daemon health --json    # machine-readable
```

The daemon's own status stays `Healthy` even when individual plugins
fail — plugin trouble is an observability concern, not a daemon
liveness one.

### `animus daemon metrics`

Counters, gauges, and histograms emitted by the daemon and the
plugin host.

```bash
animus daemon metrics --pretty
animus daemon metrics --watch --interval-secs 5
```

Useful counters to know:

| Metric | Meaning |
|---|---|
| `plugin_disabled_total{plugin=<name>}` | Plugin exhausted its supervisor restart budget |
| `subscription_events_total{kind=<kind>}` | Events fanned out to control-socket subscribers |
| `workflow_runs_total{status=<status>}` | Workflow completion counts by status |
| `agent_runs_total{provider=<name>}` | `agent/run` calls per provider |

### `animus daemon preflight`

Required-plugins matrix. Exits non-zero if any required role is
unsatisfied. The error message includes the exact `animus plugin
install ...` command to fix each gap.

```bash
animus daemon preflight
animus daemon preflight --json                # schema: animus.daemon.preflight.v1
animus daemon preflight --auto-install        # fix gaps in place
```

Expected output when satisfied:

```
Daemon preflight OK
  provider:      5 installed (claude, codex, gemini, opencode, oai)
  workflow:      1 installed (animus-workflow-runner-default)
  queue:         1 installed (animus-queue-default)
  subject:       5 installed (default, requirements, linear, sqlite, markdown)
  transport+ui:  3 installed (transport-http, transport-graphql, web-ui)
```

The same preflight runs at `animus daemon start`. Skip it for dev
iteration with `--skip-preflight`; not recommended in production.

---

## 2. Common failure scenarios

### Daemon won't start

```
error: daemon preflight failed
  provider: 0 installed (need at least one)
  fix: animus plugin install-defaults
```

The daemon stops shipping with bundled providers, workflow runners,
queues, subjects, or transports as of v0.4.12/v0.5. The startup
preflight refuses to boot without at least one provider, a
`workflow_runner` plugin, a `queue` plugin, and the required subject
backends installed.

Fix:

```bash
animus plugin install-defaults --include-subjects --include-transports
animus daemon start --autonomous
```

Or in one shot:

```bash
animus daemon start --autonomous --auto-install
```

### Plugin disabled by supervisor

The trigger and (in v0.5) provider supervisors give each plugin 5
restart attempts under exponential backoff. After the budget is
exhausted, the plugin is disabled until the next daemon restart.

Diagnose:

```bash
animus daemon metrics --pretty | grep plugin_disabled_total
animus daemon health
animus plugin info --name <plugin-name>   # now also fails early on missing required env
animus daemon logs --limit 200            # look for restart-loop messages
```

Fix path:

1. Read the plugin's `last_error` from `animus daemon health` to
   identify the root cause (missing env var, upstream rate limit,
   crash bug).
2. Fix the underlying issue (set the env, wait out the rate limit,
   pin to a known-good plugin version).
3. Restart the daemon to clear the disable flag:

```bash
animus daemon stop
animus daemon start --autonomous
```

### Workflow blocked

```bash
animus workflow get <workflow-id>
```

Look for `blocked_reason`. Common values:

- **`"Blocked by status update"`** — the workflow ended in `Failed`
  and the task projector blocked it. Inspect the failing phase's
  output, fix the underlying issue, then move the task back to
  `ready`:

```bash
animus subject status --kind task --id task:TASK-001 --status ready
```

- **`"Idempotency: phase requires explicit retry decision"`** — a
  phase without an `idempotency: idempotent` annotation crashed
  mid-execution. The daemon refuses to silently re-run side-effecting
  phases. Inspect, then if you've decided it's safe:

```bash
animus workflow resume <workflow-id> --force
```

- **`"Subject backend unavailable"`** — the subject plugin that owns
  this kind crashed or was disabled. Run `animus daemon health` and
  follow the plugin-disabled fix path above.

### Tokens not streaming to UI

The web UI receives streaming events over the transport plugins. If
events stop flowing:

1. Verify transports are installed:

```bash
animus plugin list | grep -E 'transport|web-ui'
```

If missing:

```bash
animus plugin install-defaults --include-transports
```

2. Verify subscribers are receiving:

```bash
animus daemon metrics --pretty | grep subscription_events_total
```

A flat counter means the daemon isn't producing events; an incrementing
counter that the UI isn't rendering points at the transport plugin or
the browser side.

3. **Known gap (v0.4.x → v0.5):** workflow events emitted from
   subprocess-spawned workflow runners are not yet plumbed through the
   long-lived plugin host. Single-process workflows stream fine; the
   subprocess path is tracked for v0.5. See
   [`docs/architecture/plugin-host-concurrency.md`](../architecture/plugin-host-concurrency.md).

### Plugin install fails signature

```
error: plugin signature verification failed
  plugin: animus-subject-foo
  policy: strict
  reason: signature not trusted by built-in keys
```

The current default policy is `warn`, so installs record signature status
without failing closed by default. Verification is keyless — trust is anchored on
Sigstore Fulcio + Rekor (built into the `cosign` binary) plus the
per-publisher identity regex; there is no PEM trust anchor to manage.

To temporarily skip verification for an unsigned local build:

```bash
animus plugin install <owner/repo> --signature-policy disabled
```

To verify a published artifact manually:

```bash
cosign verify-blob \
  --certificate-identity-regexp "https://github.com/launchapp-dev/<repo>/.github/workflows/release\.yml@refs/tags/v.*" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  --bundle <asset>.tar.gz.bundle \
  <asset>.tar.gz
```

See [`docs/reference/security.md`](../reference/security.md) and
[`docs/architecture/plugin-signing.md`](../architecture/plugin-signing.md)
for the full policy model. The pre-v0.4.12 `--trust-key <PATH>` flag is
deprecated and a no-op.

---

## 3. Debug procedures

### Enable JSON logs

```bash
ANIMUS_LOG_FORMAT=json animus daemon start --autonomous
```

Structured logs ship to `logs/events.jsonl` under the scoped state root.
Pipe through `jq` for filtering:

```bash
tail -F ~/.animus/<scope>/logs/events.jsonl | jq -c 'select(.level == "error")'
```

### Tail daemon logs

Use the CLI to read the active log storage backend. When the daemon is down,
the command reads the local `logs/events.jsonl` mirror directly.

```bash
animus logs tail --level info --limit 100
```

Startup and shutdown emit structured JSON lines (`daemon_startup`,
`daemon_shutdown`), and workflow events include phase and run metadata.

### Stream live events

```bash
animus daemon stream --pretty
animus daemon stream --workflow <wf-id>            # filter by workflow
animus daemon stream --cat phase --level info      # filter by category
```

Streams structured log events across daemon, workflows, and runs.
Useful for watching a single workflow in real time.

### Check plugin logs

Each plugin spawns as a child process and inherits the daemon's
stderr by default. Daemon-managed provider, subject, trigger, and
log-storage plugins also run with cwd pinned to the resolved
`project_root`, so repo-local state under `.animus/` does not depend on
which shell launched the daemon. To isolate a specific plugin:

```bash
animus plugin info --name <plugin-name>           # locate the binary
animus plugin ping --name <plugin-name>           # liveness + handshake; fails if env_required is missing
animus plugin call --name <plugin-name> \         # send raw JSON-RPC
  --method health/check --params '{}'
```

Long-running plugins (subjects, triggers, transports, and v0.5+
providers) route logs through the active `log_storage_backend` when one
is installed. Daemon events still remain mirrored in
`~/.animus/<scope>/logs/events.jsonl` for local polling and fallback
reads.

---

## 4. Backup and restore

### Files to back up

| Path | Contains |
|---|---|
| `<project>/.animus/` | Project-local config + workflow YAML overlays |
| `~/.animus/<scope>/workflow.db` | Persisted workflows, checkpoints, and subject-backed runtime records |
| `~/.animus/<scope>/logs/` | Runtime event logs and run logs |
| `~/.animus/<scope>/state/` | Compiled runtime config and supporting runtime state |
| `~/.animus/plugins/` | Installed plugin binaries + `plugins.yaml` |
| `~/.animus/trusted-signers.yaml` | Cosign trust allowlist (if you customized it) |

`<scope>` is the repo-scope hash derived from the project's git common
root. See `protocol::scoped_state_root` for the exact derivation.

### Snapshot script

```bash
#!/bin/bash
SCOPE=$(animus doctor --json | jq -r '.repo_scope')
DEST=/backups/animus-$(date +%Y%m%d-%H%M%S)

mkdir -p "$DEST"
animus daemon stop                                  # quiesce writers
tar czf "$DEST/project-animus.tar.gz" -C /path/to/project .animus
tar czf "$DEST/scoped-state.tar.gz" -C ~/.animus "$SCOPE"
tar czf "$DEST/plugins.tar.gz" -C ~/.animus plugins plugins.yaml trusted-signers.yaml 2>/dev/null
animus daemon start --autonomous
```

### Restore

```bash
animus daemon stop
tar xzf project-animus.tar.gz -C /path/to/project
tar xzf scoped-state.tar.gz -C ~/.animus
tar xzf plugins.tar.gz -C ~/.animus
animus daemon start --autonomous --auto-install     # re-installs plugins if any missing
animus daemon preflight                             # verify
animus daemon health                                # verify
```

---

## 5. Upgrade procedure

Cross-version steps live in
[`docs/guides/upgrading.md`](upgrading.md).

For v0.4.11 → v0.4.12 specifically, follow
[`docs/migration/v0.4.11-to-v0.4.12.md`](../migration/v0.4.11-to-v0.4.12.md).
The TL;DR:

```bash
animus daemon stop
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus plugin install-defaults --include-subjects --include-transports
animus daemon preflight
animus daemon start --autonomous
```

For routine plugin updates:

```bash
animus plugin update                                # all installed plugins
animus plugin update <plugin-name>                  # specific plugin
animus plugin update <plugin-name> --tag v0.2.0     # pin to a tag
animus plugin update --dry-run                      # preview
```

---

## 6. Disaster recovery

### Crashed mid-workflow

The daemon writes `<phase>.session.json` for every in-flight phase
(`{ provider, session_id, status }`). On restart the scheduler attempts
`provider.resume_agent` through the original plugin.

Behavior by `idempotency` annotation:

| Annotation | Crash-recovery behavior |
|---|---|
| `idempotent` | Auto-retried on daemon restart |
| `sideeffecting` | Blocked on crash; operator decides |
| `unknown` (default for unannotated phases) | Blocked on crash; operator decides |

Decision tree after a crash:

```bash
animus workflow list --status blocked
animus workflow get <id>                            # read blocked_reason
animus subject get --kind task --id <task-id>       # context

# If you've decided it's safe to resume:
animus workflow resume <id> --force

# If it should be re-run from scratch:
animus workflow cancel <id>
animus subject status --kind task --id <task-id> --status ready

# If the provider plugin doesn't support resume:
# Install the matching plugin or change the agent's provider in workflow YAML
animus plugin install launchapp-dev/animus-provider-claude
```

### Daemon corrupt state

If `~/.animus/<scope>/state/` is corrupt (rare):

```bash
animus daemon stop
mv ~/.animus/<scope>/state ~/.animus/<scope>/state.broken-$(date +%s)
animus daemon start --autonomous            # daemon rebuilds compiled config
animus daemon preflight                     # verify
```

Workflow YAML overlays in `.animus/workflows*.yaml` are preserved because they
are project-local authored config. Subject and workflow runtime state now lives
under `~/.animus/<scope>/`, so restore the scoped runtime snapshot if you need
the operational state back as well.

### Provider plugin returns garbage

`ANIMUS_PROVIDER_DISABLE_PLUGIN` was removed in v0.4.12 — there is no
in-tree fallback to switch to, so the kill-switch had nothing left to
do. To quarantine a specific provider plugin, move it out of discovery
or uninstall it:

```bash
animus daemon stop
animus plugin uninstall --name <plugin-name>
animus daemon start --autonomous
```

Any phase that targets the removed provider will Block with a hard error pointing at the
install/uninstall surface, instead of silently routing to a missing
in-tree implementation.

To disable subject discovery entirely:

```bash
ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1 animus daemon start --autonomous
```

To skip the trigger supervisor:

```bash
ANIMUS_DAEMON_DISABLE_TRIGGERS=1 animus daemon start --autonomous
```

All three kill switches require a daemon restart to take effect and to
re-enable. See `docs/reference/configuration.md#plugin-kill-switches`.

---

## 7. Quick smoke test

After any non-trivial change (upgrade, plugin install, env update):

```bash
animus --version
animus plugin list
animus daemon preflight
animus daemon start --autonomous
animus daemon health
animus subject list --kind task
animus workflow list --limit 5
```

If any step fails, `animus doctor` runs a broader environment check
and prints actionable remediation.

---

## Related docs

- [Daemon Operations](daemon-operations.md) — start/stop/pause/configure
- [Troubleshooting](troubleshooting.md) — issue catalog with fixes
- [Upgrading](upgrading.md) — cross-version upgrade procedure
- [v0.4.11 → v0.4.12 migration](../migration/v0.4.11-to-v0.4.12.md) — the plugin-extraction cut
- [Plugin Author Guide](plugin-author-guide.md) — what plugins look like from the inside
- [Architecture Diagram](../architecture/diagram.md) — runtime topology
