# Architecture Overview

Animus is a Rust-only agent orchestrator built as a Cargo workspace of 11 crates.
It provides the `animus` CLI, daemon runtime, shared workflow execution/runtime
helpers, MCP server, plugin host, and plugin protocol crates.
Provider, subject, transport,
and web UI integrations run as external stdio plugins rather than in-process
desktop or web shell frameworks. The workspace also depends on external
`launchapp-dev/animus-protocol` crates, mixing legacy `v0.1.13`
provider/session wires with newer `v0.5.x` queue/workflow/subject protocols.

Trust code and generated references over hand-maintained summaries when they
disagree. Start with:

- [Full System Architecture](full-system-architecture.md)
- [Runtime Architecture](runtime-architecture.md)
- [Plugin System](plugin-system.md)
- [Crate Map](crate-map.md)
- [Runtime Topology Diagram](diagram.md)

## Crate Dependency Graph

```mermaid
graph TD
    CLI[orchestrator-cli]
    CORE["orchestrator-core<br/>(incl. subject_adapter + store)"]
    PROTO[protocol]
    CONFIG[orchestrator-config]
    DAEMON[orchestrator-daemon-runtime]
    WR[animus-runtime-shared]
    SESSION["orchestrator-plugin-host::session"]
    PLUGIN_HOST[orchestrator-plugin-host]
    PLUGIN_PROTO[animus-plugin-protocol]
    PLUGIN_RUNTIME[animus-plugin-runtime]
    SUBJECT_PROTO[animus-subject-protocol]
    LOG[orchestrator-logging]

    CLI --> CORE
    CLI --> DAEMON
    CLI --> WR
    CLI --> PLUGIN_HOST
    CLI --> PROTO

    DAEMON --> CORE
    DAEMON --> WR
    DAEMON --> PLUGIN_HOST
    DAEMON --> LOG
    DAEMON --> PROTO

    WR --> CORE
    WR --> CONFIG
    WR --> PROTO

    SESSION --> PLUGIN_HOST
    SESSION --> PLUGIN_PROTO

    PLUGIN_HOST --> PLUGIN_PROTO
    PLUGIN_HOST --> SUBJECT_PROTO

    CORE --> CONFIG
    CORE --> LOG
    CORE --> PLUGIN_HOST
    CORE --> PROTO

    CONFIG --> PROTO
    NOTIF --> PROTO
    LOG --> PROTO

    PLUGIN_RUNTIME --> PLUGIN_PROTO
```

`protocol` sits at the foundation for shared types, configuration shapes, and
runtime path derivation.

`orchestrator-core` provides the domain services and state mutation APIs used by
the CLI, daemon, and plugin preflight paths.

`orchestrator-cli` composes the workspace into the user-facing `animus` command
surface.

## Kernel and Plugin Roles

The kernel crates host the daemon and CLI; everything provider-, subject-, transport-, trigger-, and UI-specific runs as an installed stdio plugin behind the plugin host.

```mermaid
graph TB
    subgraph Kernel["Animus kernel (in-tree crates)"]
        CLI["orchestrator-cli<br/>(animus CLI + MCP)"]
        DAEMON["orchestrator-daemon-runtime<br/>(scheduler / queue / control)"]
        CORE["orchestrator-core<br/>(ServiceHub + state)"]
        WR["animus-runtime-shared<br/>(workflow execution helpers)"]
        PHOST["orchestrator-plugin-host<br/>(stdio JSON-RPC host + session bridge)"]
    end

    subgraph Plugins["Installed plugins (out-of-tree, launchapp-dev)"]
        PROV["provider<br/>(claude / codex / gemini / opencode / oai)"]
        SUBJ["subject_backend<br/>(task / requirement / linear / sqlite / markdown)"]
        WFRUN["workflow_runner<br/>(animus-workflow-runner-default)"]
        QUEUE["queue<br/>(animus-queue-default)"]
        TRIG["trigger_backend<br/>(webhook / slack)"]
        TRANS["transport_backend<br/>(http / graphql)"]
        WEBUI["web_ui<br/>(animus-web-ui)"]
    end

    CLI --> CORE
    CLI --> DAEMON
    DAEMON --> CORE
    DAEMON --> WR
    CLI --> PHOST
    DAEMON --> PHOST

    PHOST --> PROV
    PHOST --> SUBJ
    PHOST --> WFRUN
    PHOST --> QUEUE
    PHOST --> TRIG
    PHOST --> TRANS
    PHOST --> WEBUI
```

