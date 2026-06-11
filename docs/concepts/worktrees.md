# Worktree Isolation

## In-Tree Tasks Get Managed Worktrees

When the [daemon](./daemon.md) dispatches a task workflow backed by the built-in
task store, Animus creates a dedicated git worktree for that task. This lets an
agent write code, run tests, and commit without interfering with other running
tasks or the main checkout.

Tasks resolved from an installed `subject_backend` plugin are different: the
workflow runtime can resolve them by subject id, but there is no in-tree task
record to provision against, so execution stays in `project_root` unless the
plugin manages its own checkout strategy.

---

## Managed Worktree Path

Worktrees are stored under the repository-scoped state directory:

```
~/.animus/<repo-scope>/worktrees/<task-id>/
```

Where `<repo-scope>` is the sanitized repository name plus a SHA-256 hash prefix (see [State Management](./state-management.md) for the scoping rules).

For example:

```
~/.animus/my-saas-a1b2c3d4e5f6/worktrees/TASK-042/
```

---

## Managed Branch Naming

Each managed worktree gets a dedicated branch:

```
animus/<sanitized-task-id>
```

For example, task `TASK-042` gets branch `animus/task-042`. The task ID is sanitized to lowercase with special characters replaced by hyphens, using the same `sanitize_identifier` function used for repository scoping.

---

## Isolation Guarantees

Because built-in task execution runs in its own worktree:

- **No file conflicts** -- Two agents implementing different tasks modify files independently.
- **Independent test runs** -- `cargo test` (or any test command) runs against the task's own working tree.
- **Clean git history** -- Each task's commits are on its own branch, making PRs clean and reviewable.
- **No main branch pollution** -- The main checkout stays untouched while tasks execute.

---

## Worktree Lifecycle

```mermaid
flowchart LR
    dispatch["SubjectDispatch<br/>for TASK-042"]
    create["Create worktree<br/>~/.animus/.../worktrees/TASK-042/<br/>Branch: animus/task-042"]
    execute["Execute workflow phases<br/>Agent writes code, tests, commits"]
    result["Workflow completes"]
    post["Post-success actions"]
    cleanup["Cleanup worktree"]

    dispatch --> create --> execute --> result --> post --> cleanup
```

### 1. Create

When the daemon spawns `workflow-runner` for a built-in task subject, it
creates a git worktree from the current main branch. The worktree is checked
out to a new branch named `animus/<task-id>`.

### 2. Execute

For built-in task subjects, all workflow phases run inside the worktree
directory. Agents can:

- Read and write files
- Run build and test commands
- Create git commits
- Access MCP tools

The agent's working directory is set to the managed worktree path.

For plugin-owned task subjects, the execution cwd is `project_root`. That keeps
plugin-backed task workflows runnable even when the plugin does not expose an
Animus-managed worktree path.

### 3. Post-success actions

After all phases pass, the workflow can perform post-success actions defined in the YAML:

| Action | Effect |
|--------|--------|
| `create_pr: true` | Create a pull request from the task branch to the target branch. |
| `auto_merge: true` | Merge the PR after creation (if CI passes). |
| `cleanup_worktree: true` | Remove the worktree directory after merge. |

### 4. Cleanup

When cleanup runs, the worktree directory is removed and the local branch can
be pruned. If cleanup is not configured, the worktree persists for manual
inspection.

---

## Merge Conflict Recovery

If the task branch has conflicts with the target branch, workflow-runner can attempt AI-powered conflict resolution as a workflow phase. The merge recovery logic detects conflicts, presents them to an agent, and the agent resolves them before committing.

If automatic resolution fails, the workflow is marked as blocked and the conflict is reported for manual intervention.

---

## Managing Worktrees

```bash
animus git worktree list          # List active worktrees
animus git worktree prune         # Remove managed worktrees for done/cancelled tasks
animus runner orphans detect      # Detect orphaned runner processes in stale worktrees
```

The daemon performs orphan recovery on startup for workflows whose runner
processes are no longer alive, but it skips workflows started within the last
90 seconds so async `workflow run` dispatch has time to register its pid/run
metadata before reconciliation.
