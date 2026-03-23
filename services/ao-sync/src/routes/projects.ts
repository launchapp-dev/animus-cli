import type { Hono } from "hono";
import { z } from "zod";
import { db } from "../db/index.js";
import { projects, member } from "../db/schema.js";
import { eq, inArray } from "drizzle-orm";
import { requireAuth } from "../middleware/auth.js";

const CreateProjectSchema = z.object({
  name: z.string().min(1),
  organizationId: z.string(),
  repoOriginUrl: z.string().min(1),
});

export function registerProjectRoutes(app: Hono) {
  app.use("/api/projects/*", requireAuth);
  app.use("/api/projects", requireAuth);

  app.get("/api/projects", async (c) => {
    const user = (c as any).get("user") as { id: string };

    const memberships = await db
      .select({ organizationId: member.organizationId })
      .from(member)
      .where(eq(member.userId, user.id));

    const orgIds = memberships.map((m) => m.organizationId);
    if (orgIds.length === 0) {
      return c.json({ projects: [] });
    }

    const results = await db
      .select()
      .from(projects)
      .where(inArray(projects.organizationId, orgIds));

    return c.json({ projects: results });
  });

  app.post("/api/projects", async (c) => {
    const body = await c.req.json();
    const input = CreateProjectSchema.parse(body);

    const [project] = await db
      .insert(projects)
      .values({
        name: input.name,
        organizationId: input.organizationId,
        repoOriginUrl: input.repoOriginUrl,
      })
      .returning();

    return c.json({ project }, 201);
  });

  app.get("/api/projects/by-repo", async (c) => {
    const url = c.req.query("url");
    if (!url) {
      return c.json({ error: "url query parameter required" }, 400);
    }

    const [project] = await db
      .select()
      .from(projects)
      .where(eq(projects.repoOriginUrl, url))
      .limit(1);

    if (!project) {
      return c.json({ error: "No project found for this repo URL" }, 404);
    }

    return c.json({ project });
  });

  app.get("/api/projects/:projectId", async (c) => {
    const projectId = c.req.param("projectId") as string;

    const [project] = await db
      .select()
      .from(projects)
      .where(eq(projects.id, projectId))
      .limit(1);

    if (!project) {
      return c.json({ error: "Project not found" }, 404);
    }

    return c.json({ project });
  });

  app.patch("/api/projects/:projectId", async (c) => {
    const projectId = c.req.param("projectId") as string;
    const body = await c.req.json();

    const setValues: Record<string, unknown> = { updatedAt: new Date() };
    if (body.name !== undefined) setValues.name = body.name;
    if (body.repoOriginUrl !== undefined) setValues.repoOriginUrl = body.repoOriginUrl;

    const [updated] = await db
      .update(projects)
      .set(setValues)
      .where(eq(projects.id, projectId))
      .returning();

    if (!updated) {
      return c.json({ error: "Project not found" }, 404);
    }

    return c.json({ project: updated });
  });

  app.delete("/api/projects/:projectId", async (c) => {
    const projectId = c.req.param("projectId") as string;

    await db.delete(projects).where(eq(projects.id, projectId));

    return c.json({ ok: true });
  });
}
