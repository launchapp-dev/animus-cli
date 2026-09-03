# Deployment Guide: Docker, Railway, HTTP API, and GraphQL

This guide covers running Animus in a container, deploying it to Railway,
and using the HTTP and GraphQL transport APIs that the `animus web` surface
exposes via installed plugins.

## 1. Containerizing Animus

### Multi-stage Dockerfile

The recommended pattern downloads a prebuilt `animus` release binary (no
in-image Rust compilation) and bakes required plugins into the image at build
time so the container boots without network access.

```dockerfile
# ---- stage 1: fetch the prebuilt animus binary ----
FROM debian:bookworm-slim AS animus-fetch
ARG ANIMUS_VERSION=v0.7.0-rc.49
ARG ANIMUS_TARGET=x86_64-unknown-linux-gnu
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL -o /tmp/ao.tar.gz \
      "https://github.com/launchapp-dev/animus-cli/releases/download/${ANIMUS_VERSION}/ao-${ANIMUS_VERSION}-${ANIMUS_TARGET}.tar.gz" \
 && tar xzf /tmp/ao.tar.gz -C /tmp \
 && cp "/tmp/ao-${ANIMUS_VERSION}-${ANIMUS_TARGET}/animus" /usr/local/bin/animus \
 && chmod +x /usr/local/bin/animus

# ---- stage 2: runtime ----
# Use a glibc-new enough base. animus binaries built on Ubuntu 24.04 need
# GLIBC >= 2.39; debian:trixie ships 2.41.
FROM debian:trixie-slim
ENV DEBIAN_FRONTEND=noninteractive \
    ANIMUS_TRANSPORT_HTTP_BIND=0.0.0.0:8080

# libdbus-1-3 + libssl3: animus links them for the secret backend and TLS.
# git + curl: plugin install uses them at build time.
# nodejs + npm: required if using the claude provider CLI.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates git curl libdbus-1-3 libssl3 \
    && rm -rf /var/lib/apt/lists/*

# The claude provider plugin shells out to the `claude` CLI. Install it if you
# use animus-provider-claude. Omit this block for other providers.
RUN apt-get update && apt-get install -y --no-install-recommends nodejs npm \
    && npm install -g @anthropic-ai/claude-code && npm cache clean --force \
    && rm -rf /var/lib/apt/lists/*

# Run as non-root. The claude CLI refuses --dangerously-skip-permissions as root,
# which the claude provider uses for autonomous agent runs.
RUN useradd -m -u 10001 -s /bin/bash appuser
WORKDIR /app

COPY --from=animus-fetch /usr/local/bin/animus /usr/local/bin/animus
COPY .animus /app/.animus

RUN chown -R appuser:appuser /app
USER appuser
ENV HOME=/home/appuser

# Bake all required plugins into the image (no runtime network needed).
# --signature-policy=disabled: use this only when pulling from a registry
# whose cosign identity is not yet in animus's trusted-publisher list.
# Remove this flag once your plugin repos are listed in trusted-signers.yaml.
ARG SIG=--signature-policy=disabled

RUN animus plugin install launchapp-dev/animus-provider-claude  $SIG --allow-shadow-builtin --project-root /app \
 && animus plugin install launchapp-dev/animus-subject-default  $SIG --project-root /app \
 && animus plugin install launchapp-dev/animus-queue-default    $SIG --project-root /app \
 && animus plugin install launchapp-dev/animus-workflow-runner-default $SIG --project-root /app \
 && animus plugin install launchapp-dev/animus-transport-http   $SIG --project-root /app

# Confirm all required roles are satisfied before shipping the image.
RUN animus daemon preflight --project-root /app

COPY deploy/start.sh /app/deploy/start.sh
RUN chmod +x /app/deploy/start.sh
EXPOSE 8080
CMD ["/app/deploy/start.sh"]
```

**Key build arguments:**

