import { beforeEach, describe, expect, it, vi } from "vitest";

import { api, requestAo } from "./client";

function okEnvelope(data: unknown) {
  return {
    schema: "ao.cli.v1",
    ok: true,
    data,
  };
}

function jsonResponse(payload: unknown): Response {
  return {
    json: async () => payload,
  } as Response;
}

describe("requestAo", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  it("applies AO JSON headers and preserves caller headers", async () => {
    fetchMock.mockResolvedValue(jsonResponse(okEnvelope({ id: "TASK-011" })));

    await requestAo<{ id: string }>("/api/v1/tasks/TASK-011", {
      method: "POST",
      headers: {
        Authorization: "Bearer token",
      },
      body: JSON.stringify({}),
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];

    expect(path).toBe("/api/v1/tasks/TASK-011");
    expect(init.method).toBe("POST");
    expect(init.headers).toEqual({
      Accept: "application/json",
      "Content-Type": "application/json",
      Authorization: "Bearer token",
    });
  });

  it("maps network failures to unavailable API errors", async () => {
    fetchMock.mockRejectedValue(new Error("network offline"));

    const result = await requestAo("/api/v1/system/info");

    expect(result).toEqual({
      kind: "error",
      code: "network_error",
      message: "network offline",
      exitCode: 5,
    });
  });

  it("maps invalid JSON responses to deterministic invalid_json errors", async () => {
    fetchMock.mockResolvedValue({
      json: async () => {
        throw new SyntaxError("Unexpected token <");
      },
    } as Response);

    const result = await requestAo("/api/v1/system/info");

    expect(result).toEqual({
      kind: "error",
      code: "invalid_json",
      message: "Invalid JSON response for /api/v1/system/info: Unexpected token <",
      exitCode: 1,
    });
  });
});

describe("api endpoint contract", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
    fetchMock.mockResolvedValue(jsonResponse(okEnvelope({})));
  });

  it("uses stable read endpoints for shell routes", async () => {
    await api.daemonStatus();
    await api.projectsList();
    await api.tasksList();
    await api.workflowsList();
    await api.projectsActive();

    const requestedPaths = fetchMock.mock.calls.map(
      (call) => call[0] as string,
    );

    expect(requestedPaths).toEqual([
      "/api/v1/daemon/status",
      "/api/v1/projects",
      "/api/v1/tasks",
      "/api/v1/workflows",
      "/api/v1/projects/active",
    ]);
  });

  it("uses POST with JSON body for write endpoints", async () => {
    await api.daemonStart();
    await api.reviewHandoff({ taskId: "TASK-011" });

    const daemonStartInit = fetchMock.mock.calls[0][1] as RequestInit;
    const reviewHandoffInit = fetchMock.mock.calls[1][1] as RequestInit;

    expect(daemonStartInit.method).toBe("POST");
    expect(daemonStartInit.body).toBe("{}");
    expect(reviewHandoffInit.method).toBe("POST");
    expect(reviewHandoffInit.body).toBe(JSON.stringify({ taskId: "TASK-011" }));
  });

  it("returns invalid_payload when an ok envelope fails endpoint guard checks", async () => {
    fetchMock.mockResolvedValue(jsonResponse(okEnvelope({ not: "an-array" })));

    const result = await api.tasksList();

    expect(result).toEqual({
      kind: "error",
      code: "invalid_payload",
      message: "Invalid payload for /api/v1/tasks: tasks must be an array",
      exitCode: 1,
    });
  });

  it("preserves server error envelope code, message, and exit code", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse({
        schema: "ao.cli.v1",
        ok: false,
        error: {
          code: "not_found",
          message: "task not found",
          exit_code: 3,
        },
      }),
    );

    const result = await api.tasksById("TASK-404");

    expect(result).toEqual({
      kind: "error",
      code: "not_found",
      message: "task not found",
      exitCode: 3,
    });
  });
});
