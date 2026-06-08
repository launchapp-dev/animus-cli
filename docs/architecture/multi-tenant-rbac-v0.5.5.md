# Multi-Tenant + RBAC Design Proposal (v0.5.5+)

## Status (v0.5.8 small-core landed)

The small-core implementation called for in §"Recommended next steps"
step 2 shipped in **v0.5.8**. What landed:

| Piece | Status |
|-------|--------|
| `Principal` enum (User / Daemon / ServiceAccount) | **Landed** in `crates/orchestrator-core/src/principal.rs` |
| `policy.rbac` config (`single-user` default, `enforce` opt-in) | **Landed** as `RbacMode` + `RbacConfig` |
| `~/.animus/principals.yaml` auto-bootstrap (collision-guarded) | **Landed** in `bootstrap_principals_file_if_absent` |
| Chokepoint #1 — control-dispatch permission hook | **Landed** in `crates/orchestrator-daemon-runtime/src/control/policy.rs` + `connection.rs` |
| `getpeereid` / `SO_PEERCRED` peer-cred check (`cfg(unix)`) | **Landed** in `control/server.rs` `accept_loop` |
| `animus auth whoami` CLI + global `--as <principal>` flag | **Landed** in `cli_types/auth_types.rs` + `ops_auth.rs` |
| `AuditActor::Principal { id, kind }` shim (additive) | **Landed** in `daemon-runtime/src/audit.rs` |

Still **deferred to v0.6** per the original "Explicit non-goals":

- Per-tenant scope migration (`~/.animus/<repo-scope>/tenants/<tenant>/`).
- Per-principal secret routing through `launchapp-dev/animus-provider-*`.
- Plugin-host trust model changes — a single plugin process still
  serves every principal routed through it with the same
  manifest-declared secret bag.
- Chokepoints #2 (plugin mutation), #3 (secret read), #4 (audit write
  enforcement). The audit shim is in place but write-time enforcement
  is not.

Single-user installs are unchanged: `policy.rbac` defaults to
`single-user`, and the chokepoint hook is a no-op there. The honest
positioning from the original §"Why this is an honest stop" still
holds — we now ship the *vocabulary* of multi-tenancy (principals,
roles, audit attribution, `--as`) without claiming multi-tenant
isolation.

---

## Original proposal

- **Version:** v0.5.5 design proposal — pre-implementation review
- **Type:** Architectural proposal. No code lands in this commit.
- **Posture:** **Honest stop.** A first-pass design survey concluded the
  tractable core is genuinely small but the full vision sits in the v0.6
  banner-work bucket. This document lays out which pieces are safe to ride
  v0.5.5 and which need to wait.
- **Supersedes:** Nothing. Animus has been implicitly single-user since v0.1.
- **Builds on:** [kernel-and-flavors.md](./kernel-and-flavors.md),
  [plugin-system.md](./plugin-system.md),
  [plugin-signing.md](./plugin-signing.md),
  [`docs/reference/security.md`](../reference/security.md),
  [`crates/orchestrator-daemon-runtime/src/audit.rs`](../../crates/orchestrator-daemon-runtime/src/audit.rs),
  [`crates/orchestrator-daemon-runtime/src/control/server.rs`](../../crates/orchestrator-daemon-runtime/src/control/server.rs),
  [`crates/protocol/src/repository_scope.rs`](../../crates/protocol/src/repository_scope.rs).

## TL;DR

Animus is single-user today. The control socket, the scoped state root, the
plugin host's env-inheritance trust model, and every audit-log line implicitly
attribute every action to "the OS user who started the daemon."

A real multi-tenant + RBAC story has to answer four questions at once:

1. **Identity** — who acts on the system?
2. **Authentication** — how does the daemon believe them?
3. **Authorization** — what are they allowed to touch?
4. **Isolation** — whose state does their action read and write?

This proposal recommends:

- A small **kernel principal model** that lands in v0.5.5 (typed `Principal`
  enum, threaded through `AuditActor`, surfaced on the control protocol).
