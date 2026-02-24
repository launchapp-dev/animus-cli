import { describe, expect, it } from "vitest";

import { APP_ROUTE_PATHS } from "./router";

describe("APP_ROUTE_PATHS", () => {
  it("contains required task-011 route architecture", () => {
    const requiredPaths = [
      "/",
      "/dashboard",
      "/daemon",
      "/projects",
      "/projects/:projectId",
      "/projects/:projectId/requirements/:requirementId",
      "/tasks",
      "/tasks/:taskId",
      "/workflows",
      "/workflows/:workflowId",
      "/workflows/:workflowId/checkpoints/:checkpoint",
      "/events",
      "/reviews/handoff",
      "*",
    ];

    expect(APP_ROUTE_PATHS).toEqual(requiredPaths);
  });
});
