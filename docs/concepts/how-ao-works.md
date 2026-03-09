# How AO Works: Core Architecture

## Core Principle

**Everything is a YAML workflow.**

The CLI does not contain AI logic. It dispatches YAML-defined workflows through a single execution path. Vision drafting, requirements generation, code implementation, review -- they are all workflows. The CLI is the remote control. The workflows are the brains.

```mermaid
flowchart LR
    subgraph CLI["ao CLI (thin dispatcher)"]
        direction TB
        cmd["Parse command"]
        dispatch["Emit SubjectDispatch"]
        output["Stream output"]
    end

    subgraph YAML["YAML Workflows (.ao/workflows/)"]
        direction TB
        builtin["Builtin Workflows<br/>vision-draft, requirements-draft<br/>requirements-refine, requirements-execute"]
        task_wf["Task Workflows<br/>standard, hotfix, research<br/>refactor, bugfix"]
        custom["Custom Workflows<br/>incident-response, lead-qualify<br/>nightly-ci, onboarding"]
    end

    subgraph ENGINE["Execution Engine"]
        runner["workflow-runner<br/>Resolves YAML, executes phases"]
        agents["AI Agents<br/>Each phase = agent + tools"]
        tools["MCP Tools<br/>ao, github, slack, db..."]
    end

    CLI --> YAML
    YAML --> ENGINE
    agents --> tools
    runner --> agents
```

---

## The Big Picture

Every interaction with AO follows the same path: a surface (CLI, Web, MCP) produces a `SubjectDispatch` envelope, the daemon schedules it, `workflow-runner` executes the YAML workflow, and projectors apply execution facts back to domain state.

```mermaid
flowchart TB
    subgraph YOU["You (Founder / PM)"]
        vision["ao vision draft<br/>dispatches builtin/vision-draft"]
        reqs["ao requirements draft<br/>dispatches builtin/requirements-draft"]
        execute["ao requirements execute<br/>dispatches builtin/requirements-execute"]
    end

    subgraph DAEMON["Daemon (Dumb Scheduler)"]
        tick["Tick Loop<br/>Consume SubjectDispatch"]
        queue["Dispatch Queue<br/>Priority-ordered"]
        spawn["Spawn workflow-runner<br/>Create worktree"]
        facts["Emit execution facts"]
    end

    subgraph RUNNER["workflow-runner (Execution Host)"]
        resolve["Resolve workflow_ref to YAML"]
        phases["Execute phases sequentially"]
        rework["Rework loop on failure"]
        result["Emit workflow result"]
    end

    subgraph PROJECTION["Projection Layer"]
        taskp["Task projector"]
        reqp["Requirement projector"]
        schedp["Schedule projector"]
        notifp["Notification projector"]
    end

    subgraph OUTPUT["Deliverables"]
        pr["Pull Requests"]
        state["Updated state"]
        metrics["Dashboard metrics"]
    end

    vision --> queue
    reqs --> queue
    execute --> queue
    tick --> queue
    queue --> spawn
    spawn --> resolve
    resolve --> phases
    phases --> rework
    rework --> phases
    phases --> result
    result --> facts
    facts --> PROJECTION
    PROJECTION --> OUTPUT
```

---

## Architecture: Three Layers

AO has exactly three layers. Each has a single responsibility.

### Layer 1: Surfaces (CLI, Web, MCP)

Surfaces accept user input and produce [SubjectDispatch](./subject-dispatch.md) values. They never run AI directly.

```mermaid
flowchart TB
    subgraph SURFACES["Ingress Surfaces"]
        cli["ao CLI commands"]
        web["Web API / Dashboard"]
        mcp["MCP tool calls"]
        cron["Cron schedules"]
        queue_in["Ready queue"]
    end

    subgraph CONTRACT["Dispatch Contract"]
        sd["SubjectDispatch<br/>subject + workflow_ref + input + trigger"]
    end

    SURFACES --> sd
```

Every workflow start -- whether from `ao vision draft`, a cron schedule, the ready queue, or an MCP tool call -- produces the same envelope. See [Subject Dispatch](./subject-dispatch.md) for the full field reference.

### Layer 2: Daemon Runtime (Dumb Scheduler)

The [daemon](./daemon.md) consumes `SubjectDispatch`, manages capacity, spawns `workflow-runner` subprocesses, and emits execution facts. It does not know about tasks, requirements, or business logic.

```mermaid
flowchart TB
    subgraph DAEMON["orchestrator-daemon-runtime"]
        consume["Consume SubjectDispatch"]
        capacity["Check capacity + headroom"]
        spawn["Spawn workflow-runner subprocess"]
        track["Track active subjects"]
        poll["Poll for completion"]
        emit["Emit execution facts"]
    end

    sd["SubjectDispatch"] --> consume
    consume --> capacity
    capacity --> spawn
    spawn --> track
    track --> poll
    poll --> emit
    emit --> facts["Execution Facts"]
```

**The daemon knows about:** subjects, dispatch envelopes, slots, headroom, subprocess lifecycle, runner telemetry.

**The daemon does NOT know about:** task status policy, backlog promotion, retry policy, requirement transitions, AI logic, git workflow policy.

### Layer 3: Workflow Runner (Execution Host)

`workflow-runner` resolves `workflow_ref` from YAML and executes phases. This is where all AI behavior lives. See [Agents and Phases](./agents-and-phases.md) for details on phase execution.

```mermaid
flowchart TB
    subgraph RUNNER["workflow-runner"]
        resolve["Resolve workflow_ref to YAML definition"]
        state["Initialize workflow state machine"]
        loop["Phase execution loop"]
        agent["Spawn agent for phase"]
        decision["Collect PhaseDecision"]
        gate["Evaluate gates + guards"]
        transition["Apply transition<br/>advance / rework / skip / fail"]
        complete["Emit workflow result"]
    end

    resolve --> state --> loop
    loop --> agent --> decision --> gate --> transition
    transition -->|"next phase"| loop
    transition -->|"rework"| loop
    transition -->|"done"| complete
```

---

## Key Architecture Patterns

| Pattern | Description |
|---------|-------------|
| [Subject Dispatch](./subject-dispatch.md) | All work flows through a unified `SubjectDispatch` envelope. One envelope, one execution path. |
| [Dumb Daemon](./daemon.md) | The daemon is a scheduler, not a feature host. It manages capacity and subprocesses. |
| [Tool-Driven Mutation](./mcp-tools.md) | Agents mutate state through MCP tools, not through daemon-internal logic. All changes are auditable. |
| Projectors | Execution facts from `workflow-runner` are projected onto domain state by projectors (task, requirement, schedule, notification). |
| [Worktree Isolation](./worktrees.md) | Every task executes in its own git worktree. Agents can code, test, and commit independently. |
| Self-Correcting Pipelines | The rework loop is the quality guarantee. Code review sends work back with failure context. |
