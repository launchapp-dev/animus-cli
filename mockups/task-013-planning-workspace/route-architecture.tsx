/**
 * TASK-013 wireframe scaffold.
 * Planning-focused route tree + state contracts for implementation handoff.
 */

import { type ReactNode } from "react";
import { Link, Navigate, Outlet, createBrowserRouter, useParams } from "react-router-dom";

type RouteState = "loading" | "ready" | "empty" | "not_found" | "error";
type MutationState = "idle" | "pending" | "success" | "failure";
type RefineScope = "single" | "selected" | "all";
type TraceabilityId =
  | "AC-01"
  | "AC-02"
  | "AC-03"
  | "AC-04"
  | "AC-05"
  | "AC-06"
  | "AC-07"
  | "AC-08";

type ApiResult<TData> =
  | { kind: "ok"; data: TData }
  | { kind: "error"; code: string; message: string; exitCode: number };

type UiError = {
  code: string;
  message: string;
  exitCode: number;
};

type PlanningRouteDescriptor = {
  path: string;
  screen: string;
  acceptance: TraceabilityId[];
};

type PlanningEndpointDescriptor = {
  route: string;
  reads: string[];
  mutations: string[];
};

type VisionDraftInput = {
  projectName: string;
  problemStatement: string;
  targetUsers: string;
  goals: string;
  constraints: string;
  valueProposition: string;
};

type VisionModel = VisionDraftInput & {
  updatedAt: string;
};

type RequirementModel = {
  id: string;
  title: string;
  description: string;
  priority: "must" | "should" | "could" | "wont";
  status: "draft" | "refined" | "planned" | "in_progress" | "done";
};

type RequirementRefineInput = {
  scope: RefineScope;
  requirementIds: string[];
};

type RequirementRefineResult = {
  updatedIds: string[];
  skippedIds: string[];
  errors: UiError[];
};

type AoSuccessEnvelope<TData> = {
  schema: "ao.cli.v1";
  ok: true;
  data: TData;
};

type AoErrorEnvelope = {
  schema: "ao.cli.v1";
  ok: false;
  error: {
    code: string;
    message: string;
    exit_code: number;
  };
};

export const planningRouteCoverage: PlanningRouteDescriptor[] = [
  {
    path: "/planning",
    screen: "Planning Entry Redirect",
    acceptance: ["AC-01"],
  },
  {
    path: "/planning/vision",
    screen: "Vision Workspace",
    acceptance: ["AC-01", "AC-02", "AC-06", "AC-07", "AC-08"],
  },
  {
    path: "/planning/requirements",
    screen: "Requirements Workspace",
    acceptance: ["AC-01", "AC-03", "AC-04", "AC-06", "AC-07", "AC-08"],
  },
  {
    path: "/planning/requirements/new",
    screen: "New Requirement",
    acceptance: ["AC-01", "AC-03", "AC-06", "AC-07", "AC-08"],
  },
  {
    path: "/planning/requirements/:requirementId",
    screen: "Requirement Detail",
    acceptance: ["AC-01", "AC-03", "AC-04", "AC-05", "AC-06", "AC-07", "AC-08"],
  },
  {
    path: "/projects/:projectId/requirements/:requirementId",
    screen: "Project Requirement Handoff",
    acceptance: ["AC-05"],
  },
];

export const planningEndpointMap: PlanningEndpointDescriptor[] = [
  {
    route: "/planning/vision",
    reads: ["GET /api/v1/vision"],
    mutations: ["POST /api/v1/vision", "POST /api/v1/vision/refine"],
  },
  {
    route: "/planning/requirements",
    reads: ["GET /api/v1/requirements"],
    mutations: [
      "POST /api/v1/requirements",
      "POST /api/v1/requirements/draft",
      "POST /api/v1/requirements/refine",
    ],
  },
  {
    route: "/planning/requirements/new",
    reads: [],
    mutations: ["POST /api/v1/requirements"],
  },
  {
    route: "/planning/requirements/:requirementId",
    reads: ["GET /api/v1/requirements/:id"],
    mutations: [
      "PATCH /api/v1/requirements/:id",
      "DELETE /api/v1/requirements/:id",
      "POST /api/v1/requirements/refine",
    ],
  },
  {
    route: "/projects/:projectId/requirements/:requirementId",
    reads: ["GET /api/v1/project-requirements/:projectId/:requirementId"],
    mutations: [],
  },
];

