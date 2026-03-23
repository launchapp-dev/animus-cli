import type { Hono } from "hono";
import { db } from "../db/index.js";
import { tasks } from "../db/schema.js";
import { eq, and, sql } from "drizzle-orm";
import { requireAuth } from "../middleware/auth.js";
import { OrchestratorTaskSchema } from "../types/index.js";

function taskRowToJson(row: typeof tasks.$inferSelect) {
  return {
    id: row.id,
    title: row.title,
    description: row.description,
    type: row.type,
    status: row.status,
    priority: row.priority,
    risk: row.risk,
    scope: row.scope,
    complexity: row.complexity,
    impact_area: row.impactArea,
    assignee: row.assignee,
    estimated_effort: row.estimatedEffort,
    deadline: row.deadline,
    paused: row.paused,
    cancelled: row.cancelled,
    blocked_reason: row.blockedReason,
    blocked_at: row.blockedAt,
    blocked_phase: row.blockedPhase,
    blocked_by: row.blockedBy,
    dependencies: row.dependencies,
    linked_requirements: row.linkedRequirements,
    linked_architecture_entities: row.linkedArchitectureEntities,
    worktree_path: row.worktreePath,
    branch_name: row.branchName,
    resolution: row.resolution,
    checklist: row.checklist,
    tags: row.tags,
    workflow_metadata: row.workflowMetadata,
    dispatch_history: row.dispatchHistory,
    consecutive_dispatch_failures: row.consecutiveDispatchFailures,
    last_dispatch_failure_at: row.lastDispatchFailureAt,
    resource_requirements: row.resourceRequirements,
    metadata: row.metadata,
  };
}

function taskInputToRow(input: ReturnType<typeof OrchestratorTaskSchema.parse>, projectId: string) {
  return {
    id: input.id,
    projectId,
    title: input.title,
    description: input.description,
    type: input.type,
    status: input.status,
    priority: input.priority,
    risk: input.risk,
    scope: input.scope,
    complexity: input.complexity,
    impactArea: input.impact_area,
    assignee: input.assignee as Record<string, unknown>,
    estimatedEffort: input.estimated_effort ?? null,
    deadline: input.deadline ?? null,
    paused: input.paused,
    cancelled: input.cancelled,
    blockedReason: input.blocked_reason ?? null,
    blockedAt: input.blocked_at ?? null,
    blockedPhase: input.blocked_phase ?? null,
    blockedBy: input.blocked_by ?? null,
    dependencies: input.dependencies,
    linkedRequirements: input.linked_requirements,
    linkedArchitectureEntities: input.linked_architecture_entities,
    worktreePath: input.worktree_path ?? null,
    branchName: input.branch_name ?? null,
    resolution: input.resolution ?? null,
    checklist: input.checklist as Array<Record<string, unknown>>,
    tags: input.tags,
    workflowMetadata: input.workflow_metadata as Record<string, unknown>,
    dispatchHistory: input.dispatch_history as Array<Record<string, unknown>>,
    consecutiveDispatchFailures: input.consecutive_dispatch_failures ?? null,
    lastDispatchFailureAt: input.last_dispatch_failure_at ?? null,
    resourceRequirements: input.resource_requirements as Record<string, unknown>,
    metadata: input.metadata as Record<string, unknown>,
  };
}

export { taskRowToJson, taskInputToRow };

