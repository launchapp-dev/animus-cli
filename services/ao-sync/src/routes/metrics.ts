import type { Hono } from "hono";
import { db } from "../db/index.js";
import { tasks, requirements } from "../db/schema.js";
import { eq, sql } from "drizzle-orm";
import { requireAuth } from "../middleware/auth.js";

export function registerMetricsRoutes(app: Hono) {
  app.get("/api/projects/:projectId/metrics", requireAuth, async (c) => {
    const projectId = c.req.param("projectId") as string;

    const [
      tasksByStatus,
      tasksByPriority,
      tasksByType,
      reqsByStatus,
      reqsByPriority,
      weeklyTimeline,
      monthlyTimeline,
      weeklyByStatus,
    ] = await Promise.all([
      db
        .select({ status: tasks.status, count: sql<number>`count(*)` })
        .from(tasks)
        .where(eq(tasks.projectId, projectId))
        .groupBy(tasks.status),
      db
        .select({ priority: tasks.priority, count: sql<number>`count(*)` })
        .from(tasks)
        .where(eq(tasks.projectId, projectId))
        .groupBy(tasks.priority),
      db
        .select({ type: tasks.type, count: sql<number>`count(*)` })
        .from(tasks)
        .where(eq(tasks.projectId, projectId))
        .groupBy(tasks.type),
      db
        .select({ status: requirements.status, count: sql<number>`count(*)` })
        .from(requirements)
        .where(eq(requirements.projectId, projectId))
        .groupBy(requirements.status),
      db
        .select({ priority: requirements.priority, count: sql<number>`count(*)` })
        .from(requirements)
        .where(eq(requirements.projectId, projectId))
        .groupBy(requirements.priority),
      db.execute(sql`
        SELECT
          date_trunc('week', (metadata->>'created_at')::timestamptz)::date AS week,
          count(*) AS created,
          count(*) FILTER (WHERE status = 'done') AS completed,
          count(*) FILTER (WHERE status = 'cancelled') AS cancelled
        FROM tasks
        WHERE project_id = ${projectId}
          AND metadata->>'created_at' IS NOT NULL
          AND (metadata->>'created_at')::timestamptz >= now() - interval '12 weeks'
        GROUP BY 1
        ORDER BY 1
      `),
      db.execute(sql`
        SELECT
          date_trunc('month', (metadata->>'created_at')::timestamptz)::date AS month,
          count(*) AS created,
          count(*) FILTER (WHERE status = 'done') AS completed,
          count(*) FILTER (WHERE status = 'cancelled') AS cancelled,
          count(*) FILTER (WHERE status = 'blocked') AS blocked
        FROM tasks
        WHERE project_id = ${projectId}
          AND metadata->>'created_at' IS NOT NULL
          AND (metadata->>'created_at')::timestamptz >= now() - interval '12 months'
        GROUP BY 1
        ORDER BY 1
      `),
      db.execute(sql`
        SELECT
          date_trunc('week', (metadata->>'created_at')::timestamptz)::date AS week,
          status,
          count(*) AS count
        FROM tasks
        WHERE project_id = ${projectId}
          AND metadata->>'created_at' IS NOT NULL
          AND (metadata->>'created_at')::timestamptz >= now() - interval '12 weeks'
        GROUP BY 1, 2
        ORDER BY 1
      `),
    ]);

    const totalTasks = tasksByStatus.reduce((sum, r) => sum + Number(r.count), 0);
    const totalReqs = reqsByStatus.reduce((sum, r) => sum + Number(r.count), 0);

    return c.json({
      tasks: {
        total: totalTasks,
        by_status: tasksByStatus.map((r) => ({ name: r.status, value: Number(r.count) })),
        by_priority: tasksByPriority.map((r) => ({ name: r.priority, value: Number(r.count) })),
        by_type: tasksByType.map((r) => ({ name: r.type, value: Number(r.count) })),
      },
      requirements: {
        total: totalReqs,
        by_status: reqsByStatus.map((r) => ({ name: r.status, value: Number(r.count) })),
        by_priority: reqsByPriority.map((r) => ({ name: r.priority, value: Number(r.count) })),
      },
      timeline: {
        weekly: (weeklyTimeline as any[]).map((r: any) => ({
          week: r.week,
          created: Number(r.created),
          completed: Number(r.completed),
          cancelled: Number(r.cancelled),
        })),
        monthly: (monthlyTimeline as any[]).map((r: any) => ({
          month: r.month,
          created: Number(r.created),
          completed: Number(r.completed),
          cancelled: Number(r.cancelled),
          blocked: Number(r.blocked),
        })),
        weekly_by_status: (weeklyByStatus as any[]).map((r: any) => ({
          week: r.week,
          status: r.status,
          count: Number(r.count),
        })),
      },
    });
  });
}
