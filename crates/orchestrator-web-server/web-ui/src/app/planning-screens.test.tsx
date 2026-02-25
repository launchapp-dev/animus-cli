import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  PlanningRequirementCreatePage,
  PlanningRequirementDetailPage,
  PlanningRequirementsPage,
  PlanningVisionPage,
} from "./planning-screens";

function okEnvelope(data: unknown) {
  return {
    schema: "ao.cli.v1",
    ok: true,
    data,
  };
}

function errorEnvelope(code: string, message: string, exitCode: number) {
  return {
    schema: "ao.cli.v1",
    ok: false,
    error: {
      code,
      message,
      exit_code: exitCode,
    },
  };
}

function jsonResponse(payload: unknown): Response {
  return {
    json: async () => payload,
  } as Response;
}

function visionDocument(overrides: Record<string, unknown> = {}) {
  return {
    id: "VISION-1",
    project_root: "/repo",
    markdown: "# Vision\n- Name: AO",
    problem_statement: "Planning is fragmented.",
    target_users: ["Product", "Engineering"],
    goals: ["Ship planning workspace"],
    constraints: ["No regressions"],
    value_proposition: "Faster planning with traceable links.",
    created_at: "2026-02-25T00:00:00.000Z",
    updated_at: "2026-02-25T00:00:00.000Z",
    ...overrides,
  };
}

function requirementItem(
  id: string,
  title: string,
  overrides: Record<string, unknown> = {},
) {
  return {
    id,
    title,
    description: `${title} description`,
    body: `${title} body`,
    acceptance_criteria: [`${title} acceptance`],
    priority: "should",
    status: "draft",
    source: "ao-web",
    tags: [],
    linked_task_ids: [],
    created_at: "2026-02-25T00:00:00.000Z",
    updated_at: "2026-02-25T00:00:00.000Z",
    ...overrides,
  };
}

