# Actor-bound application contract

This document defines the v0.7 trust boundary between an authenticated
application host (for example LaunchApp Portal) and Animus. The application
authenticates the request. Animus accepts identity only through the trusted
process/control boundary and never from model output, tool payloads, YAML, or a
subject record.

## Actor

The host serializes the protocol `Actor` as JSON:

```json
{
  "user_id": "alice",
  "tenant_id": "workspace-7",
  "claims": ["application-defined-capability"]
}
```

Animus partitions ownership by `(user_id, tenant_id)`. Claims are forwarded to
plugins for their own authorization decisions; the kernel does not interpret
an `admin` claim or use mutable claims as record identity.

## MCP server call

An authenticated host starts:

```text
animus --project-root <root> mcp serve \
  --require-actor \
  --actor-json '<Actor JSON>'
```

`--require-actor` prevents an omitted or unserializable actor from producing a
global server. Invalid JSON fails startup. Runtime-contract assembly removes
any host-supplied actor flags and appends the authenticated actor, so a nested
agent cannot replace or remove the pin.

The actor-bound tool router currently exposes exactly:

- `animus.workflow.run`
- `animus.workflow.list`
- `animus.workflow.get`
- `animus.workflow.pause`
- `animus.workflow.cancel`
- `animus.workflow.resume`
- `animus.workflow.run-multiple`
- `animus.workflow.execute`
- `animus.workflow.phase.approve`
- `animus.workflow.phase.reject`
- `animus.workflow.decisions`
- `animus.workflow.checkpoints.list`
- `animus.workflow.config.get`
- `animus.workflow.config.validate`
- `animus.output.run`
- `animus.output.phase-outputs`
- `animus.subject.list`
- `animus.subject.get`
- `animus.subject.create`
- `animus.subject.update`
- `animus.subject.batch-create`
- `animus.subject.batch-update`
- `animus.subject.next`
- `animus.subject.status`
- `animus.agent.ask`
- `animus.agent.request_approval`
- `animus.interactions.list`
- `animus.interactions.answer`
- `animus.tools.search`
- `animus.tools.list`

MCP resources are unavailable on an actor-bound server because the resource
protocol has no actor carrier. Each MCP invocation emits an
`mcp_tool_invocation` audit record attributed to the pinned user and tenant.

## Direct application reads

The Portal subprocess contract is:

```text
animus --json --project-root <root> workflow list \
  [filters] [--limit <n>] [--offset <n>] --actor-json '<Actor JSON>'

animus --json --project-root <root> workflow get \
  --id <workflow_id> --actor-json '<Actor JSON>'

animus --json --project-root <root> output read \
  (--run-id <run_id> | --workflow-id <workflow_id> [--phase <phase_id>]) \
  --actor-json '<Actor JSON>'

animus --json --project-root <root> output phase-outputs \
  --workflow-id <workflow_id> [--phase-id <phase_id>] \
  --actor-json '<Actor JSON>'

animus --json --project-root <root> subject list \
  --kind <kind> [filters] --actor-json '<Actor JSON>'

animus --json --project-root <root> subject get \
  --kind <kind> --id <subject_id> --actor-json '<Actor JSON>'

animus --json --project-root <root> subject create \
  --kind <kind> --title <title> [fields] --actor-json '<Actor JSON>'

animus --json --project-root <root> subject update \
  --kind <kind> --id <subject_id> [patch fields] \
  --actor-json '<Actor JSON>'

animus --json --project-root <root> subject status \
  --kind <kind> --id <subject_id> --status <status> \
  --actor-json '<Actor JSON>'

animus --json --project-root <root> subject delete \
  --kind <kind> --id <subject_id> --yes --actor-json '<Actor JSON>'
```

Workflow lists filter ownership before applying `limit` / `offset` and include
`actor`, `workspace_id`, `initiated_by`, `visibility`, and `audience`
projection fields. JSON list/get results also project the current `agent_id`
from the current phase's resolved execution definition when that phase is in
agent mode; command/manual phases omit it. This is keyed by the persisted
`current_phase`, never inferred from a title. Workflow details and output reads
check the actor persisted when the workflow started.
A run-id read first resolves the owning workflow through its phase-session
checkpoint. Unowned, unknown, cross-user, and cross-tenant records are all
reported as not found.

Actor-bound pause, cancel, resume, approve, and reject calls apply the same
persisted `(user_id, tenant_id)` check before confirmation, phase validation,
environment teardown, or workflow mutation. Resume and manual-approval
continuation derive their runner actor and task-projection partition from the
persisted workflow owner, never from an untrusted payload and never from a
global fallback. A local operator can still control an actor-owned workflow,
but its lifecycle continues inside the original actor partition.

Actor-bound subject calls use the distinct `subject/v2/*` /
`<kind>/v2/*` protocol. Its required `SubjectRequestContext` carries the typed
actor and optional request, correlation, and idempotency identifiers. The
backend partitions every read, uniqueness check, idempotency lookup, update,
and delete by exact `(user_id, tenant_id)`. Legacy v1 rows have no owner
columns and are intentionally invisible to v2 calls. An actor-bound command
never falls back to v1 when an older backend does not implement v2.

The actor-bound interaction inbox uses:

```text
animus --json --project-root <root> agent interactions list \
  [--all] [--agent <agent_id>] --actor-json '<Actor JSON>'

animus --json --project-root <root> agent interactions show \
  <interaction_id> --actor-json '<Actor JSON>'

animus --json --project-root <root> agent interactions answer \
  <interaction_id> <answer flags> [--by <display name>] \
  --actor-json '<Actor JSON>'
```

Interaction records persist:

- `actor: { user_id, tenant_id }` as the creator principal reference (claims
  are not persisted);
- `workspace_id` as the tenant/workspace projection key;
- `initiated_by` as trusted creator attribution; and
- `eligible_responder_user_ids` as the responder allowlist.

Reads require the same tenant and either creator ownership or responder
eligibility. Answers require the same tenant and responder eligibility.
`answered_by` / `--by` is display attribution only and cannot grant access.
Legacy global records are not adopted into an authenticated partition.

## Correlation and idempotency

The subject v2 protocol defines typed request, correlation, and idempotency
fields, but the v0.7 CLI does not yet expose flags that populate them. The
application should retain those values in its transport/audit layer and must
not append unknown CLI flags until that CLI surface ships.

## Deliberately denied protocol gaps

These surfaces remain absent from actor-bound MCP until their protocols change:

- Config writes: `animus_config_protocol::ConfigWriteRequest` needs an
  `actor: Option<Actor>` field and config-source write implementations must
  resolve and write the same actor partition used by reads.
- Queue list/control/mutation: queue requests need actor filters and
  authorization context, and the queue/subject dispatch bridge must preserve
  the current `Actor`.
- MCP resources: list/read resource requests need the same typed actor/request
  context and backend enforcement.

There is no kernel-side “admin bypass” for these gaps. A future privileged
capability must be explicit in the typed protocol and enforced by the owning
backend.
