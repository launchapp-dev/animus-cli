// @vitest-environment jsdom

import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const requestAoMock = vi.hoisted(() => vi.fn());

vi.mock("../api/client", () => ({
  requestAo: requestAoMock,
}));

import { useRunTelemetry } from "./use-run-telemetry";

type MockReader = {
  read: () => Promise<ReadableStreamReadResult<string>>;
};

function createStreamingResponse(chunks: string[]): Response {
  let index = 0;
  const reader: MockReader = {
    read: () => {
      if (index < chunks.length) {
        const value = chunks[index];
        index += 1;
        return Promise.resolve({ done: false, value });
      }

      return new Promise<ReadableStreamReadResult<string>>(() => {
        // Keep stream open until the hook unmounts and aborts.
      });
    },
  };

  return {
    ok: true,
    status: 200,
    body: {
      pipeThrough: () => ({
        getReader: () => reader,
      }),
    },
  } as unknown as Response;
}

describe("useRunTelemetry", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    fetchMock.mockReset();
    requestAoMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("parses run telemetry entries from SSE stream", async () => {
    fetchMock.mockResolvedValue(
      createStreamingResponse([
        [
          "id: 5",
          "event: run-telemetry",
          'data: {"cursor":5,"source_file":"events.jsonl","line":"{\\"kind\\":\\"run-start\\"}"}',
          "",
          "",
        ].join("\n"),
      ]),
    );

    const { result, unmount } = renderHook(() =>
      useRunTelemetry({
        runId: "run-1",
      }),
    );

    await waitFor(() => {
      expect(result.current.connectionState).toBe("streaming");
    });
    await waitFor(() => {
      expect(result.current.entries).toHaveLength(1);
    });

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = new Headers(init.headers);

    expect(path).toBe("/api/v1/output/runs/run-1/telemetry/stream?limit=200");
    expect(headers.get("Accept")).toBe("text/event-stream");
    expect(result.current.entries[0]).toMatchObject({
      cursor: 5,
      source_file: "events.jsonl",
      line: "{\"kind\":\"run-start\"}",
    });
    expect(requestAoMock).not.toHaveBeenCalled();

    unmount();
  });

  it("falls back to polling when stream connection fails", async () => {
    fetchMock.mockResolvedValue({
      ok: false,
      status: 503,
      body: null,
    } as Response);
    requestAoMock.mockResolvedValue({
      kind: "ok",
      data: {
        cursor: 2,
        entries: [
          {
            cursor: 2,
            source_file: "events.jsonl",
            line: "{\"kind\":\"poll\"}",
          },
        ],
      },
    });

    const { result, unmount } = renderHook(() =>
      useRunTelemetry({
        runId: "  run-1  ",
        taskId: " task-9 ",
        phaseId: " phase-1 ",
      }),
    );

    await waitFor(() => {
      expect(result.current.connectionState).toBe("polling");
    });
    await waitFor(() => {
      expect(requestAoMock).toHaveBeenCalledTimes(1);
    });

    expect(requestAoMock).toHaveBeenCalledWith(
      "/api/v1/output/runs/run-1/telemetry?after=0&limit=200&task_id=task-9&phase_id=phase-1",
    );
    await waitFor(() => {
      expect(result.current.entries).toHaveLength(1);
    });
    expect(result.current.entries[0]).toMatchObject({
      cursor: 2,
      line: "{\"kind\":\"poll\"}",
    });

    unmount();
  });
});
