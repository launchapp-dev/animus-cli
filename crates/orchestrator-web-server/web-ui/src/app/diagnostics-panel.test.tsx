// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { DiagnosticsPanel } from "./diagnostics-panel";
import { recordTelemetryEvent, resetTelemetryStoreForTests } from "../lib/telemetry";

describe("DiagnosticsPanel", () => {
  beforeEach(() => {
    resetTelemetryStoreForTests();
    Object.defineProperty(globalThis.navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: vi.fn().mockResolvedValue(undefined),
      },
    });
  });

  it("shows an empty state when no matching failures are present", () => {
    render(<DiagnosticsPanel title="Daemon Diagnostics" actionPrefixes={["daemon."]} />);

    expect(screen.getByText("No failed actions recorded in this session.")).toBeTruthy();
    expect(screen.getByText("No diagnostics failures available.")).toBeTruthy();
  });

  it("renders matching failures, supports detail toggle, copy, and clear", async () => {
    recordTelemetryEvent({
      eventType: "request_failure",
      timestamp: "2026-02-25T10:00:00.000Z",
      correlationId: "cid-daemon",
      method: "POST",
      path: "/api/v1/daemon/start",
      action: "daemon.start",
      durationMs: 42,
      httpStatus: 409,
      request: {
        headers: {
          "x-ao-correlation-id": "cid-daemon",
        },
        query: {},
        body: {
          token: "[REDACTED]",
        },
      },
      response: {
        headers: {
          "content-type": "application/json",
        },
        body: {
          schema: "ao.cli.v1",
          ok: false,
        },
      },
      error: {
        code: "conflict",
        message: "daemon already running",
        exitCode: 4,
      },
    });

    recordTelemetryEvent({
      eventType: "request_failure",
      timestamp: "2026-02-25T10:01:00.000Z",
      correlationId: "cid-review",
      method: "POST",
      path: "/api/v1/reviews/handoff",
      action: "reviews.handoff.submit",
      durationMs: 28,
      request: {
        headers: {
          "x-ao-correlation-id": "cid-review",
        },
        query: {},
      },
      error: {
        code: "invalid_payload",
        message: "bad payload",
        exitCode: 1,
      },
    });

    render(<DiagnosticsPanel title="Daemon Diagnostics" actionPrefixes={["daemon."]} />);

    expect(screen.getByText("1 failed action recorded.")).toBeTruthy();
    expect(screen.queryByText("reviews.handoff.submit")).toBeNull();
    expect(screen.getByText("daemon.start")).toBeTruthy();

    const expandButton = screen.getByRole("button", {
      name: "Show diagnostics for daemon.start failure",
    });
    fireEvent.click(expandButton);

    expect(screen.getByText("Correlation ID")).toBeTruthy();

    const copyButton = screen.getByRole("button", {
      name: "Copy correlation ID for daemon.start failure",
    });
    fireEvent.click(copyButton);

    const clipboard = globalThis.navigator.clipboard as { writeText: (value: string) => Promise<void> };
    await waitFor(() => {
      expect(clipboard.writeText).toHaveBeenCalledWith("cid-daemon");
    });

    const clearButton = screen.getByRole("button", {
      name: "Clear diagnostics from Daemon Diagnostics",
    });
    fireEvent.click(clearButton);

    expect(screen.getByText("No diagnostics failures available.")).toBeTruthy();
  });
});
