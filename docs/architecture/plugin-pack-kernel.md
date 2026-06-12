# Plugin Pack Kernel Architecture

## Purpose

Animus should stay a workflow kernel that hosts bundled and third-party domain
packs instead of baking task and requirement semantics into daemon-core.

The daemon stays dumb. The workflow runner stays the execution host. Product
domains such as tasks, requirements, incidents, CRM leads, or external issue
trackers should arrive as installable plugin packs that can ship workflows, MCP
servers, runtime overlays, and optional native adapters.

This document defines that target shape.

## Core Decision

Animus should adopt a package-style plugin system over the existing dumb-daemon
kernel.

- The kernel owns dispatch, capacity, subprocesses, workflow execution, and
  execution facts.
- Plugin packs own domain workflows, MCP server configuration, mutation
  surfaces, schedules, and subject-specific behavior.
- Built-in `task` and `requirements` become installable first-party plugin packs,
  not special cases in daemon-core.
- Animus should prefer process- and package-oriented plugins over runtime-loaded
  Rust dynamic libraries.

## Non-Decision

This is not a proposal for:

- runtime-loaded Rust `.so` / `.dylib` / `.dll` plugins in the daemon
- embedding third-party Rust code into the daemon process at runtime
- embedding Python or Node.js interpreters into the Animus kernel
- letting workflows mutate Animus state through hidden daemon-only side effects

Those choices would weaken isolation and make the daemon harder to keep dumb.

## Plugin Tiers

Not every extension needs native code. Animus should support three extension tiers.

### Tier 1: Declarative Pack

Ships only data and configuration:

- workflow YAML files
- phase catalog overlays
- agent/runtime overlays
- MCP server descriptors
- schedules
- subject schemas and display metadata

This should cover most Animus extensions.

These packs may still rely on external runtimes such as Node.js or Python by
declaring command-phase programs or MCP server commands that execute outside the
Animus process.

### Tier 2: Connector Pack

A declarative pack plus MCP-backed integration behavior:

- external MCP server processes
- Animus MCP tool namespaces
- connector-specific workflow bundles
- import/export or sync workflows

This should cover Jira, Linear, GitHub, CRM, incident, and support-style
extensions.

Node.js and Python should be first-class citizens at this tier.

Examples:

- a Node.js MCP server launched via `node` or `npx`
- a Python MCP server launched via `python`, `python3`, `uv run`, or `pipx run`
- command phases that invoke TypeScript, JavaScript, or Python utilities

### Tier 3: Native Module

A compiled module linked into Animus behind Cargo features for behavior that cannot
be expressed through workflows, projectors, MCP tools, or schemas alone.

Examples:

- worktree / execution-cwd policy for a subject kind
- custom subject resolution
- projector logic for a new subject kind
- provider-backed list/query services with local caching

Native modules should be rare and treated as bundled or feature-gated modules,
not arbitrary runtime-loaded code.

If a plugin can be expressed as workflows plus an external Python or Node.js
process, Animus should prefer that over a native Rust module.

## Kernel Boundary

The kernel should know only about:

- project-root resolution
- subject identity
- `SubjectDispatch`
- workflow refs and workflow pack loading
- execution facts and runner events
- queue ordering, capacity, headroom, subprocess lifecycle
- MCP process hosting and phase-local tool availability

The kernel should not know:

- task lifecycle rules
- requirement lifecycle rules
- issue tracker semantics
- domain-specific queue promotion logic
- product-specific planning behavior
- external system business rules

That boundary extends the dumb-daemon model already described in
[subject-dispatch-daemon.md](./subject-dispatch-daemon.md) and
[tool-driven-mutation-surfaces.md](./tool-driven-mutation-surfaces.md).

## Subject Model

The current `WorkflowSubject` enum (`Task | Requirement | Custom`) is still too
product-shaped for a real plugin system. The kernel should move toward a generic
subject identity contract.

### Target Shape

