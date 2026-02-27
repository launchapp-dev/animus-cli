# TASK-060 Requirements: Design EM Agent Prompt and Scheduling Decision Contract

## Phase
- Workflow phase: `requirements`
- Workflow ID: `4f9d9307-3bee-4ea9-9098-d7c1c79328bb`
- Task: `TASK-060`
- Requirement: unlinked in current task metadata

## Objective
Define a deterministic prompt template and machine-readable decision contract for
an EM (engineering manager) scheduling agent that recommends which tasks to
queue, defer, or preempt.

Target behavior from task brief:
- Input includes current tasks (status, priority, dependencies, linked
  requirements), running workflows/phases, recent decisions/failures, and
  available agent slots.
- Output is a single scheduling decision object:
  `{"kind":"em_scheduling_decision","queue":[{"task_id":"TASK-XXX","reason":"..."}],"defer":["TASK-YYY"],"preempt":["TASK-ZZZ"]}`
- Decision logic must account for priority, dependencies, risk, recent failures,
  requirement coverage, and resource constraints.
- EM produces scheduling decisions only (no code edits, no direct state mutation).

## Current Baseline Audit
Snapshot date: `2026-02-27`.

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Ready-task selection | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` (`run_ready_task_workflows_for_project`) | daemon starts workflows from `tasks.list_prioritized()` with dependency gating | no EM-authored queue/defer/preempt contract |
| Generic phase prompt assembly | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs` + `crates/orchestrator-cli/prompts/runtime/workflow_phase.prompt` | phase prompt is task-phase oriented and assumes repository changes | no focused prompt template for scheduling-only decisions |
| Decision-contract primitives | `crates/orchestrator-core/src/agent_runtime_config.rs` (`PhaseDecisionContract`) and `crates/orchestrator-core/src/types.rs` (`PhaseDecision`) | supports `phase_decision` (`advance/rework/fail`) payloads | no typed contract for EM scheduling decision payload |
| Failure/decision history data sources | workflow state and phase execution records in daemon/runtime services | failure context exists in runtime state | no defined input projection for EM scheduling prompt |
| Capacity signal | daemon health/task execution limits (`active_agents`, `max_tasks_per_tick`) | capacity constraints exist in scheduler execution | no explicit input/output rule binding slot count to queue recommendations |

## Scope
In scope for implementation after this requirements phase:
- Define a focused EM scheduling prompt template with explicit input sections.
- Define strict JSON output contract for `em_scheduling_decision`.
- Define deterministic prioritization and tie-break expectations for queue order.
- Define validity rules for `queue`, `defer`, and `preempt` IDs.
- Define acceptance criteria and test checklist for parser/contract enforcement.

Out of scope for this task:
- Wiring daemon scheduler to consume EM queue (`TASK-061`).
- Exposing queue via MCP (`TASK-062`) or `ao queue` CLI (`TASK-063`).
- Introducing non-deterministic ranking heuristics or hidden state.
- Direct edits to `/.ao/*.json`.

## Constraints
- Scheduling decision output must be exactly one JSON object, machine-readable,
  and deterministic for identical inputs.
- EM agent must not perform repository mutations; output is recommendation only.
- All task IDs in output must come from the input task/workflow context.
- Contract must remain additive and compatible with existing phase decision
  plumbing.
- Prompt language must stay concise and scheduling-specific.

## EM Prompt Template (Normative)
Use this template shape for EM scheduling invocations:

```text
You are the EM scheduling agent for the Agent Orchestrator daemon.
Decide what to run next. Do not modify files, run implementation steps, or emit prose.

Inputs:
1) tasks_json: current tasks with status, priority, dependencies, linked_requirements, risk, and resource requirements.
2) running_workflows_json: currently running workflows and active phases.
3) decision_history_json: recent scheduling decisions and outcomes.
4) failure_history_json: recent workflow/phase failures with reasons.
5) available_agent_slots: integer count of immediately available execution slots.

Scheduling policy:
- Prioritize by urgency and impact, while respecting dependency readiness.
- Penalize tasks with repeated recent failures unless there is clear recovery evidence.
- Favor tasks with requirement coverage over unlinked work when urgency is otherwise similar.
- Respect resource constraints against available slots and active workload.
- Prefer defer over risky preemption unless preemption materially improves throughput or risk control.

Output:
- Return exactly one JSON object (single line), no markdown:
{"kind":"em_scheduling_decision","queue":[{"task_id":"TASK-XXX","reason":"..."}],"defer":["TASK-YYY"],"preempt":["TASK-ZZZ"]}
```

## EM Scheduling Decision Contract (Normative)

### Canonical JSON Shape
```json
{
  "kind": "em_scheduling_decision",
  "queue": [
    {
      "task_id": "TASK-123",
      "reason": "High priority, deps satisfied, low recent failure risk."
    }
  ],
  "defer": ["TASK-456"],
  "preempt": ["TASK-789"]
}
```

