# Naming: One Brand, One Contract

## TL;DR

Everything is `animus`. There is no legacy `ao.*` prefix. The CLI binary, the
plugin protocol, MCP tool names, environment variables, configuration
directories, pack ids, JSON envelopes, crate prefixes, and standalone plugin
repositories all use the `animus` name.

This is a v0.4.0 hard cut. v0.3.x used `ao.*` for several internal surfaces
during a partial rebrand; v0.4.0 finishes the rename in one go. There are no
deprecation aliases. Existing v0.3.x users migrate their working state once,
manually, using the migration steps in
[`docs/migration/v0.3-to-v0.4.md`](../migration/v0.3-to-v0.4.md).

## The catalog

Every public surface uses the `animus` name:

| Surface | Convention | Examples |
|---|---|---|
| CLI binary | `animus` | `animus subject next --kind task`, `animus workflow run`, `animus daemon start` |
| Daemon binary | `animus` (subcommand today; `animusd` reserved) | `animus daemon start --autonomous` |
| MCP tool namespace | `animus.*` | `animus.subject.create`, `animus.workflow.run`, `animus.plugin.call` |
| Environment variables | `ANIMUS_*` | `ANIMUS_CONFIG_DIR`, `ANIMUS_PLUGIN_PATH`, `ANIMUS_RUNNER_SCOPE` |
| Project-local config dir | `.animus/` | `.animus/config.json`, `.animus/workflows.yaml`, `.animus/plugins/` |
| Scoped runtime state | `~/.animus/<repo-scope>/` | runs, artifacts, compiled config, state machines |
| Pack ids | `animus.*` | `animus.task`, `animus.review`, `animus.requirement` |
| JSON output schema envelope | `animus.cli.v1` | every `--json` response carries `"schema": "animus.cli.v1"` |
| Plugin protocol host name | `animus` | host announces itself as `animus` on `initialize` |
| Rust workspace crates (in-tree) | `orchestrator-*` (existing) and `animus-*` (new) | `orchestrator-cli`, `orchestrator-core`, `animus-plugin-protocol`, `animus-plugin-runtime` |
| Standalone plugin repos | `animus-{kind}-{name}` | `animus-subject-linear`, `animus-provider-claude`, `animus-trigger-slack` |
| Standalone plugin binaries | `animus-{kind}-{name}` | the binary name matches the repo name |
| Standalone plugin crates | `animus-{kind}-{name}` | published to crates.io under the same name |
| Documentation voice | `animus` | guides, reference, README, marketing |

One name across every surface. No translation table required.

## Why one name now

v0.3.x kept dual surfaces because changing the wire-level names mid-rebrand
would have broken external integrations (MCP clients, CI scripts, packs).
By v0.4.0 the user base is small enough that a hard cut costs less than
maintaining the dual surface forever, and the rename is a precondition for
treating Animus as an ecosystem rather than a tool: external plugin
authors, multi-language SDK maintainers, and alternative orchestrator
implementations should encounter one name, not two.

Hard cut beats deprecation aliases here because there is no significant
fleet to migrate. v0.4.0 is the inflection point where the brand commits
to itself.

## Migration from v0.3.x

Every v0.3.x install needs three local actions before running v0.4.0. There
is no automatic migration tool because the rename touches user filesystem
state and shell environment in ways the daemon cannot safely modify on its
own.

```bash
# 1. Per project repo
mv .ao .animus

# 2. Scoped runtime state
mv ~/.ao ~/.animus

# 3. Shell environment (zsh shown; adapt for bash, fish)
sed -i '' 's/AO_/ANIMUS_/g' ~/.zshrc
```

In addition:

- Update any `.mcp.json` files that reference `ao.task.*` (or any `ao.*`
  tool name) to use the `animus.*` namespace. The full rename table is in
  [`docs/migration/v0.3-to-v0.4.md`](../migration/v0.3-to-v0.4.md).
- Update CI pipelines that reference `AO_*` env vars or `.ao/` paths.
- Plugins built against v0.3.x crate names (`ao-provider-*`,
  `ao-plugin-protocol`) need a single-character rename in `Cargo.toml` and
  a recompile.

## For integrators

If you write code that calls Animus, use the `animus.*` names. They are
the only names. There is no fallback path.

- MCP clients bind to `animus.subject.*`, `animus.workflow.*`,
  `animus.daemon.*`, etc. See [MCP Tools](../reference/mcp-tools.md).
- Shell scripts and CI read and set `ANIMUS_*` environment variables.
  See [Configuration](../reference/configuration.md).
- Project automation treats `.animus/` and `~/.animus/<repo-scope>/` as
  the canonical state locations.
- Plugin packs declare `animus.*` pack ids and depend on `animus.*`
  subject kinds.
- JSON consumers validate against the `animus.cli.v1` envelope. See
  [JSON Envelope Contract](../reference/json-envelope.md).

If these surfaces ever rename again, it would be a v2.0+ break, and that
is not on any roadmap. Treat the `animus.*` names as stable from v0.4.0
onward.

## For contributors

When you add a new surface, the rule is: it uses the `animus` name.

- New MCP tool: name it `animus.<group>.<verb>` (for example,
  `animus.queue.reorder`).
- New environment variable: prefix it `ANIMUS_` (for example,
  `ANIMUS_RUNNER_BUILD_ID`).
- New CLI command: invoked as `animus <command>`. The subcommand text
  itself uses ordinary words.
- New first-party pack: id is `animus.<name>`.
- New standalone plugin repo: repo is `animus-<kind>-<name>`, binary
  inside is `animus-<kind>-<name>`, crate published as
  `animus-<kind>-<name>`.
- New state file: keep it under `.animus/` or
  `~/.animus/<repo-scope>/`. No parallel paths.

The intent is structural: one name per surface, the same name everywhere,
forever.

## Related

- [MCP Tools](../reference/mcp-tools.md) -- `animus.*` tool names
- [Configuration](../reference/configuration.md) -- `ANIMUS_*` environment variables
- [Data Layout](../reference/data-layout.md) -- `.animus/` and `~/.animus/<repo-scope>/` layout
- [JSON Envelope Contract](../reference/json-envelope.md) -- the `animus.cli.v1` schema
- [Plugin Pack Kernel](plugin-pack-kernel.md) -- `animus.task`, `animus.review`, `animus.requirement` pack ids
- [Migration: v0.3 to v0.4](../migration/v0.3-to-v0.4.md) -- the rename + plugin extraction migration
