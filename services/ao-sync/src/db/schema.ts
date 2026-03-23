export {
  user,
  session,
  account,
  verification,
  organization,
  member,
  invitation,
} from "./auth-schema.js";

import {
  pgTable,
  text,
  uuid,
  timestamp,
  boolean,
  jsonb,
  integer,
  primaryKey,
  index,
} from "drizzle-orm/pg-core";
import { organization } from "./auth-schema.js";

export const projects = pgTable("projects", {
  id: uuid("id").defaultRandom().primaryKey(),
  organizationId: text("organization_id")
    .notNull()
    .references(() => organization.id, { onDelete: "cascade" }),
  name: text("name").notNull(),
  repoOriginUrl: text("repo_origin_url").notNull(),
  createdAt: timestamp("created_at", { withTimezone: true }).defaultNow().notNull(),
  updatedAt: timestamp("updated_at", { withTimezone: true }).defaultNow().notNull(),
}, (table) => [
  index("projects_repo_origin_url_idx").on(table.repoOriginUrl),
]);

export const tasks = pgTable(
  "tasks",
  {
    id: text("id").notNull(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => projects.id, { onDelete: "cascade" }),
    title: text("title").notNull(),
    description: text("description").notNull().default(""),
    type: text("type").notNull().default("feature"),
    status: text("status").notNull().default("backlog"),
    priority: text("priority").notNull().default("medium"),
    risk: text("risk").default("medium"),
    scope: text("scope").default("medium"),
    complexity: text("complexity").default("medium"),
    impactArea: jsonb("impact_area").$type<string[]>().default([]),
    assignee: jsonb("assignee").$type<Record<string, unknown>>().default({ type: "unassigned" }),
    estimatedEffort: text("estimated_effort"),
    deadline: text("deadline"),
    paused: boolean("paused").default(false),
    cancelled: boolean("cancelled").default(false),
    blockedReason: text("blocked_reason"),
    blockedAt: text("blocked_at"),
    blockedPhase: text("blocked_phase"),
    blockedBy: text("blocked_by"),
    dependencies: jsonb("dependencies").$type<Array<{ task_id: string; dependency_type: string }>>().default([]),
    linkedRequirements: jsonb("linked_requirements").$type<string[]>().default([]),
    linkedArchitectureEntities: jsonb("linked_architecture_entities").$type<string[]>().default([]),
    worktreePath: text("worktree_path"),
    branchName: text("branch_name"),
    resolution: text("resolution"),
    checklist: jsonb("checklist").$type<Array<Record<string, unknown>>>().default([]),
    tags: jsonb("tags").$type<string[]>().default([]),
    workflowMetadata: jsonb("workflow_metadata").$type<Record<string, unknown>>().default({}),
    dispatchHistory: jsonb("dispatch_history").$type<Array<Record<string, unknown>>>().default([]),
    consecutiveDispatchFailures: integer("consecutive_dispatch_failures"),
    lastDispatchFailureAt: text("last_dispatch_failure_at"),
    resourceRequirements: jsonb("resource_requirements").$type<Record<string, unknown>>().default({}),
    metadata: jsonb("metadata").$type<Record<string, unknown>>().notNull().default({}),
    syncedAt: timestamp("synced_at", { withTimezone: true }).defaultNow().notNull(),
  },
  (table) => [
    primaryKey({ columns: [table.id, table.projectId] }),
    index("tasks_project_status_idx").on(table.projectId, table.status),
    index("tasks_project_priority_idx").on(table.projectId, table.priority),
    index("tasks_project_type_idx").on(table.projectId, table.type),
  ]
);

export const requirements = pgTable(
  "requirements",
  {
    id: text("id").notNull(),
    projectId: uuid("project_id")
      .notNull()
      .references(() => projects.id, { onDelete: "cascade" }),
    title: text("title").notNull(),
    description: text("description").notNull().default(""),
    body: text("body"),
    legacyId: text("legacy_id"),
    category: text("category"),
    type: text("type"),
    priority: text("priority").notNull().default("should"),
    status: text("status").notNull().default("draft"),
    acceptanceCriteria: jsonb("acceptance_criteria").$type<string[]>().default([]),
    tags: jsonb("tags").$type<string[]>().default([]),
    links: jsonb("links").$type<Record<string, string[]>>().default({}),
    linkedTaskIds: jsonb("linked_task_ids").$type<string[]>().default([]),
    source: text("source").default(""),
    comments: jsonb("comments").$type<Array<Record<string, unknown>>>().default([]),
    relativePath: text("relative_path"),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
    syncedAt: timestamp("synced_at", { withTimezone: true }).defaultNow().notNull(),
  },
  (table) => [
    primaryKey({ columns: [table.id, table.projectId] }),
    index("requirements_project_status_idx").on(table.projectId, table.status),
    index("requirements_project_priority_idx").on(table.projectId, table.priority),
  ]
);
