# Skill System

Animus skills are reusable prompt + runtime config bundles that compose into
phases at workflow execution time. This document specifies the source-chain
priority, the two-tier trust model that protects unvetted SKILL.md files, and
the `animus:` vendor namespace used to extend portable SKILL.md frontmatter
with Animus-specific runtime fields.

## Source chain

`load_skill_sources(project_root, user_config_dir)` returns sources in
**lowest-to-highest priority** order. `resolve_skill` iterates that slice in
reverse, so the first match wins. The chain is:

| Priority | Origin | Trust tier | Source |
| --- | --- | --- | --- |
| 1 (highest) | `Project` | High | `<project>/.animus/skills/`, `<project>/.animus/config/skill_definitions/` (v0.3 `.ao/skills/` is no longer scanned — see "Migrating from v0.3" below) |
| 2 | `User` | High | `~/.animus/skills/`, `~/.animus/config/skill_definitions/` |
| 3a | `Installed { registry, source, version, integrity, artifact }` | High | Pack `[skills]` contributions and registry-tracked installs from `animus skill install` |
| 3b | `Installed` (registry snapshots) | High | `~/.animus/<scope>/state/skills-registry.v1.json` |
| 4a | `AgentHost { host, scope: Project }` | **Prompt-text-only** | `<project>/.claude/skills/`, `<project>/.codex/skills/`, ... |
| 4b (lowest) | `AgentHost { host, scope: Global }` | **Prompt-text-only** | `~/.claude/skills/`, `~/.codex/skills/`, ... |

Trust-tier semantics are enforced at load time. See "Two-tier trust model" below.

### Migrating from v0.3 paths

v0.3 stored project skills under `<project>/.ao/skills/` and user skills under
`~/.ao/skills/`. v0.4's naming contract renamed these to `.animus/skills/`. The
resolver no longer scans the legacy `.ao/skills/` paths, but it does probe them
once per process and emits a single `warning:` line per legacy path when one
exists without its `.animus/skills/` replacement. To migrate cleanly:

```bash
animus skill migrate-from-ao         # both project + user scopes
animus skill migrate-from-ao --dry-run --project-only  # preview only
```

The migration moves every entry under `.ao/skills/` into `.animus/skills/`,
drops a `.migrated-from-ao` marker so the warning stops firing, and removes
the now-empty legacy directory. It refuses to clobber a non-empty
`.animus/skills/` that lacks the marker — operators must merge manually.

### Builtin fallback removal

Earlier v0.4 builds carried a legacy built-in skill fallback and bundled-pack
install path for `animus.core-skills`. Current builds removed that fallback.
Skills now resolve only from installed packs, registry-tracked installs,
project/user scopes, and prompt-text-only agent-host probes.

## Two-tier trust model

`SkillSourceOrigin` splits skills into two trust tiers:

### High trust (`Project`, `User`, `Installed`)

These sources may contribute every field of `SkillDefinition`:

- `prompt.system`, `prompt.prefix`, `prompt.suffix`, `prompt.directives`
- `tool_policy` (`allow`/`deny` lists)
- `extra_args`, `env`, `mcp_servers`
- `adapters` (per-tool prompt and runtime overrides)
- `codex_config_overrides`, `capabilities`, `timeout_secs`, `model`, `tags`,
  `activation`

