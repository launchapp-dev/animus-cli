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

## --no-cache

(v0.5.9) Bypass hot-path read caches for this invocation.

Animus currently wires `--no-cache` into three hot-path reads for performance:

- **CI status cache** — file at `~/.animus/<repo-scope>/cache/ci-status.json`,
  default TTL 60s. Read by `animus status` to avoid re-running `gh run list`.
- **Workflow YAML compile cache** — file at
  `~/.animus/<repo-scope>/cache/workflow-config.compiled.v1.json`. Keyed by a
  hash of source file paths + mtimes + content; auto-invalidates when any
  source changes. Bypassed automatically when sources reference env vars
  (`${VAR}`), secrets (`${secret.NAME}`), or `system_prompt_file:` paths.
- **Daemon health snapshot** — in-process memory cache with a 1s freshness
  window, only relevant when one CLI invocation makes multiple internal calls.

`--no-cache` skips the read step on all of the above for the current
invocation. The on-disk caches stay in place; the next call without the flag
will use them as normal. The flag is invocation-scoped: it sets process-global
toggles during CLI startup so downstream library code does not have to thread a
cache-bypass parameter through every call site.

Per-cache environment overrides:

- `ANIMUS_DISABLE_CI_CACHE=1` — disable the CI status cache (read + write).
- `ANIMUS_CI_CACHE_TTL_SECS=<n>` — override the 60s default TTL.
- `ANIMUS_DISABLE_WORKFLOW_CACHE=1` — disable the workflow compile cache
  (read + write).

Related but separate cache controls:

- `ANIMUS_DISABLE_PROJECT_ROOT_CACHE=1` disables the project-root resolver's
  process-local cache. This is **not** currently toggled by `--no-cache`.
- `ANIMUS_DISABLE_MANIFEST_CACHE=1` disables the plugin-host manifest cache.
  This is also separate from the global CLI `--no-cache` flag.

All caches are best-effort. Any deserialize, I/O, hash, or schema mismatch
silently falls through to a fresh live read so a corrupt cache file never
masks a real source change.

```bash
animus --no-cache status --json
animus --no-cache workflow config get
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