export function registerTaskRoutes(app: Hono) {
  app.get("/api/projects/:projectId/tasks", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const status = c.req.query("status");
    const priority = c.req.query("priority");
    const type = c.req.query("type");
    const limit = parseInt(c.req.query("limit") || "100");
    const offset = parseInt(c.req.query("offset") || "0");

    const conditions = [eq(tasks.projectId, projectId)];
    if (status) conditions.push(eq(tasks.status, status));
    if (priority) conditions.push(eq(tasks.priority, priority));
    if (type) conditions.push(eq(tasks.type, type));

    const rows = await db
      .select()
      .from(tasks)
      .where(and(...conditions))
      .limit(limit)
      .offset(offset);

    const [countResult] = await db
      .select({ count: sql<number>`count(*)` })
      .from(tasks)
      .where(and(...conditions));

    return c.json({
      tasks: rows.map(taskRowToJson),
      total: Number(countResult.count),
      limit,
      offset,
    });
  });

  app.get("/api/projects/:projectId/tasks/:taskId", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const taskId = c.req.param("taskId") as string;

    const [row] = await db
      .select()
      .from(tasks)
      .where(and(eq(tasks.id, taskId), eq(tasks.projectId, projectId)))
      .limit(1);

    if (!row) {
      return c.json({ error: "Task not found" }, 404);
    }

    return c.json({ task: taskRowToJson(row) });
  });

  app.post("/api/projects/:projectId/tasks", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const body = await c.req.json();
    const input = OrchestratorTaskSchema.parse(body);

    const [row] = await db
      .insert(tasks)
      .values(taskInputToRow(input, projectId))
      .returning();

    return c.json({ task: taskRowToJson(row) }, 201);
  });

  app.patch("/api/projects/:projectId/tasks/:taskId", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const taskId = c.req.param("taskId") as string;
    const body = await c.req.json();

    const setValues: Record<string, unknown> = { syncedAt: new Date() };
    if (body.title !== undefined) setValues.title = body.title;
    if (body.description !== undefined) setValues.description = body.description;
    if (body.type !== undefined) setValues.type = body.type;
    if (body.status !== undefined) setValues.status = body.status;
    if (body.priority !== undefined) setValues.priority = body.priority;
    if (body.risk !== undefined) setValues.risk = body.risk;
    if (body.scope !== undefined) setValues.scope = body.scope;
    if (body.complexity !== undefined) setValues.complexity = body.complexity;
    if (body.impact_area !== undefined) setValues.impactArea = body.impact_area;
    if (body.assignee !== undefined) setValues.assignee = body.assignee;
    if (body.estimated_effort !== undefined) setValues.estimatedEffort = body.estimated_effort;
    if (body.deadline !== undefined) setValues.deadline = body.deadline;
    if (body.paused !== undefined) setValues.paused = body.paused;
    if (body.cancelled !== undefined) setValues.cancelled = body.cancelled;
    if (body.blocked_reason !== undefined) setValues.blockedReason = body.blocked_reason;
    if (body.blocked_at !== undefined) setValues.blockedAt = body.blocked_at;
    if (body.blocked_phase !== undefined) setValues.blockedPhase = body.blocked_phase;
    if (body.blocked_by !== undefined) setValues.blockedBy = body.blocked_by;
    if (body.dependencies !== undefined) setValues.dependencies = body.dependencies;
    if (body.linked_requirements !== undefined) setValues.linkedRequirements = body.linked_requirements;
    if (body.linked_architecture_entities !== undefined) setValues.linkedArchitectureEntities = body.linked_architecture_entities;
    if (body.worktree_path !== undefined) setValues.worktreePath = body.worktree_path;
    if (body.branch_name !== undefined) setValues.branchName = body.branch_name;
    if (body.resolution !== undefined) setValues.resolution = body.resolution;
    if (body.checklist !== undefined) setValues.checklist = body.checklist;
    if (body.tags !== undefined) setValues.tags = body.tags;
    if (body.workflow_metadata !== undefined) setValues.workflowMetadata = body.workflow_metadata;
    if (body.dispatch_history !== undefined) setValues.dispatchHistory = body.dispatch_history;
    if (body.consecutive_dispatch_failures !== undefined) setValues.consecutiveDispatchFailures = body.consecutive_dispatch_failures;
    if (body.last_dispatch_failure_at !== undefined) setValues.lastDispatchFailureAt = body.last_dispatch_failure_at;
    if (body.resource_requirements !== undefined) setValues.resourceRequirements = body.resource_requirements;
    if (body.metadata !== undefined) setValues.metadata = body.metadata;

    const [row] = await db
      .update(tasks)
      .set(setValues)
      .where(and(eq(tasks.id, taskId), eq(tasks.projectId, projectId)))
      .returning();

    if (!row) {
      return c.json({ error: "Task not found" }, 404);
    }

    return c.json({ task: taskRowToJson(row) });
  });

  app.delete("/api/projects/:projectId/tasks/:taskId", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const taskId = c.req.param("taskId") as string;

    await db
      .delete(tasks)
      .where(and(eq(tasks.id, taskId), eq(tasks.projectId, projectId)));

    return c.json({ ok: true });
  });
}
