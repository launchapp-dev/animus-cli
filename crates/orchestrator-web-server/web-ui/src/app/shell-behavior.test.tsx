// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { RouterProvider, createMemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  useQuery: vi.fn(),
  useMutation: vi.fn(),
}));

vi.mock("@/lib/graphql/client", async () => {
  const actual = await vi.importActual("@/lib/graphql/client");
  return {
    ...actual,
    useQuery: mocks.useQuery.mockReturnValue([{ data: null, fetching: false, error: null }, vi.fn()]),
    useMutation: mocks.useMutation.mockReturnValue([{ fetching: false }, vi.fn()]),
  };
});

vi.mock("@/lib/graphql/provider", () => ({
  GraphQLProvider: ({ children }: { children: ReactNode }) => children,
}));


import { AppShellLayout, MAIN_CONTENT_ID, PRIMARY_NAV_ITEMS } from "./shell";

describe("AppShellLayout structure and navigation", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders main content area with stable id", () => {
    renderShell();

    const main = document.getElementById(MAIN_CONTENT_ID);
    expect(main).toBeTruthy();
    expect(main?.tagName).toBe("MAIN");
  });

  it("renders all primary nav links in desktop sidebar", () => {
    renderShell();

    for (const item of PRIMARY_NAV_ITEMS) {
      const links = screen.getAllByText(item.label);
      expect(links.length).toBeGreaterThan(0);
    }
  });

  it("renders breadcrumb navigation", () => {
    renderShell();

    expect(screen.getByLabelText("Breadcrumb")).toBeTruthy();
  });

  it("renders command palette trigger button", () => {
    renderShell();

    expect(screen.getByText("Search")).toBeTruthy();
  });

});

function renderShell() {
  const router = createMemoryRouter(
    [
      {
        path: "/",
        element: <AppShellLayout />,
        children: [
          {
            path: "dashboard",
            element: <section>Dashboard</section>,
          },
          {
            path: "*",
            element: <section>Fallback</section>,
          },
        ],
      },
    ],
    {
      initialEntries: ["/dashboard"],
    },
  );

  return render(<RouterProvider router={router} />);
}
