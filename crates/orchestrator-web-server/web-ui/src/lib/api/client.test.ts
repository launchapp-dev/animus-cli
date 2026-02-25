import { beforeEach, describe, expect, it, vi } from "vitest";

import { api, requestAo } from "./client";
import {
  listTelemetryEvents,
  listFailedTelemetryEvents,
  REDACTED_VALUE,
  resetCorrelationSequenceForTests,
  resetTelemetryStoreForTests,
} from "../telemetry";

function okEnvelope(data: unknown) {
  return {
    schema: "ao.cli.v1",
    ok: true,
    data,
  };
}

function jsonResponse(
  payload: unknown,
  options: {
    status?: number;
    headers?: Record<string, string>;
  } = {},
): Response {
  return {
    status: options.status ?? 200,
    headers: new Headers(options.headers),
    json: async () => payload,
  } as Response;
}

describe("requestAo", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
    resetTelemetryStoreForTests();
    resetCorrelationSequenceForTests();
  });

  it("applies AO JSON headers, preserves caller headers, and injects correlation header", async () => {
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

    const headers = new Headers(init.headers);
    expect(path).toBe("/api/v1/tasks/TASK-011");
    expect(init.method).toBe("POST");
    expect(headers.get("Accept")).toBe("application/json");
    expect(headers.get("Content-Type")).toBe("application/json");
    expect(headers.get("Authorization")).toBe("Bearer token");
    expect(headers.get("X-AO-Correlation-ID")).toBeTruthy();
  });

  it("maps network failures to unavailable errors and keeps diagnostics metadata", async () => {
    fetchMock.mockRejectedValue(new Error("network offline"));

    const result = await requestAo("/api/v1/system/info");

    expect(result).toMatchObject({
      kind: "error",
      code: "network_error",
      message: "network offline",
      exitCode: 5,
      method: "GET",
      requestPath: "/api/v1/system/info",
    });
    if (result.kind === "error") {
      expect(result.correlationId).toBeTruthy();
    }
  });

  it("maps invalid JSON responses to deterministic invalid_json errors with context", async () => {
    fetchMock.mockResolvedValue({
      status: 500,
      headers: new Headers({
        "X-AO-Correlation-ID": "server-correlation-123",
      }),
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
      correlationId: "server-correlation-123",
      httpStatus: 500,
      method: "GET",
      requestPath: "/api/v1/system/info",
    });
  });

  it("captures failure telemetry with normalized error, correlation id, and redacted request body", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse(
        {
          schema: "ao.cli.v1",
          ok: false,
          error: {
            code: "conflict",
            message: "daemon already running",
            exit_code: 4,
          },
        },
        {
          status: 409,
          headers: {
            "X-AO-Correlation-ID": "srv-cid-42",
          },
        },
      ),
    );

    const result = await requestAo(
      "/api/v1/daemon/start",
      {
        method: "POST",
        body: JSON.stringify({
          token: "super-secret",
        }),
      },
      undefined,
      { actionName: "daemon.start" },
    );

    expect(result).toEqual({
      kind: "error",
      code: "conflict",
      message: "daemon already running",
      exitCode: 4,
      correlationId: "srv-cid-42",
      httpStatus: 409,
      method: "POST",
      requestPath: "/api/v1/daemon/start",
    });

    const failureEvents = listFailedTelemetryEvents();
    expect(failureEvents).toHaveLength(1);
    expect(failureEvents[0]).toMatchObject({
      eventType: "request_failure",
      action: "daemon.start",
      method: "POST",
      path: "/api/v1/daemon/start",
      correlationId: "srv-cid-42",
      httpStatus: 409,
      error: {
        code: "conflict",
        message: "daemon already running",
        exitCode: 4,
      },
      request: {
        body: {
          token: "[REDACTED]",
        },
      },
    });
  });

  it("emits request_start and request_success telemetry with canonical correlation and redaction", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse(
        okEnvelope({
          taskId: "TASK-019",
          token: "server-secret",
        }),
        {
          status: 201,
          headers: {
            "Set-Cookie": "session=secret",
            "X-AO-Correlation-ID": "server-cid-9",
          },
        },
      ),
    );

    const result = await requestAo<{ taskId: string; token: string }>(
      "/api/v1/tasks?mode=sync&token=super-secret",
      {
        method: "POST",
        headers: {
          Authorization: "Bearer hidden",
        },
        body: JSON.stringify({
          password: "top-secret",
          title: "Structured observability",
        }),
      },
      undefined,
      {
        actionName: "tasks.create",
        correlationId: "  client-cid-1  ",
      },
    );

    expect(result).toEqual({
      kind: "ok",
      data: {
        taskId: "TASK-019",
        token: "server-secret",
      },
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = new Headers(init.headers);
    expect(headers.get("X-AO-Correlation-ID")).toBe("client-cid-1");

    const events = listTelemetryEvents();
    expect(events).toHaveLength(2);

    expect(events[0]).toMatchObject({
      eventType: "request_start",
      action: "tasks.create",
      correlationId: "client-cid-1",
      method: "POST",
      path: "/api/v1/tasks",
      request: {
        headers: {
          authorization: REDACTED_VALUE,
          "x-ao-correlation-id": "client-cid-1",
        },
        query: {
          mode: "sync",
          token: REDACTED_VALUE,
        },
        body: {
          password: REDACTED_VALUE,
          title: "Structured observability",
        },
      },
    });

    expect(events[1]).toMatchObject({
      eventType: "request_success",
      action: "tasks.create",
      correlationId: "server-cid-9",
      httpStatus: 201,
      response: {
        headers: {
          "set-cookie": REDACTED_VALUE,
          "x-ao-correlation-id": "server-cid-9",
        },
        body: {
          data: {
            taskId: "TASK-019",
            token: REDACTED_VALUE,
          },
          ok: true,
          schema: "ao.cli.v1",
        },
      },
    });
  });
});

describe("api endpoint contract", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
    fetchMock.mockResolvedValue(jsonResponse(okEnvelope({})));
    resetTelemetryStoreForTests();
    resetCorrelationSequenceForTests();
  });

  it("uses stable read endpoints for shell routes", async () => {
    await api.daemonStatus();
    await api.projectsList();
    await api.tasksList();
    await api.workflowsList();
    await api.projectsActive();

    const requestedPaths = fetchMock.mock.calls.map((call) => call[0] as string);

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

    expect(result).toMatchObject({
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

    expect(result).toMatchObject({
      kind: "error",
      code: "not_found",
      message: "task not found",
      exitCode: 3,
      method: "GET",
      requestPath: "/api/v1/tasks/TASK-404",
    });
  });
});
