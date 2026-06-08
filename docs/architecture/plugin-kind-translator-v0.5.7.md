# Plugin kind translator (v0.5.7)

> Status: Shipped in v0.5.7. Replaces the "make every plugin parametric"
> proposal that was on the table before this work. Plugins keep their
> hardcoded native `subject_kind` / `provider_tool`; the daemon translates
> at the wire boundary.

## Problem

Two plugin scenarios produced "duplicate kind" errors at daemon startup:

1. Two `subject_backend` plugins each declaring `subject_kind:task` — e.g.
   `animus-subject-default` (the curated backend) plus a second backend
   the operator wants to install side-by-side as an "archive" view of the
   same kind.
2. Two `provider` plugins each declaring `provider_tool:claude` — e.g. the
   curated `animus-provider-claude` plus a fork the operator runs in
   parallel.

The `SubjectRouter` rejected both cases at registration time
(`duplicate subject kind 'task' claimed by 'A' and 'B'`), so the second
install was effectively unusable.

## Decision

Solve the collision **daemon-side** instead of pushing parametric kinds
into every plugin. Each install gets a per-install **`installed_kind`**
(user-facing prefix the SubjectRouter dispatches against) that may differ
from the manifest-declared **`native_kind`** (the prefix the plugin
actually implements on the wire). At RPC dispatch time the daemon
rewrites outbound method strings and the top-level `kind` field on
inbound responses.

Plugins are **not modified**. The translator is entirely a daemon-side
concern.

## On-disk schema

`.animus/plugins.lock` entries gain two optional fields:

```toml
[[plugins]]
name = "animus-plugin-task-archive"
version = "v0.1.0"
artifact_sha256 = "…"
installed_at = "2026-06-07T…"
installed_kind = "task-2"   # user-facing — what `animus subject` dispatches against
native_kind    = "task"      # plugin-native — what the plugin emits on the wire
```

Both fields default to `None` for back-compat. A lockfile entry written
by a pre-v0.5.7 installer parses cleanly; consumers treat it as
`installed_kind == native_kind`.

Helpers on `LockEntry`:

- `effective_installed_kind() -> Option<&str>` — falls back to
  `native_kind` when `installed_kind` is unset.
- `effective_native_kind() -> Option<&str>` — falls back to
  `installed_kind` when `native_kind` is unset.

## Install pipeline

`animus plugin install` gains a `--as-kind <KIND>` flag and an
auto-increment policy.

When the install pipeline computes the `installed_kind` for a new
`plugins.lock` entry it follows this order:

1. **`--as-kind <KIND>`** — operator-supplied. If `KIND` collides with
   another entry's `installed_kind`, the install fails with an
   actionable error naming the colliding plugin. Suggests dropping
   `--as-kind` to let auto-increment run.
2. **Manifest-declared native kind, uncontested** — used as-is.
3. **Auto-increment** — `<native>-2 -> <native>-3 -> …`. The suffix is
   the smallest integer `>= 2` not already claimed by another entry's
   `installed_kind`. The user is notified via stderr (text mode) and
   via the `animus.plugin.install.v1` envelope (JSON mode):

   ```
   animus.plugin.install.v1: plugin '<name>' assigned installed_kind
   'task-2' (native 'task'); a previously-installed plugin already
   claimed 'task'. Pass --as-kind on a future install to override.
   ```

Re-installing / upgrading the same plugin name skips collision detection
against its own existing lockfile entry, so a `--force` upgrade keeps
its prior `installed_kind`.

**Provider plugins are intentionally NOT renamed in v0.5.7.** The
provider dispatch path derives `provider_tool` from the plugin binary
name and does not consult `plugins.lock`. Recording a provider alias
in the install pipeline would mint an `installed_kind` the runtime
cannot honor; that promise is deferred to a future release. Plugins
that declare no `subject_kind:*` capability — including providers,
transports, workflow_runners, queues, and triggers — skip the rename
pipeline entirely. Both `installed_kind` and `native_kind` stay `None`
for those entries.

Re-installing / upgrading the same plugin name without `--as-kind`
preserves the prior `installed_kind` even when the auto-increment slot
is free again. Without this, a routine `--force` upgrade of a backend
installed as `archive` would silently revert it to `task`, breaking
every caller still dispatching against `archive`.

## SubjectRouter translation

The router accepts a `KindAliasMap` at construction time via
`SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases)`.
The alias map records `(plugin_name, native_kind) -> installed_kind`
pairs and is built from the project's `plugins.lock` by
`load_kind_aliases_from_lockfile`.

### Outbound

`route_call(method, params)` extracts the kind prefix from the method.
The kind prefix is the `installed_kind`. Before forwarding to the
plugin's stdio, the router rewrites:

```
<installed_kind>/<verb>  ->  <native_kind>/<verb>
```

