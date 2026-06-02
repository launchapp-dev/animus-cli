# Plugin System

Animus plugins are standalone executables that communicate with the host over
JSON-RPC 2.0 on stdin/stdout. The canonical framing is one JSON value per line
(NDJSON), and current host-side readers also accept pretty-printed multi-line
frames from plugins. The host never loads third-party code into the daemon
process as a dynamic library.

## Source Files

| Area | Source |
|---|---|
| Wire protocol types | [`crates/animus-plugin-protocol/src/lib.rs`](../../crates/animus-plugin-protocol/src/lib.rs) |
| External provider/session protocol crates | `animus-provider-protocol`, `animus-session-backend` from `launchapp-dev/animus-protocol` declared in the workspace `Cargo.toml` |
| Plugin discovery | [`crates/orchestrator-plugin-host/src/discovery.rs`](../../crates/orchestrator-plugin-host/src/discovery.rs) |
| Plugin host/router | [`crates/orchestrator-plugin-host/src/host.rs`](../../crates/orchestrator-plugin-host/src/host.rs) |
| Subject router | [`crates/orchestrator-plugin-host/src/subject_router.rs`](../../crates/orchestrator-plugin-host/src/subject_router.rs) |
| Lockfile and signature checks | [`crates/orchestrator-plugin-host/src/lockfile.rs`](../../crates/orchestrator-plugin-host/src/lockfile.rs), [`crates/orchestrator-plugin-host/src/signature_verifier.rs`](../../crates/orchestrator-plugin-host/src/signature_verifier.rs) |
| Provider session bridge | [`crates/orchestrator-session-host/src/`](../../crates/orchestrator-session-host/src/) |
| Curated default plugins | [`crates/orchestrator-core/src/plugin_registry.rs`](../../crates/orchestrator-core/src/plugin_registry.rs) |
| Web plugin resolution | [`crates/orchestrator-cli/src/services/operations/ops_web.rs`](../../crates/orchestrator-cli/src/services/operations/ops_web.rs) |

## Design Rules

- Compatibility is defined by the wire protocol, not Rust crate linkage.
- Discovery uses `--manifest`; runtime use starts with `initialize`.
- Plugin processes start with `env_clear()`.
- The host forwards only a small base env allowlist, manifest-declared env vars,
  and request-local extras.
- Daemon-managed subject, provider, trigger, log-storage, and health-probe
  spawns pin plugin cwd to `project_root` so cwd-relative state resolves
  predictably under `.animus/`.
- Manifest probe failures become discovery warnings so operators can see why a
  binary was skipped.
- Install state records enough metadata to explain where a plugin came from and
  which binary hash was approved.

## Plugin Kinds

| Kind | Role | Required method family |
|---|---|---|
| `provider` | Drives an AI provider or CLI | `agent/run`, `agent/resume`, `agent/cancel`, `health/check` |
| `subject_backend` | Lists, reads, updates, and optionally watches work subjects | Host routes kind-scoped calls such as `<kind>/list`, `<kind>/get`, `<kind>/update`; control surfaces expose `subject/*` |
| `trigger_backend` | Watches external event sources and emits dispatchable trigger events | `trigger/watch`, `trigger/event`, `trigger/ack` |
| `transport_backend` | Hosts an inbound control transport such as HTTP or GraphQL | `transport/start`, `transport/info` or initialize metadata, `transport/shutdown`, `health/check` |
| `web_ui` | Legacy/compat UI wrapper kind | Treated as a browser-facing transport by `animus web` |
| `log_storage_backend` | Receives and serves structured log entries | `log/entry`, `log_storage/tail` |
| `custom` | Plugin-owned behavior, usually bridged to MCP | Any plugin-defined method plus lifecycle methods |
| `task_backend` | Legacy alias | Reserved; new plugins should use `subject_backend` |

`transport_backend` plugins that serve a browser UI advertise the `$ui/web`
capability. `animus web open` uses that marker to prefer a UI URL over a raw API
URL.

## Wire Protocol

Runtime communication uses JSON-RPC 2.0 over stdio. Canonical host writes stay
newline-delimited; host readers also accept pretty-printed multi-line frames
from plugins:

1. Host spawns plugin.
2. Host sends `initialize`.
3. Plugin returns `InitializeResult` with protocol version, plugin info, and
   capabilities.
4. Host sends `initialized`.
5. Host sends plugin-specific requests.
6. Plugin sends responses and may emit notifications.

