# Global Flags

Flags available on every `animus` command.

## --json

Enables machine-readable JSON output using the [`animus.cli.v1` envelope](../json-envelope.md).

- Success responses are written to **stdout**.
- Error responses are written to **stderr**.
- The envelope schema is always `"animus.cli.v1"`.

```bash
animus subject list --kind task --json
```

```json
{
  "schema": "animus.cli.v1",
  "ok": true,
  "data": [ ... ]
}
```

Without `--json`, commands produce human-readable text. With `--json`, every command wraps its output in the envelope contract, making it safe to parse programmatically.

## --project-root \<PATH\>

Override the project root directory. When omitted, Animus resolves the project
root from the current git common root when available, and otherwise falls back
to the current working directory.

```bash
animus subject list --kind task --project-root /path/to/my-project
```

This flag is required when running Animus commands from outside a project
directory, or when automating across multiple projects.

## --as \<PRINCIPAL\>

Impersonate a declared principal id for the current CLI invocation. This is an
honor-system override carried to the daemon over the local control socket and
logged loudly.

- Under `policy.rbac=single-user`, the daemon accepts the override.
- Under `policy.rbac=enforce`, the daemon rejects undeclared principals and
  impersonation attempts that peer credentials do not permit.
- `animus auth whoami --as <id>` shows the effective principal the daemon would
  resolve for the same invocation.

```bash
animus --as release-bot auth whoami
```

## Common Cross-Command Flags

Many destructive commands expose command-specific confirmation and preview flags
such as `--confirm`, `--confirmation-id`, and `--dry-run`. Check the relevant
command entry in the [CLI Command Surface](index.md) before scripting a
mutation.

### --input-json \<JSON\>

Pass structured input as a JSON string. Used by commands that accept complex input beyond simple flags.

```bash
animus workflow run --input-json '{"task_id":"TASK-001","workflow_ref":"standard-workflow"}'
```

Many mutation and workflow commands accept `--input-json` as an alternative to
individual flags.
