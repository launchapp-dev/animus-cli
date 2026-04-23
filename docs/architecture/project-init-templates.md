# Project Init Templates

## Purpose

Animus needs a better first-run experience than "write four default YAML files
and then hand-edit everything." Operators should be able to start from an
opinionated project pattern such as:

- conductor-driven orchestration
- task-queue-only execution
- direct workflow execution without a conductor
- future domain patterns owned by LaunchApp or third parties

This document defines a template-driven init architecture that builds on the
existing pack system instead of bypassing it.

## Problem

Today `animus setup` does three useful but narrow things:

1. bootstraps `.ao/` and scoped runtime state
2. writes a generic workflow YAML scaffold
3. toggles daemon automation flags

That is enough for bootstrap safety, but not enough for product-quality
onboarding:

- every repository starts from the same low-context scaffold
- there is no concept of "pattern" or "operating model"
- packs are installable, but setup does not help a project choose them
- there is no curated path for starter agents, MCP defaults, or workflow refs
- LaunchApp cannot ship a beautiful first-run catalog without editing the binary

## Core Decision

Animus should add a **template-driven init layer** above packs.

- Packs remain the reusable unit of behavior.
- Templates become the reusable unit of project bootstrap.
- `animus init` becomes the primary first-run command.
- `animus setup` remains as a compatibility alias or a lower-level bootstrap
  command.

This preserves the current architecture:

- the daemon stays dumb
- packs still own domain behavior
- project-local YAML remains the authored override surface

## Product Shape

### Operator Experience

The intended first-run flow:

1. `animus init`
2. Detect current repo state:
   - inside an existing git repo
   - empty directory
   - dirty worktree
   - existing `.ao/`
3. Ask the operator what kind of Animus project they want:
   - `Conductor Pattern`
   - `Task Queue Pattern`
   - `Direct Workflow Pattern`
   - `Custom / Advanced`
4. Show a short explanation of tradeoffs for each pattern.
5. Ask a few focused follow-up questions:
   - desired AI CLIs
   - GitHub / Linear / MCP integrations
   - autonomy level
   - PR / merge behavior
6. Render a preview plan:
   - files to create
   - packs to install / activate
   - workflows to export
   - starter repo or starter files to copy
7. Apply the selected template.
8. Print the next 2-3 commands to run.

The init path should feel like a product wizard, not a raw config writer.

### Command Surface

Target command shape:

```text
animus init
animus init --template conductor
animus init --template task-queue --non-interactive
animus init templates list
animus init templates inspect --template conductor
animus init templates search queue
```

Compatibility expectations:

- `animus setup` should continue to work
- `animus setup` can eventually delegate to `animus init --bootstrap-only`
- existing scriptable flows must keep a non-interactive path

## Relationship to Packs

Templates should not replace packs.

### Packs Own Reusable Behavior

Packs remain responsible for:

- workflow refs
- phase catalog overlays
- MCP server descriptors
- runtime requirements
- schedules
- subject-specific behavior

### Templates Own Bootstrap Decisions

Templates should own:

- which packs are installed and activated for a new project
- which project-local workflow wrappers are written into `.ao/workflows/`
- which starter agents / model defaults are suggested
- optional repository starter files outside `.ao/`
- the onboarding questionnaire and explanatory copy

The rule is:

- if something should be reusable across many repos at runtime, it belongs in a
  pack
- if something is about how a repo gets started, it belongs in a template

## Template Registry Model

Animus already has Git-backed marketplace mechanics for packs. The template
system should reuse that operational model instead of inventing a completely
separate distribution path.

### Recommended First Registry

The first curated registry should live under LaunchApp control:

`launchapp-dev/animus-project-templates`

That repo becomes the canonical source for first-party init patterns.

Later, the same registry mechanism can allow additional registries from other
owners.

### Registry Responsibilities

A template registry should provide:

- searchable template metadata
- versioned template manifests
- localizable explanatory copy
- optional starter repositories or skeleton directories
- references to packs that should be installed / activated

The existing marketplace cache model under `~/.ao/` is a good fit for this.

## Template Bundle Contract

Each template should be a directory with one manifest plus optional skeleton and
prompting assets.

### Filesystem Shape

```text
template.toml
README.md
questions.yaml
preview.md
skeleton/
  .ao/
    workflows/
    workflows.yaml
  .github/
  docs/
render/
  values.schema.json
```

### Manifest Example

