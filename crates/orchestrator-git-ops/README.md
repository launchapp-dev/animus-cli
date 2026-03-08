# orchestrator-git-ops

Safe, reusable Git operations for AO daemon and workflow flows.

## Overview

`orchestrator-git-ops` consolidates Git-facing behavior that would otherwise be scattered across runtime crates. It handles worktree naming and cleanup, post-success merge and PR flows, git-integration retries, and runtime binary refresh when `main` advances.

## Targets

- Library: `orchestrator_git_ops`

## Architecture

```mermaid
graph TD
    subgraph "orchestrator-git-ops"
        HELP["daemon_git_helpers.rs"]
        WORKTREE["daemon_git_worktree.rs"]
        MERGE["daemon_git_merge.rs"]
        OUTBOX["daemon_git_integration.rs"]
        REFRESH["daemon_git_runtime_refresh.rs"]
    end

    MERGE --> HELP
    MERGE --> WORKTREE
    MERGE --> OUTBOX
    MERGE --> REFRESH
    WORKTREE --> HELP
    REFRESH --> HELP
```

## Key components

### Helpers

`daemon_git_helpers.rs` contains:

- post-success git config loading
- branch and worktree naming conventions
- repo AO root and worktree root resolution
- git command wrappers
- merge ancestry and branch-merged checks

### Worktree management

`daemon_git_worktree.rs` handles:

- parsing `git worktree list --porcelain`
- identifying task worktrees
- rebasing worktrees on main
- pruning completed-task worktrees
- cleanup of individual task worktrees

### Merge and PR flow

`daemon_git_merge.rs` owns:

- direct merge flow
- branch push helpers
- `gh`-based PR creation
- auto-merge enablement
- merge-conflict finalization
- post-success cleanup hooks

### Integration outbox and runtime refresh

- `daemon_git_integration.rs` persists retryable git operations into an outbox.
- `daemon_git_runtime_refresh.rs` tracks main-head advancement and can rebuild AO runtime binaries.

## Post-success flow

```mermaid
sequenceDiagram
    participant Runtime
    participant Merge as git_merge
    participant Worktree as git_worktree
    participant Refresh as git_runtime_refresh

    Runtime->>Merge: post_success_merge_push_and_cleanup()
    alt PR path
        Merge->>Merge: push branch + create PR + optional auto-merge
    else direct merge path
        Merge->>Merge: merge in worktree and push result
    end
    Merge->>Worktree: cleanup_task_worktree_if_enabled()
    Merge->>Worktree: auto_prune_completed_task_worktrees_after_merge()
    Merge->>Refresh: refresh_runtime_binaries_if_main_advanced()
```

## Workspace dependencies

```mermaid
graph LR
    GITOPS["orchestrator-git-ops"]
    CORE["orchestrator-core"]
    PROTO["protocol"]
    WFR["workflow-runner"]

    GITOPS --> CORE
    GITOPS --> PROTO
    GITOPS --> WFR
```

## Notes

- The crate shells out to `git`, `gh`, and in some refresh paths `cargo`.
- It is shared infrastructure, not the owner of workflow policy.
