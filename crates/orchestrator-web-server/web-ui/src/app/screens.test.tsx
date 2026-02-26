// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api } from "../lib/api/client";
import { resetTelemetryStoreForTests } from "../lib/telemetry";
import { ReviewHandoffPage } from "./screens";

describe("ReviewHandoffPage accessibility semantics", () => {
  beforeEach(() => {
    resetTelemetryStoreForTests();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("associates helper and validation text with form controls", async () => {
    render(<ReviewHandoffPage />);

    const runIdInput = screen.getByRole("textbox", { name: /run id/i });
    const questionInput = screen.getByRole("textbox", { name: /question/i });
    const contextInput = screen.getByRole("textbox", { name: /context json/i });

    expect(runIdInput.getAttribute("aria-describedby")).toBe("review-handoff-run-id-hint");
    expect(questionInput.getAttribute("aria-describedby")).toBe("review-handoff-question-hint");
    expect(contextInput.getAttribute("aria-describedby")).toBe("review-handoff-context-hint");

    fireEvent.change(contextInput, { target: { value: "{" } });
    const submitButton = screen.getByRole("button", { name: "Submit Handoff" });
    const form = submitButton.closest("form");
    if (!form) {
      throw new Error("Expected review handoff form to exist");
    }
    fireEvent.submit(form);

    await waitFor(() => {
      expect(screen.getByText("Run ID is required.")).toBeTruthy();
      expect(screen.getByText("Question is required.")).toBeTruthy();
      expect(screen.getByText("Context JSON must be valid JSON.")).toBeTruthy();
    });

    expect(runIdInput.getAttribute("aria-invalid")).toBe("true");
    expect(questionInput.getAttribute("aria-invalid")).toBe("true");
    expect(contextInput.getAttribute("aria-invalid")).toBe("true");
    expect(runIdInput.getAttribute("aria-describedby")).toBe(
      "review-handoff-run-id-hint review-handoff-run-id-error",
    );
    expect(questionInput.getAttribute("aria-describedby")).toBe(
      "review-handoff-question-hint review-handoff-question-error",
    );
    expect(contextInput.getAttribute("aria-describedby")).toBe(
      "review-handoff-context-hint review-handoff-context-error",
    );
  });

  it("announces successful submissions in a polite status region", async () => {
    const reviewHandoffSpy = vi.spyOn(api, "reviewHandoff").mockResolvedValue({
      kind: "ok",
      data: {
        success: true,
      },
    } as never);

    render(<ReviewHandoffPage />);

    fireEvent.change(screen.getByRole("textbox", { name: /run id/i }), {
      target: { value: "run-123" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: /question/i }), {
      target: { value: "Need EM review on scope." },
    });
    fireEvent.change(screen.getByRole("textbox", { name: /context json/i }), {
      target: { value: "{\"scope\":\"task-017\"}" },
    });

    fireEvent.click(screen.getByRole("button", { name: "Submit Handoff" }));

    await waitFor(() => {
      expect(reviewHandoffSpy).toHaveBeenCalledWith({
        run_id: "run-123",
        target_role: "em",
        question: "Need EM review on scope.",
        context: {
          scope: "task-017",
        },
      });
    });

    const statusMessage = await screen.findByText("Review handoff submitted successfully.");
    expect(statusMessage.getAttribute("role")).toBe("status");
    expect(screen.getByText("Response")).toBeTruthy();
  });
});
