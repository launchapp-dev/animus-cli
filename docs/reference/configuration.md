# Configuration Reference

AO resolves behavior from project YAML, pack layers, bundled defaults, and
environment overrides.

## Project-Local Sources

### `.ao/workflows.yaml` and `.ao/workflows/*.yaml`

These YAML files are the editable workflow source of truth for a project.
Typical uses:

- define repo-specific workflow ids such as `standard-workflow`
- override bundled workflow metadata
- compose pack-owned workflow refs such as `ao.task/standard`
- add project MCP servers, tools, and phase definitions

### `.ao/plugins/<pack-id>/`

Project-local pack overrides. Use this directory when a repository needs to
override an installed or bundled pack without changing AO globally.

### `.ao/state/pack-selection.v1.json`

Project pack selection and pinning state, managed by `ao pack ...`.

### `.ao/config.json`

Project configuration and registry-scoped metadata created during setup.

## Bundled and Installed Sources

### Bundled kernel workflows

Canonical bundled kernel refs currently include:

- `ao.vision/draft`
- `ao.vision/refine`

Legacy `builtin/*` aliases remain supported for compatibility.

### Bundled first-party packs

Bundled pack manifests live under:

- `crates/orchestrator-config/config/bundled-packs/`

Current first-party packs include:

- `ao.task`
- `ao.requirement`
- `ao.review`

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
2. environment variables
3. project pack overrides in `.ao/plugins/`
4. project YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`
5. machine-installed packs in `~/.ao/packs/`
6. bundled kernel workflows and bundled first-party packs

## Environment Variables

| Variable | Description |
|---|---|
| `PROJECT_ROOT` | Override project root directory |
| `AO_CONFIG_DIR` | Override the global AO config directory |
| `AO_RUNNER_CONFIG_DIR` | Override the runner config directory |
| `AO_ALLOW_NON_EDITING_PHASE_TOOL` | Allow non-write-capable tools to execute any phase without fallback |
| `AO_MCP_SCHEMA_DRAFT` | Select Draft-07 MCP tool input schemas |
| `CLAUDECODE` | Signals an embedded Claude Code environment; unset before daemon start if needed |

## Notes

- Project YAML is the authored workflow surface.
- Packs own reusable domain behavior.
- The daemon never becomes the place where pack or subject policy is encoded.
