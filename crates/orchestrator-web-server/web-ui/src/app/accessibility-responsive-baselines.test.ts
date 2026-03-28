// @vitest-environment jsdom

import { createElement } from "react";
import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { RouterProvider, createMemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  useQuery: vi.fn(),
  useMutation: vi.fn(),
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
  GraphQLProvider: ({ children }: { children: ReactNode }) => children,
}));

import { AppShellLayout, MAIN_CONTENT_ID } from "./shell";

describe("accessibility and responsive baselines", () => {
  beforeEach(() => {
    mocks.useQuery.mockReturnValue([{ data: null, fetching: false, error: null }, vi.fn()]);
    mocks.useMutation.mockReturnValue([{ fetching: false }, vi.fn()]);
  });

  it("renders keyboard navigation landmarks in the shell", () => {
    renderShell();

    const main = screen.getByRole("main");
    expect(main.id).toBe(MAIN_CONTENT_ID);
    expect(main.getAttribute("tabindex")).toBe("-1");
    expect(screen.getByRole("navigation", { name: "Primary" })).toBeDefined();
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toBeDefined();
  });
});

function renderShell() {
  const router = createMemoryRouter(
    [
      {
        path: "/",
        element: createElement(AppShellLayout),
        children: [
          {
            path: "dashboard",
            element: createElement("section", null, "Dashboard"),
          },
        ],
      },
    ],
    {
      initialEntries: ["/dashboard"],
    },
  );

  return render(createElement(RouterProvider, { router }));
}
