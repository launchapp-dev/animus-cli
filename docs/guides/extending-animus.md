# Extending Animus

Three extension points exist in Animus: **skills**, **packs**, and **plugins**.
They serve different needs. This guide helps you pick the right one.

## Decision tree

```
What are you trying to add?
│
├─ A prompt or behavior overlay for an existing agent?
│   └─ Skill  (SKILL.md or YAML skill definition)
│
├─ A versioned bundle of workflows + skills for a domain?
│   └─ Pack  (pack.toml + workflow YAML + skills, published via git tag)
│
└─ An executable capability the daemon or CLI needs to call?
    └─ Plugin  (stdio JSON-RPC binary, installed via `animus plugin install`)
        │
        ├─ Subject backend (task tracker, issue board, …)
        ├─ Provider (LLM CLI adapter: claude, codex, gemini, opencode)
        ├─ Trigger backend (file watcher, webhook, Slack, …)
        ├─ Transport backend (HTTP/GraphQL for the web UI)
        └─ Log storage, notifier, …
```

## Comparison matrix

| | **Skill** | **Pack** | **Plugin** |
|---|---|---|---|
| **Definition format** | `SKILL.md` (Markdown) or `skill_definition.yaml` | `pack.toml` + workflow YAML + optional skills | Standalone binary (any language) with stdio JSON-RPC contract |
| **Install** | `animus skill install` (or drop in `.animus/skills/`) | `animus pack install` (local path or marketplace registry) | `animus plugin install` (local path or URL + sha256) |
| **Scope** | Project, user, or agent-host (Claude Code / Codex) | Machine-wide (`~/.animus/packs/<id>/<version>/`); activated per-project | Machine-wide (`~/.animus/plugins/<name>`); discovered at daemon start |
| **Runtime binding** | Injected into agent system prompt at phase dispatch time | Resolved via pack registry; workflow refs resolved to pack content | Spawned as a child process by the plugin host; communicates over stdio |

## Skills

Skills are prompt and behavior overlays injected into an agent session before it runs. They do not ship executable code.

Use a skill when you want to:

- Give an agent a coding style guide or domain vocabulary.
- Encode a repeatable multi-step reasoning pattern.
- Surface project-specific conventions without shipping a full workflow.

**Deep doc:** [Skill System Architecture](../architecture/skill-system.md)

## Packs

Packs bundle workflows and skills into a versioned, distributable unit. They are the right choice for domain-specific automation (task management, code review, requirements, etc.) that other teams can adopt.

Use a pack when you want to:

- Ship a cohesive set of workflows + skills as a single installable artifact.
- Publish domain automation to a marketplace registry for others to install.
- Pin a specific version of workflow definitions across machines.

Packs are installed from a git URL or a local path. Marketplace packs are discovered via `animus pack search` once a registry is added with `animus pack registry add`.

**Deep doc:** [Plugin, Pack & Kernel Architecture](../architecture/plugin-pack-kernel.md) · [Pack Contract reference](../architecture/plugin-pack-kernel.md#plugin-pack-contract)

## Plugins

Plugins are standalone binaries that communicate with the Animus daemon (or CLI) over a stdio JSON-RPC contract. They extend the kernel with executable capabilities: subject backends, providers, transports, triggers, and more.

Use a plugin when you want to:

- Connect Animus to a new task tracker (Linear, Jira, SQLite, …).
- Add a new LLM provider CLI adapter.
- Implement a custom trigger (file watcher, GitHub webhook, Slack event, …).
- Ship a transport or web UI backend.

**Deep doc:** [Plugin Author Guide](plugin-author-guide.md) · [Plugin System Architecture](../architecture/plugin-system.md)

## Quick reference

```sh
# Skills
animus skill list
animus skill install --path ./my-skill/
animus skill show my-skill

# Packs
animus pack list
animus pack install --path ./my-pack/
animus pack registry add --id audiogenius --url https://github.com/example/pack-registry
animus pack search --query "code review"

# Plugins
animus plugin list
animus plugin install --path ./my-plugin/bin/my-plugin
animus plugin install-defaults          # install all recommended first-party plugins
animus plugin status
```
