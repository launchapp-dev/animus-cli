# ACP (Agent Client Protocol) Integration for Animus

**Date:** April 2026  
**Status:** Design - Research & Planning Phase  
**Scope:** Animus as ACP-compliant agent server for IDE integration (VS Code, JetBrains, Cursor)

---

## Executive Summary

This document evaluates the Agent Client Protocol (ACP) specification and proposes how the Animus CLI could expose an ACP-compatible server interface, enabling IDEs to connect to Animus as a standardized agent provider without vendor lock-in. The integration positions Animus as a universal agent orchestrator that can serve any IDE that implements ACP, expanding its accessibility and value proposition.

---

## 1. Agent Client Protocol (ACP) Specification Summary

### 1.1 Overview

The Agent Client Protocol is a standardized, open protocol for communication between code editors/IDEs and coding agents, created by JetBrains and Zed. It solves the integration overhead problem where each agent-editor pairing requires custom integration work.

**Core Principle:** Agents implementing ACP work with any ACP-compatible editor without vendor-specific modifications.

### 1.2 Architecture & Communication Models

#### Local Deployment
- Agent runs as a sub-process of the editor
- Communication via **JSON-RPC** over standard input/output (stdio)
- Low latency, no network overhead
- Suitable for local agent execution

#### Remote Deployment
- Agent hosted in cloud or separate infrastructure
- Communication over **HTTP** or **WebSocket** (HTTP support stable, WebSocket documented as work-in-progress)
- Enables centralized agent management
- Supports collaborative and distributed workflows

### 1.3 Core Concepts & Message Flow

#### Session-Based Workflow
ACP organizes agent activity around **sessions** — isolated conversation contexts that persist state and history.

**Key Session Methods:**
- `session/new` — Create a new conversation session
- `session/load` — Resume an existing session
- `session/list` — Enumerate available sessions
- `session/prompt` — Send user input and receive agent responses
- `session/cancel` — Cancel ongoing operations
- `session/setMode` — Switch agent operating modes (e.g., planning, editing, analysis)
- `session/setConfigOption` — Adjust session-specific settings

#### Initialization & Authentication
- `initialize` — Establishes connection, negotiates capabilities, exchanges protocol versions
- `authenticate` — Validates client identity (optional, implementation-dependent)

#### Bidirectional Operations

**Client-Initiated (Editor → Agent):**
- Session and conversation management
- Mode and configuration changes

**Agent-Initiated (Agent → Editor):**
- `fs/readTextFile`, `fs/writeTextFile` — File system access with user approval
- `fs/createFile`, `fs/deleteFile`, `fs/renameFile` — File lifecycle operations
- `terminal/create`, `terminal/output`, `terminal/kill`, `terminal/release` — Terminal access
- `requestPermission` — Request user authorization for sensitive operations

#### Content Representation

ACP supports multiple content types for rich communication:

| Content Type | Use Case |
|---|---|
| **TextContent** | Markdown-formatted responses, explanations, code snippets |
| **ImageContent** | Visual diagrams, UI mockups, terminal screenshots |
| **AudioContent** | Voice feedback, spoken explanations |
| **ResourceLink** | External references (docs, tools, artifacts) |
| **EmbeddedResource** | Self-contained attachments (base64 encoded) |

**Default Format:** Markdown, chosen for flexibility without requiring HTML rendering capabilities in all editors.

#### Capability Negotiation

Both clients and agents declare capabilities during initialization:

**Client Capabilities (what the editor can do):**
- File system (read, write, delete, rename)
- Terminal (create, execute, interact)
- MCP tool support (stdio, HTTP, SSE transports)
- Sampling and prompt handling
- Planning and session management

**Agent Capabilities (what the agent can do):**
- Session persistence
- Mode switching (planning, editing, analysis, etc.)
- Tool/MCP integration
- Artifact generation
- Cost tracking

### 1.4 MCP Integration

ACP leverages the Model Context Protocol (MCP) for tool extension:
- Agents declare available MCP servers during initialization
- Editors configure MCP clients to connect to agent-managed tool servers
- Supported MCP transports: **stdio, HTTP, SSE**
- Enables standardized tool access across any agent

### 1.5 Planning & Execution

ACP includes first-class support for planning workflows:
- Agents can expose a `plan` capability showing steps, status, and dependencies
- Plans include priority levels, execution order, and completion tracking
- Editors can visualize and interact with agent plans

