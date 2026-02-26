// @vitest-environment node

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const CURRENT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(CURRENT_DIR, "../../../../../");
const WORKFLOWS_DIR = path.join(REPO_ROOT, ".github", "workflows");
const CARGO_CONFIG_PATH = path.join(REPO_ROOT, ".cargo", "config.toml");
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

function countMatches(content: string, pattern: RegExp): number {
  return content.match(pattern)?.length ?? 0;
}

describe("web gui release workflow gates", () => {
  it("keeps node matrix and smoke e2e in web ui ci workflow", () => {
    const workflow = readWorkflow("web-ui-ci.yml");

    expect(workflow).toContain('.github/workflows/release-rollback-validation.yml');
    expect(workflow).toContain('.github/release-checklists/web-gui-release.md');
    expect(workflow).toMatch(/matrix:\s*[\s\S]*node:\s*[\s\S]*["']20\.x["'][\s\S]*["']22\.x["']/);
    expect(workflow).toContain("run: npm run test");
    expect(workflow).toContain("run: npm run build");
    expect(workflow).toContain("run: npm run test:e2e:smoke");
    expect(workflow).toMatch(/Upload smoke diagnostics[\s\S]*if:\s*failure\(\)/);
  });

  it("fails closed in release workflow before publishing artifacts", () => {
    const workflow = readWorkflow("release.yml");

    expect(workflow).toMatch(/web-ui-gates:\s*\n\s*name:\s*Web UI Gates/);
    expect(workflow).toMatch(/build:\s*\n[\s\S]*needs:\s*web-ui-gates/);
    expect(workflow).toContain("run: npm run test");
    expect(workflow).toContain("run: npm run build");
    expect(workflow).toContain("run: npm run test:e2e:smoke");
    expect(workflow).toMatch(/Upload smoke diagnostics[\s\S]*if:\s*failure\(\)/);
  });

  it("keeps deterministic release matrix and artifact packaging contract", () => {
    const workflow = readWorkflow("release.yml");
    const cargoConfig = readFileSync(CARGO_CONFIG_PATH, "utf8");

    expect(workflow).toMatch(/os:\s*ubuntu-latest[\s\S]*target:\s*x86_64-unknown-linux-gnu[\s\S]*archive_ext:\s*tar\.gz/);
    expect(workflow).toMatch(/os:\s*macos-15-intel[\s\S]*target:\s*x86_64-apple-darwin[\s\S]*archive_ext:\s*tar\.gz/);
    expect(workflow).toMatch(/os:\s*macos-14[\s\S]*target:\s*aarch64-apple-darwin[\s\S]*archive_ext:\s*tar\.gz/);
    expect(workflow).toMatch(/os:\s*windows-latest[\s\S]*target:\s*x86_64-pc-windows-msvc[\s\S]*archive_ext:\s*zip/);

    expect(workflow).toContain("cargo ao-bin-build-release --target ${{ matrix.target }}");
    expect(cargoConfig).toContain(
      'ao-bin-build-release = "build --release -p orchestrator-cli -p agent-runner -p llm-cli-wrapper -p llm-mcp-server"',
    );
    expect(countMatches(workflow, /\n\s*-\sos:\s/g)).toBe(4);
    expect(
      countMatches(
        workflow,
        /target:\s*(x86_64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin|x86_64-pc-windows-msvc)/g,
      ),
    ).toBe(4);
    expect(countMatches(workflow, /archive_ext:\s*(tar\.gz|zip)/g)).toBe(4);

    expect(workflow).toContain("BINARIES=(ao agent-runner llm-cli-wrapper llm-mcp-server)");
    expect(workflow).toContain('"files": binaries');
    expect(workflow).toContain('json.dumps(metadata, indent=2, sort_keys=True) + "\\n"');
    expect(workflow).toContain('$BinaryFiles = $Binaries | ForEach-Object { "$_.exe" }');
    expect(workflow).toContain('$Binaries = @("ao", "agent-runner", "llm-cli-wrapper", "llm-mcp-server")');
    expect(workflow).toContain("dry_run_note");
    expect(workflow).toContain("event_name");
    expect(workflow).toContain("files = $BinaryFiles");
    expect(countMatches(workflow, /- name:\s*Upload artifact\n/g)).toBe(1);
    expect(workflow).toContain("path: ao-${{ steps.version.outputs.value }}-${{ matrix.target }}.${{ matrix.archive_ext }}");
    expect(workflow).toContain("no release archives found after download-artifact step");
  });

  it("sanitizes preview version names for artifact-safe output", () => {
    const workflow = readWorkflow("release.yml");

    expect(workflow).toContain("sed -E 's/[^A-Za-z0-9._-]+/-/g; s/^-+//; s/-+$//')");
    expect(workflow).toContain("BRANCH_SANITIZED=\"ref\"");
    expect(workflow).toContain("VERSION=\"${BRANCH_SANITIZED}-${GITHUB_SHA::7}\"");
  });

  it("keeps rollback validation as smoke only and non-publishing", () => {
    const workflow = readWorkflow("release-rollback-validation.yml");

    expect(workflow).toMatch(/candidate_ref:\s*[\s\S]*required:\s*true/);
    expect(workflow).toMatch(/rollback_ref:\s*[\s\S]*required:\s*true/);
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
