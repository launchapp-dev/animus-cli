// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  useQuery: vi.fn(),
  useMutation: vi.fn(),
}));

vi.mock("@/lib/graphql/client", async () => {
  const actual = await vi.importActual("@/lib/graphql/client");
  return {
    ...actual,
    useQuery: mocks.useQuery,
    useMutation: mocks.useMutation,
  };
});

vi.mock("@/lib/graphql/provider", () => ({
  GraphQLProvider: ({ children }: { children: React.ReactNode }) => children,
}));

import {
  PlanningRequirementCreatePage,
  PlanningRequirementDetailPage,
  PlanningRequirementsPage,
  PlanningVisionPage,
} from "./planning-screens";

describe("planning screens", () => {
  let executeMutation: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    executeMutation = vi.fn().mockResolvedValue({ data: {} });
    mocks.useMutation.mockReturnValue([{ fetching: false }, executeMutation]);
  });

  it("renders vision page with save and refine buttons", () => {
    mocks.useQuery.mockReturnValue([
      {
        data: {
          vision: {
            title: "AO Platform",
            summary: "Agent orchestrator",
            goals: ["Ship planning"],
            targetAudience: ["Engineers"],
            successCriteria: ["Coverage"],
            constraints: ["No regressions"],
            raw: "# Vision\nAO Platform",
          },
        },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);

    render(
      <MemoryRouter initialEntries={["/planning/vision"]}>
        <Routes>
          <Route path="/planning/vision" element={<PlanningVisionPage />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(screen.getByText("Planning Vision")).toBeTruthy();
    expect(screen.getByDisplayValue("AO Platform")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save Vision" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refine Vision" })).toBeTruthy();
  });

  it("renders requirements list with selection and refine controls", () => {
    mocks.useQuery.mockReturnValue([
      {
        data: {
          requirements: [
            {
              id: "REQ-1",
              title: "Planning authoring",
              description: "Planning authoring description",
              priority: "Should",
              priorityRaw: "should",
              status: "Draft",
              statusRaw: "draft",
              requirementType: "functional",
              tags: [],
              linkedTaskIds: [],
            },
            {
              id: "REQ-2",
              title: "Planning deep links",
              description: "Deep link description",
              priority: "Could",
              priorityRaw: "could",
              status: "Draft",
              statusRaw: "draft",
              requirementType: "functional",
              tags: [],
              linkedTaskIds: [],
            },
          ],
        },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);

    render(
      <MemoryRouter initialEntries={["/planning/requirements"]}>
        <Routes>
          <Route path="/planning/requirements" element={<PlanningRequirementsPage />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(screen.getByText("Planning Requirements")).toBeTruthy();
    expect(screen.getByText(/REQ-1/)).toBeTruthy();
    expect(screen.getByText(/REQ-2/)).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "Select REQ-1" })).toBeTruthy();
  });

  it("renders requirement detail with edit form and actions", () => {
    mocks.useQuery.mockReturnValue([
      {
        data: {
          requirement: {
            id: "REQ-1",
            title: "Planning authoring",
            description: "Description",
            priority: "Should",
            priorityRaw: "should",
            status: "Draft",
            statusRaw: "draft",
            requirementType: "functional",
            tags: [],
            linkedTaskIds: [],
          },
        },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);

    render(
      <MemoryRouter initialEntries={["/planning/requirements/REQ-1"]}>
        <Routes>
          <Route
            path="/planning/requirements/:requirementId"
            element={<PlanningRequirementDetailPage />}
          />
        </Routes>
      </MemoryRouter>,
    );

    expect(screen.getByText("REQ-1")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save Changes" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Delete" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refine with AI" })).toBeTruthy();
  });

  it("renders not found state when requirement is missing", () => {
    mocks.useQuery.mockReturnValue([
      {
        data: { requirement: null },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);

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

    expect(screen.getByText(/REQ-404 not found/)).toBeTruthy();
  });

  it("shows delete confirmation flow", async () => {
    mocks.useQuery.mockReturnValue([
      {
        data: {
          requirement: {
            id: "REQ-9",
            title: "Refine planning UX",
            description: "",
            priority: "Should",
            priorityRaw: "should",
            status: "Draft",
            statusRaw: "draft",
            requirementType: null,
            tags: [],
            linkedTaskIds: [],
          },
        },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);

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

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(screen.getByRole("button", { name: "Confirm Delete" })).toBeTruthy();
  });

  it("creates requirement and calls mutation", async () => {
    mocks.useQuery.mockReturnValue([
      { data: null, fetching: false, error: null },
      vi.fn(),
    ]);

    executeMutation.mockResolvedValue({
      data: { createRequirement: { id: "REQ-77" } },
    });

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

    const inputs = screen.getAllByRole("textbox");
    fireEvent.change(inputs[0], {
      target: { value: "Planning QA coverage" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create Requirement" }));

    await waitFor(() => {
      expect(executeMutation).toHaveBeenCalledWith(
        expect.objectContaining({
          title: "Planning QA coverage",
        }),
      );
    });
  });

  it("renders vision create form when no vision exists", () => {
    mocks.useQuery.mockReturnValue([
      {
        data: { vision: null },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);

    render(
      <MemoryRouter initialEntries={["/planning/vision"]}>
        <Routes>
          <Route path="/planning/vision" element={<PlanningVisionPage />} />
        </Routes>
      </MemoryRouter>,
    );

    expect(screen.getByRole("button", { name: "Save Vision" })).toBeTruthy();
  });
});
