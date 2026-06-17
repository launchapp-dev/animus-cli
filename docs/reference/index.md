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
| [MCP OAuth](mcp-oauth.md) | `authorization_code` and broker-backed M2M OAuth flows for MCP servers |
| [Harness Hooks](harness-hooks.md) | Wiring a provider CLI's native harness hook mechanism |
| [Configuration](configuration.md) | Config files, environment variables, and precedence |
| [Secrets](secrets.md) | Project-scoped OS keychain storage, CLI flows, and precedence |
| [Chat](chat.md) | Multi-turn `animus chat` conversations and the provider-owned continuity model |
| [Data Layout](data-layout.md) | Project-local `.animus/` config plus repo-scoped runtime state |
| [Plugin Scope](plugin-scope.md) | The `.animus/plugin-scope.yaml` plugin allow/deny model |
| [Status Values & Enums](status-values.md) | All accepted enum values across the system |
| [Observability](observability.md) | Daemon metrics and structured logging surface |
| [Self-update](self-update.md) | The binary self-update path used by `animus update` |
| [Security](security.md) | Plugin signature trust model, lockfile policy, kill switches, and multi-tenant + RBAC roadmap |
