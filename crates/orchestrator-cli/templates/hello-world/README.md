# Hello World Workflow

This is the canonical "first run" workflow installed by `animus init` (and
also available as `animus init --template hello-world` once the bundled
template registry is updated).

## What it does

A single-phase workflow that asks the Claude Haiku model (cheap + fast) to
say hello and list three things it can help with on this project. No file
edits, no state changes — purely a smoke test for the provider plugin →
runner → daemon pipeline.

## How to run it

```bash
# Synchronously, in your terminal (recommended for the first run):
animus workflow run hello-world --sync

# Or asynchronously via the daemon:
animus daemon start
animus workflow run hello-world
```

## How to inspect output

```bash
animus workflow list
animus output tail
animus output run --workflow-id <id>
```

## What to do next

Once you see the greeting come back, you know your install is working.
Replace this workflow with your own from `docs/concepts/workflows.md`, or
delete `.animus/workflows/hello-world.yaml` to keep the repo clean.
