# Orchestrator Web UI

React shell and route architecture for `TASK-011`.

## Scope

- Persistent shell with primary navigation and project context frame.
- Route tree for dashboard, daemon, projects, tasks, workflows, events, and review handoff.
- Shared API client that validates `ao.cli.v1` envelope responses.
- SSE stream hook for `/api/v1/events` with reconnect behavior and `Last-Event-ID` header resume.

## Commands

```bash
npm install
npm run dev
npm run test
npm run build
```

Build output targets `crates/orchestrator-web-server/embedded/` via Vite config.