---

## 2. How Animus Maps to ACP Concepts

### 2.1 Current Animus Architecture

Animus is a Rust-only agent orchestrator with:

- **Rust-only Cargo workspace** (12 current workspace members) with clean separation of concerns
- **CLI surface** exposing `project`, `queue`, `subject`, `workflow`, `plugin`, `mcp`, and other command groups
- **Web UI** served through out-of-tree `transport_backend` + `web_ui` plugins resolved by `animus web`
- **Runtime state** scoped under `~/.animus/<repo-scope>/`
- **Workflow YAML** overlays in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`
- **Provider session bridge** orchestrating multi-step tasks with LLM and tool execution through installed plugins
- **Built-in MCP service** (`animus mcp serve`) plus plugin-aggregated tool surfaces
- **Daemon mode** for background task execution and status tracking

### 2.2 ACP Server Mapping

#### Session → Animus Workflow/Task Context

| ACP Concept | Animus Equivalent | Mapping |
|---|---|---|
| `session/new` | `animus workflow run` | Creates or enqueues a workflow run with isolation |
| `session/load` | `animus workflow get` or `animus workflow resume-status` | Reads workflow state from scoped runtime |
| `session/list` | `animus queue list` or `animus workflow list` | Lists active/pending subjects and workflow runs |
| `session/prompt` | `animus agent run --prompt` or `animus workflow run --input-json` | Accepts user input, executes an agent or workflow step |
| `session/cancel` | `animus workflow cancel --id` | Cancels running workflow |
| `session/setMode` | Workflow config mode selection | Switch between agent execution strategies |

#### Agent Capabilities → Animus Services

| ACP Capability | Animus Service |
|---|---|
| `executeCommand` / agent-initiated code execution | Orchestrator agent runner with sandbox isolation |
| `fs/readTextFile`, `fs/writeTextFile` | `FileServiceHub` plus CLI git/worktree operations with version-control integration |
| `terminal/create`, `terminal/output` | External `workflow_runner` plugin plus `animus-runtime-shared` subprocess management |
| MCP tool server | `animus mcp serve` plus plugin-aggregated tool surfaces |
| Session persistence | Scoped runtime state at `~/.animus/<repo-scope>/` |

#### File System Access & Git Safety

Animus can map ACP file operations to its current git/worktree operation layer:
- `fs/readTextFile` → Read from working tree or staged state
- `fs/writeTextFile` → Write to working tree (with user approval via `requestPermission`)
- `fs/createFile` / `fs/deleteFile` → Git-tracked creation/deletion
- Benefits: Automatic version control, change tracking, easy rollback

#### Terminal Integration

Animus delegates workflow subprocess execution to the installed
`workflow_runner` plugin binary, with the shared execution/runtime helpers
living in `animus-runtime-shared`:
- `terminal/create` → Spawn workflow task subprocess
- `terminal/output` → Stream task output to editor
- `terminal/kill` → Terminate task execution
- `terminal/release` → Clean up task resources

### 2.3 Project Scope Management

ACP doesn't natively define "project scope," but Animus can map it:

| Scenario | ACP Handling | Animus Enhancement |
|---|---|---|
| Single project | Editor passes project path in session metadata | Extract repo scope from `.git` |
| Multiple projects | Editor manages separate sessions per project | Use `--project-root` to link session to scope |
| Monorepo / workspace | Separate logical projects within filesystem | Scoped runtime per logical project |

Animus's natural scoping via `.animus/` and `~/.animus/<repo-scope>/` aligns well with ACP's session isolation model.

### 2.4 Workflow Visualization & Planning

ACP's planning capabilities can expose Animus's workflow structure:

```json
{
  "plan": {
    "taskId": "TASK-123",
    "title": "Implement new feature",
    "status": "in-progress",
    "steps": [
      {
        "id": "step-1",
        "title": "Gather requirements",
        "status": "completed",
        "priority": 1
      },
      {
        "id": "step-2",
        "title": "Design architecture",
        "status": "in-progress",
        "priority": 2
      },
      {
        "id": "step-3",
        "title": "Implement solution",
        "status": "pending",
        "priority": 3
      }
    ]
  }
}
```

Animus can expose workflow execution plans through this structure, giving editors first-class visibility into multi-step agent execution.

---

## 3. Implementation Plan

### 3.1 High-Level Architecture

```
┌─────────────────────────────────────────────────────┐
│           IDE (VS Code, JetBrains, Cursor)           │
│              ACP Client Implementation               │
└─────────────────────────────────────────────────────┘
                        ↓ JSON-RPC (HTTP/WebSocket)
                        ↓
