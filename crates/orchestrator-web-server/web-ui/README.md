# Orchestrator Web UI

React shell and route architecture for `TASK-011`.

## Scope

- Persistent shell with primary navigation and project context frame.
- Route tree for dashboard, daemon, projects, tasks, workflows, events, and review handoff.
- Shared API client that validates `ao.cli.v1` envelope responses.
- SSE stream hook for `/api/v1/events` with reconnect behavior and `Last-Event-ID` header resume.

## Task docs

- `TASK-011` requirements and UX docs:
  `../docs/task-011-react-shell-requirements.md`,
  `../docs/task-011-react-shell-ux-brief.md`,
  `../docs/task-011-react-shell-implementation-notes.md`
- `TASK-019` requirements and implementation notes:
  `../docs/task-019-observability-diagnostics-requirements.md`,
  `../docs/task-019-observability-diagnostics-ux-brief.md`,
  `../docs/task-019-observability-diagnostics-mockup-review.md`,
  `../docs/task-019-observability-diagnostics-implementation-notes.md`
- `TASK-017` requirements and implementation notes:
  `../docs/task-017-accessibility-responsive-performance-requirements.md`,
  `../docs/task-017-accessibility-responsive-performance-implementation-notes.md`
- `TASK-018` requirements and implementation notes:
  `../docs/task-018-web-gui-ci-e2e-release-gates-requirements.md`,
  `../docs/task-018-web-gui-ci-e2e-release-gates-ux-brief.md`,
  `../docs/task-018-web-gui-ci-e2e-release-gates-implementation-notes.md`
- Web GUI release checklist:
  `../../../.github/release-checklists/web-gui-release.md`

## Commands

```bash
npm install
npm run dev
npm run test
npm run build
```

Build output targets `crates/orchestrator-web-server/embedded/` via Vite config.
