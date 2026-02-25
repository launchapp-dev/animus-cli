import type { DaemonStatusValue, TaskStatusValue, TaskTypeValue } from "./models";

function normalizeToken(value: string): string {
  return value.trim().toLowerCase().replace(/[\s_-]/g, "");
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