```toml
schema = "animus.template.v1"
id = "launchapp.conductor"
version = "0.1.0"
title = "Conductor Pattern"
description = "A planning-heavy project shape with a conductor workflow that feeds the queue."
pattern = "conductor"

[compatibility]
animus = ">=0.3.0"

[source]
mode = "copy"
root = "skeleton"

[[packs]]
id = "ao.task"
version = "^0.1"
activate = true

[[packs]]
id = "ao.requirement"
version = "^0.1"
activate = true

[[packs]]
id = "ao.review"
version = "^0.1"
activate = true

[questionnaire]
file = "questions.yaml"

[preview]
file = "preview.md"
```

### Manifest Fields

Minimum useful fields:

- `id`
- `version`
- `title`
- `description`
- `pattern`
- compatibility requirements
- source application mode
- packs to activate
- questionnaire file
- preview / explanation file

## Starter Source Modes

Templates should support three source modes.

### 1. Copy

Copy files from a template skeleton directory into the target repo.

Use this for:

- `.ao/` starter config
- minimal docs
- GitHub workflow examples
- repo-local helper scripts

### 2. Clone

Clone a dedicated starter repository and then materialize Animus selections into
it.

Use this when the starter repo itself is the real product scaffold, for example
a fully opinionated monorepo baseline.

### 3. Overlay

Apply a small delta over an already-existing repo.

Use this when the operator is adding Animus to a mature codebase and should not
replace the repo structure.

For most first-party templates, `copy` plus pack activation should be the
default.

## Pattern Catalog

Initial first-party pattern catalog:

### Conductor Pattern

Use when the team wants a planning workflow that continuously feeds
implementation work into the queue.

Characteristics:

- conductor / planner workflow present
- requirements and queue flows enabled
- more opinionated review / QA defaults
- optimized for autonomous backlog management

### Task Queue Pattern

Use when the team already manages tasks elsewhere and just wants execution and
review.

Characteristics:

- queue and execution flows
- minimal planning surface
- straightforward task lifecycle
- optimized for predictable throughput

### Direct Workflow Pattern

Use when the team wants explicit human-driven workflow runs without a standing
conductor.

Characteristics:

- simple project-local workflow wrappers
- fewer background schedules
- less daemon policy
- optimized for manual triggering

## Application Flow

Target execution pipeline for `animus init`:

1. Resolve project root and repo state.
2. Load template registries and cached metadata.
3. Ask or resolve the selected template.
4. Load questionnaire defaults.
5. Produce a deterministic init plan:
   - packs to install
   - template files to apply
   - variables to render
   - runtime requirements to verify
6. Show preview output.
7. Stage template materialization in a temp directory.
8. Apply copy / overlay / clone strategy.
9. Activate required packs and save selection state.
10. Run bootstrap validation and doctor checks.
11. Emit next-step guidance.

This should be implemented as a transactional staging flow where possible so a
half-applied template does not leave the repo in a confusing state.

## Why a Separate Template Contract

Templates and packs solve related but different problems.

- A pack answers: "what reusable behavior can Animus run?"
- A template answers: "what should a new project start with?"

Trying to force templates into raw pack manifests would blur those concerns and
make pack manifests own repository bootstrapping concerns they should not own.

The better design is:

- keep packs as runtime content
- keep templates as init content
- let templates reference packs

## Implementation Plan

### Phase 1: Curated First-Party Templates

- Add template manifest parsing and validation.
- Add `animus init --template ...` with local or cached template loading.
- Support `copy` mode only.
- Ship 2-3 first-party LaunchApp templates.

### Phase 2: Registry and Search

- Extend the marketplace model to index templates in addition to packs.
- Add `animus init templates list/search/inspect`.
- Add registry sync for LaunchApp-managed template catalogs.

### Phase 3: Clone + Overlay

- Support starter repos and overlay mode.
- Add preview diffs and conflict handling.
- Add richer non-interactive rendering variables.

### Phase 4: Polished Operator UX

- Rich terminal wizard with clear tradeoff copy.
- Optional web UI init flow.
- Better doctor integration for pack/runtime prerequisites.

## Constraints

- The daemon must stay dumb.
- Template application must be explicit and previewable.
- Non-interactive automation must stay deterministic.
- Packs remain the only reusable runtime behavior unit.
- Project-local YAML stays editable after init.
- Template registries must be Git-backed and cacheable under `~/.ao/`.

## Recommended Next Step

The next implementation slice should be:

1. add a template manifest type and validator in `orchestrator-config`
2. add a dedicated `animus init` command in `orchestrator-cli`
3. support one first-party LaunchApp template registry repo
4. implement `copy` mode with pack activation and preview output

That is the smallest slice that creates a materially better onboarding
experience without redesigning the rest of the runtime.
