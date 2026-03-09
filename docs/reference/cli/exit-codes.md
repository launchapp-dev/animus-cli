# Exit Codes

AO uses a fixed set of exit codes to classify errors. These codes are stable across versions and safe to match in scripts and CI pipelines.

## Exit Code Table

| Code | Name | Description |
|---|---|---|
| 0 | success | Command completed successfully |
| 1 | internal | Unclassified or unexpected error (panics, serialization failures, unhandled conditions) |
| 2 | invalid_input | Invalid arguments, flags, or values supplied by the caller |
| 3 | not_found | Requested resource does not exist (task, workflow, file, etc.) |
| 4 | conflict | Operation conflicts with current state (duplicate resource, concurrent modification) |
| 5 | unavailable | Service or dependency is unreachable (daemon down, runner not responding, connection refused) |

## Error Classification

Errors are classified using a typed error system. The CLI wraps errors in a `CliError` struct carrying a `CliErrorKind` variant:

| CliErrorKind | code string | exit_code |
|---|---|---|
| `InvalidInput` | `"invalid_input"` | 2 |
| `NotFound` | `"not_found"` | 3 |
| `Conflict` | `"conflict"` | 4 |
| `Unavailable` | `"unavailable"` | 5 |
| `Internal` | `"internal"` | 1 |

The classifier walks the anyhow error chain looking for:

1. **Typed `CliError`** -- If any error in the chain is a `CliError`, its kind determines the exit code.
2. **`std::io::Error` kinds** -- `NotFound` maps to exit code 3. Connection-related IO errors (`ConnectionRefused`, `TimedOut`, `BrokenPipe`, etc.) map to exit code 5.
3. **Fallback** -- Any unrecognized error defaults to exit code 1 (`internal`).

Classification is purely type-based. Error messages are never string-matched to determine exit codes.

## JSON Error Envelope

When `--json` is active, errors are emitted to stderr as a JSON envelope:

```json
{
  "schema": "ao.cli.v1",
  "ok": false,
  "error": {
    "code": "not_found",
    "message": "task not found: TASK-999",
    "exit_code": 3
  }
}
```

The `error` object may include an optional `details` field with structured context:

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

## Usage in Scripts

```bash
ao task get --id TASK-001 --json 2>/dev/null
case $? in
  0) echo "success" ;;
  2) echo "invalid input" ;;
  3) echo "not found" ;;
  5) echo "service unavailable" ;;
  *) echo "unexpected error" ;;
esac
```

## Human-Readable Errors

Without `--json`, errors print to stderr as plain text. For `invalid_input` errors, a hint is appended suggesting `--help`:

```
error: invalid priority '<empty>'; expected one of: critical|high|medium|low
hint: run with --help to view accepted arguments and values
```

See also: [JSON Envelope Contract](../json-envelope.md), [Global Flags](global-flags.md).
