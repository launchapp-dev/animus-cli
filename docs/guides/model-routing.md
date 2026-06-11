# Model Routing Guide

Animus selects a `(tool, model)` pair for each agent phase from the resolved
agent runtime config plus any workflow or pack overrides. The current planner
logic lives in:

- `crates/animus-runtime-shared/src/config_context.rs`
- `crates/protocol/src/model_routing.rs`
- `crates/orchestrator-config/src/agent_runtime_config.rs`
- `.animus/workflows.yaml` or `.animus/workflows/*.yaml`

## How Primary Target Resolution Works

The phase target planner resolves the primary execution target in this order:

1. Per-phase override in `phase_routing.per_phase.<PHASE_KEY>`
2. Phase-family override for UI/UX phases via `phase_routing.ui_ux_*`
3. Phase-family override for research phases via `phase_routing.research_*`
4. Global override via `phase_routing.global_*`
5. Built-in fallback: `claude-sonnet-4-6`

Tool resolution follows the same precedence. If no tool is explicitly set,
Animus derives it from the chosen model id.

## Current Built-In Defaults

The built-in fallback compiled from
`crates/orchestrator-config/config/agent-runtime-config.v2.json` currently
leaves `phase_routing` unset, so there is no built-in complexity table in the
live code path.

That means:

- The generic built-in primary model fallback is `claude-sonnet-4-6`.
- Bundled packs and project workflow overlays provide the phase-specific
  defaults you actually see in task, requirement, and review workflows.
- Task, requirement, and review pack overlays currently pin their runtime
  phases to `claude-sonnet-4-6` unless you override them.
- The editable project source of truth is workflow YAML, not a repo-scoped
  `agent-runtime-config.v2.json` file.

## Config Cascade

The first matching source wins:

1. Workflow or pack phase runtime override authored in `.animus/workflows.yaml`
   or `.animus/workflows/*.yaml`
2. Resolved agent runtime config (`animus workflow agent-runtime get`)
3. Built-in fallback in the phase target planner

Set the model directly on an agent or phase in workflow YAML:

```yaml
agents:
  my-agent:
    model: claude-opus-4-6
    tool: claude
```

Or inspect the resolved runtime:

```bash
animus workflow agent-runtime get
animus workflow agent-runtime validate
```

If you prefer to replace the runtime as structured JSON, `animus workflow
agent-runtime set` accepts the compiled schema directly and writes it back into
project workflow YAML:

```json
{
  "agents": {
    "default": {
      "model": "claude-sonnet-4-6",
      "tool": "claude"
    }
  }
}
```

Set these fields to `null` to fall back to the planner defaults:

```json
{
  "agents": {
    "default": {
      "model": null,
      "tool": null
    }
  }
}
```

## Fallback Models

Fallback targets are assembled in this order:

1. The primary `(tool, model)` pair
2. Explicit `fallback_models` from the active phase runtime
3. `phase_routing.per_phase.<PHASE_KEY>.fallback_models`
4. `phase_routing.ui_ux_fallback_models` for UI/UX phases
5. `phase_routing.research_fallback_models` for research phases
6. `phase_routing.global_fallback_models`

Every fallback model is normalized through `canonical_model_id()`, deduplicated,
and then mapped to a tool automatically unless you provide explicit
`fallback_tools`.

Example:

```yaml
agents:
  swe:
    model: claude-sonnet-4-6
    fallback_models:
      - gpt-5.4
      - gemini-2.5-pro
```

## Complexity Note

Task complexity is still tracked and passed through workflow execution, but the
current phase target planner does not choose different primary models based on
low/medium/high complexity. Older docs that showed a compiled complexity routing
table no longer reflect the live code.

## Tool Assignment

Tool inference comes from `tool_for_model_id()` in
`crates/protocol/src/model_routing.rs`:

| Model family | CLI tool |
|-------------|----------|
| `claude-*` | `claude` |
| `gpt-*` | `codex` |
| `gemini-*` | `gemini` |
| `zai-*`, `glm-*`, `minimax-*`, `openrouter/*` | `oai-runner` |
| `deepseek-*`, `qwen-*`, `opencode*` | `opencode` |

You can inspect the tool defaults directly in code:

- `claude` -> `claude-sonnet-4-6`
- `codex` -> `gpt-5.4`
- `gemini` -> `gemini-2.5-pro`
- `opencode` -> `zai-coding-plan/glm-5`
- `oai-runner` -> `openrouter/minimax/minimax-m2.7`

## Write-Capable Tools

The current `tool_supports_repository_writes()` implementation marks these tools
as write-capable:

- `claude`
- `codex`
- `gemini`
- `opencode`
- `oai-runner`

## Environment Variables

Model/tool validation is provider-specific. Check the effective environment
with the doctor's CLI-tool and API-key checks:

```bash
animus doctor --check cli_tools
animus doctor --check api_keys
```

The effective model/tool assignments come from the config cascade described
above: `phase.runtime.model` in workflow YAML, then the agent profile in
`~/.animus/<repo-scope>/config/agent-runtime-config.v2.json`, then the
compiled defaults in `protocol::model_routing`. Inspect or edit the agent
runtime layer with `animus workflow agent-runtime get|validate|set` (below).

## Agent Runtime Commands

```bash
animus workflow agent-runtime get
animus workflow agent-runtime validate
animus workflow agent-runtime set --input-json '{"agents":{"default":{"model":null,"tool":null}}}'
```
