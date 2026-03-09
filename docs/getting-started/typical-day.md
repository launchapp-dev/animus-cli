# A Typical Day Using AO

This guide walks through the full lifecycle of using AO to build software, from initial idea to shipped features.

## The Full Lifecycle

```mermaid
flowchart TB
    IDEA["Your Idea"]
    --> VISION["ao vision draft<br/>Dispatches builtin/vision-draft workflow"]
    --> REQS["ao requirements draft<br/>Dispatches builtin/requirements-draft workflow"]
    --> EXECUTE["ao requirements execute<br/>Dispatches builtin/requirements-execute workflow<br/>Agent creates tasks via ao.task.create"]
    --> DAEMON["ao daemon start --autonomous"]

    DAEMON --> LOOP{"Daemon Tick"}

    LOOP -->|"SubjectDispatch dequeued"| SPAWN["Spawn workflow-runner"]
    SPAWN --> PIPELINE["YAML workflow pipeline<br/>triage > research > plan ><br/>implement > review > test > accept"]
    PIPELINE -->|"All phases pass"| PR["Auto-create PR + merge"]
    PIPELINE -->|"Phase fails"| REWORK["Rework or escalate"]
    REWORK --> PIPELINE
    PR --> DONE["Execution fact > task projector > Done"]
    DONE --> LOOP

    LOOP -->|"All tasks done"| SHIPPED["Feature Shipped"]

    SHIPPED -->|"Next sprint"| REQS
```

## Step-by-Step Walkthrough

### Step 1: Setup

```bash
ao setup
```

This creates `.ao/workflows/` with your workflow definitions and `.ao/state/` for runtime state. See [Project Setup](project-setup.md) for details on what gets created.

### Step 2: Define What You Are Building

```mermaid
flowchart LR
    A["ao vision draft<br/>Agent generates vision doc"]
    --> B["ao requirements draft<br/>Agent scans code + generates reqs"]
    --> C["ao requirements refine<br/>Agent sharpens acceptance criteria"]
    --> D["ao requirements execute<br/>Agent creates tasks + queues work"]
```

Every command dispatches a YAML workflow. The CLI streams output while the agent runs. Under the hood, each command follows the same execution path as any other workflow.

**The hierarchy:**

| Level | Entity | Created By |
|-------|--------|-----------|
| Vision | Single document | `builtin/vision-draft` workflow |
| Requirements | REQ-001..REQ-N | `builtin/requirements-draft` workflow |
| Tasks | TASK-001..TASK-N | `builtin/requirements-execute` workflow (agent uses `ao.task.create` MCP tool) |

### Step 3: Start the Daemon

```bash
ao daemon start --autonomous
```

The daemon runs a tick loop every 5 seconds:

```mermaid
flowchart TB
    subgraph TICK["Daemon Tick"]
        direction TB
        load["Load dispatch queue"]
        check["Check capacity"]
        dequeue["Dequeue highest priority SubjectDispatch"]
        spawn["Spawn workflow-runner<br/>with workflow_ref + subject"]
        poll["Poll running subprocesses"]
        emit["Emit execution facts"]
    end

    load --> check --> dequeue --> spawn --> poll --> emit
    emit -->|next tick| load

    subgraph LIMITS["Capacity Controls"]
        slots["Max concurrent workflows"]
        headroom["Slot headroom"]
        priority["Priority ordering"]
    end

    check -.-> LIMITS
```

The daemon is a dumb scheduler. It does not know what a "task" is or what "requirements" are. It processes `SubjectDispatch` envelopes, manages subprocess capacity, and emits execution facts.

### Step 4: Workflow Pipeline Executes

Each task runs through a multi-phase pipeline with specialized agents:

