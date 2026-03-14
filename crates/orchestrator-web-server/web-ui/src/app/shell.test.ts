import { describe, expect, it } from "vitest";

import { APP_ROUTE_PATHS } from "./router";
import { MAIN_CONTENT_ID, PRIMARY_NAV_ITEMS } from "./shell";

describe("PRIMARY_NAV_ITEMS", () => {
  it("matches required top-level navigation order", () => {
    const labels = PRIMARY_NAV_ITEMS.map((item) => item.label);
    expect(labels).toEqual([
      "Dashboard",
      "Tasks",
      "Workflows",
      "Queue",
      "Agents",
      "Vision",
      "Requirements",
      "Events",
      "Errors",
      "Daemon",
      "Builder",
      "Skills",
      "Settings",
    ]);
  });

  it("points to registered routes only", () => {
    const routePathSet = new Set(APP_ROUTE_PATHS);
    const unknownNavTargets = PRIMARY_NAV_ITEMS
      .map((item) => item.to)
      .filter((path) => !routePathSet.has(path));

    expect(unknownNavTargets).toEqual([]);
  });

  it("uses a stable main content id for keyboard skip navigation", () => {
    expect(MAIN_CONTENT_ID).toBe("main-content");
  });
});
