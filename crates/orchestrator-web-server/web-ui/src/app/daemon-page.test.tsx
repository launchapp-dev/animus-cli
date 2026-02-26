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

const okResult = <T,>(data: T) => ({ kind: "ok" as const, data });
const errorResult = (code: string, message: string, status: number) => ({
  kind: "error" as const,
  error: { code, message, status },
});

const apiMocks = vi.hoisted(() => ({
  daemonStart: vi.fn(),
  daemonPause: vi.fn(),
  daemonResume: vi.fn(),
  daemonStop: vi.fn(),
  daemonClearLogs: vi.fn(),
}));

vi.mock("@/lib/api/client", () => ({
  useDaemonStart: () => [apiMocks.daemonStart],
  useDaemonPause: () => [apiMocks.daemonPause],
  useDaemonResume: () => [apiMocks.daemonResume],
  useDaemonStop: () => [apiMocks.daemonStop],
  useDaemonClearLogs: () => [apiMocks.daemonClearLogs],
}));

async function renderDaemonPage() {
  return render(<DaemonPage />);
}

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
    apiMocks.daemonStart.mockReturnValue(okResult({ message: "start ok" }));
    apiMocks.daemonPause.mockReturnValue(okResult({ message: "pause ok" }));
    apiMocks.daemonResume.mockReturnValue(okResult({ message: "resume ok" }));
    apiMocks.daemonStop.mockReturnValue(okResult({ message: "stop ok" }));
    apiMocks.daemonClearLogs.mockReturnValue(okResult({ message: "clear ok" }));
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

  it("opens modal safeguards and enforces exact typed phrase before execution", async () => {
    await renderDaemonPage();

    fireEvent.click(screen.getByRole("button", { name: "Stop Daemon" }));

    const dialog = screen.getByRole("dialog", { name: "Review High-Risk Action" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(screen.getByText("STOP DAEMON")).toBeTruthy();
    expect(apiMocks.daemonStop).not.toHaveBeenCalled();

    const confirmButton = screen.getByRole("button", { name: "Confirm and Execute" }) as HTMLButtonElement;
    expect(confirmButton.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Confirmation phrase"), {
      target: { value: "stop daemon" },
    });
    expect(confirmButton.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Confirmation phrase"), {
      target: { value: "  STOP DAEMON  " },
    });
    expect(confirmButton.disabled).toBe(false);
  });

  it("records dry-run preview for high-risk actions without mutating API calls", async () => {
    await renderDaemonPage();

    fireEvent.click(screen.getByRole("button", { name: "Clear Daemon Logs" }));
    fireEvent.click(screen.getByRole("button", { name: "Run Dry-Run Preview" }));

    expect(apiMocks.daemonClearLogs).not.toHaveBeenCalled();
    expect(screen.getByRole("status").textContent).toContain(
      "Dry-run preview ready for Clear daemon logs.",
    );
    expect(screen.getByRole("dialog", { name: "Review High-Risk Action" })).toBeTruthy();
    const feedbackPanel = screen.getByRole("heading", { name: "Action Feedback" }).closest("div");
    expect(feedbackPanel).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/^daemon\.clear_logs$/)).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/^dry_run$/)).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/^Preview$/)).toBeTruthy();
  });

  it("executes confirmed high-risk actions and records successful auditable feedback", async () => {
    await renderDaemonPage();

    fireEvent.click(screen.getByRole("button", { name: "Stop Daemon" }));
    fireEvent.change(screen.getByLabelText("Confirmation phrase"), {
      target: { value: "STOP DAEMON" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm and Execute" }));

    await waitFor(() => {
      expect(apiMocks.daemonStop).toHaveBeenCalledTimes(1);
    });

    expect(screen.queryByRole("dialog", { name: "Review High-Risk Action" })).toBeNull();
    expect(screen.getByRole("status").textContent).toContain("stop ok");
    const feedbackPanel = screen.getByRole("heading", { name: "Action Feedback" }).closest("div");
    expect(feedbackPanel).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/^daemon\.stop$/)).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/^ok$/)).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/stop ok/)).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/Correlation ID:/)).toBeTruthy();
    expect(within(feedbackPanel!).getByText(/ao-web-/)).toBeTruthy();
  });

  it("supports escape dismissal and restores focus to the triggering control", async () => {
    await renderDaemonPage();

    const stopButton = screen.getByRole("button", { name: "Stop Daemon" });
    fireEvent.click(stopButton);

    const dialog = screen.getByRole("dialog", { name: "Review High-Risk Action" });
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Review High-Risk Action" })).toBeNull();
      expect(document.activeElement).toBe(stopButton);
    });

    expect(apiMocks.daemonStop).not.toHaveBeenCalled();
  });

  it("executes medium-risk actions directly and renders auditable failures", async () => {
    apiMocks.daemonPause.mockReturnValue(errorResult("conflict", "daemon already paused", 4));
    await renderDaemonPage();

    fireEvent.click(screen.getByRole("button", { name: "Pause Daemon" }));

    await waitFor(() => {
      expect(apiMocks.daemonPause).toHaveBeenCalledTimes(1);
    });

    expect(screen.queryByRole("dialog", { name: "Review High-Risk Action" })).toBeNull();
    expect(screen.getByRole("alert").textContent).toContain("Error: daemon_action_failed");
    expect(screen.getByRole("alert").textContent).toContain("conflict: daemon already paused");
    expect(screen.getByText(/^daemon\.pause$/)).toBeTruthy();
    expect(screen.getByText(/conflict: daemon already paused/)).toBeTruthy();
  });

  it("prevents duplicate submissions while an action request is pending", async () => {
    let resolvePause: ((value: unknown) => void) | null = null;
    apiMocks.daemonPause.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolvePause = resolve;
        }),
    );

    await renderDaemonPage();

    const pauseButton = screen.getByRole("button", { name: "Pause Daemon" }) as HTMLButtonElement;
    fireEvent.click(pauseButton);

    await waitFor(() => {
      expect(pauseButton.disabled).toBe(true);
      expect(apiMocks.daemonPause).toHaveBeenCalledTimes(1);
    });

    fireEvent.click(pauseButton);
    expect(apiMocks.daemonPause).toHaveBeenCalledTimes(1);

    resolvePause?.({
      kind: "ok",
      data: {
        message: "pause delayed ok",
      },
    });
  });

  it("keeps daemon feedback bounded to 50 records with most-recent-first ordering", async () => {
    let startSequence = 0;
    apiMocks.daemonStart.mockImplementation(() => {
      startSequence += 1;
      return okResult({ message: `start ok ${startSequence}` });
    });

    await renderDaemonPage();

    const feedbackPanel = screen.getByRole("heading", { name: "Action Feedback" }).closest("div");
    expect(feedbackPanel).toBeTruthy();
    expect(within(feedbackPanel!).getAllByText(/^daemon\.start$/).length).toBe(50);

    const feedbackItems = feedbackPanel!.querySelectorAll(".daemon-feedback-item");
    expect(feedbackItems.length).toBe(50);
    expect(feedbackItems[0]?.textContent).toContain("start ok 55");
    expect(feedbackItems[49]?.textContent).toContain("start ok 6");
  });
});
