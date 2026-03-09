# Web Dashboard Guide

AO includes an embedded web dashboard for visual project management. It provides a browser-based interface for monitoring workflows, managing tasks, and viewing requirements.

## Starting the Web Server

Launch the web server (default port 3000):

```bash
ao web serve
```

Specify a custom port:

```bash
ao web serve --port 8080
```

Open the dashboard in your default browser:

```bash
ao web open
```

## Features

The web dashboard provides:

- **Project overview** -- Summary of project health, active workflows, and task statistics.
- **Requirements board** -- Visual board for requirements with status columns (Draft, Refined, Planned, In-Progress, Done).
- **Task management** -- List and filter tasks by status, priority, and type. Update task status directly from the UI.
- **Workflow monitor** -- Real-time view of running workflows with phase progress indicators.

## REST API

The web server exposes a REST API at `/api/v1/`. All responses follow the standard `ao.cli.v1` JSON envelope format:

```json
{
  "schema": "ao.cli.v1",
  "ok": true,
  "data": { ... }
}
```

### Key Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/tasks` | GET | List tasks |
| `/api/v1/tasks/:id` | GET | Get task by ID |
| `/api/v1/tasks` | POST | Create task |
| `/api/v1/tasks/:id` | PATCH | Update task |
| `/api/v1/workflows` | GET | List workflows |
| `/api/v1/workflows/:id` | GET | Get workflow by ID |
| `/api/v1/daemon/status` | GET | Daemon status |
| `/api/v1/daemon/health` | GET | Daemon health |
| `/api/v1/requirements` | GET | List requirements |
| `/api/v1/requirements/:id` | GET | Get requirement by ID |

### Filtering

List endpoints support query parameters for filtering:

```
GET /api/v1/tasks?status=in-progress&priority=high
GET /api/v1/requirements?status=refined
```

## SSE Events

The web server supports Server-Sent Events (SSE) for real-time updates. Connect to the SSE endpoint to receive live workflow and task state changes:

```
GET /api/v1/events
```

Events are pushed as workflows progress through phases, tasks change status, and agents produce output. This powers the real-time workflow monitor in the dashboard.

## Architecture

The web stack is split across three crates:

| Crate | Role |
|-------|------|
| `orchestrator-web-contracts` | Shared request/response types |
| `orchestrator-web-api` | Business logic (`WebApiService`) |
| `orchestrator-web-server` | Axum HTTP server with embedded static assets |

Static assets for the dashboard UI are built from `crates/orchestrator-web-server/web-ui/` (a Node.js / npm project) and embedded into the binary at compile time.
