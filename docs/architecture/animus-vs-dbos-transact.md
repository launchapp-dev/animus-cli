# Animus vs DBOS Transact

> **Status:** Strategy artifact. Captured conversation comparing Animus to
> DBOS Transact, including a design sketch for a hypothetical DBOS-backed
> durable-step plugin for Animus. Verbatim from a Q&A; some paths reference
> the pre-v0.4.0 `.ao/` naming and have not been retroactively rewritten
> (see [naming-contract.md](./naming-contract.md) for the current convention).
>
> **v0.5 status:** No DBOS integration is in v0.5 scope. This document is
> preserved because the analysis informs future plugin-kind extensibility
> decisions. See [kernel-and-flavors.md](./kernel-and-flavors.md)
> "Future plugin kinds" for the v0.5 commitment.

A conversation captured verbatim comparing Animus (this repo) to DBOS Transact
(`dbos-transact-ts`, `dbos-transact-py`), including a design sketch for a
hypothetical DBOS-backed durable-step plugin for Animus.

---

## Q1: How is Animus different from `dbos-transact-ts` / `dbos-transact-py`?

Different layer of the stack entirely, even though both touch "agent
orchestration."

**DBOS Transact (ts/py)** is a *library* you embed in your application. You
annotate functions with `@workflow` / `@step`, and execution state is
checkpointed to **Postgres** so functions resume exactly-once across crashes.
It's a general durable-execution primitive — payments, pipelines, webhooks,
queues — and AI agents are one example use case because they call flaky APIs.
The unit of work is "a Python/TS function call in your process."

**Animus (this repo)** is a *standalone Rust CLI + daemon* that orchestrates
**coding-agent CLI subprocesses** (Claude Code, Codex, etc.) on your repo.
The unit of work is a task → workflow → agent run executing inside a **git
worktree**, with state stored as scoped JSON under `~/.ao/<repo-scope>/` and
workflow YAML overlays in `.ao/`. There's a queue, daemon, web UI, MCP
surface, and built-in concepts like vision/requirements/architecture/skills.

### Concrete contrasts

|                      | DBOS Transact                          | Animus                                                        |
| -------------------- | -------------------------------------- | ------------------------------------------------------------- |
| **Form factor**      | Library you import                     | CLI + daemon you run                                          |
| **Lives in**         | Your app process                       | Your repo, as a tool                                          |
| **State store**      | Postgres                               | Scoped JSON + git worktrees                                   |
| **Unit of work**     | Annotated function                     | Agent run in a worktree                                       |
| **Language coupling**| TS / Python                            | Rust core; agents are language-agnostic CLI subprocesses      |
| **Primary user**     | App dev adding durability to a service | Dev running autonomous coding agents on a repo                |
| **AI orientation**   | General-purpose; agents are one use case | AI-coding-agent orchestrator end-to-end                     |

If you wanted, you could in principle build something like Animus's task/queue
layer *on top of* DBOS for durability — they're not competitors so much as
different altitudes. DBOS gives you "this function will finish exactly once";
Animus gives you "this coding agent will pick up TASK-123, branch a worktree,
run, and land a PR."

---

## Q2: Can I use either one fully in replacement of the other? Why or why not?

**No, neither replaces the other** — they operate at different layers, and
the surface area each provides isn't a superset of the other.

**Animus → DBOS: no.** Animus can't be dropped into your Node or Python
service to make a `chargeCustomer()` function resumable. It doesn't run inside
your app process, doesn't checkpoint function calls, has no Postgres-backed
exactly-once semantics, and exposes no `@workflow` / `@step` decorators. Its
"workflows" are YAML pipelines of agent runs in git worktrees — useless if what
you wanted was durable in-process execution of arbitrary code.

**DBOS → Animus: no.** DBOS gives you primitives (durable functions, queues,
cron, notifications). It has no notion of: coding-agent CLI subprocesses,
Claude Code / Codex / Cursor providers, git worktree isolation per run, `.ao/`
project state, scoped runtime under `~/.ao/<repo-scope>/`, tasks / vision /
requirements / architecture, an MCP server surface, a daemon that picks up
work and dispatches it to agents, or the web UI for observing runs. You'd
have to build all of that on top — at which point you've built Animus.

### Where they could compose, not substitute

- You could plausibly use DBOS *inside* Animus's daemon to make its tick loop
  and queue drain durable across crashes (Animus currently uses scoped JSON;
  DBOS would move that to Postgres). That's an implementation detail swap,
  not a replacement.
- You could call Animus from a DBOS `@step` if you wanted an app workflow to
  kick off a coding-agent run and treat that as one durable unit.

**The simple test:** can the tool do the *primary* thing the other is built for?

- Use DBOS to autonomously implement TASK-123 in a worktree and open a PR? **No.**
- Use Animus to make a payment function resume exactly-once after a Node
  process crash? **No.**

Different altitudes. The overlap in vocabulary ("workflow", "queue", "task")
is misleading — the nouns refer to different things.

---

