import { describe, expect, it } from "vitest";

import { resolveProjectContext } from "./project-context";

describe("resolveProjectContext", () => {
  it("prefers route param over cached and server active project", () => {
    const result = resolveProjectContext({
      routeProjectId: "route-project",
      cachedProjectId: "cached-project",
      serverActiveProjectId: "server-project",
    });

    expect(result).toEqual({
      activeProjectId: "route-project",
      source: "route-param",
    });
  });

  it("falls back to cached project when route param is absent", () => {
    const result = resolveProjectContext({
      routeProjectId: null,
      cachedProjectId: "cached-project",
      serverActiveProjectId: "server-project",
    });

    expect(result).toEqual({
      activeProjectId: "cached-project",
      source: "cached-selection",
    });
  });

  it("falls back to server active project when route and cached values are absent", () => {
    const result = resolveProjectContext({
      routeProjectId: null,
      cachedProjectId: null,
      serverActiveProjectId: "server-project",
    });

    expect(result).toEqual({
      activeProjectId: "server-project",
      source: "server-active",
    });
  });

  it("returns none context when no source provides a project", () => {
    const result = resolveProjectContext({
      routeProjectId: null,
      cachedProjectId: null,
      serverActiveProjectId: null,
    });

    expect(result).toEqual({
      activeProjectId: null,
      source: "none",
    });
  });
});