describe("planning screens", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  it("renders a recoverable not_found state for requirement deep links", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse(errorEnvelope("not_found", "requirement not found", 3)),
    );

    render(
      <MemoryRouter initialEntries={["/planning/requirements/REQ-404"]}>
        <Routes>
          <Route
            path="/planning/requirements/:requirementId"
            element={<PlanningRequirementDetailPage />}
          />
        </Routes>
      </MemoryRouter>,
    );

    expect(
      await screen.findByText(
        "Requirement not found. It may have been deleted or moved.",
      ),
    ).toBeDefined();
    expect(
      screen.getByRole("link", { name: "Back to Requirements List" }),
    ).toBeDefined();
  });

  it("renders labeled controls for first-run vision authoring", async () => {
    fetchMock.mockResolvedValue(jsonResponse(okEnvelope(null)));

    render(
      <MemoryRouter initialEntries={["/planning/vision"]}>
        <Routes>
          <Route path="/planning/vision" element={<PlanningVisionPage />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(await screen.findByLabelText("Project Name")).toBeDefined();
    expect(screen.getByLabelText("Problem Statement")).toBeDefined();
    expect(screen.getByRole("button", { name: "Save Vision" })).toBeDefined();
  });

  it("submits normalized authoring payload for vision save", async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse(okEnvelope(null)))
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope(
            visionDocument({
              markdown: "# Vision\n- Name: AO Platform",
              problem_statement: "Plan once and execute consistently.",
              target_users: ["PM", "EM"],
              goals: ["Make planning deterministic", "Improve traceability"],
              constraints: ["Stay CLI compatible"],
              value_proposition: "Fewer planning handoff failures.",
              updated_at: "2026-02-25T01:00:00.000Z",
            }),
          ),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope(
            visionDocument({
              markdown: "# Vision\n- Name: AO Platform",
              problem_statement: "Plan once and execute consistently.",
              target_users: ["PM", "EM"],
              goals: ["Make planning deterministic", "Improve traceability"],
              constraints: ["Stay CLI compatible"],
              value_proposition: "Fewer planning handoff failures.",
              updated_at: "2026-02-25T01:00:00.000Z",
            }),
          ),
        ),
      );

    render(
      <MemoryRouter initialEntries={["/planning/vision"]}>
        <Routes>
          <Route path="/planning/vision" element={<PlanningVisionPage />} />
        </Routes>
      </MemoryRouter>,
    );

    fireEvent.change(await screen.findByLabelText("Project Name"), {
      target: { value: "  AO Platform  " },
    });
    fireEvent.change(screen.getByLabelText("Problem Statement"), {
      target: { value: "  Plan once and execute consistently.  " },
    });
    fireEvent.change(screen.getByLabelText("Target Users (one per line)"), {
      target: { value: " PM \n\n EM " },
    });
    fireEvent.change(screen.getByLabelText("Goals (one per line)"), {
      target: { value: " Make planning deterministic \n Improve traceability " },
    });
    fireEvent.change(screen.getByLabelText("Constraints (one per line)"), {
      target: { value: " Stay CLI compatible " },
    });
    fireEvent.change(screen.getByLabelText("Value Proposition"), {
      target: { value: "  Fewer planning handoff failures.  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save Vision" }));

    expect(await screen.findByText("Vision saved.")).toBeDefined();

    const [path, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(path).toBe("/api/v1/vision");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(
      JSON.stringify({
        project_name: "AO Platform",
        problem_statement: "Plan once and execute consistently.",
        target_users: ["PM", "EM"],
        goals: ["Make planning deterministic", "Improve traceability"],
        constraints: ["Stay CLI compatible"],
        value_proposition: "Fewer planning handoff failures.",
      }),
    );
  });

  it("runs vision refine with focus and surfaces refinement rationale", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope(
            visionDocument({
              markdown: "# Vision\n- Name: AO Platform",
            }),
          ),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope({
            updated_vision: visionDocument({
              markdown: "# Vision\n- Name: AO Platform v2",
              updated_at: "2026-02-25T01:10:00.000Z",
            }),
            refinement: {
              mode: "focused",
              focus: "handoff quality",
              rationale: "Emphasized handoff quality and traceability.",
            },
          }),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope(
            visionDocument({
              markdown: "# Vision\n- Name: AO Platform v2",
              updated_at: "2026-02-25T01:10:00.000Z",
            }),
          ),
        ),
      );

    render(
      <MemoryRouter initialEntries={["/planning/vision"]}>
        <Routes>
          <Route path="/planning/vision" element={<PlanningVisionPage />} />
        </Routes>
      </MemoryRouter>,
    );

    fireEvent.change(await screen.findByLabelText("Refine Focus"), {
      target: { value: "  handoff quality  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Refine Vision" }));

    expect(
      await screen.findByText("Emphasized handoff quality and traceability."),
    ).toBeDefined();
    expect(screen.getByDisplayValue("AO Platform v2")).toBeDefined();

    const [path, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(path).toBe("/api/v1/vision/refine");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(JSON.stringify({ focus: "handoff quality" }));
  });

  it("supports requirement list selection and selected-scope refinement", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope([
            requirementItem("REQ-1", "Planning authoring"),
            requirementItem("REQ-2", "Planning deep links"),
          ]),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope({
            requirements: [requirementItem("REQ-1", "Planning authoring")],
            updated_ids: ["REQ-1"],
            requested_ids: ["REQ-1"],
            scope: "selected",
            focus: "traceability",
          }),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          okEnvelope([
            requirementItem("REQ-1", "Planning authoring", { status: "refined" }),
            requirementItem("REQ-2", "Planning deep links"),
          ]),
        ),
      );

    render(
      <MemoryRouter initialEntries={["/planning/requirements"]}>
        <Routes>
          <Route path="/planning/requirements" element={<PlanningRequirementsPage />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(await screen.findByText("REQ-1 · Planning authoring")).toBeDefined();
    const requirementLink = screen.getByRole("link", {
      name: "REQ-1 · Planning authoring",
    });
    expect(requirementLink.getAttribute("href")).toBe("/planning/requirements/REQ-1");

    fireEvent.click(screen.getByRole("checkbox", { name: "Select REQ-1" }));
    fireEvent.change(screen.getByLabelText("Refine Focus"), {
      target: { value: "  traceability  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Refine Selected" }));

    expect(
      await screen.findByText("Refined 1 requirement(s) in selected scope."),
    ).toBeDefined();

    const [path, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(path).toBe("/api/v1/requirements/refine");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(
      JSON.stringify({
        requirement_ids: ["REQ-1"],
        focus: "traceability",
      }),
    );
  });

  it("navigates from requirement create to requirement detail deep-link", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse(okEnvelope(requirementItem("REQ-77", "Planning QA coverage"))),
      )
      .mockResolvedValueOnce(
        jsonResponse(okEnvelope(requirementItem("REQ-77", "Planning QA coverage"))),
      );

    render(
      <MemoryRouter initialEntries={["/planning/requirements/new"]}>
        <Routes>
          <Route
            path="/planning/requirements/new"
            element={<PlanningRequirementCreatePage />}
          />
          <Route
            path="/planning/requirements/:requirementId"
            element={<PlanningRequirementDetailPage />}
          />
        </Routes>
      </MemoryRouter>,
    );

    fireEvent.change(await screen.findByLabelText("Title"), {
      target: { value: "Planning QA coverage" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Requirement" }));

    expect(await screen.findByRole("button", { name: "Save Requirement" })).toBeDefined();
    expect(screen.getByDisplayValue("Planning QA coverage")).toBeDefined();

    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/v1/requirements",
      "/api/v1/requirements/REQ-77",
    ]);
  });

  it("deletes a requirement from detail and returns to requirements list", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse(okEnvelope(requirementItem("REQ-9", "Refine planning UX"))),
      )
      .mockResolvedValueOnce(
        jsonResponse(okEnvelope({ message: "Requirement deleted." })),
      )
      .mockResolvedValueOnce(jsonResponse(okEnvelope([])));

    render(
      <MemoryRouter initialEntries={["/planning/requirements/REQ-9"]}>
        <Routes>
          <Route
            path="/planning/requirements"
            element={<PlanningRequirementsPage />}
          />
          <Route
            path="/planning/requirements/:requirementId"
            element={<PlanningRequirementDetailPage />}
          />
        </Routes>
      </MemoryRouter>,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Delete Requirement" }));
    expect(await screen.findByText("Delete requirement REQ-9?")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Confirm Delete" }));

    expect(
      await screen.findByText("No requirements yet. Create one or run draft suggestions."),
    ).toBeDefined();

    await waitFor(() => {
      expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
        "/api/v1/requirements/REQ-9",
        "/api/v1/requirements/REQ-9",
        "/api/v1/requirements",
      ]);
    });

    const deleteInit = fetchMock.mock.calls[1]?.[1] as RequestInit;
    expect(deleteInit.method).toBe("DELETE");
  });
});