```rust
pub struct SubjectRef {
    pub kind: String,
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub labels: Vec<String>,
    pub metadata: serde_json::Value,
}

pub struct SubjectDispatch {
    pub subject: SubjectRef,
    pub workflow_ref: String,
    pub input: Option<serde_json::Value>,
    pub vars: std::collections::HashMap<String, String>,
    pub priority: Option<String>,
    pub trigger_source: String,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}
```

### Subject Kinds

Examples:

- `animus.task`
- `animus.requirement`
- `jira.issue`
- `linear.issue`
- `incident.alert`
- `crm.lead`
- `custom`

The subject kind becomes the routing key for adapters, projectors, list
surfaces, and pack-owned defaults.

## Plugin Pack Contract

Each pack should install as a versioned directory with one manifest and a small
set of well-known subdirectories.

### Filesystem Shape

```text
pack.toml
workflows/
runtime/
  agent-runtime.overlay.yaml
  workflow-runtime.overlay.yaml
mcp/
  servers.toml
  tools.toml
schedules/
  schedules.yaml
subjects/
  schemas.yaml
ui/
  labels.yaml
```

Optional runtime-owned directories are also valid:

```text
python/
node/
bin/
```

### Manifest Example

```toml
schema = "animus.pack.v1"
id = "animus.requirement"
version = "0.1.0"
kind = "domain-pack"
title = "AO Requirement"
description = "Planning and materialization flows for requirement subjects."

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.requirement"]
default_kind = "animus.requirement"

[workflows]
root = "workflows"
exports = [
  "animus.requirement/draft",
  "animus.requirement/refine",
  "animus.requirement/plan",
  "animus.requirement/execute",
]

[[dependencies]]
id = "animus.task"
version = ">=0.1.0"
reason = "Requirement workflows materialize and bootstrap task execution."

[[requires_plugins]]
repo = "launchapp-dev/animus-subject-linear"
tag = "v0.2.0"
role = "subject_backend:linear"
reason = "Linear-backed subjects power the triage workflows."

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"

[native_module]
feature = "plugin_ao_requirement"
module_id = "animus.requirement"
optional = true
```

### Pack Dependencies

`[[dependencies]]` entries are acted on by `animus pack install` (v0.5.14+):
after the requested pack installs, each non-optional dependency is resolved —
already-installed versions that satisfy the semver `version` requirement are
skipped, otherwise the dependency installs through the same source-resolution
path its parent came from (the `ANIMUS_INIT_PACK_SOURCE_DIR` offline override
always wins, then the marketplace registries when the parent was installed via
`--name`, then the pinned GitHub release for packs in the recommended
`default-install.json` set). Resolution recurses into dependencies of
dependencies up to a depth of 5 with cycle detection by pack id. Optional
dependencies are printed as suggestions, never auto-installed. A failing
dependency never aborts the parent install — the failure is reported together
with the exact manual install command. `--no-deps` skips dependency
resolution entirely; `--dry-run` prints the full dependency closure without
installing anything.

### Required Plugins

`[[requires_plugins]]` (v0.5.14+) lets a pack declare the Animus plugins its
workflows need — e.g. a subject backend — so the gap surfaces at install time
instead of phase time:

- `repo` (required): GitHub `OWNER/REPO` slug of the plugin.
- `tag` (optional): release tag to suggest/install.
- `role` (optional, informational): e.g. `subject_backend:linear`.
- `optional`, `reason`: same semantics as pack dependencies.

At `animus pack install`, every declared plugin is checked against the
installed-plugin registry. Missing required plugins are never silent: in an
interactive terminal Animus offers to install them (default yes),
`--install-plugins` installs them non-interactively, and otherwise the exact
`animus plugin install <repo>@<tag>` commands are printed. `animus pack info`
shows both dependencies and required plugins with installed/missing status.

Compatibility: the section is additive within `animus.pack.v1`. Older Animus
versions (< 0.5.14) reject manifests that declare `[[requires_plugins]]`
(unknown manifest fields are denied), while packs WITHOUT the section keep
loading everywhere. Packs that use it should therefore also declare
`compatibility.animus_core = ">=0.5.14"`.

