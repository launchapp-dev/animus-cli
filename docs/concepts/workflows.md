# Workflows

## Everything Is a Workflow

In Animus, every autonomous operation resolves through a `workflow_ref`. The CLI,
web API, daemon queue, and MCP surfaces all emit a
[SubjectDispatch](./subject-dispatch.md) that points at a workflow definition,
and `workflow-runner` executes the resulting phase plan.

The daemon does not own domain behavior. Workflow behavior comes from bundled
kernel baseline config, installable first-party packs, installed packs, and project-local
overrides.

## Workflow Sources

Animus currently resolves workflows from these sources:

| Source | Typical Refs | What It Owns |
|---|---|---|
| Kernel baseline config | phase ids such as `requirements`, `implementation`, `testing` | Shared phase catalog UI metadata, default MCP server wiring, and empty workflow config baseline |
| Installable first-party packs | `animus.task/standard`, `animus.requirement/draft`, `animus.requirement/execute` | Task, requirement, review, and QA behavior shipped as versioned pack overlays |
| Installed machine packs | `vendor.pack/ref` | Shared packs installed under `~/.animus/packs/<pack-id>/<version>/` |
| Project pack overrides | `vendor.pack/ref` | Per-project overrides under `.animus/plugins/<pack-id>/` (inside the same `.animus/plugins/` tree used for project-local plugin discovery) |
| Project-local ad hoc YAML | `standard-workflow`, `incident-response` | Repository-specific workflows in `.animus/workflows.yaml` or `.animus/workflows/*.yaml` |

### Resolution Order

1. Project pack overrides in `.animus/plugins/<pack-id>/`
2. Project-local YAML in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`
3. Installed packs in `~/.animus/packs/<pack-id>/<version>/`
4. Kernel baseline config embedded in Animus

This means a project can override installed workflow content without teaching
the daemon any new behavior.

## Canonical Workflow Refs

Pack-qualified refs are the canonical surface. Current public CLI entrypoints
dispatch them through the workflow engine:

| Operator Entry Point | Canonical Ref | Notes |
|---|---|---|
| `animus workflow run animus.task/standard --task-id TASK-001` | `animus.task/standard` | Explicit workflow ref execution through the CLI |
| `animus workflow run animus.requirement/execute --requirement-id REQ-001` | `animus.requirement/execute` | Requirement execution resolves to the canonical pack ref |
| `animus workflow run standard-workflow` | `animus.task/standard` | Repository-specific workflows can wrap canonical pack refs |

The first-party pack boundary is currently most visible in task, requirement,
review, and QA behavior. For example, task routing and task execution phases now
flow through the installable `animus.task` and `animus.review` packs instead of living in
the kernel baseline.

## Installable First-Party Packs

Animus's first-party workflow packs live in separate repositories under
`launchapp-dev/animus-pack-*` and are installed into `~/.animus/packs/`. Today those packs
include:

- `animus.task` for task workflows and task-owned runtime overlays
- `animus.requirement` for requirement planning and execution flows
- `animus.review` for review, QA, and command-phase runtime overlays

`animus init` does not materialize these packs automatically. It surfaces the
recommended pack/plugin set from
`crates/orchestrator-cli/config/default-install.json`, and the onboarding
walkthrough installs default plugins separately via
`animus plugin install-defaults --include-subjects --include-transports`.

These packs can contribute:

- workflow overlays
- phase catalog entries
- runtime overlays
- MCP server descriptors
- runtime requirements
- permissions and secrets policy

## Pack Operations

Operators can inspect and control which packs are active for a project:

```bash
animus pack list
animus pack info --pack-id animus.task
animus pack install --path /tmp/vendor.pack --activate
animus pack pin --pack-id animus.task --version =0.1.0
```

Project-specific pack selections are stored in
`~/.animus/<repo-scope>/state/pack-selection.v1.json`. Pack override content
lives in `.animus/plugins/<pack-id>/`, while the parent `.animus/plugins/`
directory is also scanned for project-local plugin binaries.

## Project-Local Workflow Composition

Project YAML usually wraps canonical pack refs instead of redefining domain
logic:

```yaml
workflows:
  - id: standard-workflow
    name: Standard Workflow
    description: Repository default delivery workflow
    phases:
      - workflow_ref: animus.task/standard

  - id: hotfix-workflow
    name: Hotfix Workflow
    description: Fast-track workflow for urgent fixes
    phases:
      - workflow_ref: animus.task/quick-fix
```

That keeps repository customization local while task and requirement semantics
stay owned by the relevant pack.

## Supported Features

Workflow definitions can combine:

- ordered phase execution
- verdict routing (`advance`, `rework`, `skip`, `fail`)
- sub-workflow composition
- command phases
- manual approval phases
- per-phase MCP bindings
- post-success merge and PR behavior
- pack-owned runtime overlays and policy checks

See [Writing Workflows](../guides/writing-workflows.md) for authoring guidance
and [Subject Dispatch](./subject-dispatch.md) for how workflow refs reach the
runner.
