import { describe, expect, it } from "vitest";

import { APP_ROUTE_PATHS } from "./router";

describe("APP_ROUTE_PATHS", () => {
  it("keeps route paths unique", () => {
    expect(new Set(APP_ROUTE_PATHS).size).toBe(APP_ROUTE_PATHS.length);
  });

  it("includes landing, review, settings, and fallback routes", () => {
    expect(APP_ROUTE_PATHS).toEqual(
      expect.arrayContaining([
        "/",
        "/dashboard",
        "/reviews/handoff",
        "/settings/mcp",
        "*",
      ]),
    );
  });
});
