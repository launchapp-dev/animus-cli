# Plugin Author Guide

Audience: developers writing new plugins for Animus.

Animus is plugin-first. The daemon ships with no bundled providers or
subject backends — every LLM CLI integration, every ticket system, every
transport, and every webhook listener lives behind the stdio plugin
contract documented in this guide.

This guide walks through the lifecycle of authoring, testing, signing,
and publishing a plugin. It assumes you have read the
[Naming Contract](../architecture/naming-contract.md) and have a working
`animus` install on your dev box.

---

## 1. What is a plugin?

A plugin is a standalone executable that:

- Speaks **JSON-RPC 2.0 over `stdin`/`stdout`**. The canonical wire form is
  one JSON value per line (NDJSON); current host-side readers also accept
  pretty-printed multi-line frames from plugins.
- Declares a **plugin kind** (`provider`, `subject_backend`,
  `trigger_backend`, `transport_backend`, `log_storage_backend`, or
  `custom`). The kind tells the daemon which method family the plugin
  must implement.
- Emits a one-shot **manifest** when invoked with `--manifest`. The
  manifest is the discovery surface used by `animus plugin install`,
  `animus plugin info`, and the startup preflight check.
- Implements the **lifecycle methods** every plugin must answer
  (`initialize`, `initialized`, `$/ping`, `health/check`, `shutdown`,
  `exit`) plus the domain methods defined for its kind.

