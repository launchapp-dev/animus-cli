import { describe, expect, it } from "vitest";

import {
  decodeDaemonHealth,
  decodeDaemonLogs,
  decodeDaemonStatus,
  decodeMessagePayload,
  decodePlanningRequirementDetail,
  decodePlanningRequirementsDraftResult,
  decodePlanningRequirementsList,
  decodePlanningRequirementsRefineResult,
  decodeProjectDetail,
  decodeProjectRequirementDetail,
  decodeProjectRequirementsById,
  decodeProjectRequirementsSummary,
  decodeProjectsActive,
  decodeProjectsList,
  decodeProjectTasksPayload,
  decodeProjectWorkflowsPayload,
  decodeReviewHandoffResponse,
  decodeSystemInfo,
  decodeTaskDetail,
  decodeTaskStats,
  decodeTasksList,
  decodeVisionDocument,
  decodeVisionDocumentNullable,
  decodeVisionRefineResult,
  decodeWorkflowCheckpointDetail,
  decodeWorkflowCheckpoints,
  decodeWorkflowDecisions,
  decodeWorkflowDetail,
  decodeWorkflowsList,
} from "./guards";
import type { DecodeResult } from "./guards";

function expectOk<TData>(result: DecodeResult<TData>): TData {
  expect(result.ok).toBe(true);
  if (!result.ok) {
    throw new Error(result.message);
  }
  return result.data;
}

