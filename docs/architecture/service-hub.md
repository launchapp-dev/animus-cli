# ServiceHub Pattern

The `ServiceHub` trait is Animus's dependency injection boundary. It gives the CLI, daemon, tests, and web layers one uniform way to access domain services.

## The Trait

Defined in `crates/orchestrator-core/src/services.rs`, `ServiceHub` exposes the core service APIs:

- `daemon()`
- `tasks()`
- `task_provider()`
- `subject_resolver()`
- `workflows()`
- `planning()`
- `requirements_provider()`
- `requirements_planning()`
- `project_adapter()`
- `review()`

Each accessor returns an `Arc<dyn Trait>` so callers can share service implementations across async boundaries.

`ServiceHub` is the single trait both the production and test composition roots implement, so every caller depends on the trait rather than on `FileServiceHub` or `InMemoryServiceHub` directly:

```mermaid
classDiagram
    class ServiceHub {
        <<trait>>
        +daemon() Arc~DaemonServiceApi~
        +tasks() Arc~TaskServiceApi~
        +task_provider() Arc~TaskProvider~
        +subject_resolver() Arc~SubjectResolver~
        +workflows() Arc~WorkflowServiceApi~
        +planning() Arc~PlanningServiceApi~
        +requirements_provider() Arc~RequirementsProvider~
        +requirements_planning() Arc~RequirementsPlanningService~
        +project_adapter() Arc~ProjectAdapter~
        +review() Arc~ReviewServiceApi~
    }
    class FileServiceHub {
        +file + SQLite backed state
    }
    class InMemoryServiceHub {
        +in-memory test state
    }
    ServiceHub <|.. FileServiceHub : production
    ServiceHub <|.. InMemoryServiceHub : tests
```

## Production: `FileServiceHub`

`FileServiceHub` is the production implementation.

At startup it:

1. resolves the project root
2. bootstraps `.animus/` project config and the repo-scoped runtime root under `~/.animus/<repo-scope>/`
3. loads `core-state.json`
4. loads persisted workflows, tasks, and requirements from `workflow.db`
5. returns a service hub backed by filesystem and SQLite state

Important runtime files:

```text
~/.animus/<repo-scope>/
├── core-state.json
├── resume-config.json
├── workflow.db
├── config/state-machines.v1.json
├── state/
├── daemon/
└── worktrees/
```

`FileServiceHub` uses file locking around `core-state.json` mutations so concurrent CLI invocations and daemon work do not trample each other.

## Tests: `InMemoryServiceHub`

`InMemoryServiceHub` keeps the same service surface but stores everything in
memory. That lets tests exercise the same business logic without filesystem
I/O.

When a test needs plugin-backed subject or project fallback behavior, construct
the hub with `InMemoryServiceHub::new().with_project_root(...)`. Without a
project root, `subject_resolver()` and `project_adapter()` stay in-memory-only
and intentionally skip plugin fallback wiring.

## Dependency Flow

```mermaid
graph TD
    CLI["CLI / Daemon / Web API"]
    HUB["ServiceHub trait"]
    FILE["FileServiceHub"]
    MEM["InMemoryServiceHub"]
    APIS["Service API traits"]
    STATE["CoreState + workflow.db"]
    STORE["orchestrator-core::store"]

    CLI --> HUB
    HUB --> FILE
    HUB --> MEM
    FILE --> APIS
    MEM --> APIS
    FILE --> STATE
    MEM --> STATE
    FILE --> STORE
    STORE --> FS["Filesystem"]
```

All higher layers depend on `ServiceHub` and the service traits, not on concrete storage details.

## Mutation Flow

State mutations always run through a service API rather than hand-editing JSON, so locking and persistence stay centralized:

```mermaid
sequenceDiagram
    participant Caller as CLI / MCP / control handler
    participant Hub as ServiceHub
    participant Api as service API trait
    participant Lock as core-state lock
    participant Store as core-state.json + workflow.db

    Caller->>Hub: hub.workflows() / hub.tasks() / ...
    Hub-->>Caller: Arc~ServiceApi~
    Caller->>Api: mutation call
    Api->>Lock: acquire file lock (FileServiceHub)
    Api->>Store: read-modify-write
    Store-->>Api: persisted state
    Api->>Lock: release
    Api-->>Caller: result
```