| ARG | Default | Description |
|---|---|---|
| `ANIMUS_VERSION` | `v0.7.0-rc.49` | Release tag to download from `launchapp-dev/animus-cli` |
| `ANIMUS_TARGET` | `x86_64-unknown-linux-gnu` | Target triple; must match the container's glibc |
| `SIG` | `--signature-policy=disabled` | Signature policy flag passed to every `plugin install` |

### Plugin preflight and required roles

The daemon refuses to start when any of these roles is unsatisfied:

| Role | Default plugin |
|---|---|
| At least one provider | `launchapp-dev/animus-provider-claude` (or another) |
| At least one `subject_backend` (any kind, since v0.5.20) | `launchapp-dev/animus-subject-default` |
| `workflow_runner` | `launchapp-dev/animus-workflow-runner-default` |
| `queue` | `launchapp-dev/animus-queue-default` |

`animus daemon preflight` in the `RUN` layer above catches any missing role at
build time, not at runtime.

Two escape hatches are available at daemon startup (not substitutes for a
working plugin set in production):

- `--auto-install` — install any missing required-role plugins from their
  pinned defaults before the daemon starts. Causes a runtime network fetch.
- `--skip-preflight` — bypass the check entirely. The daemon will fail at the
  first plugin RPC if the plugin is really missing.

### State and persistence

