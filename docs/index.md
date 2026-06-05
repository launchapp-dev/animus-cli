---
layout: home

hero:
  name: ANIMUS
  text: The runtime for the agent era.
  tagline: Orchestrate AI agents from your terminal. Define YAML workflows, dispatch to any model, and ship code autonomously — it shouldn't be you.
  image:
    src: /logo.svg
    alt: Animus
  actions:
    - theme: brand
      text: Get Started →
      link: /getting-started/
    - theme: alt
      text: View on GitHub
      link: https://github.com/launchapp-dev/animus-cli

features:
  - icon: "\U0001F3AF"
    title: Workflow-First
    details: Every AI operation is a YAML workflow. Define multi-phase pipelines with gates, rework loops, and sub-workflows.
    link: /concepts/workflows
    linkText: Learn about workflows
  - icon: "\U0001F916"
    title: Multi-Agent
    details: Orchestrate Claude, Codex, Gemini, and OpenAI from one CLI. Each phase picks the right model for the job.
    link: /concepts/agents-and-phases
    linkText: See how agents work
  - icon: "\U0001F504"
    title: Autonomous Daemon
    details: A dumb scheduler that dispatches SubjectDispatch envelopes. No business logic — just capacity management and phase execution.
    link: /concepts/daemon
    linkText: Understand the daemon
  - icon: "\U0001F527"
    title: MCP Tools
    details: 77 built-in MCP tools for subject management, workflow control, plugin operations, output inspection, and runtime state mutations. Agents act through tools, not code.
    link: /concepts/mcp-tools
    linkText: Explore MCP tools
  - icon: "\U0001F333"
    title: Git Worktrees
    details: Each task gets an isolated git worktree. Parallel execution without branch conflicts. Clean merges back to main.
    link: /concepts/worktrees
    linkText: Learn about isolation
  - icon: "\U0001F680"
    title: Built in Rust
    details: Rust-only Cargo workspace with atomic state persistence, async Tokio runtime, and zero desktop dependencies. Fast and reliable.
    link: /architecture/crate-map
    linkText: See the architecture
---
