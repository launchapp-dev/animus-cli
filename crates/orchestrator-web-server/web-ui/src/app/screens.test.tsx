// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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

import { ReviewHandoffPage } from "./review-page";

describe("ReviewHandoffPage", () => {
  let executeMutation: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    executeMutation = vi.fn().mockResolvedValue({ data: { reviewHandoff: true } });
    mocks.useMutation.mockReturnValue([{ fetching: false }, executeMutation]);
    mocks.useQuery.mockReturnValue([{ data: null, fetching: false, error: null }, vi.fn()]);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders the review handoff form with required fields", () => {
    render(<ReviewHandoffPage />);

    expect(screen.getByRole("heading", { name: "Review Handoff" })).toBeTruthy();
    expect(screen.getByText("Target Role")).toBeTruthy();
    expect(screen.getByText("Question")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Submit Handoff" })).toBeTruthy();
  });

  it("submits handoff mutation with form values", async () => {
    render(<ReviewHandoffPage />);

    const textareas = screen.getAllByRole("textbox");
    fireEvent.change(textareas[0], { target: { value: "Need EM review on scope." } });

    fireEvent.click(screen.getByRole("button", { name: "Submit Handoff" }));

    await waitFor(() => {
      expect(executeMutation).toHaveBeenCalledWith(
        expect.objectContaining({
          targetRole: "em",
          question: "Need EM review on scope.",
        }),
      );
    });
  });

  it("shows success feedback after submission", async () => {
    render(<ReviewHandoffPage />);

    const textareas = screen.getAllByRole("textbox");
    fireEvent.change(textareas[0], { target: { value: "Review scope" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit Handoff" }));

    await waitFor(() => {
      expect(mocks.toastSuccess).toHaveBeenCalledWith("Review handoff submitted.");
    });
  });

  it("shows error feedback when mutation fails", async () => {
    executeMutation.mockResolvedValue({ error: { message: "Network error" } });

    render(<ReviewHandoffPage />);

    const textareas = screen.getAllByRole("textbox");
    fireEvent.change(textareas[0], { target: { value: "Review scope" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit Handoff" }));

    await waitFor(() => {
      expect(mocks.toastError).toHaveBeenCalledWith("Network error");
    });
  });
});
