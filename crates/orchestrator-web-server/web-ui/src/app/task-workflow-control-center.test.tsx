// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

const apiMocks = vi.hoisted(() => ({
  tasksPrioritized: vi.fn(),
  tasksStats: vi.fn(),
  tasksById: vi.fn(),
  taskSetStatus: vi.fn(),
  workflowsList: vi.fn(),
  workflowsById: vi.fn(),
  workflowDecisions: vi.fn(),
  workflowCheckpoints: vi.fn(),
  workflowRun: vi.fn(),
  workflowPause: vi.fn(),
  workflowResume: vi.fn(),
  workflowCancel: vi.fn(),
}));

vi.mock("../lib/api/client", () => ({
  api: {
    tasksPrioritized: apiMocks.tasksPrioritized,
    tasksStats: apiMocks.tasksStats,
    tasksById: apiMocks.tasksById,
    taskSetStatus: apiMocks.taskSetStatus,
    workflowsList: apiMocks.workflowsList,
    workflowsById: apiMocks.workflowsById,
    workflowDecisions: apiMocks.workflowDecisions,
    workflowCheckpoints: apiMocks.workflowCheckpoints,
    workflowRun: apiMocks.workflowRun,
    workflowPause: apiMocks.workflowPause,
    workflowResume: apiMocks.workflowResume,
    workflowCancel: apiMocks.workflowCancel,
  },
  firstApiError: (...results: Array<{ kind: "ok" | "error" }>) =>
    results.find((result) => result.kind === "error") ?? null,
}));

import { TasksPage, WorkflowDetailPage, WorkflowsPage } from "./screens";

function okResult<TData>(data: TData) {
  return Promise.resolve({
    kind: "ok" as const,
    data,
  });
}

function deferred<TData>() {
  let resolve: (value: TData) => void = () => {};
  const promise = new Promise<TData>((innerResolve) => {
    resolve = innerResolve;
  });
  return { promise, resolve };
}

function renderInRouter(element: ReactElement) {
  return render(<MemoryRouter>{element}</MemoryRouter>);
}

