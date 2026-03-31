# Configuration Reference

AO resolves behavior from project YAML, installed pack layers, scoped runtime state, and environment overrides.

## Project-Local Sources

### `.ao/config.json`

Repository-local AO configuration created during setup.

Project-local `mcp_servers` entries support both stdio servers and remote
streamable HTTP endpoints. Legacy stdio entries remain valid.

Example:

```json
{
  "mcp_servers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem"],
      "assign_to": ["swe"]
    },
    "docs": {
      "url": "https://docs.example/mcp",
      "auth_token": "Bearer <token>",
      "assign_to": ["researcher"]
    }
  }
}
```

### `.ao/workflows.yaml` and `.ao/workflows/*.yaml`

These YAML files are the editable workflow source of truth for a project.

Typical uses:

- define repo-specific workflow ids such as `standard-workflow`
- define the repository's default workflow explicitly
- declare project MCP servers, agents, variables, phases, and workflow definitions

### `.ao/plugins/<pack-id>/`

Project-local pack overrides. Use this when a repository needs to override installed pack content without changing AO globally.

## Repo-Scoped Runtime Config

AO stores mutable project runtime config under `~/.ao/<repo-scope>/`.

Key files:

- `config/state-machines.v1.json`
- `state/pack-selection.v1.json`
- `daemon/pm-config.json`
- `resume-config.json`

These files are AO-managed state. Treat them as runtime data, not hand-authored config.

## Global User Config

### `~/.ao/config.json`

The global AO config stores machine-local user settings such as:

- agent runner auth token
- user-defined MCP server entries
- Claude profile launch environments

Use `AO_CONFIG_DIR` to override the global config root in tests or custom environments.

Example:

```json
{
  "claude_profiles": {
    "main": {
      "env": {
        "CLAUDE_CONFIG_DIR": "/Users/alice/.claude-main"
      }
    }
  }
}
```

## Installed Sources

### Machine-installed packs

Installed packs live at:

```text
~/.ao/packs/<pack-id>/<version>/
```

Manage them with:

```bash
ao pack list
ao pack inspect --pack-id ao.task
ao pack install --path /tmp/vendor.pack --activate
ao pack pin --pack-id vendor.pack --version =1.2.3
```

## Configuration Precedence

Behavior resolves in this order:

1. CLI flags
2. supported environment variables
3. project pack overrides in `.ao/plugins/`
4. project YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`
5. installed packs in `~/.ao/packs/`

## Environment Variables

| Variable | Description |
|---|---|
| `AO_CONFIG_DIR` | Override the global AO config directory |
| `AO_RUNNER_CONFIG_DIR` | Override the runner config directory |
| `AO_MCP_SCHEMA_DRAFT` | Select Draft-07 MCP tool input schemas |
| `CLAUDECODE` | Signals an embedded Claude Code environment |

## Notes

- Project YAML is the authored workflow surface.
- AO no longer ships bundled workflows; a project must author workflows locally or install a pack.
- Mutable runtime state lives under `~/.ao/<repo-scope>/`.
- The daemon schedules and supervises work; workflow and pack content still define behavior.
