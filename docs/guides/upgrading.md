# Upgrading Animus

This guide covers the general procedure for upgrading Animus to a new
release. For breaking changes specific to a release, see the matching
per-version migration guide under [`docs/migration/`](../migration/).

## General upgrade procedure

The default safe upgrade looks like:

```bash
# 1. Stop the running daemon in every project where you have one
animus daemon stop

# 2. Upgrade the binary
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash

# 3. Verify the binary
animus --version

# 4. Run daemon preflight to surface any new plugin requirements
animus daemon preflight

# 5. Install or update plugins as the preflight output requests
animus plugin install-defaults --include-subjects --include-transports
# (or `animus plugin update` to bump installed plugins to their latest tags)

# 6. Start the daemon
animus daemon start --autonomous

# 7. Sanity check
animus daemon health
animus subject list --kind task
```

The two non-obvious steps are (1) stopping the daemon before upgrading the
binary, and (4) running `animus daemon preflight` before starting the new
daemon. Skipping either can produce confusing errors.

## Stop the daemon before upgrading

The on-disk daemon protocol is stable within a major version but the daemon
process holds in-memory state (control socket, plugin host pool, in-flight
workflow handles) that a new binary cannot pick up. Always stop the daemon
before swapping the binary:

```bash
# In each project where the daemon is running
animus daemon stop
```

You can list which projects have a running daemon by checking
`~/.animus/<repo-scope>/daemon.pid` files, or by running
`animus daemon status` from each project root.

## Verify with `animus daemon preflight`

From v0.4.12 onward, `animus daemon preflight` reports which plugins are
installed, which roles are required by the daemon, and the exact
`animus plugin install ...` command for any missing plugin. In the current
v0.5 plugin-only path that includes: at least one provider, a `workflow_runner`
plugin, a `queue` plugin, and subject backends for `task` and `requirement`.
It exits non-zero if any required role is unsatisfied, which is handy for scripting:

```bash
if ! animus daemon preflight --json | jq -e '.summary.status == "ok"' >/dev/null; then
  animus plugin install-defaults --include-subjects --include-transports
fi
animus daemon start --autonomous
```

JSON envelope is `animus.daemon.preflight.v1`.

## Plugin updates

Plugins are versioned separately from the `animus` binary. To bump all
installed plugins to their latest tags:

```bash
animus plugin update
```

To pin a specific plugin to a specific tag:

```bash
animus plugin install launchapp-dev/animus-provider-claude@v0.2.2
```

To see which plugins have updates available:

```bash
animus plugin update --dry-run
```

## Per-version migration guides

When a release contains breaking changes, the matching migration guide
walks through every change with concrete before/after examples:

- [v0.4.11 → v0.4.12](../migration/v0.4.11-to-v0.4.12.md) — web stack
  deleted, in-tree subject backends deleted, provider plugins extracted,
  daemon preflight added, idempotency annotations on phases, env var
  deprecations.
- [v0.3 → v0.4](../migration/v0.3-to-v0.4.md) — full rename from `ao` to
  `animus`, plugin extraction across 8 standalone repos, env var rename,
  MCP tool rename, pack id rename.

If you are jumping more than one release (e.g. v0.3.x → v0.4.12), read the
migration guides in order. Each one assumes the previous one ran cleanly.

## Rollback

Each per-version migration guide includes a rollback procedure. The
general shape is:

```bash
animus daemon stop
ANIMUS_VERSION=v0.4.<previous> curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus daemon start --autonomous
```

Installed plugins under `~/.animus/plugins/` are not removed by an `animus`
binary downgrade. The previous daemon version will keep using whichever
plugins it finds, and may also fall back to its in-tree backends if those
were available in that version.

If the rollback target is a major version older (e.g. v0.4.x → v0.3.x), use
the matching migration guide in reverse — some state directory renames are
not auto-reversed.

## Where to look when an upgrade goes sideways

- `animus doctor` — environment + prerequisite check
- `animus daemon preflight` — plugin presence check (v0.4.12+)
- `animus daemon health` — runtime health snapshot once the daemon is up
- `animus daemon stream --pretty` — live structured daemon/workflow events
- `animus logs tail --limit 100` — recent persisted log entries
- [`docs/guides/troubleshooting.md`](./troubleshooting.md) — common issues + fixes