- A small **declarative role/permission scaffold** in `~/.animus/principals.yaml`
  (off by default — opt-in via `policy.rbac = "enforce"`).
- A `--as <principal>` CLI flag and a typed `principal:` field on the control
  request envelope. Default resolution = local OS user.
- Permission checks at **four narrow chokepoints**: workflow run, plugin
  install, secret read, audit-write.
- An explicit **honest stop** on per-tenant state isolation, per-principal
  secret routing, and authenticated multi-process socket access. Each of
  those needs a plugin-protocol bump and is sized for v0.6.

If review accepts this split, the small core can ship in v0.5.5 without
breaking single-user installs (default principal = `local`, default role =
`admin`, default policy = `single-user`). Everything more ambitious waits.

## Why this proposal exists

The v0.5 wave brought the kernel + flavors model. Reviewers immediately asked:
"if Animus is a daemon a team shares, what's the security story?" Today the
answer is "the OS user who started the daemon owns everything." That is fine
for an individual portfolio builder but blocks every conversation about teams,
shared CI runners, or hosted deployments.

This document is the structural survey that the v0.5.5 dispatch instructions
called for, plus a recommendation on what is and is not safe to land now.

## Survey of the current single-user model

### 1. Identity is implicit

There is no `Principal` type in the workspace. The only identity-shaped
concept is [`AuditActor`](../../crates/orchestrator-daemon-runtime/src/audit.rs)
with two variants:

```rust
pub enum AuditActor {
    User,
    Daemon,
}
```

That is a *role hint*, not an identity. Audit lines say `"actor": "user"`
without recording *which* user. There is no caller principal threaded into
core services, the control protocol, the plugin host, or any of the ~245
sites in the workspace that touch `scoped_state_root`.

### 2. Authentication is filesystem ACL

The control plane is a Unix-domain socket at
`~/.animus/<repo-scope>/control.sock`, chmod'd to `0700`. See
[`crates/orchestrator-daemon-runtime/src/control/server.rs`](../../crates/orchestrator-daemon-runtime/src/control/server.rs)
around line 228:

```rust
std::fs::set_permissions(&socket_path, Permissions::from_mode(0o700))
```

"Auth" = "can you `read(2)` the socket file?" Only the daemon-owning OS user
can. Connection accept calls `listener.accept()` and immediately spawns a
serve task — peer credentials (`SO_PEERCRED` / `getpeereid`) are not
inspected, and no per-connection identity is recorded.

There is no token-on-the-wire, no mTLS, no in-band auth handshake. Adding any
of these is a control-protocol bump (`animus-protocol` major).

### 3. Authorization does not exist

No permission check runs on any control request. `ControlSurface` is a
typed dispatch trait — every request that arrives over the socket is
serviced. Plugin install, queue mutate, workflow run, daemon shutdown, secret
read: all gated by "did the OS let you `connect(2)` the socket."

The closest thing to a policy hook is the existing **plugin signature
policy** (`warn` / `enforce` / `block`) at the install boundary. That is the
shape we want to copy for RBAC.

### 4. State is per-repository, not per-tenant

`~/.animus/<repo-scope>/` holds everything: config, runs, artifacts, audit
log, workflow.db. There are 245 call sites against `scoped_state_root` /
`scoped_root` across the workspace. Migrating to
`~/.animus/<repo-scope>/tenants/<tenant>/` would touch FileServiceHub, the
queue plugin, the workflow runner plugin, every subject backend plugin, the
audit log, the log storage plugin, the transport plugins, and the web UI
plugin. Several of those are **out-of-tree plugins** at
`launchapp-dev/animus-*`.

This is the largest single obstacle to "real" multi-tenancy and the
reason this proposal **does not migrate the scope layout**.

### 5. Plugin env is narrowly scoped per-plugin, but shared per-principal