### Runtime Requirements

Plugin packs should be able to declare required external runtimes without
making Animus itself depend on those runtimes.

Examples:

- `node >=20`
- `python >=3.11`
- `uv`
- `npm` or `pnpm`

Animus should validate these requirements at install or activation time and surface
clear diagnostics when a pack cannot run because the local runtime is missing.

## Workflow Pack Loading

Plugin packs should become a first-class workflow source alongside builtins and
project-local YAML.

### Resolution Order

1. Project-local overrides in `.animus/plugins/<pack-id>/`
2. Project-local ad hoc workflows in `.animus/workflows/`
3. Installed pack workflows in `~/.animus/packs/<pack-id>/<version>/`
4. Bundled first-party packs embedded into the Animus binary

This preserves the current override model while allowing installable pack
distribution.

### Workflow Ref Format

Workflow refs should move toward pack-qualified names:

- `animus.task/standard`
- `animus.task/quick-fix`
- `animus.requirement/execute`
- `jira.issue/sync`
- `incident.alert/respond`

Builtin aliases such as `builtin/requirements-execute` can remain as migration
compatibility shims, but pack-qualified refs should become canonical.

## MCP Packaging

MCP should be a first-class plugin boundary.

A plugin pack may ship:

- MCP server process descriptors
- environment variable requirements
- default tool allowlists
- phase-to-tool binding defaults
- tool namespace documentation

### MCP Server Descriptor Example

```toml
[[server]]
id = "jira"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-jira"]
required_env = ["JIRA_BASE_URL", "JIRA_API_TOKEN"]
tool_namespace = "jira"
startup = "phase-local"
```

Python-backed example:

```toml
[[server]]
id = "mlops"
command = "uv"
args = ["run", "python", "-m", "ao_mlops_mcp"]
required_env = ["OPENAI_API_KEY"]
tool_namespace = "mlops"
startup = "phase-local"
```

### Hosting Rule

The daemon should not become an integration host. MCP servers should be started
by the workflow execution layer or a dedicated MCP host layer, scoped to the
workflow or phase as policy requires.

This preserves the dumb-daemon boundary.

The same rule applies to Python and Node.js runtimes: they can participate as
plugin-owned subprocesses, but they should not be linked into daemon-core.

## Command-Phase Runtime Support

Workflow command phases already model arbitrary subprocess execution, which
makes them a natural extension point for Python and Node.js support.

Examples:

```yaml
phases:
  build-web-assets:
    mode: command
    command:
      program: node
      args: ["scripts/build.mjs"]
      cwd_mode: project_root

  classify-dataset:
    mode: command
    command:
      program: python3
      args: ["scripts/classify.py", "--input", "data/train.jsonl"]
      cwd_mode: project_root
```

This allows plugin packs to add Node.js or Python capabilities without changing
Animus core language or daemon responsibilities.

## Native Module Surface

Some domains need behavior beyond workflows and MCP tools. For those cases Animus
should expose stable native module interfaces, but load them through normal Rust
compilation and feature selection rather than runtime library loading.

### Suggested Native Traits

- `SubjectResolver`
- `ExecutionProjector`
- `DispatchPlanner`
- `ProjectAdapter`
- `TaskProvider`
- `RequirementsProvider`

The existing provider seams already point in this direction in
[service-hub.md](./service-hub.md).

### Registration Model

The kernel should own registries keyed by subject kind or pack id:

```rust
pub trait SubjectAdapter: Send + Sync {
    fn kind(&self) -> &'static str;
    fn resolve_context(&self, subject: &SubjectRef) -> anyhow::Result<SubjectContext>;
    fn ensure_execution_cwd(&self, project_root: &str, subject: &SubjectRef) -> anyhow::Result<String>;
}

pub trait ExecutionProjector: Send + Sync {
    fn kind(&self) -> &'static str;
    fn project(&self, fact: &SubjectExecutionFact, hub: std::sync::Arc<dyn ServiceHub>) -> anyhow::Result<()>;
}
```