describe("task and workflow control center", () => {
  beforeEach(() => {
    for (const mock of Object.values(apiMocks)) {
      mock.mockReset();
    }

    apiMocks.tasksPrioritized.mockReturnValue(okResult([]));
    apiMocks.tasksStats.mockReturnValue(okResult({ total: 0, in_progress: 0, blocked: 0, completed: 0 }));
    apiMocks.tasksById.mockReturnValue(okResult(null));
    apiMocks.taskSetStatus.mockReturnValue(okResult({ id: "TASK-000", status: "done" }));
    apiMocks.workflowsList.mockReturnValue(okResult([]));
    apiMocks.workflowsById.mockReturnValue(
      okResult({
        id: "wf-default",
        task_id: "TASK-000",
        status: "paused",
        current_phase: "implementation",
        started_at: "2026-02-26T00:00:00.000Z",
        completed_at: null,
      }),
    );
    apiMocks.workflowDecisions.mockReturnValue(okResult([]));
    apiMocks.workflowCheckpoints.mockReturnValue(okResult([]));
    apiMocks.workflowRun.mockReturnValue(
      okResult({
        id: "wf-run",
        task_id: "TASK-000",
        status: "pending",
      }),
    );
    apiMocks.workflowPause.mockReturnValue(okResult({ id: "wf-default", status: "paused" }));
    apiMocks.workflowResume.mockReturnValue(okResult({ id: "wf-default", status: "running" }));
    apiMocks.workflowCancel.mockReturnValue(okResult({ id: "wf-default", status: "cancelled" }));
  });

  it("sorts queue deterministically and supports status/search filtering", async () => {
    apiMocks.tasksPrioritized.mockReturnValue(
      okResult([
        {
          id: "TASK-LOW",
          status: "ready",
          type: "feature",
          priority: "low",
          title: "Low priority work",
          updated_at: "2026-02-26T04:00:00.000Z",
        },
        {
          id: "TASK-HIGH-OLD",
          status: "ready",
          type: "feature",
          priority: "high",
          title: "High old",
          updated_at: "2026-02-25T23:00:00.000Z",
        },
        {
          id: "TASK-HIGH-NEW",
          status: "done",
          type: "feature",
          priority: "high",
          title: "High new",
          updated_at: "2026-02-26T05:00:00.000Z",
        },
      ]),
    );

    renderInRouter(<TasksPage />);

    await waitFor(() => {
      expect(apiMocks.tasksPrioritized).toHaveBeenCalledTimes(1);
    });

    const queueButtons = screen.getAllByRole("button", { name: /Select task/ });
    expect(queueButtons[0].textContent).toContain("TASK-HIGH-NEW");
    expect(queueButtons[1].textContent).toContain("TASK-HIGH-OLD");
    expect(queueButtons[2].textContent).toContain("TASK-LOW");

    fireEvent.change(screen.getByLabelText("Status filter"), {
      target: { value: "done" },
    });
    expect(screen.getAllByRole("button", { name: /Select task/ }).length).toBe(1);
    expect(screen.getByRole("button", { name: /Select task TASK-HIGH-NEW/ })).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Search"), { target: { value: "not-present" } });
    expect(screen.getByText("No tasks match the active filters.")).toBeTruthy();
  });

  it("gates active task cancellation until typed confirmation is valid", async () => {
    apiMocks.tasksPrioritized.mockReturnValue(
      okResult([
        {
          id: "TASK-014",
          status: "in-progress",
          type: "feature",
          priority: "high",
          title: "Build control center",
          updated_at: "2026-02-26T05:00:00.000Z",
          checklist: [],
          dependencies: [],
        },
      ]),
    );
    apiMocks.taskSetStatus.mockReturnValue(
      okResult({
        id: "TASK-014",
        status: "cancelled",
      }),
    );

    renderInRouter(<TasksPage />);

    await waitFor(() => {
      expect(apiMocks.tasksPrioritized).toHaveBeenCalledTimes(1);
    });

    fireEvent.change(screen.getByLabelText("Next status"), { target: { value: "cancelled" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply Status Transition" }));

    expect(screen.getByRole("heading", { name: "Confirm High-Impact Action" })).toBeTruthy();
    expect(apiMocks.taskSetStatus).not.toHaveBeenCalled();

    const confirmButton = screen.getByRole("button", {
      name: "Confirm Task Cancellation",
    }) as HTMLButtonElement;
    expect(confirmButton.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Confirmation phrase"), {
      target: { value: "CANCEL TASK-014" },
    });
    expect(confirmButton.disabled).toBe(false);
    fireEvent.click(confirmButton);

    await waitFor(() => {
      expect(apiMocks.taskSetStatus).toHaveBeenCalledTimes(1);
      expect(apiMocks.taskSetStatus).toHaveBeenCalledWith("TASK-014", { status: "cancelled" });
    });
  });

  it("gates workflow cancellation and suppresses duplicate submit while pending", async () => {
    apiMocks.workflowsList.mockReturnValue(
      okResult([
        {
          id: "wf-1",
          task_id: "TASK-014",
          status: "running",
          current_phase: "implementation",
          started_at: "2026-02-26T05:00:00.000Z",
        },
      ]),
    );
    const cancelDeferred = deferred<{ kind: "ok"; data: { id: string; status: string } }>();
    apiMocks.workflowCancel.mockReturnValue(cancelDeferred.promise);

    renderInRouter(<WorkflowsPage />);

    await waitFor(() => {
      expect(apiMocks.workflowsList).toHaveBeenCalledTimes(1);
    });

    fireEvent.click(screen.getByRole("button", { name: "Cancel workflow wf-1" }));
    expect(apiMocks.workflowCancel).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("Confirmation phrase"), {
      target: { value: "CANCEL wf-1" },
    });
    const confirmButton = screen.getByRole("button", {
      name: "Confirm Workflow Cancellation",
    });
    fireEvent.click(confirmButton);
    fireEvent.click(confirmButton);

    expect(apiMocks.workflowCancel).toHaveBeenCalledTimes(1);
    cancelDeferred.resolve({
      kind: "ok",
      data: { id: "wf-1", status: "cancelled" },
    });

    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toContain("wf-1");
    });
  });

  it("renders workflow timeline in deterministic checkpoint/order/timestamp sequence", async () => {
    apiMocks.workflowsById.mockReturnValue(
      okResult({
        id: "wf-ordered",
        task_id: "TASK-014",
        status: "running",
        current_phase: "implementation",
        started_at: "2026-02-26T00:00:00.000Z",
        completed_at: null,
      }),
    );
    apiMocks.workflowCheckpoints.mockReturnValue(
      okResult([
        {
          number: 3,
          timestamp: "2026-02-26T03:00:00.000Z",
          reason: "pause",
          status: "paused",
          machine_state: "paused",
        },
        {
          number: 1,
          timestamp: "2026-02-26T01:00:00.000Z",
          reason: "start",
          status: "running",
          machine_state: "run-phase",
        },
      ]),
    );
    apiMocks.workflowDecisions.mockReturnValue(
      okResult([
        {
          checkpoint_order: 2,
          phase_id: "implementation",
          decision: "advance",
          source: "llm",
          risk: "low",
          reason: "ready to continue",
          timestamp: "2026-02-26T02:00:00.000Z",
        },
      ]),
    );

    render(
      <MemoryRouter initialEntries={["/workflows/wf-ordered"]}>
        <Routes>
          <Route path="/workflows/:workflowId" element={<WorkflowDetailPage />} />
        </Routes>
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(apiMocks.workflowsById).toHaveBeenCalledWith("wf-ordered");
    });

    const timelineRegion = screen.getByRole("region", { name: "Phase timeline" });
    const timelineItems = within(timelineRegion).getAllByRole("listitem");
    expect(timelineItems[0].textContent).toContain("Checkpoint 1");
    expect(timelineItems[1].textContent).toContain("Decision");
    expect(timelineItems[2].textContent).toContain("Checkpoint 3");
  });
});
