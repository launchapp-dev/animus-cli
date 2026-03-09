# Crate Map

The AO workspace contains 16 crates organized into seven groups by responsibility.

## Foundation

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [protocol](../../crates/protocol/README.md) | Wire protocol types shared across all crates | Defines IPC messages, configuration schemas, scoped state paths, model routing defaults, and CLI JSON envelope contracts |

## Core

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-core](../../crates/orchestrator-core/README.md) | Domain logic and state management | ServiceHub trait, service API implementations, workflow state machines, task dispatch policy, execution projections |
| [orchestrator-config](../../crates/orchestrator-config/README.md) | Workflow and runtime configuration | YAML workflow parsing, workflow config compilation, variable expansion, phase plan resolution |
| [orchestrator-store](../../crates/orchestrator-store/README.md) | Persistence primitives | Atomic JSON writes (`write_json_atomic`), `read_json_or_default`, scoped project state directories |

## Runtime

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-daemon-runtime](../../crates/orchestrator-daemon-runtime/README.md) | Daemon tick loop and dispatch engine | Project tick execution, dispatch queue management, process manager for workflow-runner subprocesses, schedule evaluation, completion reconciliation |
| [workflow-runner](../../crates/workflow-runner/README.md) | Standalone workflow execution binary | Phase execution loop, runtime contract construction, IPC client to agent-runner, phase failover classification, merge recovery |
| [agent-runner](../../crates/agent-runner/README.md) | Standalone daemon managing LLM CLI processes | IPC server (Unix socket / TCP), token-based auth, output parsing (tool calls, artifacts, thinking blocks), environment sanitization, provider abstraction |

## CLI

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-cli](../../crates/orchestrator-cli/README.md) | Main `ao` binary (clap-based CLI) | CLI dispatch for 24+ top-level commands, JSON envelope output, MCP server (via rmcp), TUI, error classification |

## Web

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-web-api](../../crates/orchestrator-web-api/README.md) | Web API business logic | WebApiService bridging HTTP requests to core service APIs |
| [orchestrator-web-contracts](../../crates/orchestrator-web-contracts/README.md) | Shared web types | Request/response types shared between web-api and web-server |
| [orchestrator-web-server](../../crates/orchestrator-web-server/README.md) | Axum web server with embedded static assets | HTTP routing, GraphQL (optional feature), static asset serving, compression |

## Integration

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-providers](../../crates/orchestrator-providers/README.md) | External provider integrations | Git provider abstraction, task/requirements/subject providers; optional Jira, Linear, and GitLab features behind feature flags |
| [orchestrator-notifications](../../crates/orchestrator-notifications/README.md) | Notification delivery | Webhook-based notifications for daemon and workflow events |
| [orchestrator-git-ops](../../crates/orchestrator-git-ops/README.md) | Git workflow operations | Branch management, merge operations, PR creation, merge conflict handling for workflow post-success actions |

## LLM

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [llm-cli-wrapper](../../crates/llm-cli-wrapper/README.md) | Abstraction over AI CLI tools | Unified interface for spawning and interacting with claude, codex, gemini, opencode CLI processes |
| [oai-runner](../../crates/oai-runner/README.md) | OpenAI-compatible streaming API client | Direct API streaming for OpenAI-compatible endpoints, MCP client integration via rmcp |
