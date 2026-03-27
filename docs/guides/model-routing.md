# Model Routing Guide

AO automatically selects which AI model and CLI tool to use for each workflow phase. This guide explains the routing logic and how to override it.

## Default Model Assignments

The compiled defaults route models based on phase type and task complexity:

| Phase Type | Low Complexity | Medium Complexity | High Complexity |
|------------|----------------|-------------------|-----------------|
| **Implementation** | zai-coding-plan/glm-5 | claude-sonnet-4-6 | claude-sonnet-4-6 |
| **Code Review** | claude-sonnet-4-6 | claude-sonnet-4-6 | claude-opus-4-6 |
| **Requirements** | minimax/MiniMax-M2.5 | claude-sonnet-4-6 | claude-sonnet-4-6 |
| **Testing** | minimax/MiniMax-M2.5 | claude-sonnet-4-6 | claude-sonnet-4-6 |
| **Research** | gemini-2.5-flash-lite | gemini-2.5-flash-lite | gemini-2.5-flash-lite |
| **UI-UX / Design** | gemini-3.1-pro-preview | gemini-3.1-pro-preview | gemini-3.1-pro-preview |

## Config Cascade

Model selection follows a three-level cascade. The first match wins:

```
1. phase/agent YAML override     (project workflow YAML)
2. resolved agent runtime config (computed from YAML + bundled defaults)
3. compiled defaults             (protocol/src/model_routing.rs)
```

### Level 1: Per-Phase Override in Workflow YAML

Set the model directly on an agent in your workflow YAML:

```yaml
agents:
  my-agent:
    model: claude-opus-4-6
    tool: claude
```

This takes highest precedence.

### Level 2: Resolved Agent Runtime

AO resolves agent runtime from the authored workflow YAML under `.ao/workflows.yaml` and `.ao/workflows/*.yaml`, merged with bundled defaults. You can inspect the effective runtime with:

```bash
ao workflow agent-runtime get
ao workflow agent-runtime validate
```

The resolved payload includes the source path and the merged agent definitions. A minimal authored override still looks like this in YAML:

```yaml
agents:
  default:
    model: claude-sonnet-4-6
    tool: claude
```

If you prefer to replace the runtime as structured JSON, `ao workflow agent-runtime set` still accepts the compiled schema directly:

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

Set these fields to `null` to let compiled defaults take over when using the JSON setter:

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

### Level 3: Compiled Defaults

The function `default_primary_model_for_phase()` in `crates/protocol/src/model_routing.rs` contains the hardcoded routing table shown above. These apply when neither the workflow YAML nor the agent runtime config specifies a model.

## Tool Assignment

Each model maps to a CLI tool. The mapping is determined by model name prefix:

| Model prefix | CLI Tool | Required API Key |
|-------------|----------|-----------------|
| `claude-*` | `claude` | `ANTHROPIC_API_KEY` |
| `gpt-*` | `codex` | `OPENAI_API_KEY` |
| `gemini-*` | `gemini` | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| `zai-*`, `minimax-*`, `glm-*` | `oai-runner` | `MINIMAX_API_KEY`, `ZAI_API_KEY`, or `OPENAI_API_KEY` |
| `deepseek-*`, `qwen-*` | `opencode` | Multiple keys supported |

You can check model and API key status:

```bash
ao model status
ao model availability
```

## Write-Capable Tools

Not all tools support repository writes. The write-capable tools are:

- `claude`
- `codex`
- `opencode`
- `oai-runner`

The `gemini` tool is not write-capable. AO redirects non-write-capable tools to
a write-capable fallback for implementation-style phases. Use Gemini on
read-only or research phases by configuring those phases to use Gemini
explicitly in workflow YAML or pack content.

## Fallback Models

When the primary model fails, AO tries fallback models in order. Fallbacks vary by phase type and complexity. For example, a medium-complexity implementation phase falls back through:

1. zai-coding-plan/glm-5
2. minimax/MiniMax-M2.5
3. gemini-3.1-pro-preview
4. gpt-5.3-codex

## Environment Variables

| Variable | Effect |
|----------|--------|
| `ANTHROPIC_API_KEY` | Required for claude tool |
| `OPENAI_API_KEY` | Required for codex tool |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | Required for gemini tool |
| `MINIMAX_API_KEY` / `ZAI_API_KEY` | Required for oai-runner tool |

## Validating Model Selection

Check whether a model is valid and available:

```bash
ao model validate --model claude-sonnet-4-6
```

Refresh the model roster:

```bash
ao model roster refresh
ao model roster get
```

## Agent Runtime Config Commands

Read, validate, and set the resolved agent runtime:

```bash
ao workflow agent-runtime get
ao workflow agent-runtime validate
ao workflow agent-runtime set --input-json '{"agents":{"default":{"model":null,"tool":null}}}'
```
