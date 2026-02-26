// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  useQuery: vi.fn(),
  useMutation: vi.fn(),
  toastSuccess: vi.fn(),
  toastError: vi.fn(),
}));

vi.mock("@/lib/graphql/client", async () => {
  const actual = await vi.importActual("@/lib/graphql/client");
  return {
    ...actual,
    useQuery: mocks.useQuery,
    useMutation: mocks.useMutation,
  };
});

vi.mock("@/lib/graphql/provider", () => ({
  GraphQLProvider: ({ children }: { children: React.ReactNode }) => children,
}));

vi.mock("sonner", () => ({
  toast: {
    success: mocks.toastSuccess,
    error: mocks.toastError,
  },
}));

import { DaemonPage } from "./daemon-page";

describe("DaemonPage", () => {
  let executeMutation: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    executeMutation = vi.fn().mockResolvedValue({ data: {} });
    mocks.useMutation.mockReturnValue([{ fetching: false }, executeMutation]);
    mocks.useQuery.mockReturnValue([
      {
        data: {
          daemonStatus: {
            healthy: true,
            status: "Healthy",
            statusRaw: "healthy",
            runnerConnected: true,
            activeAgents: 1,
            maxAgents: 4,
            projectRoot: "/repo",
          },
          daemonHealth: {
            healthy: true,
            status: "Healthy",
            runnerConnected: true,
            runnerPid: 1234,
            activeAgents: 1,
            daemonPid: 5678,
          },
          agentRuns: [],
          daemonLogs: [
            { timestamp: "2026-02-25T10:00:00Z", level: "info", message: "daemon booted" },
          ],
        },
        fetching: false,
        error: null,
      },
      vi.fn(),
    ]);
  });

  it("renders daemon status and controls", () => {
    render(<DaemonPage />);

    expect(screen.getByText("Daemon")).toBeTruthy();
    expect(screen.getByText("healthy")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Start" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Resume" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Clear" })).toBeTruthy();
  });

  it("renders log entries", () => {
    render(<DaemonPage />);

    expect(screen.getByText("daemon booted")).toBeTruthy();
  });

  it("executes start mutation on button click", async () => {
    render(<DaemonPage />);

    fireEvent.click(screen.getByRole("button", { name: "Start" }));

    await waitFor(() => {
      expect(executeMutation).toHaveBeenCalledWith({});
    });
  });

  it("shows error feedback when mutation fails", async () => {
    executeMutation.mockResolvedValue({ error: { message: "daemon already running" } });

    render(<DaemonPage />);

    fireEvent.click(screen.getByRole("button", { name: "Start" }));

    await waitFor(() => {
      expect(mocks.toastError).toHaveBeenCalledWith("daemon already running");
    });
  });

  it("shows success feedback when mutation succeeds", async () => {
    render(<DaemonPage />);

    fireEvent.click(screen.getByRole("button", { name: "Pause" }));

    await waitFor(() => {
      expect(mocks.toastSuccess).toHaveBeenCalledWith("Pause successful.");
    });
  });

  it("shows loading state while fetching", () => {
    mocks.useQuery.mockReturnValue([{ data: null, fetching: true, error: null }, vi.fn()]);

    render(<DaemonPage />);

    const skeletons = document.querySelectorAll('[data-slot="skeleton"]');
    expect(skeletons.length).toBeGreaterThan(0);
  });

  it("shows error state when query fails", () => {
    mocks.useQuery.mockReturnValue([
      { data: null, fetching: false, error: { message: "Connection refused" } },
      vi.fn(),
    ]);

    render(<DaemonPage />);

    expect(screen.getByText("Connection refused")).toBeTruthy();
  });
});