## Q3: What about the latest stuff on the remote repo? It's a plugin architecture now: `https://github.com/launchapp-dev/animus-cli`

The plugin architecture broadens *what Animus integrates with*, not *what
Animus does*. The substitution answer is the same.

**What plugins actually extend:** providers (Claude/Codex/Gemini), subject
backends (where tasks come from — Linear, SQLite, Markdown), triggers
(webhook/Slack/file-watcher), transports (HTTP/GraphQL/web UI), log storage.
There is no "generic durable function" plugin kind. The unit of work is still
an agent run in a git worktree — plugins just let you swap *which LLM*,
*which task source*, *what triggers it*, and *who can see the output*.

### Does this narrow the gap with DBOS?

Slightly, in cosmetics:

- Triggers ≈ DBOS's "exactly-once event processing" entry points
- Workflow Packs ≈ reusable workflow modules
- Subject backends ≈ pluggable work sources

But the load-bearing thing DBOS sells — *"this function in your app process
will resume exactly-once across crashes, checkpointed to Postgres"* — has no
plugin surface in Animus. You can't write an `animus-runner-python-function`
plugin that turns Animus into a DBOS replacement, because the runtime model
is "fork a CLI subprocess into a worktree and watch it," not "wrap a function
call with checkpoints inside your service."

**Reverse direction also unchanged.** No DBOS plugin makes it spawn Claude
Code in a worktree, manage merge gates, or run an autonomous coding org. DBOS
has no concept of providers, subjects, agents, or worktrees — and adding them
would be reinventing Animus inside a Postgres library.

### Where the plugin model does change the framing

- Animus is now more credibly a *platform* than a *tool*. The core daemon +
  control protocol + signed plugin distribution looks like an extensible
  runtime, not just a CLI.
- It makes the "compose, don't substitute" answer more interesting: a future
  `animus-step-durable` plugin backed by DBOS Postgres checkpointing for the
  daemon's own state would be a natural fit. The plugin boundary makes that
  integration practical instead of a fork.

So: same altitude difference, same non-substitution. The plugin architecture
just makes Animus easier to extend along its existing axis — which is
"orchestrate coding agents," not "make arbitrary code durable."

---

## Q4: Could that durable-step plugin actually work? Explain in detail how this might work.

Yes, it can work, but the design is non-trivial because DBOS's guarantees are
*in-process* and Animus's plugins are *separate stdio subprocesses*. That gap
shapes the whole architecture.

### What needs to be durable

Today Animus persists a lot already (scoped JSON, runs, artifacts), but the
in-flight reconciliation is ad-hoc — note that
`cancel_orphaned_running_workflow.rs` exists precisely because the daemon
currently has to clean up workflows that were mid-flight when it died. The
thing missing is *step-level checkpointing within a phase*: "I dispatched the
agent, got `run_id X`, was about to evaluate the decision contract when I
crashed."

A phase typically decomposes into:

1. Build dispatch command from workflow + task state
2. Spawn agent run (Claude/Codex/Gemini subprocess in a worktree)
3. Wait for it to finish
4. Read artifacts, evaluate decision contract (advance / rework / skip / fail)
5. Apply git ops (rebase, merge, push)
6. Notify, enqueue next phase

Today, a crash anywhere in 1–6 leaves an orphan. With durable steps, the
daemon resumes from the last committed step.

### A new plugin kind

The existing surfaces (provider, subject backend, trigger, transport, log
storage) don't fit. You'd add a **`step-store`** (or **`checkpoint`**) plugin
kind. The control protocol over the existing Unix-socket stdio frame would be
roughly:

```text
begin_workflow_run {run_id, phase_id, inputs}
  -> {epoch}
begin_step {run_id, phase_id, step_name, idempotency_key, payload}
  -> {step_id, status: new | already_committed, prior_output?}
commit_step {step_id, output | error}
  -> {ack}
recover_in_flight {since_epoch}
  -> [{run_id, phase_id, last_committed_step, replay_state}]
```

The plugin's job: "before I let you do side-effect X, write a row; after you
do X, write the result. On startup, tell me what's in flight."

### Two ways to wire DBOS underneath

**Option A — DBOS as a durable journal.** The plugin is a tiny TS process
whose only DBOS workflow is `recordStep`. Each `begin_step` / `commit_step`
call from Animus becomes a DBOS step backed by Postgres. The plugin gives you
exactly-once writes and crash-safe recovery of *checkpoint state*, but
Animus's daemon still drives the loop. On daemon restart, it calls
`recover_in_flight`, gets back the journal, and resumes its own state
machine.

This is the easy, faithful-to-the-plugin-model path. You're using DBOS as a
write-ahead log with niceties.

**Option B — DBOS as the workflow runner.** Invert control: the *entire*
phase loop is a DBOS workflow inside the plugin process.

