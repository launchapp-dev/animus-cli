# MCP Integration

## What MCP Is

MCP is AO's tool boundary. Agents and workflows use MCP to read and mutate
state, and packs can contribute additional MCP server descriptors without
teaching the daemon new behavior.

## AO's Core MCP Surface

AO ships an MCP server:

```bash
ao mcp serve
```

It exposes AO mutation and query tools such as:

- `ao.task.*`
- `ao.requirements.*`
- `ao.workflow.*`
- `ao.daemon.*`

Many of those tools are now conceptually owned by bundled first-party packs
such as `ao.task` and `ao.requirement`, even though they are exposed through
the AO MCP server.

## Pack-Owned MCP Descriptors

Packs can also ship MCP descriptors under pack assets. AO loads those
descriptors, namespaces the resulting server ids by pack id, and makes them
available to workflows and phases.

Examples:

- `ao.requirement/github-sync`
- `vendor.crm/runtime`

Pack-owned MCP behavior stays outside the daemon. The daemon only supervises the
processes and records execution facts.

## Workflow-Level Usage

Project YAML can reference MCP servers directly, and pack overlays can inject
phase bindings and default server sets.

Key rules:

- project YAML defines repo-specific MCP servers
- pack overlays can contribute namespaced MCP servers
- agents and phases only see explicitly allowed tools
- AO state mutations should go through MCP or CLI mutation surfaces, not direct
  file edits

## Why This Boundary Exists

Tool-driven mutation keeps AO auditable and composable:

- state changes flow through validated surfaces
- external integrations remain process-based
- packs can add behavior without changing daemon-core

See [Workflows](./workflows.md) and [How AO Works](./how-ao-works.md) for how
MCP fits into execution.
