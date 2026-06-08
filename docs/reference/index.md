# Reference Documentation

Formal specifications and exhaustive listings for the Animus CLI.

## Pages

| Page | Description |
|---|---|
| [Feature Status](feature-status.md) | Shipped, in-flight, and planned features with status legend |
| [CLI Command Surface](cli/index.md) | Complete command tree with all subcommands and flags |
| [Global Flags](cli/global-flags.md) | Flags available on every command |
| [Exit Codes](cli/exit-codes.md) | Process exit codes and error classification |
| [JSON Envelope Contract](json-envelope.md) | The `animus.cli.v1` success/error envelope schema |
| [Workflow YAML Schema](workflow-yaml.md) | Full specification of `.animus/workflows/*.yaml` files |
| [MCP Tools](mcp-tools.md) | All MCP tools exposed by `animus mcp serve` |
| [Configuration](configuration.md) | Config files, environment variables, and precedence |
| [Secrets](secrets.md) | Project-scoped OS keychain storage, CLI flows, and precedence |
| [Chat](chat.md) | Multi-turn `animus chat` conversations and the provider-owned continuity model |
| [Data Layout](data-layout.md) | Project-local `.animus/` config plus repo-scoped runtime state |
| [Status Values & Enums](status-values.md) | All accepted enum values across the system |
| [Security](security.md) | Plugin signature trust model, lockfile policy, kill switches, and multi-tenant + RBAC roadmap |
