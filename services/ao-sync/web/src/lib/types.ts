export type TaskStatus = "backlog" | "ready" | "in-progress" | "blocked" | "on-hold" | "done" | "cancelled";
export type TaskPriority = "critical" | "high" | "medium" | "low";
export type TaskType = "feature" | "bugfix" | "hotfix" | "refactor" | "docs" | "test" | "chore" | "experiment";
export type RequirementStatus = "draft" | "refined" | "planned" | "in-progress" | "done" | "po-review" | "em-review" | "needs-rework" | "approved" | "implemented" | "deprecated";
export type RequirementPriority = "must" | "should" | "could" | "wont";

export interface Task {
  id: string;
  title: string;
  description: string;
  type: TaskType;
  status: TaskStatus;
  priority: TaskPriority;
  risk: string | null;
  scope: string | null;
  complexity: string | null;
  impact_area: string[];
  assignee: { type: string; role?: string; model?: string; user_id?: string };
  estimated_effort: string | null;
  deadline: string | null;
  paused: boolean;
  cancelled: boolean;
  blocked_reason: string | null;
  blocked_by: string | null;
  branch_name: string | null;
  dependencies: Array<{ task_id: string; dependency_type: string }>;
  linked_requirements: string[];
  tags: string[];
  checklist: Array<{ id: string; description: string; completed: boolean }>;
  metadata: { created_at: string; updated_at: string; created_by: string; updated_by: string; version: number };
}

export interface Requirement {
  id: string;
  title: string;
  description: string;
  body: string | null;
  category: string | null;
  type: string | null;
  priority: RequirementPriority;
  status: RequirementStatus;
  acceptance_criteria: string[];
  tags: string[];
  linked_task_ids: string[];
  source: string;
  created_at: string;
  updated_at: string;
}

export interface Project {
  id: string;
  organizationId: string;
  name: string;
  repoOriginUrl: string;
  createdAt: string;
  updatedAt: string;
}

export interface Organization {
  id: string;
  name: string;
  slug: string;
  logo: string | null;
  createdAt: string;
}