```mermaid
sequenceDiagram
    participant D as Daemon
    participant R as workflow-runner
    participant T as Triager Agent
    participant RS as Researcher Agent
    participant E as Engineer Agent
    participant CR as Code Reviewer
    participant TE as Tester Agent
    participant PO as PO Reviewer

    D->>R: SubjectDispatch { subject: TASK-001, workflow_ref: "standard-workflow" }
    R->>R: Resolve YAML, create worktree, init state machine

    R->>T: Phase: triage
    T->>T: ao.task.get TASK-001, validate, check duplicates
    T-->>R: verdict: advance

    R->>RS: Phase: research
    RS->>RS: Explore codebase, search docs
    RS-->>R: verdict: advance

    R->>E: Phase: implementation
    E->>E: Write code, run tests, git commit
    E-->>R: verdict: advance

    R->>CR: Phase: code-review
    CR->>CR: Review diff
    CR-->>R: verdict: rework (missing error handling)

    R->>E: Phase: implementation (rework 2/3)
    E->>E: Fix issues, recommit
    E-->>R: verdict: advance

    R->>CR: Phase: code-review (retry)
    CR-->>R: verdict: advance

    R->>TE: Phase: testing
    TE->>TE: cargo test --workspace
    TE-->>R: verdict: advance

    R->>PO: Phase: po-review
    PO->>PO: ao.task.checklist-update, verify ACs
    PO-->>R: verdict: advance

    R->>R: post_success: create PR + merge
    R-->>D: Execution fact: workflow completed
    D->>D: Project fact to task projector, TASK-001 status: done
```

Key points:
- Each phase runs a specialized agent with role-specific system prompts and MCP tool access.
- The **rework loop** is the quality guarantee. Code review can send work back to the engineer with failure context, up to a configurable number of attempts.
- Agents mutate AO state through MCP tools (`ao.task.update`, `ao.task.checklist-update`), not by editing JSON files directly.

### Step 5: Monitor Progress

```bash
# Task completion summary
ao task stats

# Daemon status and health
ao daemon status
ao daemon health

# List workflows and their status
ao workflow list

# Check for failed workflows
ao workflow list --status failed

# Stream agent output in real time
ao output tail

# Full project dashboard
ao status

# Web dashboard
ao web serve

# Terminal UI
ao tui
```

### Step 6: Review and Ship

When tasks complete, they produce pull requests:

```bash
# List generated PRs
gh pr list

# Review and merge
gh pr review <number> --approve
gh pr merge <number>
```

## Example: A Typical Day

```
Morning:
  $ ao vision draft
    Dispatches builtin/vision-draft workflow
    Agent analyzes project, generates vision doc
    Vision saved to .ao/state/vision.json

  $ ao requirements draft --include-codebase-scan
    Dispatches builtin/requirements-draft workflow
    Agent scans codebase, generates 12 requirements
    Requirements saved with acceptance criteria

  $ ao requirements execute --requirement-ids REQ-001..REQ-005
    Dispatches builtin/requirements-execute workflow
    Agent creates 15 tasks via ao.task.create MCP tool
    Tasks queued with priorities and dependencies

  $ ao daemon start --autonomous
    Daemon begins tick loop, picks up queued work

Afternoon:
  $ ao task stats
    9 done, 4 in-progress, 2 blocked (waiting on dependency)

  $ ao workflow list --status failed
    1 workflow failed at security-review (hardcoded API key detected)
    Engineer agent auto-reworked, now passing

Evening:
  $ ao task stats
    14 done, 1 in-progress

  $ gh pr list
    14 PRs ready for review
    Review, approve, merge

Next day:
  $ ao requirements execute --requirement-ids REQ-006..REQ-010
    Repeat cycle
```

## Failure Recovery

- **Phase fails**: Retried up to configured max rework attempts.
- **All retries exhausted**: Workflow fails, execution fact emitted, task projector marks the task as blocked.
- **Daemon crashes**: Orphan recovery runs on next startup.
- **Merge conflicts**: AI-powered conflict resolution in workflow phases.

## Next Steps

- [Project Setup](project-setup.md) -- Understand the `.ao/` directory structure.
- [Quick Start](quick-start.md) -- Run your first workflow.
