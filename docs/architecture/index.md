# Architecture Overview

AO is a Rust-only agent orchestrator CLI built as a 16-crate Cargo workspace. It provides a CLI, daemon, agent runner, LLM wrappers, MCP server, and web UI for orchestrating AI agent workflows.

## Crate Dependency Graph

```mermaid
graph TD
    CLI[orchestrator-cli]
    CORE[orchestrator-core]
    PROTO[protocol]
    STORE[orchestrator-store]
    CONFIG[orchestrator-config]
    DAEMON[orchestrator-daemon-runtime]
    WR[workflow-runner]
    AR[agent-runner]
    LLM[llm-cli-wrapper]
    OAI[oai-runner]
    WAPI[orchestrator-web-api]
    WCON[orchestrator-web-contracts]
    WSRV[orchestrator-web-server]
    PROV[orchestrator-providers]
    NOTIF[orchestrator-notifications]
    GIT[orchestrator-git-ops]

    CLI --> CORE
    CLI --> DAEMON
    CLI --> WR
    CLI --> WAPI
    CLI --> WCON
    CLI --> WSRV
    CLI --> GIT
    CLI --> NOTIF
    CLI --> LLM
    CLI --> PROTO

    WSRV --> WAPI
    WSRV --> WCON
    WAPI --> CORE
    WAPI --> DAEMON
    WAPI --> WCON
    WAPI --> PROTO

    DAEMON --> CORE
    DAEMON --> WR
    DAEMON --> GIT
    DAEMON --> NOTIF
    DAEMON --> PROV
    DAEMON --> PROTO

    GIT --> CORE
    GIT --> WR
    GIT --> PROTO

    WR --> CORE
    WR --> CONFIG
    WR --> PROTO

    CORE --> STORE
    CORE --> CONFIG
    CORE --> PROV
    CORE --> LLM
    CORE --> PROTO

    AR --> LLM
    AR --> PROTO

    OAI --> PROTO

    STORE --> PROTO
    CONFIG --> PROTO
    PROV --> PROTO
    NOTIF --> PROTO
    WCON --> PROTO
```

**protocol** sits at the foundation -- every crate depends on it for shared wire types, configuration, and IPC contracts.

**orchestrator-core** occupies the middle layer, providing domain logic, state management, and the ServiceHub dependency injection pattern.

**orchestrator-cli** sits at the top as the main `ao` binary, composing all other crates into the user-facing command surface.

## Architecture Decision Records

- [Subject Dispatch Daemon](subject-dispatch-daemon.md) -- How the daemon schedules and dispatches workflow subjects
- [Tool-Driven Mutation Surfaces](tool-driven-mutation-surfaces.md) -- How state mutations are channeled through tool abstractions
- [Workflow-First CLI](workflow-first-cli.md) -- Why workflows are the primary execution primitive
- [Phase Contracts](phase-contracts.md) -- Universal phase verdicts, YAML-defined fields, and runtime validation

## Deep Dives

- [Crate Map](crate-map.md) -- All 16 crates grouped by responsibility with descriptions
- [ServiceHub Pattern](service-hub.md) -- Dependency injection via the ServiceHub trait
