import { describe, expect, it } from "vitest";

import { APP_ROUTE_PATHS } from "./router";
import { MAIN_CONTENT_ID, PRIMARY_NAV_ITEMS } from "./shell";

describe("PRIMARY_NAV_ITEMS", () => {
  it("uses unique labels and destinations", () => {
    expect(new Set(PRIMARY_NAV_ITEMS.map((item) => item.label)).size).toBe(PRIMARY_NAV_ITEMS.length);
    expect(new Set(PRIMARY_NAV_ITEMS.map((item) => item.to)).size).toBe(PRIMARY_NAV_ITEMS.length);
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
