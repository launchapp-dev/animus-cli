// @vitest-environment node

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const CURRENT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(CURRENT_DIR, "../../../../../");
const WORKFLOWS_DIR = path.join(REPO_ROOT, ".github", "workflows");
const RELEASE_CHECKLIST_PATH = path.join(
  REPO_ROOT,
  ".github",
  "release-checklists",
  "web-gui-release.md",
);

function readWorkflow(fileName: string): string {
  const workflowPath = path.join(WORKFLOWS_DIR, fileName);
  return readFileSync(workflowPath, "utf8");
}

describe("web gui release workflow gates", () => {
  it("keeps node matrix and smoke e2e in web ui ci workflow", () => {
    const workflow = readWorkflow("web-ui-ci.yml");

    expect(workflow).toContain('.github/workflows/release-rollback-validation.yml');
    expect(workflow).toContain('.github/release-checklists/web-gui-release.md');
    expect(workflow).toMatch(/push:\s*\n[\s\S]*tags-ignore:\s*[\s\S]*["']\*["']/);
    expect(workflow).toMatch(/web-ui-matrix:\s*\n[\s\S]*timeout-minutes:\s*20/);
    expect(workflow).toMatch(/matrix:\s*[\s\S]*node:\s*[\s\S]*["']20\.x["'][\s\S]*["']22\.x["']/);
    expect(workflow).toMatch(/web-ui-smoke-e2e:\s*\n[\s\S]*needs:\s*web-ui-matrix/);
    expect(workflow).toMatch(/web-ui-smoke-e2e:\s*\n[\s\S]*timeout-minutes:\s*20/);
    expect(workflow).not.toMatch(/web-ui-smoke-e2e:\s*\n[\s\S]*\n\s*if:\s*always\(\)/);
    expect(workflow).toContain("run: npm run test");
    expect(workflow).toContain("run: npm run build");
    expect(workflow).toContain("run: npm run test:e2e:smoke");
    expect(workflow).toMatch(/Upload smoke diagnostics[\s\S]*if:\s*failure\(\)/);
  });

  it("fails closed in release workflow before publishing artifacts", () => {
    const workflow = readWorkflow("release.yml");

    expect(workflow).toMatch(/web-ui-gates:\s*\n\s*name:\s*Web UI Gates/);
    expect(workflow).toMatch(/web-ui-gates:\s*\n[\s\S]*timeout-minutes:\s*25/);
    expect(workflow).toMatch(/build:\s*\n[\s\S]*needs:\s*web-ui-gates/);
    expect(workflow).toContain("run: npm run test");
    expect(workflow).toContain("run: npm run build");
    expect(workflow).toContain("run: npm run test:e2e:smoke");
    expect(workflow).toMatch(/Upload smoke diagnostics[\s\S]*if:\s*failure\(\)/);
  });

  it("keeps rollback validation as smoke only and non-publishing", () => {
    const workflow = readWorkflow("release-rollback-validation.yml");

    expect(workflow).toMatch(/candidate_ref:\s*[\s\S]*required:\s*true/);
    expect(workflow).toMatch(/rollback_ref:\s*[\s\S]*required:\s*true/);
    expect(workflow).toMatch(/candidate_smoke:\s*\n[\s\S]*timeout-minutes:\s*25/);
    expect(workflow).toMatch(/rollback_smoke:\s*\n[\s\S]*timeout-minutes:\s*25/);
    expect(workflow).toMatch(/summary:\s*\n[\s\S]*timeout-minutes:\s*10/);
    expect(workflow).toContain('echo "- candidate_smoke: \\`${{ needs.candidate_smoke.result }}\\`"');
    expect(workflow).toContain('echo "- rollback_smoke: \\`${{ needs.rollback_smoke.result }}\\`"');
    expect(workflow).toContain("run: npm run test:e2e:smoke");
    expect(workflow).toContain("candidate_ref and rollback_ref smoke validations must both pass.");
    expect(workflow).toContain('echo "- mutation: \\`none\\`"');
    expect(workflow).toContain('echo "- publish: \\`disabled\\`"');
    expect(workflow).not.toContain("action-gh-release");
    expect(workflow).not.toContain("contents: write");
  });

  it("retains operator release checklist entries for gates and rollback", () => {
    const checklist = readFileSync(RELEASE_CHECKLIST_PATH, "utf8");

    expect(checklist).toContain("`web-ui-ci.yml` matrix completed successfully for Node `20.x` and `22.x`.");
    expect(checklist).toContain("Smoke E2E check completed successfully.");
    expect(checklist).toContain("Release workflow `web-ui-gates` job completed successfully.");
    expect(checklist).toContain("`release-rollback-validation.yml` run executed for:");
    expect(checklist).toContain("Operator go/no-go sign-off recorded.");
  });
});
