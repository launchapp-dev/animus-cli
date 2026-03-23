import { z } from "zod";

export const TaskStatusSchema = z.enum([
  "backlog",
  "ready",
  "in-progress",
  "blocked",
  "on-hold",
  "done",
  "cancelled",
]);

export const PrioritySchema = z.enum(["critical", "high", "medium", "low"]);

export const TaskTypeSchema = z.enum([
  "feature",
  "bugfix",
  "hotfix",
  "refactor",
  "docs",
  "test",
  "chore",
  "experiment",
]);

export const RiskLevelSchema = z.enum(["high", "medium", "low"]);
export const ScopeSchema = z.enum(["large", "medium", "small"]);
export const ComplexitySchema = z.enum(["high", "medium", "low"]);

export const ImpactAreaSchema = z.enum([
  "frontend",
  "backend",
  "database",
  "api",
  "infrastructure",
  "docs",
  "tests",
  "cicd",
]);

export const AssigneeSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("agent"), role: z.string(), model: z.string().nullable().optional() }),
  z.object({ type: z.literal("human"), user_id: z.string() }),
  z.object({ type: z.literal("unassigned") }),
]);

export const ChecklistItemSchema = z.object({
  id: z.string(),
  description: z.string(),
  completed: z.boolean(),
  created_at: z.string(),
  completed_at: z.string().nullable().optional(),
});

export const TaskDependencySchema = z.object({
  task_id: z.string(),
  dependency_type: z.string(),
});

export const WorkflowMetadataSchema = z.object({
  workflow_id: z.string().nullable().optional(),
  requires_design: z.boolean().optional().default(false),
  requires_architecture: z.boolean().optional().default(false),
  requires_qa: z.boolean().optional().default(false),
  requires_staging_deploy: z.boolean().optional().default(false),
  requires_production_deploy: z.boolean().optional().default(false),
});

export const DispatchHistoryEntrySchema = z.object({
  workflow_id: z.string(),
  started_at: z.string(),
  ended_at: z.string().nullable().optional(),
  duration_secs: z.number().nullable().optional(),
  outcome: z.string(),
  failed_phase: z.string().nullable().optional(),
  failure_reason: z.string().nullable().optional(),
});

export const ResourceRequirementsSchema = z.object({
  max_cpu_percent: z.number().nullable().optional(),
  max_memory_mb: z.number().nullable().optional(),
  requires_network: z.boolean().optional().default(true),
});

export const TaskMetadataSchema = z.object({
  created_at: z.string(),
  updated_at: z.string(),
  created_by: z.string().default(""),
  updated_by: z.string().default(""),
  started_at: z.string().nullable().optional(),
  completed_at: z.string().nullable().optional(),
  version: z.number().default(1),
});

export const OrchestratorTaskSchema = z.object({
  id: z.string(),
  title: z.string(),
  description: z.string().default(""),
  type: TaskTypeSchema.default("feature"),
  status: TaskStatusSchema.default("backlog"),
  priority: PrioritySchema.default("medium"),
  risk: RiskLevelSchema.default("medium"),
  scope: ScopeSchema.default("medium"),
  complexity: ComplexitySchema.default("medium"),
  impact_area: z.array(ImpactAreaSchema).default([]),
  assignee: AssigneeSchema.default({ type: "unassigned" }),
  estimated_effort: z.string().nullable().optional(),
  deadline: z.string().nullable().optional(),
  paused: z.boolean().default(false),
  cancelled: z.boolean().default(false),
  blocked_reason: z.string().nullable().optional(),
  blocked_at: z.string().nullable().optional(),
  blocked_phase: z.string().nullable().optional(),
  blocked_by: z.string().nullable().optional(),
  dependencies: z.array(TaskDependencySchema).default([]),
  linked_requirements: z.array(z.string()).default([]),
  linked_architecture_entities: z.array(z.string()).default([]),
  worktree_path: z.string().nullable().optional(),
  branch_name: z.string().nullable().optional(),
  resolution: z.string().nullable().optional(),
  checklist: z.array(ChecklistItemSchema).default([]),
  tags: z.array(z.string()).default([]),
  workflow_metadata: WorkflowMetadataSchema.default({}),
  dispatch_history: z.array(DispatchHistoryEntrySchema).default([]),
  consecutive_dispatch_failures: z.number().nullable().optional(),
  last_dispatch_failure_at: z.string().nullable().optional(),
  resource_requirements: ResourceRequirementsSchema.default({}),
  metadata: TaskMetadataSchema,
});

export const RequirementPrioritySchema = z.enum(["must", "should", "could", "wont"]);

export const RequirementStatusSchema = z.enum([
  "draft",
  "refined",
  "planned",
  "in-progress",
  "done",
  "po-review",
  "em-review",
  "needs-rework",
  "approved",
  "implemented",
  "deprecated",
]);

export const RequirementCommentSchema = z.object({
  author: z.string(),
  content: z.string(),
  timestamp: z.string(),
  phase: z.string().nullable().optional(),
});

export const RequirementLinksSchema = z.object({
  tasks: z.array(z.string()).default([]),
  workflows: z.array(z.string()).default([]),
  tests: z.array(z.string()).default([]),
  mockups: z.array(z.string()).default([]),
  flows: z.array(z.string()).default([]),
  related_requirements: z.array(z.string()).default([]),
});

export const RequirementItemSchema = z.object({
  id: z.string(),
  title: z.string(),
  description: z.string().default(""),
  body: z.string().nullable().optional(),
  legacy_id: z.string().nullable().optional(),
  category: z.string().nullable().optional(),
  type: z.string().nullable().optional(),
  priority: RequirementPrioritySchema.default("should"),
  status: RequirementStatusSchema.default("draft"),
  acceptance_criteria: z.array(z.string()).default([]),
  tags: z.array(z.string()).default([]),
  links: RequirementLinksSchema.default({}),
  linked_task_ids: z.array(z.string()).default([]),
  source: z.string().default(""),
  comments: z.array(RequirementCommentSchema).default([]),
  relative_path: z.string().nullable().optional(),
  created_at: z.string(),
  updated_at: z.string(),
});

export const SyncRequestSchema = z.object({
  tasks: z.array(OrchestratorTaskSchema).default([]),
  requirements: z.array(RequirementItemSchema).default([]),
  since: z.string().nullable().optional(),
});

export type OrchestratorTask = z.infer<typeof OrchestratorTaskSchema>;
export type RequirementItem = z.infer<typeof RequirementItemSchema>;
export type SyncRequest = z.infer<typeof SyncRequestSchema>;