The wire shapes — request/response envelopes, error codes, handshake,
plugin kinds, capability flags — live in
[`crates/animus-plugin-protocol/src/lib.rs`](../../crates/animus-plugin-protocol/src/lib.rs).
For a current starter implementation, scaffold from the official
[`launchapp-dev/animus-plugin-template`](https://github.com/launchapp-dev/animus-plugin-template)
and compare against the maintained first-party plugin repos under
[`launchapp-dev`](https://github.com/launchapp-dev?q=animus&type=all&language=&sort=).

Plugin compatibility is defined by the wire shapes, not by Rust crate
linkage. A Python or TypeScript plugin that emits the same JSON over
stdio is just as compatible as one that links `animus-plugin-runtime`.

### Kinds at a glance

| Kind | Method family | Status |
|---|---|---|
| `provider` | `agent/run`, `agent/resume`, `agent/cancel` | Shipped; 5 references at [launchapp-dev](https://github.com/launchapp-dev) |
| `subject_backend` | kind-scoped calls such as `<kind>/list`, `<kind>/get`, `<kind>/update`; protocol helpers may expose `subject/schema` and `subject/watch` | Shipped; references include `animus-subject-default`, `animus-subject-requirements`, and [`animus-subject-linear`](https://github.com/launchapp-dev/animus-subject-linear) |
| `trigger_backend` | `trigger/watch`, emits `trigger/event` notifications, accepts `trigger/ack` | Shipped; references at `animus-trigger-webhook`, `animus-trigger-slack` |
| `transport_backend` | Hosts a control transport (HTTP, GraphQL, ...) | Shipped; references at `animus-transport-http`, `animus-transport-graphql`, `animus-web-ui` |
| `log_storage_backend` | Receives `log/entry` notifications, serves `log_storage/tail` | Shipped; in-tree `events.jsonl` is the fallback |
| `custom` | Plugin-defined; invoked via `animus.plugin.call` | Shipped; no domain contract |

---

## 2. Anatomy of a plugin

A Rust plugin built from the official template looks like this:

```
my-plugin/
├── Cargo.toml          # depends on animus-plugin-protocol + animus-plugin-runtime
├── plugin.toml         # static metadata (kind, env requirements)
├── src/
│   ├── main.rs         # 5-10 line entrypoint that calls the runtime's mainloop
│   ├── backend.rs      # your SubjectBackend / ProviderBackend / etc. impl
│   └── config.rs       # env-var driven config parsing
└── .github/workflows/  # release.yml signs and publishes the binary
```

`main.rs` is glue. The interesting code is in `backend.rs`, which
implements the trait for your plugin kind, and `config.rs`, which
parses environment variables into a typed config struct.

### Manifest declaration

A plugin invoked with `--manifest` prints a JSON
[`PluginManifest`](../../crates/animus-plugin-protocol/src/lib.rs) to
stdout and exits 0. The manifest must declare:

- `name`, `version`, `plugin_kind`, `description`, `protocol_version`
- `capabilities`: the closed list of methods you implement
- `env_required`: every env var your plugin reads (the daemon
  `env_clear()`s the plugin process and only forwards what you declare)
- `notification_buffer_size` (optional): broadcast channel size hint
  for chatty plugins

Example manifest:

```json
{
  "name": "animus-subject-my-tickets",
  "version": "0.1.0",
  "plugin_kind": "subject_backend",
  "description": "Subject backend for a custom ticket system",
  "protocol_version": "1.0.0",
  "capabilities": ["initialize", "$/ping", "task/list", "task/get", "task/update", "health/check"]
}
```

---

## 3. Walkthrough: write a `subject_backend` plugin

This is the most common plugin kind. You implement it when you want to
treat an external system (Linear, Jira, GitHub Issues, Notion, your own
SQLite store) as a subject source the daemon can dispatch workflows
against.

### Step 1 — scaffold

```bash
animus plugin new --kind subject --name my-tickets
cd animus-subject-my-tickets
```

This clones
[`launchapp-dev/animus-plugin-template`](https://github.com/launchapp-dev/animus-plugin-template),
fills in your plugin name, and leaves you with a compiling skeleton.

### Step 2 — implement the trait

```rust
use animus_subject_protocol::{
    Subject, SubjectBackend, SubjectFilter, SubjectId, SubjectList,
    SubjectPatch, SubjectSchema, BackendError, EventStream,
};

#[async_trait::async_trait]
impl SubjectBackend for MyTicketsBackend {
    async fn list(&self, filter: SubjectFilter) -> Result<SubjectList, BackendError> { ... }
    async fn get(&self, id: &SubjectId) -> Result<Subject, BackendError> { ... }
    async fn update(&self, id: &SubjectId, patch: SubjectPatch) -> Result<Subject, BackendError> { ... }
    async fn watch(&self) -> Option<EventStream> { None /* polling-only */ }
    fn schema(&self) -> SubjectSchema { ... }
    async fn health(&self) -> Result<HealthCheckResult, BackendError> { ... }
}
```

See
[`docs/architecture/subject-backend-plugins.md`](../architecture/subject-backend-plugins.md)
for the current kind-scoped routing contract and normalized `Subject` schema.

### Step 3 — wire the stdio loop

```rust
use animus_plugin_runtime::subject_backend_main;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    subject_backend_main(MyTicketsBackend::from_env()?).await
}
```

`subject_backend_main` handles the `initialize` handshake,
`health/check`, `$/ping`, `shutdown`, manifest emission, and request dispatch.
Current host routing sends kind-scoped methods such as `task/list` or
`linear.issue/get`; keep the advertised `subject_kinds` in sync with the methods
your backend handles.

For project-scoped runtime plugins, inspect `InitializeParams.init_extensions`
instead of inventing your own per-request project-root field. Current hosts send
`project_binding` and may also send `memory_mcp_stdio_command` with the absolute
path to the host `animus` binary for memory-sidecar launches. Unrecognized
extensions are not errors; ignore them to stay forward-compatible.

When the daemon spawns a subject backend, it pins the plugin cwd to the resolved
`project_root`. If your backend stores repo-local state via relative paths, keep
that contract intentional and document it; if you need some other base path,
resolve it explicitly instead of assuming the operator's launch shell cwd.

### Step 4 — advertise the kind

Subject backends list the subject kinds they produce in
`PluginCapabilities::subject_kinds`. The daemon's `SubjectRouter`
rejects two plugins claiming the same kind at startup, so each kind
maps to exactly one backend per project.

### Step 5 — handshake check

```bash
cargo build --release
./target/release/animus-subject-my-tickets --manifest
animus plugin install --path ./target/release/animus-subject-my-tickets --name animus-subject-my-tickets --force
animus plugin info --name animus-subject-my-tickets
animus plugin ping --name animus-subject-my-tickets
```

`--manifest` checks the local binary before install. `info` prints the
discovered manifest plus initialize-time capabilities. `ping` spawns the
installed plugin, completes the handshake, calls `$/ping`, and shuts it
down — a fast end-to-end liveness check.

---

## 4. Walkthrough: write a `provider_backend` plugin

Provider plugins back the `agent/run`, `agent/resume`, and `agent/cancel`
methods. Each provider owns the integration with one LLM CLI (Claude,
Codex, Gemini, OpenCode, OpenAI-compat) and translates the CLI's event
stream into the normalized `SessionEvent` shape.

### Reference plugins

- [`launchapp-dev/animus-provider-claude`](https://github.com/launchapp-dev/animus-provider-claude)
- [`launchapp-dev/animus-provider-codex`](https://github.com/launchapp-dev/animus-provider-codex)
- [`launchapp-dev/animus-provider-gemini`](https://github.com/launchapp-dev/animus-provider-gemini)
- [`launchapp-dev/animus-provider-opencode`](https://github.com/launchapp-dev/animus-provider-opencode)
- [`launchapp-dev/animus-provider-oai`](https://github.com/launchapp-dev/animus-provider-oai)

### Trait

```rust
#[async_trait]
pub trait ProviderBackend: Send + Sync {
    async fn start(
        &self,
        request: SessionRequest,
        resume_session: Option<&str>,
    ) -> CliResult<SessionRun>;

    async fn cancel(&self, session_id: &str) -> CliResult<()>;
}
```

`SessionRun` carries an mpsc receiver of `SessionEvent` values. Map
your CLI's event stream into:

- `SessionEvent::Started { backend, session_id, pid }` — once at start
- `SessionEvent::Thinking { text }` — for reasoning traces
- `SessionEvent::TextDelta { text }` — for streaming token output
- `SessionEvent::ToolCall { tool_name, arguments, server }`
- `SessionEvent::ToolResult { tool_name, output, success }`
- `SessionEvent::FinalText { text }` — once at completion
- `SessionEvent::Metadata { metadata }` — model, usage, etc.
- `SessionEvent::Finished { exit_code }` — final event

Use the maintained provider repos above as the current lifecycle references.

### Cancel routing (v0.4.x)

Today `cancel(session_id)` is called against a freshly-spawned plugin
process, not the one that owns the live session — a known correctness
gap tracked in
[`docs/architecture/plugin-host-concurrency.md`](../architecture/plugin-host-concurrency.md).
Until that lands, plugins should still implement `cancel` correctly
(the v0.5 host fix makes it work without plugin changes).

---

## 5. Walkthrough: write a `trigger_backend` plugin

Trigger plugins watch external systems (Slack, webhooks, file changes,
cron, ...) and emit `trigger/event` notifications when something happens
the daemon should react to.

### Lifecycle

1. Host sends `trigger/watch` once at startup with optional `cursor`
   and project-level `config`.
2. Plugin replies to the request, then keeps watching.
3. Plugin emits `trigger/event` notifications carrying a
   [`TriggerEvent`](../../crates/animus-plugin-protocol/src/lib.rs)
   payload — `event_id`, optional `trigger_id`, optional
   `subject_id` + `subject_kind`, optional `action_hint`, and the
   raw `payload`.
4. Host eventually sends `trigger/ack` notifications so the plugin
   can persist a cursor or trim a server-side queue.

The trigger supervisor enforces exponential-backoff restart with a
budget of 5 attempts before disabling the plugin. See
[`crates/orchestrator-daemon-runtime/src/`](../../crates/orchestrator-daemon-runtime/src)
(`trigger_supervisor`) for the supervision rules.

### Reference plugins

- [`launchapp-dev/animus-trigger-webhook`](https://github.com/launchapp-dev/animus-trigger-webhook)
- [`launchapp-dev/animus-trigger-slack`](https://github.com/launchapp-dev/animus-trigger-slack)

---

## 6. Walkthrough: write a `transport_backend` plugin

Transport plugins host a control transport — HTTP, GraphQL, WebSocket,
gRPC — that bridges third-party clients to the daemon's control RPC.
They are how `animus web serve` works in v0.4.12+: the in-tree axum
stack is gone; the daemon spawns `animus-transport-http` +
`animus-transport-graphql` + `animus-web-ui` and lets them bind ports.

### Contract

Transport plugins implement four methods:

- `transport/start { config }` — bind your listener, return the bound
  address. The plugin opens its own port; the daemon does not.
- `transport/shutdown` — graceful shutdown.
- `transport/schema` — return the wire schema for this transport
  (GraphQL SDL, OpenAPI document, etc.). Used by SDK generators.
- `health/check` — same as every other plugin.

### Reference plugins

- [`launchapp-dev/animus-transport-http`](https://github.com/launchapp-dev/animus-transport-http)
- [`launchapp-dev/animus-transport-graphql`](https://github.com/launchapp-dev/animus-transport-graphql)
- [`launchapp-dev/animus-web-ui`](https://github.com/launchapp-dev/animus-web-ui)

Transports speak to the daemon over the Unix-socket control RPC at
`~/.animus/<scope>/control.sock`. They are inbound from the user's
perspective but outbound from the daemon's — they pull work from the
control surface and surface it on their own port.

---

## 7. Testing with `animus-plugin-testkit`

The conformance harness lives at
[`launchapp-dev/animus-plugin-testkit`](https://github.com/launchapp-dev/animus-plugin-testkit)
(v0.1.0 ships with 8 baseline scenarios validated against
`animus-provider-claude`).

### Install

```bash
cargo install --git https://github.com/launchapp-dev/animus-plugin-testkit
```

### Run conformance

```bash
animus-plugin-harness conformance \
  --kind subject_backend \
  --plugin ./target/release/animus-subject-my-tickets
```

The harness spawns your plugin, runs the lifecycle handshake, exercises
every method in the kind's contract, validates response shapes against
the protocol, and exits non-zero on any failure.

### Add custom scenarios

Scenarios are TOML files that describe a sequence of RPC calls plus
expected response shapes. The testkit's `scenarios/` directory is the
reference. Drop new scenario files into your plugin's `tests/conformance/`
and run:

```bash
animus-plugin-harness conformance --scenarios tests/conformance \
  --plugin ./target/release/animus-subject-my-tickets
```

Use this for regressions specific to your backend, such as a Linear plugin
scenario that ensures `linear.issue/list` paginates correctly when the upstream
returns more than 50 issues.

---

## 8. Signing and publishing

Plugin distribution model (v0.4.x): "any public GitHub repo with a
release". Each plugin lives in its own repo, named
`animus-{kind}-{name}` (e.g. `animus-subject-linear`,
`animus-provider-claude`). Releases publish architecture-matched
binary tarballs as GitHub release assets.

### Release pipeline

The template's `.github/workflows/release.yml` does this for you:

1. Build the binary for `x86_64-unknown-linux-gnu`,
   `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`,
   `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`.
2. Tarball each binary plus the `plugin.toml` manifest.
3. Sign each tarball with cosign keyless (GitHub Actions OIDC →
   Sigstore Fulcio). No secrets to manage.
4. Upload the tarballs and `<asset>.tar.gz.bundle` cosign bundles to
   the GitHub release.

### Install end

```bash
animus plugin install launchapp-dev/animus-subject-my-tickets@v0.1.0
```

The install resolves the latest (or pinned) release, downloads the
arch-matched asset, verifies the cosign bundle (when present) against
the policy in
[`docs/architecture/plugin-signing.md`](../architecture/plugin-signing.md),
and drops the binary into `~/.animus/plugins/`.

### Signature policy

| Mode | Behavior |
|---|---|
| `strict` | Refuse install if signature missing, invalid, or untrusted |
| `warn` (default) | Verify when present, log on failure, install anyway |
| `disabled` | Skip verification entirely |

The default is `warn` so installs can keep recording signature status while
older releases and third-party plugins move onto keyless bundles. Verification
is cosign **keyless** — the trust anchor is Sigstore Fulcio + Rekor (built
into the `cosign` binary) plus the per-publisher identity regex + OIDC
issuer held in `TrustedPublisher`. No PEM file is required.

Override per-install with `--signature-policy <mode>`. The legacy
`--trust-key <PATH>` flag is deprecated and a no-op as of v0.4.12.

---

## 9. Common gotchas

### Env vars vs config blob

Plugins read their secrets directly from the daemon's environment.
The daemon `env_clear()`s your plugin and only forwards a minimal
shell allowlist (`PATH`, `HOME`, `USER`, `SHELL`, `TERM`, `TMPDIR`,
`LANG`, `LC_ALL`, `RUST_LOG`, `RUST_BACKTRACE`, `TZ`) plus everything
you declare in `env_required`. If you forget to declare an env var, it
will be missing at runtime even though the operator set it in their
shell.

Non-secret config (URLs, team IDs, feature flags) belongs in the
workflow YAML's `subjects:` block. Workflow YAML supports `${VAR}`
interpolation for these.

### Plugin process lifecycle

Today providers are spawned per `agent/run` call. Subjects and
triggers are long-lived. The v0.5 migration unifies on long-lived
processes — handshake runs once at spawn, `env_required` is locked
at that point, and supervisor restart picks up env changes only on
restart. See
[`docs/architecture/plugin-host-concurrency.md`](../architecture/plugin-host-concurrency.md)
for the full lifecycle model.

### Supervisor restart budgets

The trigger supervisor (and the v0.5 provider supervisor) gives each
plugin 5 restart attempts under exponential backoff before disabling
it. Operators see `plugin_disabled_total{plugin=<name>}` increment in
`animus daemon metrics`. Plan for cold-start failures to be rare and
fast — a plugin that takes 20s to initialize and crashes will exhaust
its budget in a few minutes.

### Idempotency

`subject/update` and `agent/cancel` may be retried by the daemon after
a transport hiccup. Make them safe to call twice. The daemon stamps
`event_id` on `trigger/event` so your `trigger/ack` cursor logic can
dedupe.

### Notification buffer size

The host's broadcast channel for your plugin's notifications defaults
to 256. Streaming providers that emit hundreds of `agent/output`
frames before a slow subscriber catches up should set
`notification_buffer_size` in the manifest. The capacity is fixed for
the plugin's lifetime; to change it, restart the plugin process.

---

## Related docs

- [Subject Backend Plugins](../architecture/subject-backend-plugins.md) — full subject contract
- [Plugin Host Concurrency](../architecture/plugin-host-concurrency.md) — lifecycle + cancel contract
- [Plugin Signing](../architecture/plugin-signing.md) — cosign policy detail
- [Naming Contract](../architecture/naming-contract.md) — `animus.*` everywhere
- [Operator Runbook](operator-runbook.md) — what your plugin looks like to the operator
- [Architecture Diagram](../architecture/diagram.md) — where plugins fit in the runtime
