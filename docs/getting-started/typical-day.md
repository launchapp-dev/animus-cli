# A Typical Day Using Animus

Animus is built for continuous execution. You define work through the subject
surface, mark it ready, and let the daemon or workflow runtime execute it.

## The Autonomous Workflow

```mermaid
flowchart TB
    IDEA["Your Idea"]
    --> REQ["animus subject create --kind requirement"]
    --> TASK["animus subject create --kind task"]
    --> READY["animus subject status --kind task --status ready"]
    --> DAEMON["animus daemon start"]

    DAEMON --> LOOP{"Ready subject in queue?"}
    LOOP -->|"yes"| WORKFLOW["Spawn workflow runner"]
    WORKFLOW --> RUNNER["AI agents execute phases"]
    RUNNER --> FACTS["Execution facts"]
    FACTS --> STATE["Subjects, workflows, reviews, outputs updated"]
    STATE --> LOOP
```

## The Daily Operator Loop

Day to day, you cycle through a short loop: check the inbox, pick the next subject, enqueue or run it, watch the daemon, then review and merge the result.

```mermaid
flowchart LR
    STATUS["Check inbox (animus status)"]
    --> PICK["Pick next subject (animus subject next)"]
    --> RUN["Enqueue or run (animus queue / workflow run)"]
    --> MONITOR["Monitor daemon (animus daemon health)"]
    --> REVIEW["Review and merge result"]
    --> STATUS
```

## Typical Flow

### 1. Create work

```bash
animus subject create --kind requirement \
  --title "Rate limiting rollout" \
  --body "Protect the API from burst traffic."

animus subject create --kind task \
  --title "Add rate limiting" \
  --body "Implement request throttling before upstream calls." \
  --priority p1
```

### 2. Mark a task ready

```bash
animus subject status --kind task --id task:TASK-001 --status ready
```

### 3. Start the daemon

```bash
animus daemon start
```

### 4. Monitor progress

```bash
animus status
animus subject list --kind task
animus workflow list
animus daemon health
animus logs tail
```

## Testing a Workflow Before Enabling the Daemon

```bash
animus workflow run --task-id TASK-001 --sync
```

Use synchronous runs to debug a workflow definition, prompt, or plugin setup in
the current terminal. Built-in tasks normally run in managed worktrees; tasks
resolved from a `subject_backend` plugin run from `project_root` unless that
plugin supplies its own checkout strategy.

## Separation of Concerns

- Project configuration lives in `.animus/`.
- Repo-scoped runtime state lives in `~/.animus/<repo-scope>/`.
- Workflow logic lives in YAML.
- The daemon is a scheduler, not the place where product policy lives.
