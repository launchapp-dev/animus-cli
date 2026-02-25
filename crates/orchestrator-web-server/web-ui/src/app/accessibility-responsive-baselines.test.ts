import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const shellPath = resolve(import.meta.dirname, "./shell.tsx");
const routerPath = resolve(import.meta.dirname, "./router.tsx");
const stylesPath = resolve(import.meta.dirname, "../styles.css");

describe("accessibility and responsive baselines", () => {
  it("keeps keyboard navigation landmarks and controls in the shell", () => {
    const shellSource = readFileSync(shellPath, "utf8");

    expect(shellSource).toContain('const MAIN_CONTENT_ID = "main-content"');
    expect(shellSource).toContain('<a className="skip-link" href={`#${MAIN_CONTENT_ID}`}>');
    expect(shellSource).toContain('<main className="content-scroll" id={MAIN_CONTENT_ID}');
    expect(shellSource).toContain("tabIndex={-1}");
    expect(shellSource).toContain('aria-label="Primary navigation"');
    expect(shellSource).toContain('aria-label="Primary" className="primary-nav"');
    expect(shellSource).toContain("aria-expanded={isMobileMenuOpen}");
    expect(shellSource).toContain('aria-controls="primary-navigation"');
    expect(shellSource).toContain('if (event.key === "Escape")');
  });

  it("keeps route-level suspense and lazy loading to protect route performance", () => {
    const routerSource = readFileSync(routerPath, "utf8");

    expect(routerSource).toContain("const lazyScreen = (name: ScreenExport) =>");
    expect(routerSource).toContain("lazy(async () => import(\"./screens\")");
    expect(routerSource).toContain("withRouteSuspense(<DashboardPage />)");
    expect(routerSource).toContain("withRouteSuspense(<ReviewHandoffPage />)");
    expect(routerSource).toContain("<Suspense fallback={<section className=\"loading-box\">Loading route...</section>}>");
  });

  it("keeps focus visibility, responsive breakpoints, and reduced-motion safeguards", () => {
    const stylesSource = readFileSync(stylesPath, "utf8");

    expect(stylesSource).toContain(".skip-link:focus-visible");
    expect(stylesSource).toContain("outline: 3px solid var(--focus);");
    expect(stylesSource).toContain("@media (width <= 960px)");
    expect(stylesSource).toContain(".sidebar[data-open=\"true\"]");
    expect(stylesSource).toContain("@media (width <= 680px)");
    expect(stylesSource).toContain("@media (prefers-reduced-motion: reduce)");
  });
});