┌─────────────────────────────────────────────────────┐
│     Animus ACP Server (New Crate: `animus-acp-server`)   │
│                                                      │
│  • Session Management (new sessions, load, list)     │
│  • Authentication & Capability Negotiation           │
│  • Request Router (client-initiated & agent-init)    │
│  • Response Handler                                  │
└─────────────────────────────────────────────────────┘
                        ↓
┌─────────────────────────────────────────────────────┐
│   Animus Core Services (Existing Architecture)       │
│                                                      │
│  • orchestrator-core (workflow execution, file       │
│    service hub, and persisted state coordination)    │
│  • orchestrator-config (session/mode config)         │
│  • orchestrator-cli MCP surfaces                     │
│  • workflow_runner plugin + animus-runtime-shared    │
│    (agent execution)                                 │
│  • orchestrator-plugin-host (provider sessions and   │
│    plugin routing)                                   │
└─────────────────────────────────────────────────────┘
```

### 3.2 New Components & Crates

#### `animus-acp-server` Crate
A new HTTP/WebSocket server exposing ACP:

**Responsibilities:**
1. Parse and validate ACP JSON-RPC messages
2. Manage session lifecycle (new, load, list, cancel)
3. Route client-initiated methods to orchestrator services
4. Handle agent-initiated operations (file access, terminal, permissions)
5. Implement capability negotiation during initialization
6. Stream responses and notifications to clients

**Key Modules:**
- `server::http` — HTTP/WebSocket transport layer
- `handlers::client` — Client-initiated request handlers
- `handlers::agent` — Agent-initiated operations (file, terminal, permissions)
- `session::manager` — Session lifecycle and persistence
- `capability::negotiation` — ACP capability exchange
- `mcp::bridge` — Map ACP MCP config to `animus mcp serve` and plugin-managed MCP surfaces

#### Integration Points with Existing Crates

| Existing Crate | Integration |
|---|---|
| `orchestrator-core` | Use `FileServiceHub` to execute workflow/task operations |
| `orchestrator-config` | Load/persist session config, interpret mode/settings |
| `orchestrator-core` + CLI git/worktree operations | Map `fs/*` ACP operations to git-tracked file changes |
| `animus-runtime-shared` + external `workflow_runner` plugin | Delegate task execution to the installed runner, stream output |
| `orchestrator-plugin-host::session` | Bridge provider/plugin execution and session backends |
| `orchestrator-cli` | Reuse CLI/MCP operation wiring and output contracts |
| `orchestrator-core::store` | Persist session state at `~/.animus/<repo-scope>/sessions/` and related scoped runtime state |

### 3.3 Feature Breakdown

#### Phase 1: Foundations (Weeks 1-2)
- [ ] Create `animus-acp-server` crate with HTTP transport
- [ ] Implement `initialize` and `authenticate` handlers
- [ ] Define session data model and persistence
- [ ] Add `session/new`, `session/list` methods
- [ ] Capability negotiation (read from workflow config)

#### Phase 2: Session & Prompt Execution (Weeks 2-3)
- [ ] Implement `session/load` and `session/prompt` handlers
- [ ] Integration with `orchestrator-core` to execute workflows
- [ ] Response streaming and error handling
- [ ] `session/cancel` and `session/setMode` support

#### Phase 3: Agent-Initiated Operations (Weeks 3-4)
- [ ] `fs/readTextFile`, `fs/writeTextFile` via git-ops
- [ ] `requestPermission` handler with editor interaction
- [ ] Terminal operations via external `workflow_runner` plugin
- [ ] Edge cases: permission denied, file conflicts, terminal cleanup

#### Phase 4: Advanced Features (Weeks 4-5)
- [ ] MCP bridge: Expose `animus mcp serve` and plugin-managed MCP surfaces to ACP clients
- [ ] Planning capability: expose workflow/task plans
- [ ] Session history and artifact retrieval
- [ ] WebSocket support for streaming responses

#### Phase 5: Polish & Testing (Weeks 5-6)
- [ ] End-to-end integration tests with ACP client libraries
- [ ] Documentation & examples (ACP server setup, IDE setup guides)
- [ ] Performance tuning and connection limits
- [ ] Error recovery and reconnection logic

### 3.4 Configuration

Add ACP server settings to `.animus/config.json` and `~/.animus/<repo-scope>/acp-config.json`:

```json
{
  "acp": {
    "enabled": true,
    "transport": "http",
    "bind_addr": "127.0.0.1",
    "port": 9876,
    "allowed_editors": ["vscode", "jetbrains", "cursor"],
    "session_timeout_minutes": 60,
    "mcp_servers": ["local-tools", "custom-provider"]
  }
}
```

### 3.5 Development & Testing Strategy

**Unit Tests:**
- Session manager: create, load, list, cancel
- Capability negotiation
- ACP message parsing and validation
- Permission handling

**Integration Tests:**
- End-to-end flow: initialize → create session → send prompt → receive response
- File operations: read, write, create, delete via git-ops
- Terminal execution: create, stream output, cancel
- MCP tool integration

**Manual Testing:**
- Connect VS Code with ACP client library to local Animus server
- Execute workflow from editor, observe real-time output
- Test file editing, terminal access with permission prompts
- Verify rollback and error recovery

### 3.6 ACP Client Libraries & IDE Integrations

**Leverage existing integrations:**
- **TypeScript/JavaScript SDK** (npm package `@agentclientprotocol/sdk`)
- **Python SDK** for local agent wrappers
- **Rust SDK** (if available) for closer integration with Animus core

**IDE Extension Architecture:**
- VS Code: Use the TypeScript SDK to build an out-of-tree extension package; do not add editor-extension code under `crates/` in this Rust-only workspace
- JetBrains: Use Kotlin/Java SDK to build plugin
- Cursor: Leverage existing ACP client (if available)

---

## 4. Competitive Advantage

### 4.1 Market Position

**Current State:**
- LLM-powered agents (Claude AI, Cursor, GitHub Copilot) are tightly integrated with specific editors
- Developers face friction: choose an editor or agent, but not both seamlessly
- ACP standardizes this — agents and editors become interchangeable

**Animus's Opportunity:**
Animus can become the **universal agent orchestrator** that works with **any IDE via ACP**, while maintaining its strength as a **powerful, open-source, Rust-based workflow engine**.

### 4.2 Competitive Advantages

#### 1. **Editor Agnostic Deployment**
- Animus as ACP server works with VS Code, JetBrains, Cursor, and any future ACP-compatible IDE
- Developers are not locked into one editor choice
- **Advantage:** Capture broader audience; reduce switching costs

#### 2. **Enterprise & Privacy Focus**
- **Local-first:** Animus runs on developer machines; no cloud dependency
- **Version control integration:** All agent edits tracked in Git — audit trail, rollback, collaboration
- **Self-hosted:** Teams can deploy Animus server internally; full control over data and execution
- **Advantage:** Win enterprise customers with strict data/privacy requirements

#### 3. **Workflow Orchestration Depth**
- Multi-step task execution with state persistence
- Configurable agent behavior via YAML overlays
- Built-in MCP tool integration
- Task queuing, status tracking, artifact management
- **Advantage:** More powerful than single-shot agent sessions; suited for complex, iterative workflows

#### 4. **Cost Transparency**
- Run open-source LLM backends (Llama, Mistral) or bring your own API keys
- No vendor lock-in on model provider
- Per-task cost tracking and quota management
- **Advantage:** Predictable, transparent costs; appeals to cost-conscious teams

#### 5. **Open Source & Community**
- Full codebase visible; extensible via MCP tools and workflow YAML
- Community contributions directly improve the agent orchestrator
- No closed-source black box; debuggability and trust
- **Advantage:** Developer mindshare, academic adoption, OSS ecosystem integration

#### 4.3 Differentiation vs. Other Agents

| Aspect | Cursor / Copilot | OpenHands / Aider | Animus (via ACP) |
|---|---|---|---|
| **IDE Support** | Single editor (tight coupling) | Multiple IDEs (but custom integrations) | Any ACP-compatible IDE |
| **Workflow** | Single-shot conversations | Multi-step tasks (but session-scoped) | Multi-repo workflows, persistent state, queuing |
| **Privacy** | Cloud-dependent | Local-first, but limited history | Local + Git-tracked; full audit trail |
| **Customization** | Vendor controls behavior | MCP tool plugins | YAML workflows, MCP, custom modes |
| **Cost Model** | Per-editor license | Free (open) | Free (open) + optional hosted |
| **Interoperability** | API if available | CLI + HTTP | ACP standard + CLI + Web UI |

### 4.4 Market Timing

- **ACP is new & growing:** JetBrains, Zed, Cursor are standardizing on it (as of 2025-2026)
- **Early mover advantage:** First OSS agent to expose a robust ACP server can capture mindshare
- **IDE vendors are hungry for flexibility:** Reducing agent lock-in is a key feature request
- **Enterprise AI adoption is accelerating:** Privacy + security + self-hosting are table-stakes

### 4.5 Go-to-Market Angles

1. **"Bring your agent to any IDE"** — Animus as the universal orchestrator
2. **"Enterprise-grade agent orchestration"** — Self-hosted, auditable, cost-transparent
3. **"Open-source agent workflow platform"** — Community-extensible, no vendor lock-in
4. **"Agent for teams that control their own data"** — Privacy-first, Git-integrated
5. **"Reduce agent switching costs"** — Use multiple agents; Animus coordinates them

---

## 5. Risks & Mitigations

### 5.1 Technical Risks

| Risk | Severity | Mitigation |
|---|---|---|
| **ACP spec maturity** | Medium | Monitor spec evolution; design for forward compatibility; keep ACP server modular for updates |
| **Editor integration complexity** | High | Start with VS Code (largest market); reuse existing ACP client libraries; invest in testing |
| **Session state coherence** | High | Leverage existing scoped runtime model; test concurrent sessions; clear semantics on conflict resolution |
| **Performance at scale** | Medium | Benchmark session throughput; optimize JSON parsing; consider connection pooling |
| **Dependency versioning** | Low | Pin ACP spec versions; test with multiple editor versions |

### 5.2 Market Risks

| Risk | Severity | Mitigation |
|---|---|---|
| **ACP adoption slower than expected** | Medium | Build ACP support as optional feature; maintain CLI plus Web UI as primary surfaces |
| **Tight editor integrations remain dominant** | Medium | Emphasize OSS, cost, and privacy advantages; build early examples and case studies |
| **Enterprise procurement friction** | Medium | Provide hosted SaaS option; offer support contracts; maintain audit/compliance docs |

### 5.3 Residual Concerns

- **ACP specification may continue to evolve** — Plan for periodic updates to Animus ACP server
- **IDE ecosystem is fragmented** — Each IDE (VS Code extensions, JetBrains plugins) has unique build/deploy processes
- **First IDE integration will set tone** — Prioritize quality and documentation for initial integrations
- **Session state debugging will be complex** — Invest in logging, tracing, and diagnostics

---

## 6. Success Metrics

- **Phase 1 completion:** Working ACP server that VS Code can connect to
- **Phase 2 completion:** End-to-end task execution (initialize → prompt → response → file edit) working from IDE
- **Phase 3 completion:** Permission-gated file and terminal operations tested
- **Adoption:** 100+ developers using Animus via IDE extensions within 6 months of launch
- **Enterprise wins:** 3+ enterprise customers citing "IDE + agent flexibility" as deciding factor

---

## 7. Next Steps

1. **Validate:** Confirm ACP spec gaps (remote auth, session migration) don't block Animus integration
2. **Prototype:** Spike `animus-acp-server` crate with basic `initialize` and `session/new` handlers
3. **IDE Integration:** Build VS Code extension POC connecting to local Animus server
4. **Gather Feedback:** Get early users testing and provide input on UX, performance, missing features
5. **Schedule Implementation:** Plan phased rollout with clear milestones and testing gates

---

## Appendix: References

- [Agent Client Protocol Official Site](https://agentclientprotocol.com/)
- [ACP GitHub Repository](https://github.com/agentclientprotocol/agent-client-protocol)
- [ACP Schema (JSON)](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/schema/schema.json)
- [JetBrains ACP Documentation](https://www.jetbrains.com/help/ai-assistant/acp.html)
- [Model Context Protocol (MCP) Spec](https://modelcontextprotocol.io/)

---

**Document Version:** 1.0  
**Last Updated:** April 2, 2026  
**Author:** Animus Development Team
