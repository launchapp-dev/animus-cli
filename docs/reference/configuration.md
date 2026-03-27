# Configuration Reference

AO resolves behavior from project YAML, pack layers, scoped runtime state, and environment overrides.

## Project-Local Sources

### `.ao/config.json`

Repository-local AO configuration created during setup.

### `.ao/workflows.yaml` and `.ao/workflows/*.yaml`

These YAML files are the editable workflow source of truth for a project.

Typical uses:

- define repo-specific workflow ids such as `standard-workflow`
- wrap canonical bundled refs such as `ao.task/standard`
- declare project MCP servers, agents, variables, phases, and workflow definitions

### `.ao/plugins/<pack-id>/`

Project-local pack overrides. Use this when a repository needs to override installed or bundled pack content without changing AO globally.

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

## Bundled and Installed Sources

### Bundled workflow refs

Canonical bundled refs include:

- `ao.task/standard`
- `ao.task/quick-fix`
- `ao.task/triage`
- `ao.requirement/draft`
- `ao.requirement/refine`
- `ao.requirement/plan`
- `ao.requirement/execute`
- `ao.vision/draft`
- `ao.vision/refine`

Legacy `builtin/*` aliases remain supported where the loader still provides them.

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
6. bundled workflow and pack content embedded in AO

## Environment Variables

| Variable | Description |
|---|---|
| `AO_CONFIG_DIR` | Override the global AO config directory |
| `AO_RUNNER_CONFIG_DIR` | Override the runner config directory |
| `AO_MCP_SCHEMA_DRAFT` | Select Draft-07 MCP tool input schemas |
| `CLAUDECODE` | Signals an embedded Claude Code environment |

## Notes

- Project YAML is the authored workflow surface.
- Mutable runtime state lives under `~/.ao/<repo-scope>/`.
- The daemon schedules and supervises work; workflow and pack content still define behavior.
