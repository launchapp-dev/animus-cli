import { describe, expect, it } from "vitest";
import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";

const dir = import.meta.dirname;

const featureFiles = [
  { file: "dashboard-page.tsx", exports: ["DashboardPage"] },
  { file: "tasks-pages.tsx", exports: ["TasksPage", "TaskCreatePage", "TaskDetailPage"] },
  { file: "workflow-pages.tsx", exports: ["WorkflowsPage", "WorkflowDetailPage", "WorkflowCheckpointPage"] },
  { file: "queue-page.tsx", exports: ["QueuePage"] },
  { file: "daemon-page.tsx", exports: ["DaemonPage"] },
  { file: "projects-pages.tsx", exports: ["ProjectsPage", "ProjectDetailPage", "RequirementDetailPage"] },
  { file: "events-page.tsx", exports: ["EventsPage"] },
  { file: "review-page.tsx", exports: ["ReviewHandoffPage"] },
  { file: "output-page.tsx", exports: ["TaskOutputPage"] },
  { file: "not-found-page.tsx", exports: ["NotFoundPage"] },
];

describe("feature page modules", () => {
  for (const { file, exports: requiredExports } of featureFiles) {
    describe(file, () => {
      it("exists", () => {
        expect(existsSync(resolve(dir, file))).toBe(true);
      });

      it("exports all required page components", () => {
        const source = readFileSync(resolve(dir, file), "utf8");
        for (const name of requiredExports) {
          expect(source).toContain(`export function ${name}(`);
        }
      });

      it("imports hooks from @/lib/graphql/client", () => {
        const source = readFileSync(resolve(dir, file), "utf8");
        const usesHooks = source.includes("useQuery") || source.includes("useMutation") || source.includes("useSubscription");
        if (usesHooks) {
          expect(source).toContain('@/lib/graphql/client');
          expect(source).not.toMatch(/from ["']urql["']/);
        }
      });
    });
  }
});

describe("matchesConfirmationPhrase", () => {
  it("matches exact phrase with trim-only normalization", () => {
    expect(matchesConfirmationPhrase("  STOP DAEMON  ", "STOP DAEMON")).toBe(true);
  });

  it("enforces case-sensitive matching", () => {
    expect(matchesConfirmationPhrase("stop daemon", "STOP DAEMON")).toBe(false);
  });

  it("does not normalize internal whitespace", () => {
    expect(matchesConfirmationPhrase("CLEAR  DAEMON LOGS", "CLEAR DAEMON LOGS")).toBe(false);
  });
});

function matchesConfirmationPhrase(input: string, expected: string): boolean {
  return input.trim() === expected;
}