Scoped runtime state lives under `~/.animus/<repo-scope>/` inside the
container (where `~` resolves to the running user's `$HOME`). This includes
runs, artifacts, the queue database, secrets, and compiled workflow config.

Project-local config lives at `.animus/` within the project directory
(`/app/.animus` in the skeleton above).

**Ephemeral (default):** state is lost on container restart. Acceptable for
stateless pipelines where subjects are owned by an external store and the
daemon is purely a workflow executor.

**Persistent (recommended for production):** mount a volume over
`/home/appuser/.animus` so state survives redeploys.

```dockerfile
VOLUME ["/home/appuser/.animus"]
```

Or in `docker run`:

```bash
docker run -v animus-state:/home/appuser/.animus my-animus-image
```

## 2. Running the Daemon in a Container

Use `animus daemon run` — not `daemon start` — in containerized environments.
`daemon run` is the **foreground** verb and writes to stdout/stderr, which
container runtimes collect as logs. `daemon start` forks a background process
and exits, which makes the container exit immediately.

A minimal entrypoint (`deploy/start.sh`):

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT=/app

# Write the transport bind address into project config so the plugin
# binds an externally-reachable interface instead of loopback.
BIND="${ANIMUS_TRANSPORT_HTTP_BIND:-0.0.0.0:8080}"
mkdir -p "$ROOT/.animus"
python3 - "$ROOT/.animus/config.json" "$BIND" <<'PY'
import json, sys, pathlib
path, bind = pathlib.Path(sys.argv[1]), sys.argv[2]
cfg = json.loads(path.read_text()) if path.exists() and path.read_text().strip() else {}
cfg.setdefault("transports", {}).setdefault("animus-transport-http", {})["bind_addr"] = bind
path.write_text(json.dumps(cfg, indent=2))
PY

# Start the daemon in the foreground (correct for containers).
# Preflight runs automatically; --auto-install installs missing required plugins
# from pinned defaults if any were omitted from the image.
animus daemon run --project-root "$ROOT" &
DAEMON_PID=$!

# Wait for the control socket to become healthy before spawning the transport.
for _ in $(seq 1 30); do
  animus daemon health --project-root "$ROOT" >/dev/null 2>&1 && break
  sleep 1
done

# Spawn the installed transport plugin (animus-transport-http or the full stack).
animus web serve --project-root "$ROOT" &
WEB_PID=$!
trap 'kill "$DAEMON_PID" "$WEB_PID" 2>/dev/null || true' EXIT

wait "$DAEMON_PID"
```

The daemon and the transport are co-located in one container here. For higher
availability, split them: one container runs the daemon without a transport,
another runs the transport pointed at the daemon's control socket.

### Required environment variables at runtime

| Variable | Required | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | Yes (claude provider) | Provider API key; injected into the plugin's env via process env |
| `ANIMUS_TRANSPORT_HTTP_BIND` | No (default `0.0.0.0:8080`) | Interface and port the HTTP transport binds |
| `ANIMUS_WEBAPI_TOKEN` | Strongly recommended | Bearer token that any gateway/proxy must present to the transport |

**Keychain unavailable in containers.** The OS keychain is not available in
typical container environments. Secrets must reach the daemon and its plugins
via process environment variables. The keychain layer is a no-op when the
process env already carries the value, so existing workflows work without
change. See [Secrets reference](../reference/secrets.md#headless-and-ci).

## 3. Deploying to Railway

### `railway.toml`

Place this file alongside your Dockerfile:

```toml
[build]
builder = "dockerfile"
dockerfilePath = "deploy/Dockerfile"

[deploy]
startCommand = "/app/deploy/start.sh"
restartPolicyType = "on_failure"
restartPolicyMaxRetries = 3
```

### Two-service topology (recommended)

The HTTP transport plugin has no built-in authentication. **Never assign the
Animus daemon service a public Railway domain.** Instead use Railway's private
network (`*.railway.internal`) and front it with a separate authenticated
proxy or gateway service.

```
┌──────────────────────────────────────────────┐
│  Railway project                             │
│                                              │
│  Service: animus  (PRIVATE — no public URL)  │
│  ┌────────────────────────────────────────┐  │
│  │ animus daemon run   (foreground)       │  │
│  │ animus web serve    (transport plugin) │  │  ← binds 0.0.0.0:8080
│  └────────────────────────────────────────┘  │
│           ↑ Railway private network          │
│  Service: gateway  (PUBLIC)                  │
│  ┌────────────────────────────────────────┐  │
│  │ Hono / Express / nginx / etc.          │  │  ← public HTTPS domain
│  │ Verifies caller auth                   │  │
│  │ Adds Bearer header, forwards to        │  │
│  │ http://animus.railway.internal:8080    │  │
│  └────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
```

In the gateway service, reach the Animus webapi using the Railway private DNS
name: `http://<service-name>.railway.internal:8080`. The gateway should add
`Authorization: Bearer <ANIMUS_WEBAPI_TOKEN>` to every forwarded request.

### Environment variables

Set these in the Railway dashboard under the **private `animus` service**:

| Variable | Description |
|---|---|
| `ANTHROPIC_API_KEY` | Claude provider API key (or the key for your chosen provider) |
| `ANIMUS_TRANSPORT_HTTP_BIND` | Override bind address. Default: `0.0.0.0:8080` |
| `ANIMUS_WEBAPI_TOKEN` | Bearer token the gateway must include on every request |

On the **public gateway service**, set at minimum:

| Variable | Description |
|---|---|
| `ANIMUS_WEBAPI_URL` | `http://animus.railway.internal:8080` |
| `ANIMUS_WEBAPI_TOKEN` | Must match the value set on the animus service |

### Persistence on Railway

Add a **Railway Volume** mounted at `/home/appuser/.animus` (adjust the path to
match your container user's `$HOME`). This persists scoped runtime state —
queue, runs, artifacts — across redeploys.

If subjects must also survive redeploys, install a persistent subject backend
plugin (e.g. a Postgres-backed backend) rather than relying on
`animus-subject-default`, which stores subjects on the container's local disk.

## 4. The HTTP Web API

### Installing the transport

```bash
# Installs transport-http (required), transport-graphql, and web-ui (recommended).
animus plugin install-defaults --include-transports

# Or install only the HTTP transport:
animus plugin install launchapp-dev/animus-transport-http
```

### Starting the transport

```bash
animus web serve
```

`animus web serve` discovers and spawns installed `transport_backend` and
`web_ui` plugins. It does **not** start an in-process HTTP server — the
plugin binds its own port. If no transport plugins are installed, the command
exits non-zero and prints the install command. The transport reports its bound
URL through the plugin's JSON-RPC `initialize` handshake.

### Bind address

The HTTP transport reads its bind address from (in priority order):

1. `bind_addr` in `.animus/config.json` under the `transports.animus-transport-http` key.
2. The `ANIMUS_TRANSPORT_HTTP_BIND` environment variable (default: `127.0.0.1:8080`).

In a container, configure the transport to bind `0.0.0.0` so other containers
can reach it. The entrypoint script in Section 2 patches `config.json` at boot.

### REST surface — `/api/v1`

All endpoints return the same JSON envelope:

```json
{ "ok": true,  "data": { ... } }
{ "ok": false, "error": { "code": "some_error_code", "message": "..." } }
```

#### Authentication

Pass `ANIMUS_WEBAPI_TOKEN` as a Bearer token on every request:

```bash
curl -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" http://localhost:8080/api/v1/subjects
```

The transport plugin itself has **no built-in authentication**. In production
always front it with an authenticated gateway (see Section 3).

#### Create a subject

```bash
curl -s -X POST http://localhost:8080/api/v1/subjects \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "kind":   "task",
    "title":  "Implement feature X",
    "body":   "Detailed description here",
    "status": "ready",
    "labels": ["team:backend", "priority:high"]
  }'
```

Response:

```json
{
  "ok": true,
  "data": {
    "id":         "task:TASK-001",
    "kind":       "task",
    "title":      "Implement feature X",
    "status":     "ready",
    "labels":     ["team:backend", "priority:high"],
    "body":       "Detailed description here",
    "created_at": "2026-06-17T10:00:00Z"
  }
}
```

#### Get a subject by ID

```bash
curl -s "http://localhost:8080/api/v1/subjects/task%3ATASK-001" \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN"
```

URL-encode the `:` in the subject ID (`task:TASK-001` → `task%3ATASK-001`).

#### List subjects by kind

```bash
curl -s "http://localhost:8080/api/v1/subjects?kind=task&limit=50" \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN"
```

The response envelope carries a `subjects` array. The typed list route does
not support label filtering; use the plugin-call escape hatch below for that.

#### Update a subject's labels

```bash
curl -s -X POST "http://localhost:8080/api/v1/subjects/task%3ATASK-001" \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"labels_add": ["visibility:public"], "labels_remove": ["visibility:private"]}'
```

#### Enqueue a subject for execution

Push a subject onto the daemon's dispatch queue. The daemon leases it and runs
the project's configured workflow. This is more durable than triggering a
workflow directly because it survives the HTTP request lifecycle and daemon
restarts.

```bash
curl -s -X POST http://localhost:8080/api/v1/queue/enqueue \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"task_id": "task:TASK-001"}'
```

#### Plugin-call escape hatch (server-side filtering)

```bash
curl -s -X POST "http://localhost:8080/api/v1/subject/animus-subject-default/call" \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "method": "subject/list",
    "params": {
      "kind":       ["task"],
      "labels_any": ["team:backend"]
    }
  }'
```

Replace `animus-subject-default` with the installed subject backend plugin
name. Use `animus plugin list` to see installed plugin names.

#### Check daemon health

```bash
curl -s "http://localhost:8080/api/v1/daemon/health" \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN"
```

### Summary of confirmed REST endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/v1/subjects` | Create a subject (`kind`, `title`, `body`, `status`, `labels`) |
| `GET` | `/api/v1/subjects/:id` | Get a subject by ID (URL-encode the `:` in the id) |
| `GET` | `/api/v1/subjects?kind=<k>&limit=<n>` | List subjects by kind |
| `POST` | `/api/v1/subjects/:id` | Update subject labels (`labels_add`, `labels_remove`) |
| `POST` | `/api/v1/subject/:plugin/call` | Raw plugin RPC (`method` + `params`; use for label filtering) |
| `POST` | `/api/v1/queue/enqueue` | Enqueue a subject (`task_id`) |
| `GET` | `/api/v1/daemon/health` | Daemon health check |

### Security model

In production:

1. **No public domain on the daemon service.** Use private networking so the
   transport is only reachable from within the deployment project.
2. **An authenticated gateway** sits in front, enforces caller auth, and adds
   `Authorization: Bearer $ANIMUS_WEBAPI_TOKEN` to every forwarded request.
3. **`ANIMUS_WEBAPI_TOKEN`** is a shared secret between the gateway and the
   transport-side forwarder. Set it via your platform's secret injection
   mechanism, not in source control.

## 5. GraphQL (`animus-transport-graphql`)

### Installing and starting

```bash
# Install the full transport set (HTTP required, GraphQL + web UI recommended):
animus plugin install-defaults --include-transports

# Or individually:
animus plugin install launchapp-dev/animus-transport-http
animus plugin install launchapp-dev/animus-transport-graphql
animus plugin install launchapp-dev/animus-web-ui
```

Start all installed transport plugins at once:

```bash
animus web serve
```

Each installed `transport_backend` plugin binds its own port and reports its
URL at startup. The GraphQL transport typically binds at
`http://localhost:4000/graphql` by default, but **always check the `animus web
serve` startup output** for the exact address — it may vary by install
configuration.

### Introspecting the schema

Use any GraphQL client or run:

```bash
curl -s -X POST http://localhost:4000/graphql \
  -H "Content-Type: application/json" \
  -d '{"query":"{ __schema { types { name kind } } }"}'
```

Or retrieve the SDL via the transport's plugin-call:

```bash
curl -s -X POST http://localhost:8080/api/v1/subject/animus-transport-graphql/call \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"method":"transport/schema","params":{}}'
```

The GraphQL surface covers the operations the web UI consumes: workflows,
subjects (tasks, requirements), queue, daemon control, skills, event
subscriptions, and outputs. The full CLI + MCP surface is not covered by
GraphQL — use the CLI or MCP tools for operations outside this set.

### Example queries

> Field names below reflect the documented subject model. Verify exact field
> names against the running schema introspection before using in production.

```graphql
query ListTasks {
  subjects(kind: "task", limit: 20) {
    id
    title
    status
    labels
    createdAt
  }
}

query GetWorkflows {
  workflows {
    id
    status
    currentPhase
  }
}
```

### Example subscription

The GraphQL transport exposes live event subscriptions. The exact subscription
transport (WebSocket or SSE) is version-specific — introspect the schema to
confirm:

```graphql
subscription WorkflowEvents {
  workflowEvents {
    workflowId
    kind
    phase
    timestamp
  }
}
```

### What is not locally verified

The `animus-transport-graphql` plugin is out-of-tree at
[`launchapp-dev/animus-transport-graphql`](https://github.com/launchapp-dev/animus-transport-graphql).
The following specifics are not verifiable without a running instance:

- Exact bound port (check `animus web serve` output).
- Exact field names and types (introspect via the running endpoint).
- Auth model for the GraphQL endpoint (likely the same `ANIMUS_WEBAPI_TOKEN`
  bearer pattern; confirm with `animus plugin info --name animus-transport-graphql`).
- Subscription transport type (WebSocket vs SSE).

Use `animus plugin info --name animus-transport-graphql` and schema
introspection to ground your client implementation against the installed version.

---

## Quick Reference

```bash
# Install all transport plugins
animus plugin install-defaults --include-transports

# Verify all required plugin roles are satisfied
animus daemon preflight --project-root .

# Run daemon in foreground (correct for containers)
animus daemon run --project-root . &

# Start transport plugins
animus web serve --project-root .

# Health check via HTTP transport
curl http://localhost:8080/api/v1/daemon/health \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN"

# Create a subject
curl -X POST http://localhost:8080/api/v1/subjects \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"kind":"task","title":"My task","status":"ready"}'

# Enqueue a subject for workflow execution
curl -X POST http://localhost:8080/api/v1/queue/enqueue \
  -H "Authorization: Bearer $ANIMUS_WEBAPI_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"task_id":"task:TASK-001"}'

# Introspect GraphQL schema
curl -X POST http://localhost:4000/graphql \
  -H "Content-Type: application/json" \
  -d '{"query":"{ __schema { types { name } } }"}'
```
