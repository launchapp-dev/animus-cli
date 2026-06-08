# Plugin scope (`.animus/plugin-scope.yaml`)

Status: stable as of v0.5.9.

## Motivation

Operators that experiment with many `subject_backend` / `provider` plugins
end up with 30+ binaries under `~/.animus/plugins/`, but a typical project
only uses 2-3 of them (e.g. `animus-subject-default`,
`animus-subject-requirements`, `animus-provider-claude`). Without a scope
filter every discovery, preflight, plugin-status iteration, and Tauri poll
walks the full 30+ set. This compounds across daemon ticks and CLI hot
paths.

The per-project scope file `<project_root>/.animus/plugin-scope.yaml`
lets a project opt into a subset of the globally installed plugins. The
filter applies in:

- `PluginDiscovery::discover()` and `discover_with_warnings()`
- `discover_installed_plugins()` (daemon preflight)
- `animus plugin list` (when run inside a project)
- the plugin-status registry (only scoped plugins are tracked)
- `animus daemon health` provider-healthy snapshot

Warnings for failed `--manifest` probes still surface for
installed-but-out-of-scope plugins — operators retain diagnostic info on
partial failures.

## Schema

```yaml
schema: animus.plugin-scope.v1
mode: allowlist            # one of: all | flavor-only | allowlist
allow:                     # plugin names (matches lockfile / plugins.yaml key)
  - animus-subject-default
  - animus-subject-requirements
  - animus-provider-claude
require:                   # role-style declarations (informational)
  - subject_kind:task
  - subject_kind:requirement
extras:                    # extra plugins layered on top of flavor / allow
  - animus-subject-linear
```

## Mode semantics

| Mode | Default trigger | Admit predicate |
|---|---|---|
| `all` | No scope file AND no `flavors/default.toml` present | Every discovered plugin admits (v0.5.8 behavior). |
| `flavor-only` | No scope file AND `flavors/default.toml` present | Only plugins declared by the active flavor's `required` sections (resolved via `FlavorManifest::all_plugin_slugs(false)`), plus `extras:` |
| `allowlist` | Explicit `mode: allowlist` | Only the plugin names in `allow:` plus `extras:`. |

Backwards compatibility: a project with no scope file AND no flavor
manifest gets `mode: all`, preserving v0.5.8 semantics.

## CLI surface

```text
$ animus plugin scope --help
Per-project plugin scope (.animus/plugin-scope.yaml).

Commands:
  show   Print the effective scope (mode + resolved admit-set)
  set    Write .animus/plugin-scope.yaml with the supplied flags
  reset  Delete .animus/plugin-scope.yaml
```

Common flows:

```bash
# inspect what the daemon's discovery layer will see
animus plugin scope show

# lock the project to a hand-picked set
animus plugin scope set \
  --mode allowlist \
  --allow animus-subject-default \
  --allow animus-subject-requirements \
  --allow animus-provider-claude

# layer extras on top of the active flavor's required set
animus plugin scope set --mode flavor-only --extras animus-subject-linear

# return to defaults (flavor-only when a flavor manifest exists, otherwise all)
animus plugin scope reset
```

All three subcommands accept `--json` for `animus.cli.v1` envelope
output.

## Preflight interaction

When the daemon's plugin preflight reports a required role unsatisfied
AND the plugin that *would* satisfy that role is installed but excluded
by the scope, the `fix_command` for the missing role is rewritten to:

```text
scope mode=`allowlist` excludes plugin `animus-subject-default` required
for role `subject_kind:task`. Run `animus plugin scope set --mode
allowlist --allow animus-subject-default` or `animus plugin scope reset`.
```

This distinguishes "you forgot to install the plugin" from "the plugin
is installed but you opted out of it for this project."
