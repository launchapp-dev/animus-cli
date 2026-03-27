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

Override the project root directory. When omitted, AO resolves the project root
from the current git common root when available, and otherwise falls back to the
current working directory. The CLI also honors `PROJECT_ROOT` as an environment
override, but `--project-root` wins when both are present.

```bash
ao task list --project-root /path/to/my-project
```

This flag is required when running AO commands from outside a project
directory, or when automating across multiple projects.

## Common Cross-Command Flags

Many destructive commands expose command-specific confirmation and preview flags
such as `--confirm`, `--confirmation-id`, and `--dry-run`. Check the relevant
command entry in the [CLI Command Surface](index.md) before scripting a
mutation.

### --input-json \<JSON\>

Pass structured input as a JSON string. Used by commands that accept complex input beyond simple flags.

```bash
ao task create --input-json '{"title": "Fix bug", "priority": "high", "tags": ["urgent"]}'
```

Many mutation and workflow commands accept `--input-json` as an alternative to
individual flags.
