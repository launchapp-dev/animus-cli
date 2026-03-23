import type { Hono } from "hono";
import { db } from "../db/index.js";
import { tasks, requirements } from "../db/schema.js";
import { eq, and, gte } from "drizzle-orm";
import { requireAuth } from "../middleware/auth.js";
import { SyncRequestSchema } from "../types/index.js";
import { taskRowToJson, taskInputToRow } from "./tasks.js";
import { requirementRowToJson, requirementInputToRow } from "./requirements.js";

export function registerSyncRoutes(app: Hono) {
  app.post("/api/projects/:projectId/sync", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;
    const body = await c.req.json();
    const input = SyncRequestSchema.parse(body);

    const conflicts: { type: "task" | "requirement"; id: string; reason: string }[] = [];
    const serverTime = new Date().toISOString();

    await db.transaction(async (tx) => {
      for (const task of input.tasks) {
        const [existing] = await tx
          .select()
          .from(tasks)
          .where(and(eq(tasks.id, task.id), eq(tasks.projectId, projectId)))
          .limit(1);

        const values = taskInputToRow(task, projectId);

        if (!existing) {
          await tx.insert(tasks).values(values);
        } else {
          const existingMeta = existing.metadata as { version?: number } | null;
          const incomingVersion = task.metadata.version ?? 1;
          const existingVersion = existingMeta?.version ?? 1;

          if (incomingVersion >= existingVersion) {
            await tx
              .update(tasks)
              .set({ ...values, syncedAt: new Date() })
              .where(and(eq(tasks.id, task.id), eq(tasks.projectId, projectId)));
          } else {
            conflicts.push({
              type: "task",
              id: task.id,
              reason: `server version ${existingVersion} > incoming ${incomingVersion}`,
            });
          }
        }
      }

      for (const req of input.requirements) {
        const [existing] = await tx
          .select()
          .from(requirements)
          .where(and(eq(requirements.id, req.id), eq(requirements.projectId, projectId)))
          .limit(1);

        const values = requirementInputToRow(req, projectId);

        if (!existing) {
          await tx.insert(requirements).values(values);
        } else {
          const incomingUpdated = new Date(req.updated_at).getTime();
          const existingUpdated = existing.updatedAt.getTime();

          if (incomingUpdated >= existingUpdated) {
            await tx
              .update(requirements)
              .set({ ...values, syncedAt: new Date() })
              .where(and(eq(requirements.id, req.id), eq(requirements.projectId, projectId)));
          } else {
            conflicts.push({
              type: "requirement",
              id: req.id,
              reason: `server updated_at is newer`,
            });
          }
        }
      }
    });

    const sinceDate = input.since ? new Date(input.since) : undefined;

    const taskConditions = [eq(tasks.projectId, projectId)];
    if (sinceDate) taskConditions.push(gte(tasks.syncedAt, sinceDate));

    const serverTasks = await db
      .select()
      .from(tasks)
      .where(and(...taskConditions));

    const reqConditions = [eq(requirements.projectId, projectId)];
    if (sinceDate) reqConditions.push(gte(requirements.syncedAt, sinceDate));

    const serverReqs = await db
      .select()
      .from(requirements)
      .where(and(...reqConditions));

    return c.json({
      tasks: serverTasks.map(taskRowToJson),
      requirements: serverReqs.map(requirementRowToJson),
      conflicts,
      server_time: serverTime,
    });
  });
}