`PluginHost` owns a single reader task for plugin stdout. Responses are matched
to pending JSON-RPC ids and notifications are broadcast to subscribers. This is
documented in [Plugin Host Concurrency](plugin-host-concurrency.md).

When the host binds a plugin to a project-scoped runtime, the `initialize`
params include `init_extensions.project_binding` and may also include
`init_extensions.memory_mcp_stdio_command.command`. The latter is an optional
host hint for plugins that need to launch the memory MCP sidecar using the same
`animus` binary path as the parent process. Plugins that do not use it should
ignore it.

## Discovery Order

`discover_plugins(project_root)` uses this order:

1. Registry entries from `~/.animus/plugins.yaml`.
   - Legacy fallback: `~/.config/animus/plugins.yaml` is read only when the new
     registry is absent and `ANIMUS_CONFIG_DIR` is not set.
2. Project-local plugin binaries in `<project>/.animus/plugins/`.
3. The global install dir: `$ANIMUS_PLUGIN_DIR` when explicitly set,
   otherwise `~/.animus/plugins/`.
4. `$ANIMUS_PLUGIN_PATH`.
5. `$PATH`, only when the caller opts into `--include-system-path`.

Directory scans consider executable names beginning with `animus-plugin-` or
`animus-provider-`. Registry entries may point to binaries with any name.
Plugin names are deduplicated by first match in the precedence chain, and a
broken higher-precedence entry still reserves its name so a lower-precedence
copy cannot silently shadow it.

Manifest probes are hardened:

- stdin is closed
- stdout and stderr are capped at 1 MiB
- probe timeout is 5 seconds
- plugin env is scrubbed to the base allowlist
- failures become `DiscoveryWarning` rows

## Installation State

`animus plugin install` writes three forms of state:

| State | Path | Purpose |
|---|---|---|
| Binary | `~/.animus/plugins/` or `--plugin-dir` | Executable plugin artifact |
| Registry | `~/.animus/plugins.yaml` | Logical name, binary path, install metadata, signature status |
| Lockfile | `<project>/.animus/plugins.lock` or `~/.animus/plugins.lock` | Approved version, artifact sha256, optional signature-bundle sha256 |

The lockfile prevents silent binary replacement during later upgrades.
`animus plugin lock verify` re-hashes installed binaries and reports mismatches.

Supported install sources:

- public GitHub release: `animus plugin install owner/repo[@tag]`
- local binary: `animus plugin install --path ./target/release/my-plugin`
- direct HTTPS URL: `animus plugin install --url <url> --sha256 <hex>`

Direct URL installs require an expected SHA256. Release installs resolve the
platform-specific asset, verify SHA256 and signature policy, probe `--manifest`,
then copy the binary into the install directory.

## Default Plugin Sets

The curated launchapp-dev defaults are defined in
`orchestrator-core::plugin_registry` so daemon preflight and
`animus plugin install-defaults` cannot drift.

| Flag | Installs |
|---|---|
| no extra flag | provider plugins: Claude, Codex, Gemini, OpenCode, OAI |
| `--include-oai-agent` | optional OAI agent provider |
| `--include-subjects` | default task, requirements, Linear, SQLite, and markdown subject plugins |
| `--include-transports` | HTTP transport, GraphQL transport, and web UI |

Daemon default preflight requires:

- at least one provider plugin
- a subject backend covering `task`
- a subject backend covering `requirement`

`--auto-install` installs the curated defaults for unsatisfied roles.

## Security Boundary

Plugin processes start with `env_clear()`. The host forwards:

- base variables: `PATH`, `HOME`, `USER`, `SHELL`, `TERM`, `TMPDIR`, `LANG`,
  `LC_ALL`, `RUST_LOG`, `RUST_BACKTRACE`, `TZ`
- variables declared in `PluginManifest.env_required`
- request-local extras explicitly supplied by the provider session request

If a required declared variable is missing, the host logs a warning but still
spawns the plugin. The plugin decides whether startup should fail.

Release-source installs support cosign keyless verification. Policy modes:

| Mode | Behavior |
|---|---|
| `strict` | Missing, invalid, or untrusted signatures fail the install |
| `warn` | Verification failures are recorded and warned, but install continues |
| `disabled` | Signature verification is skipped |

Additional install guards:

- manifest name should match the repository basename unless `--force` is used
- unknown GitHub owners trigger trust-on-first-use confirmation
- first-party provider tool names are reserved; installing a plugin that claims
  them requires `--allow-shadow-builtin`

