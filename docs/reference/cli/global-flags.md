# Global Flags

Flags available on every `ao` command.

## --json

Enables machine-readable JSON output using the [`ao.cli.v1` envelope](../json-envelope.md).

- Success responses are written to **stdout**.
- Error responses are written to **stderr**.
- The envelope schema is always `"ao.cli.v1"`.

```bash
ao task list --json
```

```json
{
  "schema": "ao.cli.v1",
  "ok": true,
  "data": [ ... ]
}
```

Without `--json`, commands produce human-readable text. With `--json`, every command wraps its output in the envelope contract, making it safe to parse programmatically.

## --project-root \<PATH\>

Override the project root directory. By default, AO infers the project root from the current working directory by walking up to the nearest `.ao/` directory or git repository root.

```bash
ao task list --project-root /path/to/my-project
```

This flag is required when running AO commands from outside a project directory, or when automating across multiple projects.

## PROJECT_ROOT Environment Variable

Alternative to `--project-root`. When set, AO uses this value as the project root. The `--project-root` flag takes precedence over the environment variable.

```bash
export PROJECT_ROOT=/path/to/my-project
ao task list
```

In scripts and automation, always set `--project-root "$(pwd)"` or `PROJECT_ROOT` explicitly to avoid ambiguity.

## Common Cross-Command Flags

These flags appear on many (but not all) commands:

### --confirmation \<yes|no\>

Required on destructive commands. Confirms the operation without interactive prompts.

```bash
ao task delete --id TASK-001 --confirmation yes
```

Commands that require confirmation: `task delete`, `task cancel`, `workflow pause`, `workflow cancel`, `git worktree remove`, and others.

When using `--json`, pass `--confirmation yes` to avoid interactive prompts that would break JSON parsing.

### --dry-run

Preview what a destructive command would do without executing it. Returns a dry-run envelope describing the planned effects.

```bash
ao task delete --id TASK-001 --dry-run
```

The dry-run response includes:

| Field | Description |
|---|---|
| `operation` | The operation name |
| `target` | The affected resource |
| `action` | What would happen |
| `dry_run` | Always `true` |
| `destructive` | Whether the operation is destructive |
| `requires_confirmation` | Whether `--confirmation` is needed |
| `planned_effects` | List of side effects |
| `next_step` | Hint for how to execute for real |

### --input-json \<JSON\>

Pass structured input as a JSON string. Used by commands that accept complex input beyond simple flags.

```bash
ao task create --input-json '{"title": "Fix bug", "priority": "high", "tags": ["urgent"]}'
```

Approximately 15+ commands accept `--input-json` as an alternative to individual flags.