The plugin extraction is complete: 18 standalone repositories at `launchapp-dev` cover the protocol, providers, subject backends, transports, web UI, triggers, log storage, the conformance testkit, and release tooling.

## Architecture Decision Records

- [Kernel and Flavors](kernel-and-flavors.md) -- **v0.5 product architecture commitment.** Animus is a kernel + a default flavor (curated plugin bundle) for portfolio builders. Future flavors emerge from real customer pull, not roadmap speculation. Read this before adding scope.
- [Naming Contract](naming-contract.md) -- One name everywhere: `animus.*` for MCP tools, env vars, config dirs, pack ids, and JSON envelopes
- [Full System Architecture](full-system-architecture.md) -- Canonical end-to-end architecture narrative covering crates, process topology, state, config, services, daemon, workflow execution, plugins, control surfaces, security, observability, and verification
- [Runtime Architecture](runtime-architecture.md) -- Current end-to-end runtime topology, startup flow, state model, crate responsibilities, execution pipeline, and failure boundaries
- [Plugin System](plugin-system.md) -- Current stdio plugin architecture: discovery, install state, wire protocol, hosting, security, provider/subject/trigger/transport paths, and operations
- [Plugin Pack Kernel](plugin-pack-kernel.md) -- Package-style plugin architecture for workflows, MCP servers, and bundled domain modules
- [Project Init Templates](project-init-templates.md) -- Template-driven `animus init` architecture layered above packs
- [Subject Dispatch Daemon](subject-dispatch-daemon.md) -- How the daemon schedules and dispatches workflow subjects
- [Subject Backend Plugins](subject-backend-plugins.md) -- Current subject_backend contract: normalized subjects, kind-scoped routing, preflight requirements, CLI/daemon behavior, and authoring rules
- [Knowledge / RAG Binding (v0.5.5 design)](knowledge-rag-binding-v0.5.5.md) -- Design for the `memory_store` plugin kind, agent + workflow `memory_bindings:` shape, runtime contract injection point, CLI surface, and the v0.6 implementation checklist. Ships design-only in v0.5.5; honest stops documented inline
- [Animus Chat](animus-chat.md) -- v0.5.10 chat architecture: conversation subjects, `chat_provider` plugins, daemon-side tool loop, streaming control protocol, and OpenAI-compatible HTTP surface
- [Tool-Driven Mutation Surfaces](tool-driven-mutation-surfaces.md) -- How state mutations are channeled through tool abstractions
- [Workflow-First CLI](workflow-first-cli.md) -- Why workflows are the primary execution primitive
- [Phase Contracts](phase-contracts.md) -- Universal phase verdicts, YAML-defined fields, and runtime validation
- [Multi-Tenant + RBAC Design Proposal (v0.5.5+)](multi-tenant-rbac-v0.5.5.md) -- Principal model, role/permission scaffold, four-chokepoint authz, and the explicit v0.6 deferrals for per-tenant state isolation and per-principal secret routing

## Deep Dives

- [Runtime Topology Diagram](diagram.md) -- High-level Mermaid diagram of operators, daemon, plugins, and external systems with design rationale
- [Crate Map](crate-map.md) -- All workspace crates grouped by responsibility with descriptions
- [ServiceHub Pattern](service-hub.md) -- Dependency injection via the `ServiceHub` trait
- [Provider Session Host](llm-cli-wrapper-session-backends.md) -- Historical session-backend design notes plus the current `orchestrator-plugin-host::session` provider-plugin boundary
