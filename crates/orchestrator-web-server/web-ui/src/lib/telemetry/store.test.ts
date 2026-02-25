import { beforeEach, describe, expect, it } from "vitest";

import {
  FAILED_DIAGNOSTICS_CAPACITY,
  listFailedTelemetryEvents,
  listTelemetryEvents,
  recordTelemetryEvent,
  resetTelemetryStoreForTests,
  TELEMETRY_EVENT_CAPACITY,
} from "./store";

describe("telemetry store", () => {
  beforeEach(() => {
    resetTelemetryStoreForTests();
  });

  it("evicts oldest records when capacity is exceeded", () => {
    for (let index = 0; index < TELEMETRY_EVENT_CAPACITY + 4; index += 1) {
      recordTelemetryEvent({
        eventType: "request_start",
        timestamp: `2026-02-25T00:00:${String(index).padStart(2, "0")}Z`,
        correlationId: `cid-${index}`,
        method: "GET",
        path: "/api/v1/tasks",
        action: "GET /api/v1/tasks",
        request: {
          headers: { accept: "application/json" },
          query: {},
        },
      });
    }

    const records = listTelemetryEvents();
    expect(records).toHaveLength(TELEMETRY_EVENT_CAPACITY);
    expect(records[0].correlationId).toBe("cid-4");
    expect(records[records.length - 1].correlationId).toBe(
      `cid-${TELEMETRY_EVENT_CAPACITY + 3}`,
    );
  });

  it("returns failed diagnostics in newest-first order with bounded limit", () => {
    for (let index = 0; index < FAILED_DIAGNOSTICS_CAPACITY + 2; index += 1) {
      recordTelemetryEvent({
        eventType: "request_failure",
        timestamp: `2026-02-25T00:00:${String(index).padStart(2, "0")}Z`,
        correlationId: `cid-${index}`,
        method: "POST",
        path: "/api/v1/daemon/start",
        action: "daemon.start",
        durationMs: index + 1,
        request: {
          headers: { "x-ao-correlation-id": `cid-${index}` },
          query: {},
        },
        error: {
          code: "conflict",
          message: "already running",
          exitCode: 4,
        },
      });
    }

    const failures = listFailedTelemetryEvents();
    expect(failures).toHaveLength(FAILED_DIAGNOSTICS_CAPACITY);
    expect(failures[0].correlationId).toBe(`cid-${FAILED_DIAGNOSTICS_CAPACITY + 1}`);
    expect(failures[failures.length - 1].correlationId).toBe("cid-2");
  });
});
