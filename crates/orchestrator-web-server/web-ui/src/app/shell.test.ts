import { describe, expect, it } from "vitest";

import { APP_ROUTE_PATHS } from "./router";
import { PRIMARY_NAV_ITEMS } from "./shell";

describe("PRIMARY_NAV_ITEMS", () => {
  it("matches required top-level navigation order", () => {
    expect(PRIMARY_NAV_ITEMS).toEqual([
      { to: "/dashboard", label: "Dashboard" },
      { to: "/daemon", label: "Daemon" },
      { to: "/projects", label: "Projects" },
      { to: "/tasks", label: "Tasks" },
      { to: "/workflows", label: "Workflows" },
      { to: "/events", label: "Events" },
      { to: "/reviews/handoff", label: "Review Handoff" },
    ]);
  });

  it("points to registered routes only", () => {
    const routePathSet = new Set(APP_ROUTE_PATHS);
    const unknownNavTargets = PRIMARY_NAV_ITEMS
      .map((item) => item.to)
      .filter((path) => !routePathSet.has(path));

    expect(unknownNavTargets).toEqual([]);
  });
});
