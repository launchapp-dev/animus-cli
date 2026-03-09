# Status Values & Enums

All accepted enum values across the AO system. These values are case-insensitive on input and kebab-case on output unless otherwise noted.

---

## Task Status

Used by: `ao task status`, `ao task list --status`, `ao task update --status`

| Value | Aliases | Description |
|---|---|---|
| `backlog` | `todo` | Task is in the backlog, not yet ready for work |
| `ready` | | Task is ready to be picked up |
| `in-progress` | `in_progress`, `inprogress` | Task is actively being worked on |
| `blocked` | | Task is blocked by a dependency or issue |
| `on-hold` | `on_hold`, `onhold` | Task is paused/held |
| `done` | `completed` | Task is complete |
| `cancelled` | | Task has been cancelled |

**Serialization:** kebab-case (e.g., `"in-progress"`, `"on-hold"`)

**Groupings:**
- Active: `in-progress`
- Terminal: `done`, `cancelled`
- Blocked: `blocked`, `on-hold`

---

## Task Type

Used by: `ao task create --task-type`, `ao task list --task-type`

| Value | Aliases | Description |
|---|---|---|
| `feature` | | New feature |
| `bugfix` | `bug` | Bug fix |
| `hotfix` | `hot-fix` | Urgent fix |
| `refactor` | | Code refactoring |
| `docs` | `documentation`, `doc` | Documentation |
| `test` | `tests`, `testing` | Test work |
| `chore` | | Maintenance/chore |
| `experiment` | | Experimental work |

**Serialization:** lowercase (e.g., `"feature"`, `"bugfix"`)

---

## Task Priority

Used by: `ao task set-priority --priority`, `ao task create --priority`, `ao task list --priority`

| Value | Description |
|---|---|
| `critical` | Highest priority, blocks other work |
| `high` | High priority |
| `medium` | Normal priority |
| `low` | Low priority |

**Serialization:** lowercase (e.g., `"critical"`, `"high"`)

---

## Requirement Priority (MoSCoW)

Used by: `ao requirements create --priority`, `ao requirements update --priority`

| Value | Description |
|---|---|
| `must` | Must have (non-negotiable) |
| `should` | Should have (important but not critical) |
| `could` | Could have (nice to have) |
| `wont` | Won't have (explicitly excluded) |

**Serialization:** lowercase (e.g., `"must"`, `"should"`)

**Default:** `should`

---

## Requirement Status

Used by: `ao requirements update --status`, `ao requirements list --status`

| Value | Description |
|---|---|
| `draft` | Initial state, requirement is being drafted |
| `refined` | Requirement has been refined with details and criteria |
| `planned` | Requirement is planned for implementation |
| `in-progress` | Requirement is being implemented (alias: `in_progress`) |
| `done` | Requirement is fully implemented |
| `po-review` | Awaiting product owner review |
| `em-review` | Awaiting engineering manager review |
| `needs-rework` | Sent back for rework after review |
| `approved` | Approved through review process |
| `implemented` | Implementation complete and verified |
| `deprecated` | No longer relevant |

**Serialization:** kebab-case (e.g., `"in-progress"`, `"po-review"`, `"needs-rework"`)

**Default:** `draft`

---

## Workflow Phase Decision Verdict

Used in: `PhaseDecision.verdict`, `on_verdict` routing in workflow YAML

| Value | Description |
|---|---|
| `advance` | Phase succeeded; proceed to next phase or on_verdict target |
| `rework` | Phase needs rework; route back to the specified target phase |
| `skip` | Phase should be skipped; jump to the specified target |
| `fail` | Phase failed fatally; terminate workflow or route to error handler |

**Serialization:** lowercase (e.g., `"advance"`, `"rework"`)

Unknown verdict values are deserialized as a catch-all `Unknown` variant.

---

## Workflow Phase Status

Used in: `WorkflowPhaseExecution.status`

| Value | Description |
|---|---|
| `pending` | Phase has not started |
| `ready` | Phase is ready to execute |
| `running` | Phase is currently executing |
| `success` | Phase completed successfully |
| `failed` | Phase failed |
| `skipped` | Phase was skipped |

**Serialization:** snake_case (e.g., `"pending"`, `"success"`)

---

## Merge Strategy

Used in: `post_success.merge.strategy` in workflow YAML

| Value | Description |
|---|---|
| `merge` | Standard git merge (default) |
| `squash` | Squash merge into a single commit |
| `rebase` | Rebase onto target branch |

**Serialization:** lowercase

**Default:** `merge`

---

## Batch on_error Policy

Used by: `ao.task.bulk-status`, `ao.task.bulk-update`, `ao.workflow.run-multiple`

| Value | Description |
|---|---|
| `continue` | Process all items regardless of individual failures |
| `stop` | Stop after the first failure; remaining items are marked `skipped` |

---

## Agent Action

Used by: `ao agent control --action`, `ao.agent.control`

| Value | Description |
|---|---|
| `pause` | Pause the running agent |
| `resume` | Resume a paused agent |
| `terminate` | Terminate the agent |

See also: [Workflow YAML Schema](workflow-yaml.md), [Configuration](configuration.md), [Exit Codes](cli/exit-codes.md).
