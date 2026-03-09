# Configuration Reference

AO uses a layered configuration system with project-local files, user-global files, and environment variables.

---

## Project Configuration Files

All project configuration lives under the `.ao/` directory at the project root.

### .ao/config.json

Project-level configuration. Created during `ao setup`.

Contains project metadata, registered repository paths, and project-scoped settings.

### .ao/state/agent-runtime-config.v2.json

Agent runtime configuration. Defines agent profiles with model, tool, and system prompt settings.

```json
{
  "agents": {
    "default": {
      "model": "claude-sonnet-4-6",
      "tool": "claude",
      "system_prompt": "",
      "mcp_servers": ["ao"]
    },
    "po-reviewer": {
      "model": "claude-sonnet-4-6",
      "tool": "claude",
      "system_prompt": "You are a Product Owner..."
    }
  }
}
```

Key fields per agent profile:

| Field | Type | Description |
|---|---|---|
| `model` | string? | LLM model identifier (null = use compiled default) |
| `tool` | string? | CLI tool to invoke (null = use compiled default) |
| `system_prompt` | string | System prompt for the agent |
| `role` | string? | Role identifier |
| `mcp_servers` | string[] | MCP server names this agent can access |
| `skills` | string[] | Skill identifiers |
| `capabilities` | map\<string, bool\> | Capability flags |

Setting `model` and `tool` to `null` causes the compiled defaults from `protocol/src/model_routing.rs` to take over.

### .ao/state/workflow-config.v2.json

Compiled workflow configuration. Generated from `.ao/workflows/*.yaml` files via `ao workflow config compile`.

Contains:
- Phase definitions (execution mode, prompts, agent bindings)
- Workflow definitions (phase sequences, routing, post-success hooks)
- MCP server definitions
- Agent profiles (merged from YAML)
- Checkpoint retention settings
- Integration configs and schedules

### .ao/state/state-machines.v1.json

State machine definitions used by the workflow engine for phase transitions and verdict routing.

### .ao/resume-config.json

Daemon resume configuration. Stores state needed to resume the daemon after restart.

---

## Workflow Source Files

### .ao/workflows/*.yaml

YAML workflow source files. These are the human-editable definitions that get compiled into `workflow-config.v2.json`. Multiple YAML files are merged during compilation.

See [Workflow YAML Schema](workflow-yaml.md) for the full specification.

---

## Bundled Defaults

### crates/orchestrator-core/config/agent-runtime-config.v2.json

The bundled default agent runtime configuration shipped with the `ao` binary. Used as the base config when no project-level config exists.

### crates/protocol/src/model_routing.rs

Compiled default model routing. Defines `default_primary_model_for_phase()` which maps workflow phases to default models. These defaults are overridden by agent profile settings in the runtime config.

---

## User-Global Configuration

### ~/.config/agent-orchestrator/projects.json

Registry of all known AO projects. Updated when running `ao setup`, `ao project create`, or `ao git repo init`.

```json
{
  "projects": [
    {
      "id": "my-project",
      "path": "/Users/me/my-project",
      "created_at": "2026-01-15T10:30:00Z"
    }
  ]
}
```

---

## Environment Variables

| Variable | Description |
|---|---|
| `PROJECT_ROOT` | Override project root directory (alternative to `--project-root`) |
| `AO_CONFIG_DIR` | Override the AO configuration directory (default: `~/.ao` or project-local `.ao`) |
| `AO_RUNNER_CONFIG_DIR` | Override the runner configuration directory (falls back to `AO_CONFIG_DIR`) |
| `AO_ALLOW_NON_EDITING_PHASE_TOOL` | When `true`, allow non-write-capable tools (e.g., gemini) to handle any phase without fallback redirection |
| `AO_MCP_SCHEMA_DRAFT` | Set to `07`, `draft07`, `draft-07`, or `draft_07` to use JSON Schema Draft-07 for MCP tool input schemas |
| `CLAUDECODE` | When set to `1`, indicates running inside Claude Code. The `claude` CLI refuses to start in this context. Unset before daemon start if needed. |

---

## Configuration Precedence

Configuration is resolved in the following order (highest priority first):

1. **CLI flags** -- `--project-root`, `--model`, `--tool`, etc.
2. **Environment variables** -- `PROJECT_ROOT`, `AO_ALLOW_NON_EDITING_PHASE_TOOL`, etc.
3. **YAML workflow files** -- `.ao/workflows/*.yaml` (compiled into workflow-config)
4. **Project state files** -- `.ao/state/*.json`
5. **Bundled compiled defaults** -- Built into the `ao` binary

For agent model/tool selection specifically:

1. **Phase-level runtime override** -- `phase.runtime.model` / `phase.runtime.tool`
2. **Agent profile setting** -- `agents.<profile>.model` / `agents.<profile>.tool`
3. **Compiled defaults** -- `default_primary_model_for_phase()` in `model_routing.rs`

See also: [Data Layout](data-layout.md), [Workflow YAML Schema](workflow-yaml.md).
