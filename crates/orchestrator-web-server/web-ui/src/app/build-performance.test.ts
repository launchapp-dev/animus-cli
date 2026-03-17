import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const viteConfigPath = resolve(import.meta.dirname, "../../vite.config.ts");
const packageJsonPath = resolve(import.meta.dirname, "../../package.json");
const budgetScriptPath = resolve(import.meta.dirname, "../../scripts/check-performance-budgets.mjs");

describe("build performance baselines", () => {
  it("enforces warning thresholds and stable vendor chunking", () => {
    const viteConfigContents = readFileSync(viteConfigPath, "utf8");

    expect(viteConfigContents).toContain("cssCodeSplit: true");
    expect(viteConfigContents).toContain("chunkSizeWarningLimit: 240");
    expect(viteConfigContents).toContain("manualChunks(id)");
    expect(viteConfigContents).toContain('id.includes("node_modules/react-router")');
    expect(viteConfigContents).toContain(
      'id.includes("node_modules/react") || id.includes("node_modules/scheduler")',
    );
    expect(viteConfigContents).toContain('return "routing-vendor"');
    expect(viteConfigContents).toContain('return "react-vendor"');
  });

  it("wires deterministic gzip budget checks for embedded entry assets", () => {
    const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8")) as {
      scripts?: Record<string, string>;
    };
    const budgetScriptContents = readFileSync(budgetScriptPath, "utf8");

    expect(packageJson.scripts?.build).toContain("check:performance-budgets");
    expect(packageJson.scripts?.["check:performance-budgets"]).toBe(
      "node scripts/check-performance-budgets.mjs",
    );
    expect(budgetScriptContents).toContain("embedded/index.html");
    expect(budgetScriptContents).toContain("before budget checks");
    expect(budgetScriptContents).toContain("JS_GZIP_BUDGET_BYTES = 110 * 1024");
    expect(budgetScriptContents).toContain("CSS_GZIP_BUDGET_BYTES = 36 * 1024");
    expect(budgetScriptContents).toContain("gzipSync");
    expect(budgetScriptContents).toContain("split(/[?#]/, 1)");
  });
});
