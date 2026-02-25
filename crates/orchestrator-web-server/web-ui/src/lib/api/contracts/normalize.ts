import type {
  DaemonStatusValue,
  RequirementPriorityValue,
  RequirementStatusValue,
  RequirementTypeValue,
  TaskStatusValue,
  TaskTypeValue,
} from "./models";

function normalizeToken(value: string): string {
  return value.trim().toLowerCase().replace(/[\s_'"’-]/g, "");
}

export function normalizeDaemonStatus(value: string): DaemonStatusValue {
  switch (normalizeToken(value)) {
    case "starting":
      return "starting";
    case "running":
      return "running";
    case "paused":
      return "paused";
    case "stopping":
      return "stopping";
    case "stopped":
      return "stopped";
    case "crashed":
    case "crash":
      return "crashed";
    default:
      return "unknown";
  }
}

export function normalizeTaskStatus(value: string): TaskStatusValue {
  switch (normalizeToken(value)) {
    case "todo":
    case "backlog":
      return "backlog";
    case "ready":
      return "ready";
    case "inprogress":
      return "in-progress";
    case "blocked":
      return "blocked";
    case "onhold":
      return "on-hold";
    case "done":
    case "completed":
      return "done";
    case "cancelled":
    case "canceled":
      return "cancelled";
    default:
      return "unknown";
  }
}

export function normalizeTaskType(value: string): TaskTypeValue {
  switch (normalizeToken(value)) {
    case "feature":
      return "feature";
    case "bug":
    case "bugfix":
      return "bugfix";
    case "hotfix":
      return "hotfix";
    case "refactor":
      return "refactor";
    case "docs":
    case "documentation":
    case "doc":
      return "docs";
    case "test":
    case "tests":
    case "testing":
      return "test";
    case "chore":
      return "chore";
    case "experiment":
      return "experiment";
    default:
      return "unknown";
  }
}

export function normalizeRequirementPriority(value: string): RequirementPriorityValue {
  switch (normalizeToken(value)) {
    case "must":
      return "must";
    case "should":
      return "should";
    case "could":
      return "could";
    case "wont":
      return "wont";
    default:
      return "unknown";
  }
}

export function normalizeRequirementStatus(value: string): RequirementStatusValue {
  switch (normalizeToken(value)) {
    case "draft":
      return "draft";
    case "refined":
      return "refined";
    case "planned":
      return "planned";
    case "inprogress":
      return "in-progress";
    case "done":
      return "done";
    case "poreview":
      return "po-review";
    case "emreview":
      return "em-review";
    case "needsrework":
      return "needs-rework";
    case "approved":
      return "approved";
    case "implemented":
      return "implemented";
    case "deprecated":
      return "deprecated";
    default:
      return "unknown";
  }
}

export function normalizeRequirementType(value: string): RequirementTypeValue {
  switch (normalizeToken(value)) {
    case "product":
      return "product";
    case "functional":
      return "functional";
    case "nonfunctional":
      return "non-functional";
    case "technical":
      return "technical";
    case "other":
      return "other";
    default:
      return "unknown";
  }
}
