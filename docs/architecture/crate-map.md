# Crate Map

The AO workspace contains 17 crates organized by responsibility.

## Foundation

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [protocol](../../crates/protocol/README.md) | Shared wire and config types | IPC messages, config schemas, scoped runtime paths, CLI JSON envelopes |

## Core

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-core](../../crates/orchestrator-core/README.md) | Domain logic and services | `ServiceHub`, service APIs, workflow lifecycle, task and requirement state mutation |
| [orchestrator-config](../../crates/orchestrator-config/README.md) | Workflow and runtime configuration | Workflow YAML parsing, pack loading, scaffolding, phase plan resolution |
| [orchestrator-store](../../crates/orchestrator-store/README.md) | Persistence primitives | Atomic JSON writes, repo-scoped state directory helpers |
| `orchestrator-logging` | Shared logging support | Structured logging helpers and runtime log plumbing |

## Runtime

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-daemon-runtime](../../crates/orchestrator-daemon-runtime/README.md) | Daemon scheduling runtime | Queue execution, reactive dispatch, subprocess supervision |
| `workflow-runner-v2` | Workflow execution runtime | Phase execution, state-machine transitions, checkpoint persistence |
| [agent-runner](../../crates/agent-runner/README.md) | Agent process runner | IPC server, AI CLI execution, output parsing |

## CLI

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-cli](../../crates/orchestrator-cli/README.md) | Main `ao` binary | Clap command surface, JSON output, MCP server, operational commands |

## Web

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-web-api](../../crates/orchestrator-web-api/README.md) | Web API business logic | HTTP-facing orchestration services |
| [orchestrator-web-contracts](../../crates/orchestrator-web-contracts/README.md) | Shared web contracts | Request and response types shared between web layers |
| [orchestrator-web-server](../../crates/orchestrator-web-server/README.md) | Axum web server | HTTP routing, embedded UI delivery, browser entrypoint |

## Integration

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [orchestrator-providers](../../crates/orchestrator-providers/README.md) | Provider integrations | Built-in task, requirement, subject, and git adapters |
| [orchestrator-notifications](../../crates/orchestrator-notifications/README.md) | Notification delivery | Webhook and runtime notification support |
| [orchestrator-git-ops](../../crates/orchestrator-git-ops/README.md) | Git automation | Branching, worktree, merge, and PR helper operations |

## Model and Runner Adapters

| Crate | Description | Key Responsibility |
|-------|-------------|-------------------|
| [llm-cli-wrapper](../../crates/llm-cli-wrapper/README.md) | AI CLI abstraction layer | Claude, Codex, Gemini, and related CLI integration |
| [oai-runner](../../crates/oai-runner/README.md) | OpenAI-compatible runner | Streaming API execution for OpenAI-compatible endpoints |
