# Troubleshooting Guide

Common issues and their fixes when working with AO.

## Environment Diagnostics

Run the built-in doctor command first:

```bash
ao doctor
```

This checks for required tools, API keys, configuration files, and common misconfigurations. Use `--fix` to attempt automatic repairs:

```bash
ao doctor --fix
```

## Daemon Won't Start

**Symptoms**: `ao daemon start --autonomous` exits immediately or `ao daemon status` shows not running.

**Steps**:

1. Check daemon status:
   ```bash
   ao daemon status
   ```

2. Read the daemon log:
   ```bash
   ao daemon logs
   ```

3. Try running in foreground for immediate error output:
   ```bash
   ao daemon run
   ```

4. Check if another daemon instance is already running (port conflict).

## CLAUDECODE Environment Variable Blocking claude CLI

**Symptoms**: Inside a Claude Code session, the daemon fails to start agents with an error like "Cannot be launched inside another Claude Code session".

**Cause**: Claude Code sets `CLAUDECODE=1` in the environment. The `claude` CLI refuses to run when it detects this variable.

**Fix**: Unset the variable before starting the daemon:

```bash
unset CLAUDECODE
ao daemon start --autonomous
```

Or use an env prefix:

```bash
env -u CLAUDECODE ao daemon start --autonomous
```

## Agent Runtime Config Overriding Compiled Defaults

**Symptoms**: The daemon uses unexpected models. For example, all phases use the same model instead of routing research to gemini.

**Cause**: The resolved agent runtime has explicit `model` and `tool` fields that override the compiled routing table. Those values can come from authored workflow YAML or from `ao workflow agent-runtime set`.

**Fix**: Inspect the resolved runtime first:

```bash
ao workflow agent-runtime get    # Inspect current config
```

Then either remove the YAML override from `.ao/workflows.yaml` / `.ao/workflows/*.yaml`, or replace the compiled runtime with explicit `null` values:

```bash
ao workflow agent-runtime set --input-json '{"agents":{"default":{"model":null,"tool":null}}}'
```

Or read the config cascade documentation in the [Model Routing Guide](model-routing.md).

## Paused / Ghost Task State

**Symptoms**: A task shows as blocked or paused but should be ready. The daemon skips it.

**Cause**: When tasks are blocked, `paused=true` is set internally. The daemon skips paused tasks during scheduling. Direct JSON edits or incomplete status transitions can leave ghost state.

**Fix**: Always reset via `ao task status` which clears all blocking metadata:

```bash
ao task status --id TASK-XXX --status ready
```

This clears `paused`, `blocked_at`, `blocked_reason`, and `blocked_by`. Never hand-edit AO-managed runtime JSON or SQLite state.

## Runner Connection Issues

**Symptoms**: Workflows fail with runner connection errors. Agents do not start.

**Steps**:

1. Check runner health:
   ```bash
   ao runner health
   ```

2. Detect orphaned runner processes:
   ```bash
   ao runner orphans detect
   ```

3. Clean up orphans if found:
   ```bash
   ao runner orphans cleanup
   ```

4. Check restart statistics:
   ```bash
   ao runner restart-stats
   ```

5. Verify API keys are set:
   ```bash
   ao model status
   ```

## Build Cache Stale

**Symptoms**: After editing protocol types or model routing, `cargo build` does not pick up changes.

**Cause**: Cargo's incremental compilation may not detect changes in certain files, particularly in the `protocol` crate.

**Fix**: Touch the changed file to force recompilation:

```bash
touch crates/protocol/src/model_routing.rs
cargo build -p orchestrator-cli
```

## Daemon Log Location

Use AO to inspect or clear daemon logs:

```bash
ao daemon logs
ao daemon clear-logs
```

AO stores runtime state under `~/.ao/<repo-scope>/`, and log plumbing is managed by the runtime binaries rather than a project-local `.ao/daemon.log` contract.

## Workflow Stuck or Failed

**Steps**:

1. List workflows to find the problematic one:
   ```bash
   ao workflow list
   ```

2. Inspect the workflow:
   ```bash
   ao workflow get --id WF-001
   ```

3. Check decisions made during execution:
   ```bash
   ao workflow decisions --id WF-001
   ```

4. View the agent output:
   ```bash
   ao output run --id RUN-001
   ```

5. If the workflow is stuck, you can cancel and retry:
   ```bash
   ao workflow cancel --id WF-001
   ao task status --id TASK-XXX --status ready
   ```

## Missing API Keys

**Symptoms**: Agents fail to start with authentication errors.

**Check**:

```bash
ao model status
```

Required keys by tool:

| Tool | Environment Variable |
|------|---------------------|
| `claude` | `ANTHROPIC_API_KEY` |
| `codex` | `OPENAI_API_KEY` |
| `gemini` | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| `oai-runner` | `MINIMAX_API_KEY`, `ZAI_API_KEY`, or `OPENAI_API_KEY` |

## Gemini Redirected on Write Phases

**Symptoms**: Research phases that use gemini get redirected to claude even though they do not need write access.

**Cause**: `enforce_write_capable_phase_target` redirects non-write-capable tools by default.

**Fix**: route read-only phases to Gemini in workflow YAML, and use a
write-capable tool such as `claude`, `codex`, or `oai-runner` for
implementation phases that modify the repository.
