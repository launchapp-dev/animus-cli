import type { Hono } from "hono";
import { db } from "../db/index.js";
import { requirements } from "../db/schema.js";
import { eq, and, sql } from "drizzle-orm";
import { requireAuth } from "../middleware/auth.js";
import { RequirementItemSchema } from "../types/index.js";

function requirementRowToJson(row: typeof requirements.$inferSelect) {
  return {
    id: row.id,
    title: row.title,
    description: row.description,
    body: row.body,
    legacy_id: row.legacyId,
    category: row.category,
    type: row.type,
    priority: row.priority,
    status: row.status,
    acceptance_criteria: row.acceptanceCriteria,
    tags: row.tags,
    links: row.links,
    linked_task_ids: row.linkedTaskIds,
    source: row.source,
    comments: row.comments,
    relative_path: row.relativePath,
    created_at: row.createdAt.toISOString(),
    updated_at: row.updatedAt.toISOString(),
  };
}

function requirementInputToRow(input: ReturnType<typeof RequirementItemSchema.parse>, projectId: string) {
  return {
    id: input.id,
    projectId,
    title: input.title,
    description: input.description,
    body: input.body ?? null,
    legacyId: input.legacy_id ?? null,
    category: input.category ?? null,
    type: input.type ?? null,
    priority: input.priority,
    status: input.status,
    acceptanceCriteria: input.acceptance_criteria,
    tags: input.tags,
    links: input.links as Record<string, string[]>,
    linkedTaskIds: input.linked_task_ids,
    source: input.source,
    comments: input.comments as Array<Record<string, unknown>>,
    relativePath: input.relative_path ?? null,
    createdAt: new Date(input.created_at),
    updatedAt: new Date(input.updated_at),
  };
}

export { requirementRowToJson, requirementInputToRow };

export function registerRequirementRoutes(app: Hono) {
  app.get("/api/projects/:projectId/requirements", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const status = c.req.query("status");
    const priority = c.req.query("priority");
    const category = c.req.query("category");
    const limit = parseInt(c.req.query("limit") || "100");
    const offset = parseInt(c.req.query("offset") || "0");

    const conditions = [eq(requirements.projectId, projectId)];
    if (status) conditions.push(eq(requirements.status, status));
    if (priority) conditions.push(eq(requirements.priority, priority));
    if (category) conditions.push(eq(requirements.category, category));

    const rows = await db
      .select()
      .from(requirements)
      .where(and(...conditions))
      .limit(limit)
      .offset(offset);

    const [countResult] = await db
      .select({ count: sql<number>`count(*)` })
      .from(requirements)
      .where(and(...conditions));

    return c.json({
      requirements: rows.map(requirementRowToJson),
      total: Number(countResult.count),
      limit,
      offset,
    });
  });

  app.get("/api/projects/:projectId/requirements/:reqId", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const reqId = c.req.param("reqId") as string;

    const [row] = await db
      .select()
      .from(requirements)
      .where(and(eq(requirements.id, reqId), eq(requirements.projectId, projectId)))
      .limit(1);

    if (!row) {
      return c.json({ error: "Requirement not found" }, 404);
    }

    return c.json({ requirement: requirementRowToJson(row) });
  });

  app.post("/api/projects/:projectId/requirements", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const body = await c.req.json();
    const input = RequirementItemSchema.parse(body);

    const [row] = await db
      .insert(requirements)
      .values(requirementInputToRow(input, projectId))
      .returning();

    return c.json({ requirement: requirementRowToJson(row) }, 201);
  });

  app.patch("/api/projects/:projectId/requirements/:reqId", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const reqId = c.req.param("reqId") as string;
    const body = await c.req.json();

    const setValues: Record<string, unknown> = { syncedAt: new Date() };
    if (body.title !== undefined) setValues.title = body.title;
    if (body.description !== undefined) setValues.description = body.description;
    if (body.body !== undefined) setValues.body = body.body;
    if (body.legacy_id !== undefined) setValues.legacyId = body.legacy_id;
    if (body.category !== undefined) setValues.category = body.category;
    if (body.type !== undefined) setValues.type = body.type;
    if (body.priority !== undefined) setValues.priority = body.priority;
    if (body.status !== undefined) setValues.status = body.status;
    if (body.acceptance_criteria !== undefined) setValues.acceptanceCriteria = body.acceptance_criteria;
    if (body.tags !== undefined) setValues.tags = body.tags;
    if (body.links !== undefined) setValues.links = body.links;
    if (body.linked_task_ids !== undefined) setValues.linkedTaskIds = body.linked_task_ids;
    if (body.source !== undefined) setValues.source = body.source;
    if (body.comments !== undefined) setValues.comments = body.comments;
    if (body.relative_path !== undefined) setValues.relativePath = body.relative_path;
    if (body.updated_at !== undefined) setValues.updatedAt = new Date(body.updated_at);

    const [row] = await db
      .update(requirements)
      .set(setValues)
      .where(and(eq(requirements.id, reqId), eq(requirements.projectId, projectId)))
      .returning();

    if (!row) {
      return c.json({ error: "Requirement not found" }, 404);
    }

    return c.json({ requirement: requirementRowToJson(row) });
  });

  app.delete("/api/projects/:projectId/requirements/:reqId", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const reqId = c.req.param("reqId") as string;

    await db
      .delete(requirements)
      .where(and(eq(requirements.id, reqId), eq(requirements.projectId, projectId)));

    return c.json({ ok: true });
  });
}