export const acceptanceTraceability: Record<TraceabilityId, string[]> = {
  "AC-01": ["Planning routes are represented in route tree and route coverage table."],
  "AC-02": ["Vision workspace supports first-run empty state and save/refine loop."],
  "AC-03": ["Requirements workspace supports create/update/delete flows."],
  "AC-04": ["Refine flows include explicit scope and deterministic result model."],
  "AC-05": ["Project requirement surface links to planning detail while preserving ID casing."],
  "AC-06": ["All API interactions parse ao.cli.v1 envelopes into consistent UI errors."],
  "AC-07": ["Route content assumes keyboard-operable controls and visible focus states."],
  "AC-08": ["Layouts are structured for responsive stacking down to 320px width."],
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

// Envelope parser shared across planning surfaces.
export function parseAoEnvelope<TData>(value: unknown): ApiResult<TData> {
  if (!isRecord(value) || value.schema !== "ao.cli.v1" || typeof value.ok !== "boolean") {
    return {
      kind: "error",
      code: "invalid_envelope",
      message: "Response does not match ao.cli.v1 envelope shape.",
      exitCode: 1,
    };
  }

  if (value.ok) {
    const success = value as AoSuccessEnvelope<TData>;
    return { kind: "ok", data: success.data };
  }

  const errorEnvelope = value as AoErrorEnvelope;
  return {
    kind: "error",
    code: errorEnvelope.error.code,
    message: errorEnvelope.error.message,
    exitCode: errorEnvelope.error.exit_code,
  };
}

export function buildPlanningRequirementPath(requirementId: string): string {
  // Case-preserving path construction: no normalization, only URL encoding.
  return `/planning/requirements/${encodeURIComponent(requirementId)}`;
}

function buildProjectRequirementPath(projectId: string, requirementId: string): string {
  return `/projects/${encodeURIComponent(projectId)}/requirements/${encodeURIComponent(requirementId)}`;
}

function normalizeRequirementSelection(ids: readonly string[]): string[] {
  const next = new Set<string>();

  ids.forEach((id) => {
    const trimmed = id.trim();
    if (trimmed.length > 0) {
      next.add(trimmed);
    }
  });

  return Array.from(next.values());
}

function toRefineInput(scope: RefineScope, ids: readonly string[]): RequirementRefineInput {
  const requirementIds = normalizeRequirementSelection(ids);

  if (scope === "single") {
    return {
      scope,
      requirementIds: requirementIds.slice(0, 1),
    };
  }

  return {
    scope,
    requirementIds,
  };
}

function requiresRefineConfirmation(scope: RefineScope): boolean {
  return scope === "all";
}

function AppShellLayout(): ReactNode {
  return (
    <div className="app-shell">
      <header>{/* breadcrumb + project context */}</header>
      <nav aria-label="Primary">{/* includes Planning nav entry */}</nav>
      <main>
        <Outlet />
      </main>
    </div>
  );
}

function PlanningSectionLayout(): ReactNode {
  return (
    <section aria-label="Planning workspace">
      <Outlet />
    </section>
  );
}

function RouteBoundary(props: {
  title: string;
  routeState: RouteState;
  mutationState?: MutationState;
  error?: UiError;
  children?: ReactNode;
}): ReactNode {
  return (
    <section aria-label={props.title}>
      <h1>{props.title}</h1>
      <p>
        route: {props.routeState}
        {props.mutationState ? ` | mutation: ${props.mutationState}` : ""}
      </p>

      {props.error ? (
        <p role="alert">
          code={props.error.code}; message={props.error.message}; exitCode={props.error.exitCode}
        </p>
      ) : null}

      {props.children}
    </section>
  );
}

function PlanningEntryRedirectPage(): ReactNode {
  return <Navigate to="/planning/vision" replace />;
}

function VisionWorkspacePage(): ReactNode {
  // Vision read/create/update/refine live here.
  return (
    <RouteBoundary title="Planning Vision" routeState="ready" mutationState="idle">
      {/* GET /api/v1/vision */}
      {/* POST /api/v1/vision */}
      {/* POST /api/v1/vision/refine */}
    </RouteBoundary>
  );
}

function RequirementsWorkspacePage(): ReactNode {
  const selected = ["REQ-016", "REQ-AbC-017"];
  const refineInput = toRefineInput("selected", selected);
  const refineAllNeedsConfirm = requiresRefineConfirmation("all");

  return (
    <RouteBoundary title="Planning Requirements" routeState="ready" mutationState="pending">
      <p>
        refine scope={refineInput.scope}; ids={JSON.stringify(refineInput.requirementIds)}
      </p>
      <p>refine-all confirmation required={String(refineAllNeedsConfirm)}</p>
      {/* GET /api/v1/requirements */}
      {/* POST /api/v1/requirements/draft */}
      {/* POST /api/v1/requirements/refine */}
    </RouteBoundary>
  );
}

function RequirementNewPage(): ReactNode {
  return (
    <RouteBoundary title="New Requirement" routeState="ready" mutationState="idle">
      {/* POST /api/v1/requirements */}
      {/* success -> navigate(buildPlanningRequirementPath(created.id)) */}
    </RouteBoundary>
  );
}

function RequirementDetailPage(): ReactNode {
  const params = useParams();
  const requirementId = params.requirementId ?? "";

  return (
    <RouteBoundary title="Requirement Detail" routeState="ready" mutationState="idle">
      <p>route requirementId={requirementId}</p>
      {/* GET /api/v1/requirements/:id */}
      {/* PATCH /api/v1/requirements/:id */}
      {/* DELETE /api/v1/requirements/:id */}
      {/* not_found state renders back-link to /planning/requirements */}
    </RouteBoundary>
  );
}

function ProjectRequirementHandoffPage(): ReactNode {
  const params = useParams();
  const projectId = params.projectId ?? "";
  const requirementId = params.requirementId ?? "";
  const planningDetailPath = buildPlanningRequirementPath(requirementId);

  return (
    <RouteBoundary title="Project Requirement" routeState="ready">
      <p>projectId={projectId}</p>
      <p>requirementId={requirementId}</p>
      <p>
        <Link to={planningDetailPath}>Edit in Planning Workspace</Link>
      </p>
      <p>source path: {buildProjectRequirementPath(projectId, requirementId)}</p>
    </RouteBoundary>
  );
}

function InAppNotFoundPage(): ReactNode {
  return (
    <RouteBoundary title="Planning Not Found" routeState="not_found">
      <p>
        Unknown planning route. Return to <Link to="/planning/requirements">requirements list</Link>.
      </p>
    </RouteBoundary>
  );
}

export const planningWireframeRouter = createBrowserRouter([
  {
    path: "/",
    element: <AppShellLayout />,
    children: [
      {
        path: "planning",
        element: <PlanningSectionLayout />,
        children: [
          {
            index: true,
            element: <PlanningEntryRedirectPage />,
          },
          {
            path: "vision",
            element: <VisionWorkspacePage />,
          },
          {
            path: "requirements",
            element: <RequirementsWorkspacePage />,
          },
          {
            path: "requirements/new",
            element: <RequirementNewPage />,
          },
          {
            path: "requirements/:requirementId",
            element: <RequirementDetailPage />,
          },
        ],
      },
      {
        path: "projects/:projectId/requirements/:requirementId",
        element: <ProjectRequirementHandoffPage />,
      },
      {
        path: "*",
        element: <InAppNotFoundPage />,
      },
    ],
  },
]);

export const planningWireframeMetadata = {
  routeCoverage: planningRouteCoverage,
  endpointMap: planningEndpointMap,
  acceptanceTraceability,
};
