# Plugin Pack Kernel Architecture

## Purpose

AO should evolve from a product with built-in task and requirement semantics
into a workflow kernel that can host bundled and third-party domain packs.

The daemon stays dumb. The workflow runner stays the execution host. Product
domains such as tasks, requirements, incidents, CRM leads, or external issue
trackers should arrive as installable plugin packs that can ship workflows, MCP
servers, runtime overlays, and optional native adapters.

This document defines that target shape.

## Core Decision

AO should adopt a package-style plugin system over the existing dumb-daemon
kernel.

- The kernel owns dispatch, capacity, subprocesses, workflow execution, and
  execution facts.
- Plugin packs own domain workflows, MCP server configuration, mutation
  surfaces, schedules, and subject-specific behavior.
- Built-in `task` and `requirements` become first-party bundled plugin packs,
  not special cases in daemon-core.
- AO should prefer process- and package-oriented plugins over runtime-loaded
  Rust dynamic libraries.

## Non-Decision

This is not a proposal for:

- runtime-loaded Rust `.so` / `.dylib` / `.dll` plugins in the daemon
- embedding third-party Rust code into the daemon process at runtime
- letting workflows mutate AO state through hidden daemon-only side effects

Those choices would weaken isolation and make the daemon harder to keep dumb.

## Plugin Tiers

Not every extension needs native code. AO should support three extension tiers.

### Tier 1: Declarative Pack

Ships only data and configuration:

- workflow YAML files
- phase catalog overlays
- agent/runtime overlays
- MCP server descriptors
- schedules
- subject schemas and display metadata

This should cover most AO extensions.

### Tier 2: Connector Pack

A declarative pack plus MCP-backed integration behavior:

- external MCP server processes
- AO MCP tool namespaces
- connector-specific workflow bundles
- import/export or sync workflows

This should cover Jira, Linear, GitHub, CRM, incident, and support-style
extensions.

### Tier 3: Native Module

A compiled module linked into AO behind Cargo features for behavior that cannot
be expressed through workflows, projectors, MCP tools, or schemas alone.

Examples:

- worktree / execution-cwd policy for a subject kind
- custom subject resolution
- projector logic for a new subject kind
- provider-backed list/query services with local caching

Native modules should be rare and treated as bundled or feature-gated modules,
not arbitrary runtime-loaded code.

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

- `ao.task`
- `ao.requirement`
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

### Manifest Example

```toml
schema = "ao.pack.v1"
id = "ao.requirements"
version = "0.1.0"
kind = "domain-pack"
title = "AO Requirements"
description = "Planning and materialization flows for requirement subjects."

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.requirement"]
default_kind = "ao.requirement"

[workflows]
root = "workflows"
exports = [
  "ao.requirements/draft",
  "ao.requirements/refine",
  "ao.requirements/execute",
]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[mcp]
servers = "mcp/servers.toml"
tools = "mcp/tools.toml"

[schedules]
file = "schedules/schedules.yaml"

[native_module]
feature = "plugin-ao-requirements"
module_id = "ao.requirements"
optional = true
```

## Workflow Pack Loading

Plugin packs should become a first-class workflow source alongside builtins and
project-local YAML.

### Resolution Order

1. Project-local overrides in `.ao/plugins/<pack-id>/`
2. Project-local ad hoc workflows in `.ao/workflows/`
3. Installed pack workflows in `~/.ao/packs/<pack-id>/<version>/`
4. Bundled first-party packs embedded into the AO binary

This preserves the current override model while allowing installable pack
distribution.

### Workflow Ref Format

Workflow refs should move toward pack-qualified names:

- `ao.task/standard`
- `ao.task/quick-fix`
- `ao.requirements/execute`
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

### Hosting Rule

The daemon should not become an integration host. MCP servers should be started
by the workflow execution layer or a dedicated MCP host layer, scoped to the
workflow or phase as policy requires.

This preserves the dumb-daemon boundary.

## Native Module Surface

Some domains need behavior beyond workflows and MCP tools. For those cases AO
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

The registry can ship built-in implementations for `ao.task` and
`ao.requirement`, while future packs register additional implementations.

## Built-In Packs

The current AO product domains should become bundled packs.

### `ao.task`

Owns:

- task subject kind
- standard delivery workflows
- worktree-aware execution adapter
- task status projector
- task mutation tool namespace
- scheduled work planner and reconciler workflows

### `ao.requirement`

Owns:

- requirement subject kind
- planning workflows
- requirement lifecycle projector
- requirement mutation tools
- task materialization workflows

### `ao.review`

Owns:

- review and QA gate workflows
- approval records and review projectors
- associated MCP surfaces

This lets AO keep first-party defaults without baking those domains into the
kernel.

## State and Installation Model

Plugin packs should be installable at two levels.

### Bundled Packs

Shipped with the AO binary and embedded at build time.

### Machine-Installed Packs

Installed under:

```text
~/.ao/packs/<pack-id>/<version>/
```

These can be fetched from local paths, git repositories, or future registries.

### Project-Pinned Packs

Projects should pin the packs they use in project config:

```toml
[[packs]]
id = "ao.task"
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
- preserve compatibility shims for current AO CLI surfaces

### Phase 4: Built-In Domain Packs

- move task workflows into `ao.task`
- move requirement workflows into `ao.requirement`
- register built-in task and requirement projectors/adapters via pack ids

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

- AO can install and resolve pack-qualified workflows without daemon changes
- a pack can ship workflows, runtime overlays, MCP server descriptors, and
  schedules as one unit
- `ao.task` and `ao.requirement` behave as bundled packs rather than kernel
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
