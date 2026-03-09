# orchestrator-web-api

Transport-agnostic web API business layer for AO.

## Overview

`orchestrator-web-api` sits between the Axum server crate and the AO service hub. It normalizes request inputs, delegates to core and daemon-runtime services, and publishes sequenced daemon-style events for SSE consumers.

## Targets

- Library: `orchestrator_web_api`

## Architecture

```mermaid
graph TD
    CTX["WebApiContext"] --> SVC["WebApiService"]

    subgraph "handler groups"
        SYS["system_handlers"]
        PRJ["projects_handlers"]
        TASK["tasks_handlers"]
        WF["workflows_handlers"]
        REQ["requirements_handlers"]
        VIS["vision_handlers"]
        REV["reviews_handlers"]
        DAEMON["daemon_handlers"]
        QUE["queue_handlers"]
    end

    SVC --> SYS
    SVC --> PRJ
    SVC --> TASK
    SVC --> WF
    SVC --> REQ
    SVC --> VIS
    SVC --> REV
    SVC --> DAEMON
    SVC --> QUE
    SVC --> EVT["event_stream"]
```

## Key types

- `WebApiContext`
- `WebApiService`
- `WebApiError`

## Responsibilities

- delegate task, workflow, project, planning, daemon, and queue operations
- parse and normalize web-facing inputs
- broadcast daemon-style events through an in-process channel
- replay persisted events for reconnecting clients

## Workspace dependencies

```mermaid
graph LR
    API["orchestrator-web-api"]
    CORE["orchestrator-core"]
    DRT["orchestrator-daemon-runtime"]
    CONTRACTS["orchestrator-web-contracts"]
    PROTO["protocol"]

    API --> CORE
    API --> DRT
    API --> CONTRACTS
    API --> PROTO
```

## Notes

- This crate does not define HTTP routes or bind a socket.
- `orchestrator-web-server` is the transport layer on top of it.