The plugin host calls `env_clear()` on every spawned child and forwards
**only** the union of `PLUGIN_BASE_ENV_ALLOWLIST` (locale, shell, Rust
defaults — see
[`crates/orchestrator-plugin-host/src/host.rs`](../../crates/orchestrator-plugin-host/src/host.rs)
around `PLUGIN_BASE_ENV_ALLOWLIST` and `env_clear`) plus the plugin's
manifest-declared `env_required` list. A provider plugin that declares
`OPENAI_API_KEY` gets `OPENAI_API_KEY`; it does NOT see
`ANTHROPIC_API_KEY` or `LINEAR_API_TOKEN`. The existing host tests at
`plugin_backend.rs` lines ~1553-1673 enforce this.

The multi-tenant gap is therefore narrower than "every plugin sees every
daemon secret": **a single declared provider secret is shared across
every principal whose request is routed through that plugin process**.
Two principals using the same provider plugin share the daemon's
`OPENAI_API_KEY` until the host gains per-request env injection (or
per-principal plugin processes).

Per-principal secret routing therefore requires:

- A wire protocol where each request carries the principal,
- A per-request env-injection contract on top of today's spawn-time
  env_required allowlist (or a per-principal plugin process),
- A revised plugin signing/trust story (today plugins are trusted child
  processes; in a multi-tenant world they would handle other principals'
  data even though they only see one secret bag per spawn).

That cascades through every `launchapp-dev/animus-provider-*` repo. It is a
v0.6 effort.

## Threat model (what we're solving for in v0.5.5)

The v0.5.5 scope is **collaborative single-host operation**, not hosted
multi-tenancy. The threats in scope are:

| # | Threat | Today | After v0.5.5 small core |
|---|--------|-------|-------------------------|
| 1 | Audit log cannot answer "who ran this?" | Says `"actor": "user"` | Says `"actor": {"kind": "user", "id": "alice"}` |
| 2 | Operator cannot restrict plugin install to admins | No check | Permission `plugin.install` checked when `rbac="enforce"` |
| 3 | Two team members share a daemon with no record of who did what | No record | `--as alice` / `--as bob` on the CLI, principal stamped on every audit line |
| 4 | A team wants a "read-only viewer" role for CI | No way to express it | `roles.viewer: [workflow.read, subject.read, audit.read]` |
| 5 | Plugin auth (plugin → daemon) | Trusted child | **Unchanged** — explicitly deferred |
| 6 | Multi-process auth (CLI → remote daemon over TCP) | Unsupported | **Unchanged** — explicitly deferred |
| 7 | Per-principal secret isolation | Plugins get only manifest-declared env, but every principal routed through a given plugin shares that secret bag | **Unchanged** — explicitly deferred |
| 8 | Per-principal state isolation | Single scope dir | **Unchanged** — explicitly deferred |

Threats 5-8 are the v0.6 banner-work bucket. Their resolution requires
plugin-protocol changes and out-of-tree plugin cascades that v0.5.5 cannot
absorb honestly.

## Recommended model

### Principal model

```rust
// crates/protocol/src/principal.rs (new)

/// Who initiated a request. Threaded through the control protocol,
/// core services, and audit log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    /// A human team member declared in `~/.animus/principals.yaml`.
    User { id: PrincipalId, display_name: Option<String> },

    /// The daemon itself (scheduler ticks, supervised plugin restarts,
    /// background reconciliation).
    Daemon,

    /// A non-human caller (CI, scheduled trigger, MCP client) declared
    /// as a service account.
    Service { id: PrincipalId, display_name: Option<String> },

    /// Backwards-compat fallback used when no principal resolves
    /// (single-user installs, legacy callers). Equivalent to the
    /// declared `default_principal` in policy.
    Local,
}
```

`PrincipalId` is a newtype around `String` with a stable validation rule
(slug: lowercase alphanumerics + `-`; max 64 chars). Identical shape to
`PrincipalId` in audit logs across the industry (GitLab, Sentry, Vercel).