The registry can ship built-in implementations for `animus.task` and
`animus.requirement`, while future packs register additional implementations.

## Built-In Packs

The current Animus product domains should become installable first-party packs.

### `animus.task`

Owns:

- task subject kind
- standard delivery workflows
- worktree-aware execution adapter
- task status projector
- task mutation tool namespace
- scheduled work planner and reconciler workflows

### `animus.requirement`

Owns:

- requirement subject kind
- planning workflows
- requirement lifecycle projector
- requirement mutation tools
- task materialization workflows

### `animus.review`

Owns:

- review and QA gate workflows
- approval records and review projectors
- associated MCP surfaces

This lets Animus keep first-party defaults without baking those domains into the
kernel.

## State and Installation Model

Plugin packs should be installable at two levels.

### Bundled Packs

Shipped with the Animus binary and embedded at build time.

### Machine-Installed Packs

Installed under:

```text
~/.animus/packs/<pack-id>/<version>/
```

These can be fetched from local paths, git repositories, or future registries.

### Project-Pinned Packs

Projects should pin the packs they use in project config:

```toml
[[packs]]
id = "animus.task"
version = "builtin"

[[packs]]
id = "jira.issue"
version = "0.3.0"
required = false
```

This keeps workflows reproducible and avoids silent changes from globally
installed packs.

## Migration Strategy

### Phase 1: Pack Registry

- add pack manifest support
- add workflow pack search paths
- add pack-qualified workflow refs
- keep current builtins as compatibility aliases

### Phase 2: MCP Pack Support

- move MCP server descriptors into pack-owned config
- resolve phase MCP availability from packs plus project overrides
- keep MCP host outside daemon-core

### Phase 3: Generic Subject Runtime

- replace `Task | Requirement | Custom` with generic subject refs
- migrate `SubjectDispatch`, workflow bootstrap, and execution facts
- preserve compatibility shims for current Animus CLI surfaces

### Phase 4: Built-In Domain Packs

- move task workflows into `animus.task`
- move requirement workflows into `animus.requirement`
- register task and requirement projectors/adapters via pack ids

### Phase 5: Native Module Registries

- add stable registries for subject adapters and projectors
- keep them feature-gated and statically linked
- do not introduce runtime-loaded Rust plugins into daemon-core

## Fit With Dumb Daemon

This model fits the dumb-daemon approach well because it gives the daemon less
to know, not more.

The daemon still only:

- consumes dispatches
- orders and starts work
- manages capacity and liveness
- emits facts

All product behavior moves outward:

- workflows from packs define behavior
- MCP servers from packs provide tool surfaces
- projectors and adapters from packs interpret domain facts

That is a cleaner end-state than keeping task and requirement semantics inside
daemon or CLI core.

## Acceptance Shape

The architecture is correct when:

- Animus can install and resolve pack-qualified workflows without daemon changes
- a pack can ship workflows, runtime overlays, MCP server descriptors, and
  schedules as one unit
- a pack can declare external runtime requirements such as Node.js or Python
  and execute them through command phases or MCP server processes
- `animus.task` and `animus.requirement` behave as installable packs rather than kernel
  special cases
- new subject kinds can be added without editing daemon-core dispatch logic
- MCP integrations are attached through packs and workflow policy, not daemon
  business logic
- native extension points exist for adapters and projectors, but the primary
  plugin model remains package- and process-oriented

## Relationship To Existing Work

| Requirement | Relationship |
|-------------|-------------|
| `REQ-035` | Generalizes pluggable task and requirement providers into pack-owned domain modules |
| `REQ-039` | Keeps the dumb-daemon boundary intact while moving more product logic into packs |
| `REQ-041` | Extends tool-driven mutation so domain packs can own their mutation surfaces |
| `REQ-049` | Builds on the domain-agnostic subject runtime and pushes it to a real pack model |