describe("api contract guards", () => {
  it("accepts valid payloads for all consumed endpoint categories", () => {
    expectOk(
      decodeSystemInfo({
        platform: "darwin",
        arch: "arm64",
        version: "1.0.0",
        daemon_status: "running",
        daemon_running: true,
      }),
    );
    expectOk(decodeDaemonStatus("running"));
    expectOk(decodeDaemonHealth({ healthy: true, status: "running" }));
    expectOk(decodeDaemonLogs([{ message: "daemon started" }]));
    expectOk(decodeMessagePayload({ message: "ok" }));
    expectOk(decodeProjectsList([{ id: "project-1", name: "Project 1" }]));
    expectOk(decodeProjectsActive(null));
    expectOk(decodeProjectDetail({ id: "project-1", name: "Project 1", path: "/tmp/p1" }));
    expectOk(
      decodeProjectTasksPayload({
        project: { id: "project-1", name: "Project 1" },
        tasks: [{ id: "TASK-1", status: "in_progress", type: "documentation" }],
      }),
    );
    expectOk(
      decodeProjectWorkflowsPayload({
        project: { id: "project-1", name: "Project 1" },
        workflows: [{ id: "wf-1", status: "running" }],
      }),
    );
    expectOk(
      decodeProjectRequirementsSummary([
        { project_id: "project-1", project_name: "Project 1", requirement_count: 1 },
      ]),
    );
    expectOk(
      decodeProjectRequirementsById({
        project_id: "project-1",
        project_name: "Project 1",
        requirements: [{ id: "REQ-1", title: "Requirement" }],
      }),
    );
    expectOk(
      decodeProjectRequirementDetail({
        project_id: "project-1",
        project_name: "Project 1",
        requirement: { id: "REQ-1", title: "Requirement" },
      }),
    );
    expectOk(
      decodeVisionDocumentNullable({
        id: "vision-1",
        project_root: "/tmp/project",
        markdown: "# Product Vision",
        problem_statement: "Planning is fragmented",
        target_users: ["PM", "EM"],
        goals: ["Ship planning UI"],
        constraints: ["Deterministic outputs"],
        value_proposition: "Ship faster",
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      }),
    );
    expectOk(decodeVisionDocumentNullable(null));
    expectOk(
      decodeVisionDocument({
        id: "vision-1",
        project_root: "/tmp/project",
        markdown: "# Product Vision",
        problem_statement: "Planning is fragmented",
        target_users: ["PM", "EM"],
        goals: ["Ship planning UI"],
        constraints: ["Deterministic outputs"],
        value_proposition: "Ship faster",
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      }),
    );
    expectOk(
      decodeVisionRefineResult({
        updated_vision: {
          id: "vision-1",
          project_root: "/tmp/project",
          markdown: "# Product Vision",
          problem_statement: "Planning is fragmented",
          target_users: ["PM"],
          goals: ["Ship planning UI"],
          constraints: ["Deterministic outputs"],
          value_proposition: "Ship faster",
          created_at: "2026-01-01T00:00:00Z",
          updated_at: "2026-01-01T00:00:00Z",
        },
        refinement: {
          mode: "heuristic",
          focus: "quality gates",
          rationale: "Heuristic refinement",
          changes: {
            goals_added: [],
            constraints_added: [],
          },
        },
      }),
    );
    expectOk(
      decodePlanningRequirementsList([
        {
          id: "REQ-1",
          title: "Planning route coverage",
          description: "Support deep links",
          acceptance_criteria: ["Route loads directly"],
          priority: "must",
          status: "draft",
          source: "ao-web",
          tags: ["planning"],
          linked_task_ids: ["TASK-1"],
          created_at: "2026-01-01T00:00:00Z",
          updated_at: "2026-01-01T00:00:00Z",
        },
      ]),
    );
    expectOk(
      decodePlanningRequirementDetail({
        id: "REQ-1",
        title: "Planning route coverage",
        description: "Support deep links",
        acceptance_criteria: ["Route loads directly"],
        priority: "must",
        status: "draft",
        source: "ao-web",
        tags: ["planning"],
        linked_task_ids: ["TASK-1"],
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      }),
    );
    expectOk(
      decodePlanningRequirementsDraftResult({
        requirements: [
          {
            id: "REQ-1",
            title: "Planning route coverage",
            description: "Support deep links",
            acceptance_criteria: ["Route loads directly"],
            priority: "must",
            status: "draft",
            source: "ao-web",
            tags: [],
            linked_task_ids: [],
            created_at: "2026-01-01T00:00:00Z",
            updated_at: "2026-01-01T00:00:00Z",
          },
        ],
        appended_count: 1,
      }),
    );
    expectOk(
      decodePlanningRequirementsRefineResult({
        requirements: [
          {
            id: "REQ-1",
            title: "Planning route coverage",
            description: "Support deep links",
            acceptance_criteria: ["Route loads directly"],
            priority: "must",
            status: "refined",
            source: "ao-web",
            tags: [],
            linked_task_ids: [],
            created_at: "2026-01-01T00:00:00Z",
            updated_at: "2026-01-01T00:00:00Z",
          },
        ],
        updated_ids: ["REQ-1"],
        requested_ids: ["REQ-1"],
        scope: "selected",
      }),
    );
    expectOk(decodeTasksList([{ id: "TASK-1", status: "todo", type: "bug" }]));
    expectOk(
      decodeTaskStats({
        total: 1,
        in_progress: 0,
        blocked: 0,
        completed: 0,
        by_status: { backlog: 1 },
      }),
    );
    expectOk(decodeTaskDetail({ id: "TASK-1", status: "on_hold", type: "tests" }));
    expectOk(decodeWorkflowsList([{ id: "wf-1" }]));
    expectOk(decodeWorkflowDetail({ id: "wf-1", status: "running" }));
    expectOk(decodeWorkflowDecisions([{ phase_id: "implementation", decision: "advance" }]));
    expectOk(decodeWorkflowCheckpoints([{ number: 2, reason: "status-change" }]));
    expectOk(decodeWorkflowCheckpointDetail({ number: 2, reason: "status-change" }));
    expectOk(decodeReviewHandoffResponse({ status: "completed", run_id: "run-1" }));
  });

  it("normalizes task aliases while keeping additive fields", () => {
    const tasks = expectOk(
      decodeTasksList([
        {
          id: "TASK-1",
          status: "in_progress",
          type: "documentation",
          extra: "field",
        },
      ]),
    );

    expect(tasks).toEqual([
      {
        id: "TASK-1",
        status: "in-progress",
        type: "docs",
        extra: "field",
      },
    ]);
  });

  it("rejects malformed payloads deterministically", () => {
    const invalidTasks = decodeTasksList({ tasks: [] });
    expect(invalidTasks).toEqual({
      ok: false,
      message: "tasks must be an array",
    });

    const invalidRequirement = decodeProjectRequirementDetail({
      project_id: "project-1",
      project_name: "Project 1",
      requirement: [],
    });
    expect(invalidRequirement).toEqual({
      ok: false,
      message: "project_requirement_detail.requirement must be an object",
    });

    const invalidStats = decodeTaskStats({
      by_status: { backlog: "one" },
    });
    expect(invalidStats).toEqual({
      ok: false,
      message: "task_stats.by_status.backlog must be a number",
    });

    const invalidVision = decodeVisionDocumentNullable({
      id: "vision-1",
    });
    expect(invalidVision).toEqual({
      ok: false,
      message: "planning_vision.project_root must be a string",
    });

    const invalidScope = decodePlanningRequirementsRefineResult({
      requirements: [],
      updated_ids: [],
      requested_ids: [],
      scope: "batch",
    });
    expect(invalidScope).toEqual({
      ok: false,
      message: "planning_requirements_refine.scope must be selected or all",
    });
  });
});