```typescript
@DBOS.workflow()
async function executePhase(runId, phaseId, ctx) {
  const dispatch = await DBOS.step(animus.buildDispatch)(runId, phaseId);
  const agentRunId = await DBOS.step(animus.spawnAgent)(
    dispatch,
    { idempotencyKey: `${runId}:${phaseId}:spawn` }
  );
  const result = await DBOS.step(animus.waitForAgent)(agentRunId);
  const verdict = await DBOS.step(animus.evaluateContract)(result, ctx.contract);
  if (verdict === "rework" && ctx.reworkCount < ctx.maxRework) {
    return executePhase(runId, phaseId, { ...ctx, reworkCount: ctx.reworkCount + 1 });
  }
  await DBOS.step(animus.applyGitOps)(runId, phaseId, verdict);
  return verdict;
}
```

The Animus daemon shrinks to a dispatcher ("start workflow X") and a callback
server that handles `spawnAgent`, `waitForAgent`, etc. as inbound RPCs from
the plugin. Recovery is automatic: on plugin restart, DBOS replays the
workflow.

This is the more architecturally satisfying version. It's also a major
commitment: the plugin is now load-bearing core, just written in TS.

### The four hard problems

**1. Idempotency across the IPC boundary.** DBOS guarantees a step runs once
*from its perspective* — meaning it'll retry until `commit_step` returns. If
the retry path is `spawnAgent`, you'll fork two Claude Code processes unless
Animus dedupes. Every side-effecting callback into Animus must accept an
idempotency key and return the prior result on replay. That means new code
in `agent-runner`, `orchestrator-git-ops`, `orchestrator-notifications` —
anywhere a step touches the world.

**2. Reattaching to live agent subprocesses.** The genuinely hard one. If
the daemon crashes while a Claude Code run is in flight, three cases:

- **Detached and still running.** You need to find it (PID file in the
  worktree? named socket?) and reattach to its stdout/event stream. Animus
  would need to spawn agents with `setsid` / `nohup` semantics and a
  discoverable handle. Not impossible, but a behavioral change.
- **Died with the daemon.** Worktree has partial state. Either resume from
  artifacts (if the agent flushed) or re-spawn with `resume from commit X`
  semantics if the provider supports it (Claude Code does, sort of, via
  session resumption).
- **Finished after the daemon died.** Artifacts are on disk; reconciler
  reads them on recovery and treats the step as committed.

DBOS gives you the workflow-level durability for free; the agent-level
durability you have to design yourself. Without solving this, durable steps
just mean "restart the phase from scratch," which is barely better than
today.

**3. Decision contracts and rework loops.** Replayable code must be
deterministic. If `evaluateContract` calls an LLM for "did this PR pass
review?", the replay must return the same verdict — meaning the verdict has
to be a recorded step output, not a re-invocation. Same for any random or
time-based logic in routing.

**4. Schema and migrations.** DBOS owns the Postgres schema for its
workflows. The Animus core still has its own state in
`~/.ao/<repo-scope>/`. Two sources of truth that must agree about "is run X
complete?" — easy to get drift between them. Either DBOS's tables become the
source of truth (and scoped JSON becomes a cache), or you accept eventual
consistency with a reconciler.

### What you'd actually get

If you build **Option A**, you get: clean crash recovery for the daemon's
*bookkeeping*, no more orphan cleanup logic, no more "I think this run
finished but I'm not sure." Modest, real win.

If you build **Option B with detached agent execution**: a daemon you can
`kill -9` mid-workflow and it picks up exactly where it left off, *including*
in-flight agent runs. That's a genuinely different operational story — you
can deploy mid-workflow, you can run on flaky infra, you can be confident no
work is lost. Big win, big engineering bill.

### Honest take

**Option A is worth doing** and is a near-perfect fit for the plugin
architecture. It treats DBOS as what it is — a durable execution library —
and uses it for the part it's best at (transactional state). Maybe a
weekend's work for a prototype plugin, a few weeks to harden.

**Option B is the more exciting architecture** but it's a 3–6 month refactor
and the gating problem isn't DBOS, it's agent reattach. You'd want to solve
detached agent execution as a standalone feature first (it has value even
without durable steps), and then layer DBOS-driven orchestration on top.
Doing them together couples two risky changes.

Either way: the plugin model makes it cleanly possible. Pre-plugin, you'd
have been forking the daemon.

---

## Implications for v0.5 (Animus side)

This document is preserved for the architectural insight, not as a v0.5
commitment. Concretely:

- **NOT in v0.5 scope:** building any DBOS-backed plugin, the `step-store`
  plugin kind, or the detached-agent-execution work.
- **Implicit in v0.5:** the kernel + flavors model (see
  [kernel-and-flavors.md](./kernel-and-flavors.md)) must keep the plugin host
  extensible enough that future plugin kinds — including `step-store` /
  `checkpoint` — can be added without modifying the kernel beyond a typed
  enum extension. The kernel-and-flavors doc has been updated to reflect this.
- **Future scope candidate:** `animus-step-durable` (Option A) is a strong
  candidate for v0.6.x or v0.7.x once the durability plugin kinds and the
  maintenance pack ship. Option B is later still and gated on solving
  detached agent execution as a standalone feature.

When the durable-step plugin work eventually starts, this document is the
canonical design sketch to begin from.
