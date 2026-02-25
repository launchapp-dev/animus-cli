import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const viteConfigPath = resolve(import.meta.dirname, "../../vite.config.ts");

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
});
