# JSON Envelope Contract

All `ao` commands that accept `--json` wrap their output in the `ao.cli.v1` envelope. This contract provides a stable, machine-readable interface for scripts, CI pipelines, and MCP tool integrations.

## Schema Identifier

The schema field is always the string literal `"ao.cli.v1"`. This value is defined as a constant (`CLI_SCHEMA_ID`) in the protocol crate and is shared across all producers.

## Success Envelope

Written to **stdout**.

```json
{
  "schema": "ao.cli.v1",
  "ok": true,
  "data": <T>
}
```

| Field | Type | Description |
|---|---|---|
| `schema` | `string` | Always `"ao.cli.v1"` |
| `ok` | `boolean` | Always `true` for success |
| `data` | `T` | Command-specific payload (object, array, or simple value) |

The `data` field varies by command. For simple acknowledgments it may be `{"message": "ok"}`. For queries it contains the full result object or array.

### Examples

Simple acknowledgment:

```json
{
  "schema": "ao.cli.v1",
  "ok": true,
  "data": { "message": "task TASK-001 status updated to in-progress" }
}
```

Query result:

```json
{
  "schema": "ao.cli.v1",
  "ok": true,
  "data": {
    "id": "TASK-001",
    "title": "Fix login bug",
    "status": "in-progress",
    "priority": "high"
  }
}
```

## Error Envelope

Written to **stderr**.

```json
{
  "schema": "ao.cli.v1",
  "ok": false,
  "error": {
    "code": "<error_code>",
    "message": "<human-readable message>",
    "exit_code": <N>
  }
}
```

| Field | Type | Description |
|---|---|---|
| `schema` | `string` | Always `"ao.cli.v1"` |
| `ok` | `boolean` | Always `false` for errors |
| `error.code` | `string` | Machine-readable error category (see [Exit Codes](cli/exit-codes.md)) |
| `error.message` | `string` | Human-readable error description |
| `error.exit_code` | `integer` | Process exit code (1-5) |
| `error.details` | `object?` | Optional structured context (present only when available) |

### Error Codes

| code | exit_code | Meaning |
|---|---|---|
| `"internal"` | 1 | Unclassified or unexpected error |
| `"invalid_input"` | 2 | Bad arguments or values |
| `"not_found"` | 3 | Resource does not exist |
| `"conflict"` | 4 | State conflict |
| `"unavailable"` | 5 | Service unreachable |

### Error with Details

```json
{
  "schema": "ao.cli.v1",
  "ok": false,
  "error": {
    "code": "internal",
    "message": "daemon failed to start",
    "exit_code": 1,
    "details": {
      "startup_log_tail": "error: panic in scheduler loop"
    }
  }
}
```

The `details` field is omitted (not present) when no structured context is available.

## Versioning

The `schema` field enables future evolution. Consumers should check `schema == "ao.cli.v1"` before parsing. If the schema changes (e.g., `"ao.cli.v2"`), the envelope structure may differ.

Within `ao.cli.v1`, the envelope shape is stable:
- `ok`, `schema` are always present.
- Success always has `data`.
- Error always has `error` with `code`, `message`, and `exit_code`.

## Serialization

The JSON envelope is serialized in compact form (no pretty-printing, no newlines) to ensure single-line output suitable for line-oriented log processing.

## Usage with --json

```bash
# Capture success data
data=$(ao task get --id TASK-001 --json 2>/dev/null)
echo "$data" | jq '.data.status'

# Capture error
ao task get --id NONEXISTENT --json 2>err.json
cat err.json | jq '.error.code'
```

See also: [Exit Codes](cli/exit-codes.md), [Global Flags](cli/global-flags.md).
