# Concepts

This section explains the core ideas behind AO. Each page covers one architectural concept in depth.

## Pages

- [How AO Works](./how-ao-works.md) -- Core architecture, the three-layer model, and the big picture.
- [Workflows](./workflows.md) -- Everything is a YAML workflow: builtin, task, and custom workflows.
- [Subject Dispatch](./subject-dispatch.md) -- The universal work envelope that drives all execution.
- [The Daemon](./daemon.md) -- The dumb scheduler: tick loop, capacity, and execution facts.
- [Agents and Phases](./agents-and-phases.md) -- AI personas, phase execution, rework loops, and phase guards.
- [MCP Integration](./mcp-tools.md) -- How agents use MCP tools to observe and mutate state.
- [State Management](./state-management.md) -- The `.ao/` directory, atomic writes, and mutation policy.
- [Worktree Isolation](./worktrees.md) -- Every task gets its own git worktree for safe parallel execution.