`Installed` skills carry an integrity snapshot (sha256 + artifact filename)
recorded at install time, so the high-trust grant is auditable and
reproducible. `Project`/`User` skills are checked into the project (or written
to the user's own `~/.animus/`), so they inherit the user's filesystem trust.

### Prompt-text-only (`AgentHost`)

SKILL.md files discovered under another agent host's skill directory
(`~/.claude/skills/`, `~/.codex/skills/`, `~/.cursor/skills/`,
`~/.config/opencode/skills/`, `~/.kiro/skills/`, `~/.slate/skills/`, plus
project-scoped variants where the host supports them) are PROMPT-TEXT-ONLY.

After parsing the markdown frontmatter, `strip_structural_fields_for_agent_host`
clears every runtime field on the resulting `SkillDefinition`:

```rust
definition.tool_policy = None;
definition.extra_args.clear();
definition.env.clear();
definition.mcp_servers.clear();
definition.adapters.clear();
definition.codex_config_overrides.clear();
definition.capabilities.clear();
definition.model = SkillModelPreference::default();
definition.timeout_secs = None;
```

This guarantees a malicious SKILL.md dropped under `~/.claude/skills/` cannot
silently widen Animus's tool surface, attach an MCP server, leak environment
variables, escalate codex permissions, force the runner onto a cheaper or
restricted model (`model`), or monopolize the runner with an arbitrarily
long timeout (`timeout_secs`). Only the prompt body, description, metadata,
and activation filters flow through.

A field-coverage test in `crates/orchestrator-config/src/skill_scoping.rs`
(`agent_host_strip_covers_every_runtime_field`) enumerates every serializable
field on `SkillDefinition` and fails the build if a new field is added
without an explicit allowlist / strip-list classification.

### Promoting an AgentHost skill to high trust

To use the structural fields of a SKILL.md discovered from another agent host,
the user must explicitly install it:

```bash
animus skill install --path ~/.claude/skills/my-favorite/
```

`animus skill install` converts it into a high-trust `Installed` entry with an
integrity snapshot. The user reads the SKILL.md once, decides whether the
declared `tool_policy` and MCP servers are acceptable, and pins the version.

### Agent host discovery table

| Host | Project-scoped | Global |
| --- | --- | --- |
| `claude-code` | `<project>/.claude/skills/` | `~/.claude/skills/` |
| `codex` | `<project>/.codex/skills/` | `~/.codex/skills/` |
| `opencode` | `<project>/.config/opencode/skills/` | `~/.config/opencode/skills/` |
| `cursor` | `<project>/.cursor/skills/` | `~/.cursor/skills/` |
| `kiro` | n/a | `~/.kiro/skills/` |
| `slate` | n/a | `~/.slate/skills/` |

Each child directory of a host root that contains a `SKILL.md` file is treated
as one skill. Direct `*.md` files at the root are also accepted (the filename
stem becomes the skill name when frontmatter omits `name`).

## Runtime enforcement and the model hierarchy

Which component actually honors each `SkillDefinition` field depends on the
execution path:

### Workflow phases (the `workflow_runner` plugin)

The reference runner (`launchapp-dev/animus-workflow-runner-default` v0.4.2+)
enforces every structural field when a skill activates for a phase:

- `prompt.*` — composed into the phase prompt (`render_phase_prompt_with_ctx_overrides`).
- `tool_policy`, `mcp_servers` — merged into the runtime contract's MCP wiring.
- `model` (`preferred`/`fallback`) and `adapters.<tool>.model` — feed phase
  target resolution. The effective model hierarchy on the workflow path is:
  explicit run overrides (`--model` / `--tool`) > `phase.runtime.model`
  (workflow YAML / agent runtime config) > skill model (adapter `model` wins
  over `preferred`, which wins over `fallback`) > phase-routing / agent
  profile defaults > compiled defaults in `protocol::model_routing`.
- `timeout_secs` — fallback phase timeout: explicit phase timeout > skill
  `timeout_secs` > `phase.runtime.timeout_secs`.
- `capabilities` — override the phase's `PhaseCapabilities`
  (`apply_skill_capability_overrides`); unknown capability keys fail
  validation at load time.
- `extra_args`, `env`, `codex_config_overrides` — injected into the runtime
  contract's `/cli/launch/args` and `/cli/launch/env`, which travel to the
  provider plugin on `AgentRunRequest.runtime_contract`.

### Ad-hoc runs (`animus chat` / agent MCP scope resolution)

The kernel's ad-hoc path consumes only `mcp_servers` and `tool_policy`
(profile ∪ skill ∪ `--mcp-server`). Skill `model`, `timeout_secs`,
`capabilities`, `extra_args`, `env`, and `codex_config_overrides` do not
apply to ad-hoc chat turns — declare those expectations on the agent profile
or pass explicit flags instead.

### Activation matching and inert tool ids

`activation.tools` and `adapters.<tool>` match the runtime tool id with a
literal case-insensitive comparison of the declared value (untrimmed). On
the workflow path runtime tool ids are canonical (`claude`, `codex`,
`gemini`, `opencode`, `oai-runner` — see `protocol::KNOWN_TOOL_IDS`). A typo
(`claud`) or a whitespace-padded value (`' claude '`) therefore never
matches, and an alias (`minimax`, `open-code`) never matches on workflow
phases (ad-hoc runs pass `--tool` through verbatim, where a literal alias
can still match). `skill_definition_warnings` in
`crates/orchestrator-config/src/skill_definition.rs` detects these inert
declarations; the warnings (never errors — existing definitions keep
loading) surface through `animus skill info`, `animus skill list`, and the
`animus.skill.get` / `animus.skill.create` / `animus.skill.update` MCP
tools.

## Vendor namespace `animus:`

`MarkdownSkillFrontmatter` parses these fields by default:

- `name`, `description`, `version`, `metadata.version`

To configure Animus runtime behavior from a SKILL.md without breaking
portability to other agent hosts, place the structural fields under an
`animus:` namespace:

```yaml
---
name: my-skill
description: Custom skill for X
animus:
  tool_policy:
    allow: ["task.*"]
    deny: ["task.delete"]
  mcp_servers: ["context7"]
  model:
    preferred: claude-sonnet-4-6
  adapters:
    codex:
      prompt:
        system: "Codex-specific override"
  extra_args: ["--verbose"]
  env:
    REVIEW_MODE: strict
  capabilities:
    is_review: true
  codex_config_overrides: ["max_tokens=4096"]
  timeout_secs: 300
  tags: ["quality"]
---

# Skill body
The body becomes `prompt.system`.
```

Top-level placement of these fields is intentionally NOT parsed; other agent
hosts (Claude Code, Codex, Cursor) would complain about unknown keys at the
root of the frontmatter. Nesting under `animus:` keeps the file portable.

The `animus:` namespace is honored by every source that earns high trust. On
an `AgentHost` source the structural fields under `animus:` are still parsed
into the in-memory `SkillDefinition`, but `strip_structural_fields_for_agent_host`
clears them before the skill leaves `load_skill_sources`. The trust boundary
trumps the namespace shape.

## Pack-supplied skills

Installed packs can contribute skill definitions through their `[skills]`
manifest sections. Those entries resolve as `SkillSourceOrigin::Installed`
with provenance describing the registry, source, version, integrity, and
artifact path. Current builds no longer materialize pack content from
in-repo bundled assets during `animus init`.

## Implementation map

- `crates/orchestrator-config/src/skill_definition.rs` — `SkillDefinition`,
  `SkillApplicationResult`, validation, application and merge logic.
- `crates/orchestrator-config/src/skill_scoping.rs` — source chain, agent-host
  discovery, trust enforcement, `animus:` namespace parsing.
- `crates/orchestrator-config/src/skill_resolution.rs` — `resolve_skill`,
  `resolve_skills_for_project`, `list_available_skills`.
- `crates/orchestrator-config/src/pack_config/types.rs` — `PackSkills` manifest
  shape and the optional `workflows`/`skills` invariant.