## Runtime Hosting

`PluginHost` owns one live plugin process:

- a single reader task consumes stdout
- requests are written with JSON-RPC ids
- pending responses are resolved by id
- notifications go to a broadcast channel
- shutdown resolves outstanding requests

The broadcast channel capacity defaults to 256 and can be overridden with:

```bash
ANIMUS_PLUGIN_BROADCAST_CAPACITY=512
```

When `PluginSpawnOptions.working_dir` is set, the host calls
`Command::current_dir(...)` before spawn. Daemon-owned runtime paths set this to
the resolved `project_root`; ad-hoc plugin commands such as `animus plugin
ping` and `animus plugin call` still inherit the caller's cwd unless the command
itself exposes a project-root override.

## Provider Path

Provider plugins are driven by `orchestrator-session-host`.

1. Resolve the requested provider tool.
2. Discover `provider` plugins.
3. Spawn and initialize the chosen plugin.
4. Send `agent/run` or `agent/resume`.
5. Forward provider notifications as runner events.
6. Keep the active session host so `agent/cancel` reaches the same process.

Provider dispatch binds the plugin cwd to the resolved `project_root`, so
provider-owned state and any child CLI cwd-relative lookups stay anchored to the
repository even when the daemon was started from some other shell directory.

There is no in-tree provider fallback. Missing providers return a hard error
with the install command.

The resolver canonicalizes `oai-runner` and `animus-oai-runner` to `oai`.
Reserved provider names are `claude`, `codex`, `gemini`, `opencode`, `oai`, and
`oai-runner`.

## Subject Path

Subject backends expose normalized work items through the
`animus-subject-protocol` schema. The operator and daemon control surfaces use
generic verbs such as `subject/list` and `subject/get`; the control dispatcher
adapts those calls to the kind-scoped plugin methods used by the router.

The host-side router maps subject kinds to initialized plugin hosts:

- exact kind registrations win over globs
- glob registrations use `kind.*`
- longest glob prefix wins
- duplicate exact kinds and duplicate glob prefixes fail router setup

The current CLI and daemon path relies on these kind-scoped plugin calls:

| Plugin method | Purpose |
|---|---|
| `<kind>/list` | Return filtered subjects for dispatch or CLI listing |
| `<kind>/get` | Fetch one subject |
| `<kind>/create` | Create one subject when the backend supports mutation |
| `<kind>/update` | Apply a merge-style patch |
| `<kind>/next` | Return the next runnable subject for a kind |
| `<kind>/status` | Change a subject's status |

Protocol-level subject backends may also expose schema and watch capabilities,
but current routing decisions are made from the initialized plugin manifest and
`capabilities.subject_kinds`.

Subject backend spawns also pin cwd to `project_root`. Backends that persist
state via relative paths such as `.animus/subjects/tasks.db` therefore resolve
those paths under the repository root instead of under the daemon launch shell.

## Trigger Path

Trigger plugins are long-lived watchers. The daemon sends `trigger/watch` with
optional cursor and config, then receives `trigger/event` notifications. Each
event carries an event id, optional trigger id, optional subject id/kind,
optional action hint, and plugin-owned payload. The host sends `trigger/ack`
after accepting an event.

The trigger supervisor pins cwd to `project_root` for the same reason: any
plugin-relative checkpoints or repo-local config reads stay deterministic.

Trigger supervision has a daemon kill switch:

```bash
ANIMUS_DAEMON_DISABLE_TRIGGERS=1 animus daemon start
```

## Transport and Web Path

`animus web serve` and `animus web open` use normal plugin discovery, then
partition discovered plugins into:

- `transport_backend`
- `web_ui`

The web command starts transport plugins and opens the UI URL advertised by a
plugin with `$ui/web` capability. There is no in-tree web server. Spawn uses
the plugin manifest's `env_required` contract, so missing required vars fail
fast before handshake the same way `animus plugin info`, `ping`, and `call` do.

## Operations

Useful operator commands:

```bash
animus plugin list
animus plugin install-defaults --include-subjects --include-transports
animus plugin info --name <name>
animus plugin call <name> health/check --json '{}'
animus plugin lock verify
animus daemon preflight
```

## Tests

Use these focused checks when changing plugin behavior:

```bash
cargo test -p orchestrator-plugin-host
cargo test -p orchestrator-session-host
cargo test -p orchestrator-cli plugin
cargo animus-bin-check
```