Subject IDs are encoded `<kind>:<local-id>` (the daemon control
dispatch extracts the kind from `id` via `extract_kind_from_subject_id`).
The router also rewrites the top-level `id` / `subject_id` fields in
`params` from `<installed_kind>:<local-id>` to `<native_kind>:<local-id>`
so the plugin's internal store can resolve them. IDs that carry a
different prefix (or are unprefixed) are forwarded verbatim.

When the plugin has no recorded alias (i.e. `installed_kind == native_kind`),
both method and params are forwarded unchanged.

### Inbound

The plugin's response JSON has its top-level `kind` field rewritten back
from `native_kind` to `installed_kind`, AND any `id` / `subject_id`
field whose `<kind>:` prefix matches the plugin's native kind is
rewritten to use the `installed_kind` prefix. Supported shapes:

- `Subject` — `{ "kind": "<native>", "id": "<native>:LOCAL-1", ... }`
  (top-level object).
- `SubjectList` — `{ "subjects": [{ "kind": "<native>", "id": ..., ... }, ...] }`.
- `SubjectEvent` — `{ "subject": { "kind": "<native>", "id": ..., ... }, ... }`.

The ID-prefix rewrite is what makes round-trip control calls work for
renamed backends: a `subject/list` returning `task:TASK-1` would
otherwise force the client to call `subject/get id="task:TASK-1"`,
which the daemon then re-routes against an unregistered `task` kind.
Translating both directions keeps `archive` as the only kind callers
ever see.

Deep-nested `kind` fields under `metadata`, `tags`, freeform
plugin-defined payloads, etc. are **explicitly out of scope for
v0.5.7**. Rewriting them would require schema knowledge that belongs
in the protocol crate, not the router. See "Deferred work" below.

When the alias map is empty (no installs were renamed at install time)
the inbound walker is short-circuited and behaves identically to the
pre-v0.5.7 router.

## Preflight + `animus plugin doctor`

`summarize_discovered_plugins_with_lock(plugins, lockfile)` consults the
lockfile when building the `InstalledPluginSummary` list. A plugin
renamed from `task` to `archive` reports `subject_kinds: ["archive"]`
instead of `["task"]`, so the daemon preflight role
`subject_kind:archive` will be satisfied (and `subject_kind:task` will
remain unsatisfied, prompting the operator to install a real task
backend).

`animus plugin doctor` is a new CLI subcommand that lists every
required role from `PluginPreflightSpec::daemon_default()` together
with its installed claimants. Each claimant is rendered with its
`installed_kind` and `native_kind` side-by-side. Multiple claimants
sharing the same `installed_kind` on the same role are flagged as
collisions with a `⚠` marker (or a `! duplicate ...` line in text
mode).

Doctor output (text mode):

```
plugin doctor (lockfile: /Users/foo/proj/.animus/plugins.lock)
  [ok] at_least_one_provider
    - animus-provider-claude installed=claude native=claude
  [ok] subject_kind:task
    - animus-subject-default installed=task native=task
  [COLLISION] subject_kind:archive
    - animus-plugin-archive-a installed=archive native=task (renamed)
    - animus-plugin-archive-b installed=archive native=task (renamed)
    ! duplicate installed_kind 'archive' claimed by multiple plugins
  ...
summary: 1 role(s) with collisions, 0 role(s) unsatisfied
```

`--json` emits the structured `PluginDoctorOutput` shape.

## Deferred work (out of scope for v0.5.7)

- **Deep-nested `kind` rewriting**: response payloads can carry `kind`
  inside `metadata.kind`, `tags[*].kind`, or plugin-defined freeform
  blobs. Rewriting those would require host-side schema knowledge
  (which payloads carry `kind` strings that refer to the dispatched
  subject vs. which are unrelated `kind` strings like
  `metadata.kind = "manual"`). The walker is intentionally narrow.
- **Glob kind renames**: a plugin declaring `subject_kind:task.*` (i.e.
  a glob) is currently passed through unrenamed. Exact renames are
  enough for the documented `task -> task-2` shape.
- **Multi-kind renames per plugin**: a plugin declaring two distinct
  native kinds (e.g. `subject_kind:task` and `subject_kind:incident`)
  is only renamed on its **first** declared kind. Subsequent kinds
  flow through unrenamed. A future iteration could store a full
  `{ native: installed }` map per plugin in `plugins.lock`; the v0.5.7
  schema records a single primary pair to keep the storage shape
  cheap.

## Compatibility

- Pre-v0.5.7 lockfiles parse without modification — entries are treated
  as identity renames (`installed_kind == native_kind`).
- Plugins built against the v1.0 protocol need no changes — the
  translator sits between the daemon and the plugin's stdio.
- The `SubjectRouter::from_initialized_hosts(hosts)` constructor is
  preserved for callers that don't need translation; it delegates to
  `from_initialized_hosts_with_aliases(hosts, KindAliasMap::default())`.