### Role / permission model — RBAC, not ABAC, not ACL

| Choice | Why |
|--------|-----|
| **RBAC** (recommended) | Animus has a small, stable set of verbs (~12) and a small set of role archetypes (admin/operator/viewer/runner). RBAC fits naturally and is what every reviewer asks for first. |
| ABAC | Overkill. No attribute axis matters except "which principal" and that's already in the role. |
| ACL | Painful to maintain; every resource gains its own grant table. Not worth the per-resource overhead at this scale. |

The permission verb set (initial cut, stable surface):

```
workflow.run        workflow.read       workflow.cancel
workflow.pause      workflow.resume     workflow.execute
subject.create      subject.update      subject.read
agent.run           agent.cancel        agent.read
plugin.install      plugin.uninstall    plugin.update      plugin.read
queue.mutate        queue.read
project.init        project.setup       project.read
secret.read
audit.read          audit.write
daemon.start        daemon.stop         daemon.restart     daemon.config
daemon.shutdown
```

Permissions are namespaced `<group>.<verb>` — same naming contract as MCP
tool names. The list is closed; new permissions arrive through CLAUDE.md
review. **Every existing mutating control RPC must declare exactly one
`Permission` constant.** The implementation dispatch (see "Permission
checks: four narrow chokepoints" below) treats an undeclared permission
as a compile error — there is no implicit "no permission required" path,
so adding a new control RPC forces the author to extend this list.

Three built-in roles ship in the v0.5.5 small core:

| Role | Permissions |
|------|-------------|
| `admin` | All of the above |
| `operator` | `workflow.*`, `subject.*`, `queue.*`, `plugin.read`, `audit.read` |
| `viewer` | `*.read` only |

Custom roles are user-defined in `principals.yaml`.

### Declarative config: `~/.animus/principals.yaml`

```yaml
# ~/.animus/principals.yaml — single source of truth for principals + roles.
# This file is HAND-EDITABLE (unlike scoped state JSON). It sits next to
# the existing ~/.animus/plugins.yaml and follows the same conventions.

policy:
  # "single-user" — default. Every request resolves to `default_principal`
  #                 with role `admin`. RBAC checks are skipped. Existing
  #                 single-user installs see no behavior change.
  # "enforce"    — every request must resolve to a declared principal.
  #                Unknown / unmapped principals are rejected with
  #                ControlError::PermissionDenied.
  rbac: single-user
  default_principal: local

principals:
  - id: alice
    display_name: "Alice Chen"
    os_users: [alice]
    roles: [admin]
  - id: bob
    display_name: "Bob Patel"
    os_users: [bob]
    roles: [operator]
  - id: ci-runner
    display_name: "GitHub Actions CI"
    kind: service
    roles: [viewer, runner-limited]

roles:
  runner-limited:
    permissions: [workflow.run, workflow.read, subject.read]
```

### Identity sources (resolution order)

1. **Explicit `--as <principal>`** (CLI flag). Resolved against the
   `principals` list; rejected if unknown when `rbac="enforce"`.
2. **Control envelope `principal:` field** (when a typed transport, like the
   GraphQL plugin, sets it explicitly).
3. **OS user** (`SO_PEERCRED` / `getpeereid` on the accepted Unix socket
   peer). Looked up against `principals[*].os_users`. **The peer-cred path
   is what makes this honest — we are not trusting `--as` blindly.**
4. **`default_principal`** (final fallback; only kicks in under
   `single-user`).

This is the smallest viable resolution chain that does not require a
control-protocol bump (`principal:` is added as an optional field; old
clients omit it, peer-cred path covers them). It is documented as a
control-protocol minor bump in `animus-protocol`.

### Permission checks: four narrow chokepoints

We do **not** add a permission check in every service method. The v0.5.5
core puts checks at four enforcement points:

1. **Control dispatch** — `ControlSurface::dispatch` resolves `Principal`
   from the connection (peer-cred + envelope field), then calls
   `authz::require(principal, permission)` based on the request type's
   declared permission (a `const Permission` on each request enum
   variant).
2. **Plugin mutation pipeline** (`ops_plugin.rs`) — `run_plugin_install`,
   `run_plugin_uninstall`, and `run_plugin_update` are the three direct
   entry points called by both the CLI and the MCP server, and they
   already share the audit sink. Each gains an explicit
   `require(principal, "plugin.install" | "plugin.uninstall" | "plugin.update")`
   at the same call site, so uninstall and update do not slip past
   control-dispatch gating when invoked directly.
3. **Secret read** — there is no central secret-read path today (provider
   plugins read env directly). The v0.5.5 core adds an **explicit
   chokepoint** in the form of a `SecretRequest` control RPC the daemon
   exposes for principal-aware read (used by CLI tools that don't want
   to inherit env directly). Provider plugins continue to read env until
   the v0.6 protocol bump. Audit-logs every `secret.read`.
4. **Audit write** — every existing `Audit::log` call gains a
   `Principal` parameter (back-compat shim emits `Principal::Local` for
   legacy callers, with a deprecation `tracing::warn!`).

### CLI surface

```
animus principal list
animus principal show <id>
animus policy show
animus --as alice workflow run ...
```

Hidden / advanced (under `animus auth ...` for ops):

```
animus auth whoami            # resolved principal for this invocation
animus auth check <permission> # dry-run check
```

No `principal add` / `principal remove` CLI verb. The file is hand-edited
— consistent with `plugins.yaml`'s ergonomics — and `animus principal
list` reads it. CLI add/remove can come later if real ops pull surfaces.

### Migration path

The single-user path stays bit-identical:

1. v0.5.5 ships with `policy.rbac: single-user` as the default.
2. If `~/.animus/principals.yaml` does **not** exist, the daemon writes
   one with `policy.rbac: single-user` and a single `local` principal
   (with `os_users: [<current OS user>]`).
3. Existing audit lines that lack a principal field continue to parse;
   new lines emit the typed shape.
4. All existing CLI invocations continue to work without `--as`.
5. Teams that want RBAC enforcement edit `principals.yaml`, set
   `policy.rbac: enforce`, declare their team members, and restart the
   daemon. The CLI hard-errors at startup if the policy is unparseable.

The migration **does not** change scope-state layout or the
`~/.animus/<repo-scope>/` convention. That deferral is what makes v0.5.5
honest.

## Explicit non-goals for v0.5.5

| Non-goal | Reason | Sized for |
|----------|--------|-----------|
| `~/.animus/<repo-scope>/tenants/<tenant>/` layout | 245 call sites + every out-of-tree plugin | v0.6 |
| Per-principal secret env injection | Plugin protocol bump + every provider plugin | v0.6 |
| TCP transport with token auth | Control protocol major bump | v0.6 |
| Plugin → daemon authenticated channel | Plugins are trusted children today; making them principals reshapes the trust model | v0.6 |
| Web UI principal switcher | Belongs in `animus-web-ui` plugin, not the kernel | Plugin v0.x |

Peer-credential resolution (`getpeereid` / `SO_PEERCRED`) is **in scope**
for v0.5.5. It is ~40 lines under `cfg(unix)`, closes the most embarrassing
`--as` spoofing hole, and is what makes the identity resolution chain
honest. See §"Identity sources (resolution order)" step 3 and the
`--as`-spoofing mitigation in §"Risks and open questions" item 1.

## Risks and open questions

1. **`--as` on the OS-user-owned socket is honor-system unless we
   add peer-cred.** If an attacker can `connect(2)` the socket they can
   pass any `--as` they like. The v0.5.5 small core mitigates this by
   *also* doing peer-cred lookup against `principals[*].os_users` and
   rejecting `--as alice` when the peer OS user is `bob` (unless `bob`
   has the `admin` role and the request type allows impersonation).
   Peer-cred resolution is **in scope** for v0.5.5 (~40 lines under
   `cfg(unix)`); see §"Explicit non-goals for v0.5.5" for the rationale.
2. **`Daemon`-actor audit lines exist today.** Tick-loop actions, plugin
   restart bookkeeping, etc. The proposal keeps `Principal::Daemon` as a
   first-class variant; any RBAC check on a `Daemon`-initiated action is
   skipped (the daemon is trusted).
3. **MCP tools.** MCP server callers do not carry a principal today.
   The v0.5.5 core requires every MCP call to carry an explicit principal
   in the MCP request envelope when `rbac="enforce"`. Default behavior
   (single-user) is unchanged. Open question: should the MCP server
   default to the OS-user of the calling process, or hard-fail when
   `rbac="enforce"` and no principal is supplied? Recommend hard-fail —
   it forces every MCP-aware tool to acknowledge multi-tenancy.
4. **Plugin host trust.** Even in the v0.5.5 small core, a single plugin
   process serves every principal whose request routes through it,
   using the manifest-declared env it was spawned with. Plugins don't
   see the full daemon env (the host calls `env_clear()` and forwards
   only the allowlist + `env_required`), but two principals sharing a
   provider plugin share that plugin's secret bag. We must document
   this loudly. Customers expecting per-tenant secret isolation today
   should not deploy v0.5.5 to a shared host.
5. **`local` principal collision.** If an existing `principals.yaml`
   declares a principal named `local`, the migration must refuse to
   overwrite. The boot path emits the auto-config file only when the
   file is absent.
6. **Backwards-compat audit format.** Existing audit-log readers
   (`animus history`, external SIEM exports) must keep parsing v0.5.4
   lines. The v0.5.5 audit format is a *superset*: it adds
   `principal.id`, `principal.kind`, but keeps the legacy `actor`
   string for one minor version. Removed in v0.6 with the protocol bump.

## Recommended next steps

1. **Land this design doc in v0.5.5.** (This commit.) Get reviewer
   sign-off on the four-chokepoint split and the explicit v0.6 deferrals
   before any code lands.
2. **If accepted: implement the small core as a follow-up dispatch in
   the v0.5.5 wave.** Estimated scope:
   - `crates/protocol/src/principal.rs` (~120 lines + tests)
   - `crates/protocol/src/policy.rs` (~150 lines: parse `principals.yaml`)
   - Audit shim (~30 lines + back-compat tests)
   - Control dispatch hook (~80 lines + per-request `Permission` const)
   - `--as` CLI flag + `auth whoami` (~60 lines)
   - Docs in `docs/reference/security.md` and `docs/reference/configuration.md`
3. **Schedule v0.6 banner work:** per-tenant scope migration + plugin
   protocol bump + per-principal secret routing. Sized at ~2-3 weeks of
   focused work plus every out-of-tree plugin cascade.
4. **Coordinate with `launchapp-dev/animus-web-ui`.** When the small
   core lands, the web UI plugin will need a way to know which principal
   the browser session is acting as. That probably means a
   transport-plugin-level cookie/header convention. Out of scope for
   this proposal; flagged for the web-UI plugin owners.

## Why this is an honest stop

The dispatch brief asks for honesty about structural difficulty. The
honest read:

- The **identity model** is a clean addition. No protocol bumps required
  if `principal:` is an optional control-envelope field.
- The **authorization model** is a clean addition. Four chokepoints, no
  service-method invasion.
- The **scope migration** is genuinely structural. 245 call sites + out-
  of-tree plugin cascades. It belongs to v0.6.
- The **plugin trust boundary** is genuinely structural. Plugins are
  trusted children. Making them per-principal-aware reshapes the host
  protocol. v0.6.

Shipping only the small core in v0.5.5 — with the explicit deferrals
above and the `single-user` default — gives teams the *vocabulary* of
multi-tenancy (principals, roles, audit attribution, `--as`) without
pretending we have multi-tenant isolation. That is the most honest
positioning Animus can take this minor.
