import { describe, expect, it } from "vitest";

import {
  normalizeDaemonStatus,
  normalizeRequirementPriority,
  normalizeRequirementStatus,
  normalizeRequirementType,
  normalizeTaskStatus,
  normalizeTaskType,
} from "./normalize";

describe("normalizeTaskStatus", () => {
  it("maps aliases to canonical values", () => {
    expect(normalizeTaskStatus("todo")).toBe("backlog");
    expect(normalizeTaskStatus("in_progress")).toBe("in-progress");
    expect(normalizeTaskStatus("onhold")).toBe("on-hold");
    expect(normalizeTaskStatus("completed")).toBe("done");
  });

  it("falls back to unknown for unsupported values", () => {
    expect(normalizeTaskStatus("queued")).toBe("unknown");
  });
});

describe("normalizeTaskType", () => {
  it("maps aliases to canonical values", () => {
    expect(normalizeTaskType("bug")).toBe("bugfix");
    expect(normalizeTaskType("hot-fix")).toBe("hotfix");
    expect(normalizeTaskType("documentation")).toBe("docs");
    expect(normalizeTaskType("testing")).toBe("test");
  });

  it("falls back to unknown for unsupported values", () => {
    expect(normalizeTaskType("spike")).toBe("unknown");
  });
});

describe("normalizeDaemonStatus", () => {
  it("normalizes known daemon status values", () => {
    expect(normalizeDaemonStatus("running")).toBe("running");
    expect(normalizeDaemonStatus("CRASH")).toBe("crashed");
  });

  it("uses unknown fallback for unsupported daemon statuses", () => {
    expect(normalizeDaemonStatus("hibernating")).toBe("unknown");
  });
});

describe("normalizeRequirementPriority", () => {
  it("normalizes requirement priority aliases", () => {
    expect(normalizeRequirementPriority("MUST")).toBe("must");
    expect(normalizeRequirementPriority("won't")).toBe("wont");
  });

  it("falls back to unknown for unsupported values", () => {
    expect(normalizeRequirementPriority("critical")).toBe("unknown");
  });
});

describe("normalizeRequirementStatus", () => {
  it("normalizes requirement lifecycle aliases", () => {
    expect(normalizeRequirementStatus("in_progress")).toBe("in-progress");
    expect(normalizeRequirementStatus("po_review")).toBe("po-review");
    expect(normalizeRequirementStatus("needs rework")).toBe("needs-rework");
  });

  it("falls back to unknown for unsupported values", () => {
    expect(normalizeRequirementStatus("triage")).toBe("unknown");
  });
});

describe("normalizeRequirementType", () => {
  it("normalizes requirement type aliases", () => {
    expect(normalizeRequirementType("non_functional")).toBe("non-functional");
    expect(normalizeRequirementType("TECHNICAL")).toBe("technical");
  });

  it("falls back to unknown for unsupported values", () => {
    expect(normalizeRequirementType("ux")).toBe("unknown");
  });
});