### Field Rules
- `kind`:
  - required constant string: `em_scheduling_decision`.
- `queue`:
  - required array, may be empty;
  - each item requires non-empty `task_id` and non-empty `reason`;
  - ordered from highest to lowest scheduling preference.
- `defer`:
  - required array of task ids intentionally not scheduled this tick.
- `preempt`:
  - required array of currently running task ids recommended for preemption.

### Validity Rules
- IDs must be unique across the union of `queue`, `defer`, and `preempt`.
- `queue.task_id` must refer to schedulable tasks (not `done`, `cancelled`, or
  blocked by unresolved dependencies).
- `defer` IDs must refer to known tasks present in the scheduling input set.
- `preempt` IDs must refer to currently running workflow tasks.
- If `available_agent_slots == 0` and `preempt` is empty, `queue` must be empty.
- `queue` length must not exceed `available_agent_slots + preempt.len()`.

### Determinism Rules
- Queue ordering priority:
  1. priority tier (`critical > high > medium > low`);
  2. dependency readiness;
  3. lower recent-failure risk;
  4. stronger requirement coverage;
  5. lower resource contention;
  6. lexicographic `task_id` tie-break.

## Functional Requirements

### FR-01: Focused Scheduling Prompt
Provide an EM-specific prompt that is scheduling-only and includes all required
input sections.

### FR-02: Required Decision Factors
Prompt must explicitly require reasoning over:
priority, dependencies, risk, recent failures, requirement coverage, and
resource constraints.

### FR-03: Strict Output Contract
EM output must conform to `em_scheduling_decision` with required
`kind|queue|defer|preempt` fields.

### FR-04: Queue Entry Explainability
Each queued task must include a concise reason string usable for audit/logging.

### FR-05: Capacity-Constrained Recommendations
Decision must respect available slot constraints and preemption semantics.

### FR-06: Deterministic Ordering and Membership
Output ordering and membership must be deterministic for identical inputs, with
stable tie-break behavior.

### FR-07: Non-Mutating Contract
Prompt/contract must explicitly prohibit code edits or direct state mutation by
the EM decision agent.

## Acceptance Criteria
- `AC-01`: Requirements doc contains a complete EM prompt template with explicit
  input sections (`tasks`, `running workflows`, `decision history`,
  `failure history`, `available slots`).
- `AC-02`: Prompt text explicitly instructs consideration of all six required
  factors from task brief.
- `AC-03`: Requirements doc defines canonical JSON output with
  `kind = em_scheduling_decision`.
- `AC-04`: `queue` entries require both `task_id` and `reason`.
- `AC-05`: Validity rules define ID uniqueness across `queue|defer|preempt`.
- `AC-06`: Capacity rules define relationship between available slots and
  `queue` size.
- `AC-07`: Deterministic ordering contract includes a stable final tie-breaker.
- `AC-08`: Scope explicitly excludes scheduler wiring and external surfaces
  covered by `TASK-061` to `TASK-063`.
- `AC-09`: `defer` and `preempt` IDs must resolve to known task/workflow items
  from the provided inputs.

## Testable Acceptance Checklist
- `T-01`: Unit tests for JSON parsing/validation of required top-level fields.
- `T-02`: Unit tests for queue item validation (`task_id`, `reason` required).
- `T-03`: Unit tests for duplicate-id rejection across `queue|defer|preempt`.
- `T-04`: Unit tests for capacity constraint enforcement.
- `T-05`: Determinism tests with fixed fixture input produce identical ordered
  decisions.
- `T-06`: Prompt-render test verifies required factor instructions are present.
- `T-07`: Validation tests reject unknown task IDs in `defer` and `preempt`.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02 | prompt template render/content assertions |
| FR-03, FR-04 | contract parser and required-field validation tests |
| FR-05 | slot/preemption constraint tests |
| FR-06 | repeat-run determinism tests with fixed fixtures |
| FR-07 | prompt-policy assertion and integration smoke check |

## Implementation Notes (Input to Next Phase)
Primary expected change targets:
- `crates/orchestrator-cli/prompts/runtime/` (new EM scheduling prompt template file).
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  (build EM input context and consume validated decision object).
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`
  (or extracted parser module) for structured decision parsing helpers.
- `crates/orchestrator-core/src/types.rs` (typed DTO for
  `em_scheduling_decision` if introduced).
- `crates/orchestrator-core/src/agent_runtime_config.rs` and
  `crates/orchestrator-core/config/agent-runtime-config.v2.json` for
  contract/profile plumbing if not already present.

## Deterministic Deliverables for Implementation Phase
- A focused EM prompt template that requests scheduling decisions only.
- A strict, parseable `em_scheduling_decision` JSON contract.
- Deterministic decision validation rules for membership, ordering, and
  capacity constraints.
- Focused tests proving contract correctness and deterministic behavior.
